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

use std::thread;

use crate::engine::{MinStd, PIECE_ORDER};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::piece_sequence;

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
