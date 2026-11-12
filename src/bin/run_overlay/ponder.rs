//! Pondering: the heavy searches of the Setup Konbini guidance, run ahead on
//! background threads while the player thinks, as a chess engine ponders on
//! the opponent's time.
//!
//! Every search is a pure job (`Job`) keyed by its inputs, and its result is
//! kept in a shared table. The guidance asks the table first
//! (`Ponder::get_or_compute`): a result computed ahead is used at once, a job
//! a worker is running is waited for, and anything else is computed on the
//! caller's thread as before. Speculation therefore never changes what is
//! shown, only how soon. After each new state the main loop queues the
//! states the player is likely to reach next (`Predicted`: the piece placed
//! where the guide shows it, with each piece the 7-bag allows revealed in
//! NEXT), and the workers walk them through the same jobs `KonbiniGuide`
//! will ask for (`explore`).

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use pc_overlay_core::{
    Observation, Piece, board_is_empty,
    finder::Finder,
    konbini::{self, FoundSolve, KonbiniLibrary, StagedMove},
};

use super::{SOLVE_LIMIT, Shortlisted, shortlist_for, sort_rated};

/// A search, keyed by everything its result depends on. `lib` is the version
/// of the Setup Konbini data (`Ponder::set_library`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Job {
    TwoLine(Observation),
    PerfectClears(Observation),
    Staged(Observation, usize),
    Shortlist {
        lib: u64,
        stage: u8,
        observation: Observation,
        pc_pieces: Vec<Piece>,
    },
    Rate {
        lib: u64,
        setup: usize,
        observation: Observation,
        first_index: usize,
    },
}

#[derive(Debug, Clone)]
pub enum JobResult {
    Solves(Vec<FoundSolve>),
    Staged(Vec<StagedMove>),
    Shortlist(Vec<Shortlisted>),
    Rate(Option<f32>),
}

impl JobResult {
    pub fn solves(self) -> Vec<FoundSolve> {
        match self {
            Self::Solves(solves) => solves,
            other => unreachable!("not solves: {other:?}"),
        }
    }

    pub fn staged(self) -> Vec<StagedMove> {
        match self {
            Self::Staged(moves) => moves,
            other => unreachable!("not staged moves: {other:?}"),
        }
    }

    pub fn shortlist(self) -> Vec<Shortlisted> {
        match self {
            Self::Shortlist(shortlist) => shortlist,
            other => unreachable!("not a shortlist: {other:?}"),
        }
    }

    pub fn rate(self) -> Option<f32> {
        match self {
            Self::Rate(rate) => rate,
            other => unreachable!("not a rate: {other:?}"),
        }
    }
}

/// Runs a job. The one place results come from, on the main thread and the
/// workers alike, so a result does not depend on who computed it.
pub fn run(job: &Job, finder: &mut Finder, library: Option<&KonbiniLibrary>) -> JobResult {
    match job {
        Job::TwoLine(observation) => JobResult::Solves(konbini::find_two_line_perfect_clears(
            finder,
            observation,
            SOLVE_LIMIT,
        )),
        Job::PerfectClears(observation) => JobResult::Solves(konbini::find_perfect_clears(
            finder,
            observation,
            SOLVE_LIMIT,
        )),
        Job::Staged(observation, first_index) => {
            JobResult::Staged(konbini::staged_moves(finder, observation, *first_index))
        }
        Job::Shortlist {
            stage,
            observation,
            pc_pieces,
            ..
        } => JobResult::Shortlist(library.map_or_else(Vec::new, |library| {
            shortlist_for(library, finder, *stage, observation, pc_pieces)
        })),
        Job::Rate {
            setup,
            observation,
            first_index,
            ..
        } => JobResult::Rate(library.and_then(|library| {
            let setup = library.setups.get(*setup)?;
            // As `KonbiniLibrary::candidates` derives it.
            let progress = setup.progress(&observation.board)?;
            konbini::state_pc_rate(finder, setup, &progress, observation, *first_index)
        })),
    }
}

/// A state the player is likely to reach from the one on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicted {
    pub stage: u8,
    pub observation: Observation,
    pub pc_pieces: Vec<Piece>,
}

#[derive(Debug, Clone)]
enum Task {
    Job(Job),
    Explore(Predicted),
}

#[derive(Debug)]
enum Entry {
    Running,
    Done {
        result: Arc<JobResult>,
        /// The generation it was last used in, for eviction.
        touched: u64,
    },
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<Job, Entry>,
    queue: VecDeque<Task>,
    /// Moves on with every new state on screen; queued work of older
    /// generations is dropped.
    generation: u64,
    library: Option<(u64, Arc<KonbiniLibrary>)>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    /// Signals queued work to the workers.
    work: Condvar,
    /// Signals a finished (or abandoned) job to the threads waiting for it.
    done: Condvar,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panicking job never holds the lock (jobs run unlocked), so a
        // poisoned table is still consistent.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// How the work queued by `submit_*` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Not at all, and nothing is kept: every job runs on the caller's
    /// thread, as before pondering.
    Disabled,
    /// On background threads.
    Threads,
    /// At once on the caller's thread, so tests are deterministic.
    #[cfg(test)]
    Inline,
}

/// The longest the main thread waits for a job a worker is running before
/// computing it itself.
const WAIT_LIMIT: Duration = Duration::from_millis(250);

/// Generations a result is kept after its last use.
const KEEP_GENERATIONS: u64 = 8;

/// Move-generation cache entries a worker keeps before dropping them.
const WORKER_FINDER_CACHE_LIMIT: usize = 100_000;

/// Worker threads at most. TETR.IO needs the CPU too.
const MAX_WORKERS: usize = 3;

#[derive(Debug, Clone)]
pub struct Ponder {
    shared: Arc<Shared>,
    mode: Mode,
}

impl Ponder {
    pub fn disabled() -> Self {
        Self {
            shared: Arc::default(),
            mode: Mode::Disabled,
        }
    }

    #[cfg(test)]
    pub fn inline() -> Self {
        Self {
            shared: Arc::default(),
            mode: Mode::Inline,
        }
    }

    /// Starts the workers: half the cores, 1 to `MAX_WORKERS`, below normal
    /// priority so recognition and TETR.IO come first.
    pub fn spawn() -> Self {
        let ponder = Self {
            shared: Arc::default(),
            mode: Mode::Threads,
        };
        let workers = thread::available_parallelism()
            .map_or(1, |cores| cores.get() / 2)
            .clamp(1, MAX_WORKERS);
        for index in 0..workers {
            let shared = Arc::clone(&ponder.shared);
            let spawned = thread::Builder::new()
                .name(format!("ponder-{index}"))
                .spawn(move || {
                    lower_priority();
                    worker(&shared);
                });
            if let Err(error) = spawned {
                eprintln!("pondering thread not started: {error}");
            }
        }
        ponder
    }

    pub fn enabled(&self) -> bool {
        self.mode != Mode::Disabled
    }

    /// Results used from the table, and jobs computed on the caller's thread.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.shared.hits.load(Ordering::Relaxed),
            self.shared.misses.load(Ordering::Relaxed),
        )
    }

    /// The Setup Konbini data the workers search, as version `lib`. The
    /// results of other versions are dropped (their keys name the version,
    /// so a late result of an old one is never used).
    pub fn set_library(&self, lib: u64, library: Arc<KonbiniLibrary>) {
        let mut state = self.shared.lock();
        state.library = Some((lib, library));
        state
            .entries
            .retain(|_, entry| matches!(entry, Entry::Running));
        state.queue.clear();
    }

    /// A new state is on screen: queued work for the old one is dropped (a
    /// running job finishes), and results unused for a while are evicted.
    pub fn new_state(&self) {
        let mut state = self.shared.lock();
        state.generation += 1;
        state.queue.clear();
        let generation = state.generation;
        state.entries.retain(|_, entry| match entry {
            Entry::Running => true,
            Entry::Done { touched, .. } => *touched + KEEP_GENERATIONS >= generation,
        });
    }

    /// The job's result: from the table, from a worker running it (waiting
    /// at most `WAIT_LIMIT`), or computed here.
    pub fn get_or_compute(
        &self,
        job: Job,
        finder: &mut Finder,
        library: Option<&KonbiniLibrary>,
    ) -> JobResult {
        if !self.enabled() {
            return run(&job, finder, library);
        }
        self.lookup(job, finder, library, Some(WAIT_LIMIT))
            .expect("computed when not waited for")
    }

    /// As `get_or_compute`, but `None` instead of waiting for a worker.
    pub fn try_get_or_steal(
        &self,
        job: Job,
        finder: &mut Finder,
        library: Option<&KonbiniLibrary>,
    ) -> Option<JobResult> {
        if !self.enabled() {
            return Some(run(&job, finder, library));
        }
        self.lookup(job, finder, library, None)
    }

    /// The job's result when it is already in the table.
    pub fn peek(&self, job: &Job) -> Option<JobResult> {
        let mut state = self.shared.lock();
        let generation = state.generation;
        match state.entries.get_mut(job) {
            Some(Entry::Done { result, touched }) => {
                *touched = generation;
                Some((**result).clone())
            }
            _ => None,
        }
    }

    /// Queues jobs ahead of the predicted states.
    pub fn submit_front(&self, jobs: Vec<Job>, library: Option<&KonbiniLibrary>) {
        match self.mode {
            Mode::Disabled => {}
            Mode::Threads => {
                let mut state = self.shared.lock();
                for job in jobs.into_iter().rev() {
                    if !state.entries.contains_key(&job) {
                        state.queue.push_front(Task::Job(job));
                    }
                }
                self.shared.work.notify_all();
            }
            #[cfg(test)]
            Mode::Inline => {
                let mut finder = Finder::new();
                for job in jobs {
                    self.get_or_compute(job, &mut finder, library);
                }
            }
        }
        let _ = library;
    }

    /// Queues a predicted state for the workers to explore.
    pub fn submit_explore(&self, predicted: Predicted) {
        match self.mode {
            Mode::Disabled => {}
            Mode::Threads => {
                self.shared.lock().queue.push_back(Task::Explore(predicted));
                self.shared.work.notify_one();
            }
            #[cfg(test)]
            Mode::Inline => {
                let (generation, library) = {
                    let state = self.shared.lock();
                    (state.generation, state.library.clone())
                };
                explore(self, &mut Finder::new(), generation, library, &predicted);
            }
        }
    }

    fn lookup(
        &self,
        job: Job,
        finder: &mut Finder,
        library: Option<&KonbiniLibrary>,
        wait: Option<Duration>,
    ) -> Option<JobResult> {
        let mut state = self.shared.lock();
        let deadline = wait.map(|wait| Instant::now() + wait);
        loop {
            let generation = state.generation;
            match state.entries.get_mut(&job) {
                Some(Entry::Done { result, touched }) => {
                    *touched = generation;
                    self.shared.hits.fetch_add(1, Ordering::Relaxed);
                    return Some((**result).clone());
                }
                Some(Entry::Running) => {
                    let deadline = deadline?;
                    let now = Instant::now();
                    if now >= deadline {
                        // Too slow: compute it here too. The result is the
                        // same, so either insert may win.
                        break;
                    }
                    state = self
                        .shared
                        .done
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
                None => {
                    state.entries.insert(job.clone(), Entry::Running);
                    break;
                }
            }
        }
        drop(state);
        self.shared.misses.fetch_add(1, Ordering::Relaxed);
        Some(self.compute(job, finder, library))
    }

    /// Runs a job marked running, unlocked, and stores the result.
    fn compute(
        &self,
        job: Job,
        finder: &mut Finder,
        library: Option<&KonbiniLibrary>,
    ) -> JobResult {
        let guard = RunningGuard {
            shared: &self.shared,
            job: Some(job),
        };
        let result = run(guard.job.as_ref().expect("set above"), finder, library);
        guard.finish(Arc::new(result.clone()));
        result
    }
}

/// Clears a job's `Running` mark when its computation panics, so others do
/// not wait for it.
struct RunningGuard<'a> {
    shared: &'a Shared,
    job: Option<Job>,
}

impl RunningGuard<'_> {
    fn finish(mut self, result: Arc<JobResult>) {
        let job = self.job.take().expect("finished once");
        let mut state = self.shared.lock();
        let touched = state.generation;
        state.entries.insert(job, Entry::Done { result, touched });
        drop(state);
        self.shared.done.notify_all();
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            let mut state = self.shared.lock();
            if matches!(state.entries.get(&job), Some(Entry::Running)) {
                state.entries.remove(&job);
            }
            drop(state);
            self.shared.done.notify_all();
        }
    }
}

fn worker(shared: &Arc<Shared>) {
    let ponder = Ponder {
        shared: Arc::clone(shared),
        mode: Mode::Threads,
    };
    // `Finder` is not `Send`: each worker makes its own.
    let mut finder = Finder::new();
    loop {
        let (task, generation, library) = {
            let mut state = shared.lock();
            loop {
                if let Some(task) = state.queue.pop_front() {
                    break (task, state.generation, state.library.clone());
                }
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        if finder.cached_moves() > WORKER_FINDER_CACHE_LIMIT {
            finder.clear_cache();
        }
        let lib = library.as_ref().map(|(_, library)| &**library);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &task {
            Task::Job(job) => {
                ponder.get_or_compute(job.clone(), &mut finder, lib);
            }
            Task::Explore(predicted) => {
                explore(&ponder, &mut finder, generation, library.clone(), predicted);
            }
        }));
        if outcome.is_err() {
            // A search bug must not stop pondering; the main thread will
            // hit the same job and report it.
            finder = Finder::new();
        }
    }
}

/// Walks a predicted state through the jobs `KonbiniGuide::guide` will ask
/// for there, in its order: 2-line PCs (which win), the candidate setups and
/// their PC rates, then for a built setup or a board no setup explains, the
/// perfect clears or, without one, the next placement. Stops when the state
/// on screen moves on.
fn explore(
    ponder: &Ponder,
    finder: &mut Finder,
    generation: u64,
    library: Option<(u64, Arc<KonbiniLibrary>)>,
    predicted: &Predicted,
) {
    let current = || ponder.shared.lock().generation == generation;
    let observation = &predicted.observation;
    let two_line = ponder.get_or_compute(Job::TwoLine(observation.clone()), finder, None);
    if !two_line.solves().is_empty() || !current() {
        return;
    }
    let Some((lib, library)) = library else {
        return;
    };
    let stage = predicted.stage;
    let mut shortlist = ponder
        .get_or_compute(
            Job::Shortlist {
                lib,
                stage,
                observation: observation.clone(),
                pc_pieces: predicted.pc_pieces.clone(),
            },
            finder,
            Some(&library),
        )
        .shortlist();
    // As for the PC rate: 10 pieces per earlier PC.
    let before = 10 * usize::from(stage - 1);
    let hold = usize::from(observation.hold.is_some());
    for candidate in &mut shortlist {
        if !current() {
            return;
        }
        let rate = ponder.get_or_compute(
            Job::Rate {
                lib,
                setup: candidate.setup,
                observation: observation.clone(),
                first_index: before + candidate.progress.placed_pieces() + hold,
            },
            finder,
            Some(&library),
        );
        candidate.rate = Some(rate.rate());
    }
    // The guide shows the first candidate once all are rated.
    sort_rated(&mut shortlist);
    let complete = shortlist
        .first()
        .filter(|candidate| candidate.progress.complete());
    let off_setup = shortlist.is_empty() && !board_is_empty(&observation.board);
    if (complete.is_none() && !off_setup) || !current() {
        return;
    }
    let solves = ponder
        .get_or_compute(Job::PerfectClears(observation.clone()), finder, None)
        .solves();
    if !solves.is_empty() || !current() {
        return;
    }
    let placed = match complete {
        Some(candidate) => Some(candidate.progress.placed_pieces()),
        None => {
            let filled = observation.board.iter().flatten().flatten().count();
            filled.is_multiple_of(4).then_some(filled / 4)
        }
    };
    if let Some(placed) = placed {
        ponder.get_or_compute(
            Job::Staged(observation.clone(), before + placed + hold),
            finder,
            None,
        );
    }
}

#[cfg(windows)]
fn lower_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: the pseudo handle of the calling thread needs no closing.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn lower_priority() {}
