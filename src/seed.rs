//! Finding the piece seed of a TETR.IO game.
//!
//! A replay records its seed (`options.seed`, see [`crate::ttrm::stream_seed`]).
//! During live play the seed is not shown, but MINSTD has only 2^31 - 2
//! states, so the seeds consistent with the pieces seen so far can be found
//! by brute force. Each full bag carries log2(7!) ~ 12.3 bits, so about three
//! bags (21 pieces) usually leave a single candidate; the full queue of the
//! game then follows from [`crate::engine::piece_sequence`].
//!
//! Seeds are equivalent modulo 2^31 - 1, so the search returns the canonical
//! MINSTD state in `1..2^31 - 1`.

use std::{
    sync::mpsc::{self, Receiver},
    thread,
};

use crate::engine::{MinStd, PIECE_ORDER, PieceStream};
use crate::model::Piece;

/// Result of a seed search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSearch {
    /// Matching seeds in ascending order, at most the requested limit.
    pub seeds: Vec<u32>,
    /// True when more seeds matched than were returned.
    pub truncated: bool,
}

/// Every seed whose spawn order starts with `observed`.
///
/// `observed` is the order pieces leave the queue (Current, then NEXT in
/// order), independent of Hold. An empty observation matches every seed and
/// is rejected by returning an empty, truncated result.
pub fn recover_seeds(observed: &[Piece], limit: usize) -> SeedSearch {
    if observed.is_empty() {
        return SeedSearch {
            seeds: Vec::new(),
            truncated: true,
        };
    }
    let threads = thread::available_parallelism().map_or(4, |n| n.get());
    let last = MinStd::MODULUS - 1; // inclusive upper state
    let chunk = last.div_ceil(threads as u64);
    let mut seeds = Vec::new();
    let mut truncated = false;
    thread::scope(|scope| {
        let handles: Vec<_> = (0..threads as u64)
            .map(|t| {
                let start = 1 + t * chunk;
                let end = (start + chunk).min(last + 1);
                scope.spawn(move || search_range(observed, start, end, limit + 1))
            })
            .collect();
        for handle in handles {
            seeds.extend(handle.join().expect("seed search thread"));
        }
    });
    if seeds.len() > limit {
        seeds.truncate(limit);
        truncated = true;
    }
    SeedSearch { seeds, truncated }
}

fn search_range(observed: &[Piece], start: u64, end: u64, limit: usize) -> Vec<u32> {
    let mut found = Vec::new();
    for seed in start..end {
        if matches_seed(observed, seed) {
            found.push(seed as u32);
            if found.len() >= limit {
                break;
            }
        }
    }
    found
}

/// Shuffles bag after bag, rejecting as soon as a fixed position disagrees.
/// The client's Fisher-Yates fills positions 6, 5, ..., 1 first, so a fully
/// observed bag is usually rejected after one draw.
fn matches_seed(observed: &[Piece], seed: u64) -> bool {
    let mut rng = MinStd::new(seed as i64);
    for bag_observed in observed.chunks(7) {
        let mut bag = PIECE_ORDER;
        for i in (1..bag.len()).rev() {
            let j = (rng.next_float() * (i + 1) as f64) as usize;
            bag.swap(i, j);
            if i < bag_observed.len() && bag[i] != bag_observed[i] {
                return false;
            }
        }
        if bag[0] != bag_observed[0] {
            return false;
        }
    }
    true
}

/// Pieces seen from the game start before the first search. Seven leave
/// about 2^31 / 7! seeds; two bags leave about a hundred.
pub const SEARCH_FROM: usize = 14;
/// Seeds a search returns to be narrowed as more pieces show, without
/// searching again.
pub const CANDIDATE_LIMIT: usize = 4096;
/// How far into a confirmed seed's queue a run seen mid-game (after a
/// resync) is looked for.
pub const LOCATE_RANGE: usize = 2000;

/// The search `LiveSeed` runs on its own thread; `recover_seeds` in play.
pub type SearchFn = fn(&[Piece], usize) -> SeedSearch;

/// The seed of the game being played, estimated from the pieces the screen
/// showed (`SessionTracker::spawn_run`), so the queue after NEXT is known.
///
/// The search scans every MINSTD state (about 1.3 s on all cores) on its
/// own thread, so the frame loop keeps running; until it reports, and while
/// several seeds match, nothing is predicted. A confirmed seed is kept for
/// the game (Undo included) and dropped by `reset` at a new game, or as soon
/// as a piece shown disagrees with it.
pub struct LiveSeed {
    search: SearchFn,
    /// Bumped by `reset`, so a search started in an earlier game is ignored.
    generation: u64,
    pending: Option<(u64, usize, Receiver<SeedSearch>)>,
    /// The seeds the last complete search left, narrowed as pieces show.
    candidates: Option<Vec<u32>>,
    /// Pieces the last search used when it matched more than the limit.
    truncated_at: Option<usize>,
    confirmed: Option<Confirmed>,
    discarded: bool,
    /// The run of the last update: its length and whether it starts at the
    /// game start.
    seen: (usize, bool),
}

struct Confirmed {
    seed: u32,
    stream: PieceStream,
    queue: Vec<Piece>,
    /// Where in `queue` the last run ends, when the run was placed.
    run_end: Option<usize>,
}

impl Confirmed {
    fn new(seed: u32) -> Self {
        Self {
            seed,
            stream: PieceStream::new(i64::from(seed)),
            queue: Vec::new(),
            run_end: None,
        }
    }

    fn fill(&mut self, len: usize) {
        while self.queue.len() < len {
            self.queue.push(self.stream.pop());
        }
    }

    /// Places `run` in the queue: at the start when it begins with the
    /// game, else the only place it occurs within `LOCATE_RANGE`. `None`
    /// when it occurs nowhere (the seed is wrong), `Some(None)` when it
    /// occurs more than once.
    fn locate(&mut self, run: &[Piece], from_start: bool) -> Option<Option<usize>> {
        if from_start {
            self.fill(run.len());
            return (self.queue[..run.len()] == *run).then_some(Some(0));
        }
        self.fill(LOCATE_RANGE + run.len());
        let queue = &self.queue;
        let mut found = (0..LOCATE_RANGE).filter(|start| queue[*start..][..run.len()] == *run);
        match (found.next(), found.next()) {
            (None, _) => None,
            (Some(start), None) => Some(Some(start)),
            _ => Some(None),
        }
    }
}

impl Default for LiveSeed {
    fn default() -> Self {
        Self::with_search(recover_seeds)
    }
}

impl LiveSeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// With another search, such as a narrow one for tests.
    pub fn with_search(search: SearchFn) -> Self {
        Self {
            search,
            generation: 0,
            pending: None,
            candidates: None,
            truncated_at: None,
            confirmed: None,
            discarded: false,
            seen: (0, false),
        }
    }

    /// A new game: everything learned about the old seed is dropped.
    pub fn reset(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Self::with_search(self.search)
        };
    }

    /// The confirmed seed, if any.
    pub fn seed(&self) -> Option<u32> {
        self.confirmed.as_ref().map(|confirmed| confirmed.seed)
    }

    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    /// Takes in the latest run of the game queue (`SessionTracker::spawn_run`):
    /// collects a finished search, starts one when there are enough pieces,
    /// narrows the candidates, and checks a confirmed seed against the run.
    pub fn update(&mut self, run: &[Piece], from_start: bool) {
        self.seen = (run.len(), from_start);
        self.poll();
        if self.discarded || run.is_empty() {
            return;
        }
        if let Some(confirmed) = self.confirmed.as_mut() {
            match confirmed.locate(run, from_start) {
                Some(start) => confirmed.run_end = start.map(|start| start + run.len()),
                // NEXT shows a piece the seed does not deal.
                None => {
                    self.confirmed = None;
                    self.discarded = true;
                }
            }
            return;
        }
        if !from_start {
            return;
        }
        if let Some(candidates) = self.candidates.as_mut() {
            candidates.retain(|seed| matches_seed(run, u64::from(*seed)));
            if let [seed] = candidates[..] {
                let mut confirmed = Confirmed::new(seed);
                confirmed.run_end = confirmed
                    .locate(run, true)
                    .flatten()
                    .map(|start| start + run.len());
                self.confirmed = Some(confirmed);
            }
            return;
        }
        let due = self
            .truncated_at
            .map_or(SEARCH_FROM, |searched| searched + PIECE_ORDER.len());
        if self.pending.is_none() && run.len() >= due {
            let (sender, receiver) = mpsc::channel();
            let observed = run.to_vec();
            let search = self.search;
            thread::spawn(move || {
                let _ = sender.send(search(&observed, CANDIDATE_LIMIT));
            });
            self.pending = Some((self.generation, run.len(), receiver));
        }
    }

    /// Blocks until a running search reports; for tests and tools.
    pub fn wait(&mut self) {
        if let Some((generation, len, receiver)) = self.pending.take()
            && let Ok(result) = receiver.recv()
        {
            self.accept(generation, len, result);
        }
    }

    fn poll(&mut self) {
        let Some((generation, len, receiver)) = &self.pending else {
            return;
        };
        let (generation, len) = (*generation, *len);
        match receiver.try_recv() {
            Ok(result) => {
                self.pending = None;
                self.accept(generation, len, result);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.pending = None,
        }
    }

    fn accept(&mut self, generation: u64, len: usize, result: SeedSearch) {
        if generation != self.generation {
            return;
        }
        if result.truncated {
            self.truncated_at = Some(len);
        } else {
            self.candidates = Some(result.seeds);
        }
    }

    /// The `count` pieces after the last run, from the confirmed seed, or
    /// empty while the seed or its place in the queue is unknown.
    pub fn later(&mut self, count: usize) -> Vec<Piece> {
        let Some(confirmed) = self.confirmed.as_mut() else {
            return Vec::new();
        };
        let Some(end) = confirmed.run_end else {
            return Vec::new();
        };
        confirmed.fill(end + count);
        confirmed.queue[end..end + count].to_vec()
    }

    /// The state of the estimate, for the status line.
    pub fn status(&self) -> String {
        let (seen, from_start) = self.seen;
        if self.discarded {
            return "seed discarded: NEXT disagreed".to_string();
        }
        if let Some(confirmed) = &self.confirmed {
            return match confirmed.run_end {
                Some(_) => format!("seed {} in use", confirmed.seed),
                None => format!("seed {} found; place in the queue unknown", confirmed.seed),
            };
        }
        if !from_start {
            return "seed: game start not seen (retry with R)".to_string();
        }
        if self.pending.is_some() {
            return format!("seed: searching {seen} pieces");
        }
        match &self.candidates {
            Some(candidates) if candidates.is_empty() => {
                format!("seed: none matches {seen} pieces")
            }
            Some(candidates) => {
                format!("seed: {} candidates, {seen} pieces seen", candidates.len())
            }
            None => match self.truncated_at {
                Some(searched) => format!(
                    "seed: too many candidates, {seen}/{} pieces seen",
                    searched + PIECE_ORDER.len()
                ),
                None => format!("seed: {seen}/{SEARCH_FROM} pieces seen"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::piece_sequence;

    const SEED: u32 = 1_002_385_019;

    /// The full search restricted to seeds near `SEED`, so a test takes
    /// milliseconds; the other seeds there stand in for the rest.
    fn nearby(observed: &[Piece], limit: usize) -> SeedSearch {
        let start = u64::from(SEED) - 200_000;
        let mut seeds = search_range(observed, start, start + 400_000, limit + 1);
        let truncated = seeds.len() > limit;
        seeds.truncate(limit);
        SeedSearch { seeds, truncated }
    }

    fn settled(live: &mut LiveSeed, run: &[Piece]) {
        live.update(run, true);
        live.wait();
        live.update(run, true);
    }

    #[test]
    fn a_seed_is_confirmed_from_the_game_start_and_predicts_the_queue() {
        let pieces = piece_sequence(i64::from(SEED), 40);
        let mut live = LiveSeed::with_search(nearby);
        live.update(&pieces[..13], true);
        assert!(!live.searching());
        assert_eq!(live.status(), "seed: 13/14 pieces seen");
        settled(&mut live, &pieces[..21]);
        assert_eq!(live.seed(), Some(SEED));
        assert_eq!(live.later(4), pieces[21..25]);
        // Undo shortens the run; the seed stays.
        live.update(&pieces[..18], true);
        assert_eq!(live.later(2), pieces[18..20]);
    }

    #[test]
    fn candidates_are_narrowed_as_pieces_show_without_searching_again() {
        let pieces = piece_sequence(i64::from(SEED), 40);
        let mut live = LiveSeed::with_search(nearby);
        live.update(&pieces[..14], true);
        assert!(live.searching());
        live.wait();
        // The 14 pieces the search used leave a few seeds nearby.
        let mut few = LiveSeed::with_search(nearby);
        few.candidates = Some(nearby(&pieces[..7], 64).seeds);
        assert!(few.candidates.as_ref().unwrap().len() > 1);
        few.seen = (7, true);
        assert!(few.status().ends_with("candidates, 7 pieces seen"));
        few.update(&pieces[..28], true);
        assert!(!few.searching());
        assert_eq!(few.seed(), Some(SEED));
        assert_eq!(few.later(1), pieces[28..29]);
    }

    #[test]
    fn several_seeds_predict_nothing() {
        let pieces = piece_sequence(i64::from(SEED), 40);
        let mut live = LiveSeed::with_search(nearby);
        // Seeds that all deal the 7 pieces seen.
        live.candidates = Some(nearby(&pieces[..7], 64).seeds.into_iter().take(3).collect());
        live.update(&pieces[..7], true);
        assert_eq!(live.seed(), None);
        assert!(live.later(2).is_empty());
    }

    #[test]
    fn a_disagreeing_next_discards_the_seed() {
        let pieces = piece_sequence(i64::from(SEED), 40);
        let mut live = LiveSeed::with_search(nearby);
        settled(&mut live, &pieces[..21]);
        assert_eq!(live.seed(), Some(SEED));
        let mut misread = pieces[..22].to_vec();
        misread[21] = PIECE_ORDER
            .into_iter()
            .find(|piece| *piece != pieces[21])
            .unwrap();
        live.update(&misread, true);
        assert_eq!(live.seed(), None);
        assert!(live.later(3).is_empty());
        assert_eq!(live.status(), "seed discarded: NEXT disagreed");
        // A new game starts over.
        live.reset();
        live.update(&pieces[..3], true);
        assert_eq!(live.status(), "seed: 3/14 pieces seen");
    }

    #[test]
    fn a_run_after_a_resync_is_placed_in_the_confirmed_queue() {
        let pieces = piece_sequence(i64::from(SEED), 200);
        let mut live = LiveSeed::with_search(nearby);
        settled(&mut live, &pieces[..21]);
        live.update(&pieces[100..130], false);
        assert_eq!(live.later(3), pieces[130..133]);
        // Without a seed, a run that does not start the game is not searched.
        let mut fresh = LiveSeed::with_search(nearby);
        fresh.update(&pieces[100..130], false);
        assert!(!fresh.searching());
        assert_eq!(fresh.status(), "seed: game start not seen (retry with R)");
    }

    #[test]
    fn a_search_from_an_earlier_game_is_ignored() {
        let pieces = piece_sequence(i64::from(SEED), 40);
        let mut live = LiveSeed::with_search(nearby);
        live.update(&pieces[..21], true);
        let pending = live.pending.take();
        live.reset();
        live.pending = pending;
        live.wait();
        live.update(&pieces[..5], true);
        assert_eq!(live.seed(), None);
        assert_eq!(live.status(), "seed: 5/14 pieces seen");
    }

    #[test]
    fn matches_seed_agrees_with_the_piece_stream() {
        let seed = 1_002_385_019u64;
        let pieces = piece_sequence(seed as i64, 30);
        for len in [1, 5, 7, 13, 30] {
            assert!(matches_seed(&pieces[..len], seed));
        }
        let mut wrong = pieces.clone();
        wrong.swap(0, 1);
        assert!(!matches_seed(&wrong[..7], seed));
    }
}
