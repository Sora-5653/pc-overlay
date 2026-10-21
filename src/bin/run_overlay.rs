use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use pc_overlay_core::{
    BoardCalibration, CommitOutcome, DiagramSet, FrameSize, OverlayBoard, OverlayCell,
    OverlayCellKind, OverlayState, OverlayWindow, Piece, RecognizerConfig, RuleSet, SessionTracker,
    SolveProgress, StabilityConfig, StabilityFilter, TetrioRecognizer, TrackerConfig, WindowInfo,
    WindowLocator, board_is_empty,
    finder::{Buildable, Finder},
    konbini,
    konbini::{BuildQuery, FoundSolve, KonbiniLibrary, KonbiniProgress},
    piece_code,
};

#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

#[derive(Debug, Default)]
struct Args {
    once: bool,
    verbose: bool,
    reviewed_only: bool,
    interval_ms: u64,
    rules: PathBuf,
    diagrams: PathBuf,
    cases: PathBuf,
    calibration: PathBuf,
    konbini: PathBuf,
}

impl Args {
    fn with_defaults() -> Self {
        Self {
            interval_ms: 100,
            rules: PathBuf::from("data/ijlo.json"),
            diagrams: PathBuf::from("data/ijlo/diagram_cells.json"),
            cases: PathBuf::from("data/cases"),
            calibration: PathBuf::from("data/overlay-calibration.json"),
            konbini: PathBuf::from("data/konbini"),
            ..Self::default()
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let mut rules = RuleSet::load(&args.rules)?;
    let diagrams = DiagramSet::load(&args.diagrams)?;
    if let Err(issues) = rules.attach_setup_diagrams(&diagrams) {
        for issue in issues {
            eprintln!("warning: {issue}");
        }
    }
    let library = Library::load(rules, &args.cases)?;
    eprintln!(
        "loaded {} cases: {}",
        library.cases.len(),
        library.case_ids()
    );

    let calibration = load_calibration(&args.calibration)?;
    let recognizer_config = RecognizerConfig {
        board_calibration: calibration,
        ..Default::default()
    };
    let recognizer = TetrioRecognizer::new(recognizer_config)?;
    let locator = WindowLocator::default();
    let mut stability = StabilityFilter::new(StabilityConfig::default());
    let mut tracker = SessionTracker::new(TrackerConfig::default());
    let mut resync_hotkeys = PcResyncHotkeys::default();
    let mut solve_hotkeys = SolveHotkeys::default();
    let mut solve_notice: Option<(&'static str, Instant)> = None;
    let mut konbini_hotkeys = KonbiniHotkeys::default();
    let mut konbini = KonbiniGuide::new(args.konbini.clone());
    konbini.load_or_fetch();
    // Ctrl+Alt+K: the 2nd PC uses Setup Konbini even when the source has rules.
    let mut prefer_konbini = false;
    // Whether the last stable frame showed a completed setup with a solve.
    let mut solve_available = false;
    let mut overlay = OverlayWindow::new()?;
    println!(
        "run-overlay started; Ctrl+Alt+1..7 resynchronizes the PC number; S shows / Ctrl+Alt+S toggles the solve once the setup is complete; Ctrl+Alt+N / Ctrl+Alt+B switch setups; Ctrl+Alt+K makes the 2nd PC use Setup Konbini; Ctrl+Alt+F refetches Setup Konbini; press Ctrl+C to stop"
    );

    loop {
        if !overlay.pump() {
            break;
        }
        if let Some(ordinal) = resync_hotkeys.poll() {
            tracker.resync(ordinal);
            stability.reset();
            eprintln!("PC number manually resynchronized to {}", ordinal.get());
        }
        if let Some(hotkey) = konbini_hotkeys.poll() {
            if hotkey == KonbiniHotkey::PreferKonbini {
                prefer_konbini = !prefer_konbini;
                let notice = if prefer_konbini {
                    "2nd PC: Setup Konbini"
                } else {
                    "2nd PC: source rules first"
                };
                eprintln!("{notice}");
                solve_notice = Some((notice, Instant::now()));
            } else {
                konbini.apply(hotkey);
            }
        }
        konbini.poll_fetch();
        if let Some(hotkey) = solve_hotkeys.poll()
            && let Some(notice) = apply_solve_hotkey(&mut tracker, hotkey, solve_available)
        {
            eprintln!("{notice}");
            solve_notice = Some((notice, Instant::now()));
        }
        let active_notice = solve_notice
            .filter(|(_, at)| at.elapsed() < SOLVE_NOTICE_DURATION)
            .map(|(notice, _)| notice);
        match locator.locate() {
            Ok(Some(window)) => {
                let frame = match pc_overlay_core::capture::capture_client(&window) {
                    Ok(frame) => frame,
                    Err(error) => {
                        let state = OverlayState {
                            status: "PC Overlay: capture error".to_string(),
                            detail: error.to_string(),
                            ..OverlayState::default()
                        };
                        overlay.update(Some(window.client_rect), state)?;
                        if args.once {
                            break;
                        }
                        thread::sleep(Duration::from_millis(args.interval_ms.max(20)));
                        continue;
                    }
                };
                let recognition = recognizer.recognize(&frame)?;
                let stable = if let Some(observation) = recognition.observation.clone() {
                    Some(stability.ingest(observation, recognition.confidence))
                } else {
                    stability.clear_pending();
                    None
                };
                let commit = match stable.as_ref() {
                    Some(pc_overlay_core::StabilityOutcome::Stable {
                        observation,
                        confidence,
                    }) => Some(tracker.commit(
                        (**observation).clone(),
                        *confidence,
                        frame.captured_at_ms,
                    )),
                    _ => None,
                };
                let guide = match stable.as_ref() {
                    Some(pc_overlay_core::StabilityOutcome::Stable { .. }) => {
                        tracker.snapshots().last().map(|snapshot| {
                            unified_guidance(
                                &library,
                                &mut konbini,
                                &tracker,
                                snapshot,
                                args.reviewed_only,
                                prefer_konbini,
                            )
                        })
                    }
                    _ => None,
                };
                // Kept from the last tracked state while frames are unstable,
                // as the solve hotkeys were judged on the latest snapshot.
                if let Some(guide) = &guide {
                    solve_available = guide.setup_complete;
                }
                let mut state = build_overlay_state(
                    &window,
                    &frame,
                    &recognition,
                    stable.as_ref(),
                    commit.as_ref(),
                    &tracker,
                    guide,
                    args.verbose,
                );
                if let Some(notice) = active_notice {
                    state.status.push_str(" | ");
                    state.status.push_str(notice);
                }
                overlay.update(Some(window.client_rect), state)?;
            }
            Ok(None) => {
                overlay.update(
                    None,
                    OverlayState {
                        status: "PC Overlay: waiting for visible TETR.IO".to_string(),
                        detail: "Show TETR.IO Desktop / ZEN to start recognition".to_string(),
                        ..OverlayState::default()
                    },
                )?;
            }
            Err(error) => {
                overlay.update(
                    None,
                    OverlayState {
                        status: "PC Overlay: window discovery error".to_string(),
                        detail: error.to_string(),
                        ..OverlayState::default()
                    },
                )?;
            }
        }

        if args.once {
            break;
        }
        thread::sleep(Duration::from_millis(args.interval_ms.max(20)));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct PcResyncHotkeys {
    was_down: [bool; 7],
}

impl PcResyncHotkeys {
    #[cfg(windows)]
    fn poll(&mut self) -> Option<pc_overlay_core::PcOrdinal> {
        const VK_CONTROL: i32 = 0x11;
        const VK_MENU: i32 = 0x12;
        const VK_1: i32 = 0x31;

        let modifiers = key_is_down(VK_CONTROL) && key_is_down(VK_MENU);
        let mut pressed = None;
        for (index, was_down) in self.was_down.iter_mut().enumerate() {
            let down = modifiers && key_is_down(VK_1 + index as i32);
            if down && !*was_down {
                pressed = pc_overlay_core::PcOrdinal::new(index as u8 + 1);
            }
            *was_down = down;
        }
        pressed
    }

    #[cfg(not(windows))]
    fn poll(&mut self) -> Option<pc_overlay_core::PcOrdinal> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SolveHotkey {
    /// Ctrl+Alt+S: show or hide.
    Toggle,
    /// Plain S: only shows, so S doubling as a game key never hides a solve.
    Show,
}

/// Ctrl+Alt+S and plain S, edge-triggered on S like the resync keys. The
/// overlay only reads key state, so the game still receives the S press.
#[derive(Debug, Default)]
struct SolveHotkeys {
    was_down: bool,
}

impl SolveHotkeys {
    #[cfg(windows)]
    fn poll(&mut self) -> Option<SolveHotkey> {
        const VK_CONTROL: i32 = 0x11;
        const VK_MENU: i32 = 0x12;
        const VK_S: i32 = 0x53;

        let down = key_is_down(VK_S);
        let pressed = down && !self.was_down;
        self.was_down = down;
        pressed.then(|| {
            if key_is_down(VK_CONTROL) && key_is_down(VK_MENU) {
                SolveHotkey::Toggle
            } else {
                SolveHotkey::Show
            }
        })
    }

    #[cfg(not(windows))]
    fn poll(&mut self) -> Option<SolveHotkey> {
        None
    }
}

/// How long a hotkey notice stays on the status line.
const SOLVE_NOTICE_DURATION: Duration = Duration::from_millis(1500);

/// Applies a solve hotkey press. Turning the display ON is accepted only once
/// the setup is complete; turning it OFF is always accepted. Returns the
/// notice for the status line, or `None` when the press changes nothing.
fn apply_solve_hotkey(
    tracker: &mut SessionTracker,
    hotkey: SolveHotkey,
    setup_complete: bool,
) -> Option<&'static str> {
    let shown = tracker.guidance().show_solve;
    match (hotkey, shown) {
        (SolveHotkey::Show, true) => None,
        (SolveHotkey::Toggle, true) => {
            tracker.toggle_show_solve();
            Some("solve hidden")
        }
        (_, false) if !setup_complete => Some("solve hotkey ignored: setup not complete"),
        (_, false) => {
            tracker.toggle_show_solve();
            Some("solve shown")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KonbiniHotkey {
    /// Ctrl+Alt+K: the 2nd PC uses Setup Konbini even when the source has
    /// rules for its case (press again to go back).
    PreferKonbini,
    /// Ctrl+Alt+N: the next candidate setup.
    Next,
    /// Ctrl+Alt+B: the previous candidate setup.
    Previous,
    /// Ctrl+Alt+F: download the sheets again.
    Refetch,
}

/// Ctrl+Alt+K / N / B / F, edge-triggered like the resync keys.
#[derive(Debug, Default)]
struct KonbiniHotkeys {
    was_down: [bool; 4],
}

impl KonbiniHotkeys {
    #[cfg(windows)]
    fn poll(&mut self) -> Option<KonbiniHotkey> {
        const VK_CONTROL: i32 = 0x11;
        const VK_MENU: i32 = 0x12;
        const KEYS: [(i32, KonbiniHotkey); 4] = [
            (0x4B, KonbiniHotkey::PreferKonbini),
            (0x4E, KonbiniHotkey::Next),
            (0x42, KonbiniHotkey::Previous),
            (0x46, KonbiniHotkey::Refetch),
        ];

        let modifiers = key_is_down(VK_CONTROL) && key_is_down(VK_MENU);
        let mut pressed = None;
        for ((key, hotkey), was_down) in KEYS.iter().zip(&mut self.was_down) {
            let down = modifiers && key_is_down(*key);
            if down && !*was_down {
                pressed = Some(*hotkey);
            }
            *was_down = down;
        }
        pressed
    }

    #[cfg(not(windows))]
    fn poll(&mut self) -> Option<KonbiniHotkey> {
        None
    }
}

/// Where the guidance on screen comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The verified 2nd-PC rules from "Algebruh's 2nd Methods".
    Source,
    /// A Setup Konbini setup the live queue can build.
    Konbini,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Source => "source",
            Origin::Konbini => "Konbini",
        }
    }
}

/// The guidance for one tracked state.
#[derive(Debug, Clone)]
struct Guide {
    status: String,
    cells: Vec<OverlayCell>,
    origin: Origin,
    /// Whether a solve hotkey press may turn the solve display ON.
    setup_complete: bool,
    /// Why the source rules were not used, for the detail line.
    note: Option<String>,
}

/// One guidance for every PC. The 2nd PC uses the source rules when they
/// select a setup for the case; otherwise, and on every other PC, a Setup
/// Konbini setup the live queue can build is shown.
fn unified_guidance(
    library: &Library,
    konbini: &mut KonbiniGuide,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    reviewed_only: bool,
    prefer_konbini: bool,
) -> Guide {
    let mut note = None;
    if snapshot.pc_ordinal.get() == 2 {
        if prefer_konbini {
            note = Some("source rules skipped (Ctrl+Alt+K)".to_string());
        } else {
            match guidance_stage(library, tracker, snapshot, reviewed_only) {
                GuidanceStage::Inactive(message) => {
                    note = Some(message.replacen("PC Overlay: ", "source: ", 1));
                }
                stage => {
                    let setup_complete = matches!(stage, GuidanceStage::SetupComplete { .. });
                    let (status, cells) = render_stage(library, tracker, snapshot, stage);
                    return Guide {
                        status: status.replacen("PC Overlay:", "PC Overlay [source]:", 1),
                        cells,
                        origin: Origin::Source,
                        setup_complete,
                        note: None,
                    };
                }
            }
        }
    }
    let mut guide = konbini.guide(tracker, snapshot);
    guide.note = note;
    guide
}

/// Setup Konbini as a guidance source: the sheet of the tracked PC number.
/// The sheets are read from the cache folder, and downloaded in the
/// background when it is empty.
struct KonbiniGuide {
    dir: PathBuf,
    library: Option<KonbiniLibrary>,
    fetching: Option<mpsc::Receiver<Result<KonbiniLibrary, String>>>,
    error: Option<String>,
    /// The candidate shown, cycled by Ctrl+Alt+N / B.
    index: isize,
    /// The PC the index belongs to: ordinal and start timestamp.
    pc_key: Option<(u8, u64)>,
    finder: Finder,
    /// Candidates of the last searched state, reused until it changes.
    shortlist: Option<(ShortlistKey, Vec<Shortlisted>)>,
    /// Perfect clears found once a setup was built, while the board still
    /// follows one of them.
    solve: Option<SolveSession>,
    /// The last state searched for a perfect clear, so a state without one
    /// is searched once.
    searched: Option<pc_overlay_core::Observation>,
}

/// Perfect clears found by the solve search, cycled by Ctrl+Alt+N / B.
#[derive(Debug, Clone)]
struct SolveSession {
    solves: Vec<FoundSolve>,
    index: isize,
}

/// Perfect clears kept per search.
const SOLVE_LIMIT: usize = 100;

/// What a shortlist depends on: PC number, observation, and PC pieces.
type ShortlistKey = (u8, pc_overlay_core::Observation, Vec<Piece>);

/// A candidate setup by index into the library.
#[derive(Debug, Clone)]
struct Shortlisted {
    setup: usize,
    progress: KonbiniProgress,
    buildable: Buildable,
}

/// Move-generation cache entries kept before the cache is dropped.
const FINDER_CACHE_LIMIT: usize = 200_000;

impl KonbiniGuide {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            library: None,
            fetching: None,
            error: None,
            index: 0,
            pc_key: None,
            finder: Finder::new(),
            shortlist: None,
            solve: None,
            searched: None,
        }
    }

    /// Reads the cached sheets, or starts downloading them when there are
    /// none.
    fn load_or_fetch(&mut self) {
        match KonbiniLibrary::load_dir(&self.dir) {
            Ok(library) => {
                eprintln!("Setup Konbini: {} setups loaded", library.setups.len());
                self.library = Some(library);
            }
            Err(konbini::KonbiniError::NotFetched(_)) => self.start_fetch(),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn apply(&mut self, hotkey: KonbiniHotkey) {
        match hotkey {
            // Handled by the caller, which owns the preference.
            KonbiniHotkey::PreferKonbini => {}
            // With perfect clears found, N / B switch between them.
            KonbiniHotkey::Next => match &mut self.solve {
                Some(session) => session.index += 1,
                None => self.index += 1,
            },
            // Wraps to the last candidate once the count is known.
            KonbiniHotkey::Previous => match &mut self.solve {
                Some(session) => session.index -= 1,
                None => self.index -= 1,
            },
            KonbiniHotkey::Refetch => {
                if self.fetching.is_none() {
                    self.start_fetch();
                }
            }
        }
    }

    fn start_fetch(&mut self) {
        let (sender, receiver) = mpsc::channel();
        let dir = self.dir.clone();
        eprintln!("fetching Setup Konbini into {}", dir.display());
        thread::spawn(move || {
            let result = konbini::fetch_all(&dir)
                .and_then(|()| KonbiniLibrary::load_dir(&dir))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.fetching = Some(receiver);
        self.error = None;
    }

    fn poll_fetch(&mut self) {
        let Some(receiver) = &self.fetching else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(library)) => {
                eprintln!("Setup Konbini: {} setups loaded", library.setups.len());
                self.library = Some(library);
                self.shortlist = None;
                self.fetching = None;
            }
            Ok(Err(error)) => {
                eprintln!("Setup Konbini fetch failed: {error}");
                self.error = Some(error);
                self.fetching = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("fetch thread stopped".to_string());
                self.fetching = None;
            }
        }
    }

    /// The Setup Konbini guidance for a stable tracked state.
    fn guide(
        &mut self,
        tracker: &SessionTracker,
        snapshot: &pc_overlay_core::tracker::Snapshot,
    ) -> Guide {
        let pc_key = tracker
            .current_pc_start()
            .map(|start| (start.pc_ordinal.get(), start.timestamp_ms));
        if pc_key != self.pc_key {
            self.pc_key = pc_key;
            self.index = 0;
            self.solve = None;
            self.searched = None;
        }
        let message = if self.fetching.is_some() {
            Some("fetching Setup Konbini...".to_string())
        } else if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.library.is_none() {
            Some("no data; press Ctrl+Alt+F to fetch".to_string())
        } else {
            None
        };
        let Some(library) = self.library.as_ref().filter(|_| message.is_none()) else {
            return Guide {
                status: format!(
                    "PC Overlay [Konbini]: {}",
                    message.expect("set when there is no library")
                ),
                cells: Vec::new(),
                origin: Origin::Konbini,
                setup_complete: false,
                note: None,
            };
        };
        let key = (
            snapshot.pc_ordinal.get(),
            snapshot.observation.clone(),
            pc_pieces(tracker, snapshot),
        );
        if self
            .shortlist
            .as_ref()
            .is_none_or(|(known, _)| *known != key)
        {
            if self.finder.cached_moves() > FINDER_CACHE_LIMIT {
                self.finder.clear_cache();
            }
            let shortlist = konbini_shortlist(library, &mut self.finder, tracker, snapshot);
            self.shortlist = Some((key, shortlist));
        }
        let shortlist = &self.shortlist.as_ref().expect("just computed").1;
        let observation = &snapshot.observation;
        let show = tracker.guidance().show_solve;
        if let Some(guide) = solve_guide(&mut self.solve, snapshot, show) {
            return guide;
        }
        let guide = konbini_render(library, snapshot, shortlist, &mut self.index);
        let shown_complete = usize::try_from(self.index)
            .ok()
            .and_then(|index| shortlist.get(index))
            .is_some_and(|candidate| candidate.progress.complete());
        let board_off_setup = shortlist.is_empty() && !board_is_empty(&observation.board);
        if (shown_complete || board_off_setup) && self.searched.as_ref() != Some(observation) {
            // A built setup, or a board no setup explains (the player went
            // their own way): search the perfect clears from here.
            self.searched = Some(observation.clone());
            let solves = konbini::find_perfect_clears(&mut self.finder, observation, SOLVE_LIMIT);
            if !solves.is_empty() {
                self.solve = Some(SolveSession { solves, index: 0 });
                if let Some(guide) = solve_guide(&mut self.solve, snapshot, show) {
                    return guide;
                }
            }
        }
        if shown_complete || board_off_setup {
            let stage = konbini::stage_label(snapshot.pc_ordinal.get());
            let status = if shown_complete {
                guide.status.replacen(
                    " setup complete (no solve data)",
                    " setup complete; no perfect clear with HOLD + NEXT",
                    1,
                )
            } else {
                format!(
                    "PC Overlay [Konbini {stage}]: no setup matches the board and no perfect clear with HOLD + NEXT"
                )
            };
            return Guide { status, ..guide };
        }
        guide
    }
}

/// The perfect clear the board follows, if a search found one: shown when
/// the solve display is ON, announced otherwise. A board that follows none
/// drops the session.
fn solve_guide(
    solve: &mut Option<SolveSession>,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    show: bool,
) -> Option<Guide> {
    let observation = &snapshot.observation;
    let stage = konbini::stage_label(snapshot.pc_ordinal.get());
    let session = solve.as_mut()?;
    let count = session.solves.len();
    let start = session.index.rem_euclid(count as isize) as usize;
    let Some((position, progress)) =
        (0..count)
            .map(|offset| (start + offset) % count)
            .find_map(|position| {
                session.solves[position]
                    .progress(&observation.board)
                    .map(|progress| (position, progress))
            })
    else {
        *solve = None;
        return None;
    };
    session.index = position as isize;
    let height = session.solves[position].height;
    let status = if show {
        format!(
            "PC Overlay [Konbini {stage}]: perfect clear {}/{count} (search, SRS+, {height} lines); Ctrl+Alt+N/B to switch",
            position + 1
        )
    } else {
        format!(
            "PC Overlay [Konbini {stage}]: {count} perfect clear(s) found ({height} lines); press S to show the solve"
        )
    };
    let cells = if show {
        progress
            .remaining
            .iter()
            .map(|cell| OverlayCell {
                cell: cell.cell,
                piece: cell.piece,
                kind: if cell.piece == Some(observation.current) {
                    OverlayCellKind::Current
                } else {
                    OverlayCellKind::Missing
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(Guide {
        status,
        cells,
        origin: Origin::Konbini,
        setup_complete: true,
        note: None,
    })
}

/// The PC's pieces from its start: HOLD, then the queue disclosed since.
fn pc_pieces(
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
) -> Vec<Piece> {
    tracker
        .current_pc_start()
        .and_then(|start| start.observation.hold)
        .into_iter()
        .chain(snapshot.observed_queue_history.iter().copied())
        .collect()
}

/// The setups of the snapshot's PC that match the board and can be built
/// from its HOLD and queue.
fn konbini_shortlist(
    library: &KonbiniLibrary,
    finder: &mut Finder,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
) -> Vec<Shortlisted> {
    let stage = snapshot.pc_ordinal.get();
    let pieces = pc_pieces(tracker, snapshot);
    let query = BuildQuery {
        pc_pieces: &pieces,
        observation: &snapshot.observation,
        // A perfect-clear loop deals 10 pieces per PC.
        pieces_before_pc: Some(10 * usize::from(stage - 1)),
    };
    library
        .candidates(stage, &query, finder)
        .into_iter()
        .map(|candidate| Shortlisted {
            setup: candidate.setup_index,
            progress: candidate.progress,
            buildable: candidate.buildable,
        })
        .collect()
}

#[cfg(test)]
fn konbini_guidance(
    library: &KonbiniLibrary,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    index: &mut isize,
) -> (String, Vec<OverlayCell>) {
    let shortlist = konbini_shortlist(library, &mut Finder::new(), tracker, snapshot);
    let guide = konbini_render(library, snapshot, &shortlist, index);
    (guide.status, guide.cells)
}

/// The candidate at `index` (wrapped into range).
fn konbini_render(
    library: &KonbiniLibrary,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    candidates: &[Shortlisted],
    index: &mut isize,
) -> Guide {
    let stage = snapshot.pc_ordinal.get();
    let label = konbini::stage_label(stage);
    if candidates.is_empty() {
        return Guide {
            status: format!(
                "PC Overlay [Konbini {label}]: no buildable setup matches the board ({} in sheet)",
                library.stage_count(stage)
            ),
            cells: Vec::new(),
            origin: Origin::Konbini,
            setup_complete: false,
            note: None,
        };
    }
    let position = index.rem_euclid(candidates.len() as isize) as usize;
    *index = position as isize;
    let candidate = &candidates[position];
    let setup = &library.setups[candidate.setup];
    let sol = setup
        .sol_percent
        .map(|sol| format!(" {sol:.2}%"))
        .unwrap_or_default();
    let source = if setup.source.is_empty() {
        String::new()
    } else {
        format!(" [{}]", setup.source)
    };
    let complete = if candidate.progress.complete() {
        " setup complete (no solve data)"
    } else if candidate.buildable == Buildable::Sometimes {
        " depends on next pieces"
    } else {
        ""
    };
    let status = format!(
        "PC Overlay [Konbini {label}]: {}{sol}{source} ({}/{}){complete}; Ctrl+Alt+N/B to switch",
        setup.id,
        position + 1,
        candidates.len(),
    );
    let current = snapshot.observation.current;
    let cells = candidate
        .progress
        .remaining
        .iter()
        .map(|cell| OverlayCell {
            cell: cell.cell,
            piece: cell.piece,
            kind: if cell.piece == Some(current) {
                OverlayCellKind::Current
            } else {
                OverlayCellKind::Missing
            },
        })
        .collect();
    Guide {
        status,
        cells,
        origin: Origin::Konbini,
        setup_complete: false,
        note: None,
    }
}

#[cfg(windows)]
fn key_is_down(key: i32) -> bool {
    // SAFETY: GetAsyncKeyState has no pointer arguments and accepts a virtual
    // key code. The high bit is the documented current-key-down state.
    unsafe { GetAsyncKeyState(key) < 0 }
}

#[allow(clippy::too_many_arguments)]
fn build_overlay_state(
    window: &WindowInfo,
    frame: &pc_overlay_core::CapturedFrame,
    recognition: &pc_overlay_core::RecognitionResult,
    stable: Option<&pc_overlay_core::StabilityOutcome>,
    commit: Option<&CommitOutcome>,
    tracker: &SessionTracker,
    guide: Option<Guide>,
    verbose: bool,
) -> OverlayState {
    let frame_size = FrameSize {
        width: frame.width,
        height: frame.height,
    };
    let board = OverlayBoard {
        frame: frame_size,
        rect: recognition.board_rect,
    };
    let current = recognition.current.map(piece_code).unwrap_or('-');
    let hold = recognition.hold.map(piece_code).unwrap_or('-');
    let next = recognition
        .next5
        .iter()
        .map(|piece| piece.map(piece_code).unwrap_or('-'))
        .collect::<String>();
    let mut detail = format!(
        "Current {current}  HOLD {hold}  NEXT {next}  confidence {:.2}",
        recognition.confidence
    );
    if verbose {
        detail.push_str(&format!(
            "  board={} weak={} ambiguous={} rect=({},{} {}x{})",
            recognition.board_cells,
            recognition.weak_board_cells,
            recognition.ambiguous_board_cells,
            recognition.board_rect.left,
            recognition.board_rect.top,
            recognition.board_rect.width,
            recognition.board_rect.height,
        ));
    }

    let (status, cells) = if recognition.observation.is_none() {
        (
            "PC Overlay: recognition paused (unknown or unstable)".to_string(),
            Vec::new(),
        )
    } else {
        match stable {
            Some(pc_overlay_core::StabilityOutcome::Pending { consecutive_frames }) => (
                format!("PC Overlay: waiting for stable frame ({consecutive_frames}/3)"),
                Vec::new(),
            ),
            Some(pc_overlay_core::StabilityOutcome::IgnoredLowConfidence { .. }) => (
                "PC Overlay: recognition confidence too low".to_string(),
                Vec::new(),
            ),
            Some(pc_overlay_core::StabilityOutcome::Stable { .. }) => match guide {
                Some(guide) => {
                    detail.push_str(&format!("  guide={}", guide.origin.label()));
                    if let Some(note) = &guide.note {
                        detail.push_str(&format!(" ({note})"));
                    }
                    (guide.status, guide.cells)
                }
                None => (
                    "PC Overlay: waiting for first tracked state".to_string(),
                    Vec::new(),
                ),
            },
            None => (
                "PC Overlay: recognition paused (unknown or unstable)".to_string(),
                Vec::new(),
            ),
        }
    };

    if let Some(commit) = commit {
        detail.push_str("  state=");
        detail.push_str(commit_name(commit));
    }
    detail.push_str(&format!("  PC {}", tracker.pc_ordinal().get()));
    detail.push_str(if tracker.guidance().show_solve {
        "  solve ON"
    } else {
        "  solve OFF"
    });
    if !window.title.is_empty() {
        detail.push_str(&format!("  [{}]", window.title));
    }
    OverlayState {
        status,
        detail,
        board: Some(board),
        cells,
    }
}

/// Every case's rules. The hand-checked IJLO data (`--rules`) comes first;
/// generated cases in `--cases` with the same id are skipped.
struct Library {
    cases: Vec<RuleSet>,
}

impl Library {
    fn load(primary: RuleSet, folder: &Path) -> Result<Self, Box<dyn Error>> {
        let mut cases = vec![primary];
        if folder.is_dir() {
            let mut paths: Vec<PathBuf> = fs::read_dir(folder)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            paths.sort();
            for path in paths {
                let rules = RuleSet::load(&path)
                    .map_err(|error| format!("{}: {error:?}", path.display()))?;
                if cases
                    .iter()
                    .all(|known| case_key(&known.case.id) != case_key(&rules.case.id))
                {
                    cases.push(rules);
                }
            }
        } else {
            eprintln!(
                "warning: case folder {} not found; IJLO only",
                folder.display()
            );
        }
        Ok(Self { cases })
    }

    fn case_ids(&self) -> String {
        self.cases
            .iter()
            .map(|rules| rules.case.id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The rules for the case the tracked 2nd PC started with.
    fn for_tracker(&self, tracker: &SessionTracker) -> Option<&RuleSet> {
        let start = tracker.current_pc_start()?;
        if start.pc_ordinal.get() != 2 {
            return None;
        }
        let key = observation_case(&start.observation);
        self.cases
            .iter()
            .find(|rules| case_key(&rules.case.id) == key)
    }

    #[cfg(test)]
    fn setup(&self, method_id: &str, setup_id: &str) -> Option<&pc_overlay_core::rules::SetupRule> {
        self.cases
            .iter()
            .find_map(|rules| rules.setup(method_id, setup_id))
    }
}

/// A case id's pieces, sorted ("TJJO" -> "JJOT").
fn case_key(id: &str) -> String {
    let mut letters: Vec<char> = id.chars().collect();
    letters.sort_unstable();
    letters.into_iter().collect()
}

/// Where a tracked state stands in the 2nd-PC guidance flow.
enum GuidanceStage<'a> {
    /// No setup applies; the message explains why.
    Inactive(String),
    /// A setup is selected but no verified solve has its setup completed.
    Setup {
        setup: &'a pc_overlay_core::rules::SetupRule,
    },
    /// The setup is complete and this verified solve matches the board.
    SetupComplete {
        setup: &'a pc_overlay_core::rules::SetupRule,
        solve: &'a pc_overlay_core::rules::SolveRule,
        progress: pc_overlay_core::guidance::SetupProgress,
    },
}

/// Whether the latest tracked state has a completed source setup, i.e.
/// whether a solve hotkey press may turn the display ON.
#[cfg(test)]
fn latest_setup_complete(library: &Library, tracker: &SessionTracker, reviewed_only: bool) -> bool {
    tracker.snapshots().last().is_some_and(|snapshot| {
        matches!(
            guidance_stage(library, tracker, snapshot, reviewed_only),
            GuidanceStage::SetupComplete { .. }
        )
    })
}

/// The source-rule guidance alone (without the Setup Konbini fallback).
#[cfg(test)]
fn guidance_for_snapshot(
    library: &Library,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    reviewed_only: bool,
) -> (String, Vec<OverlayCell>) {
    let stage = guidance_stage(library, tracker, snapshot, reviewed_only);
    render_stage(library, tracker, snapshot, stage)
}

/// Status line and cells of a source-rule guidance stage.
fn render_stage(
    library: &Library,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    stage: GuidanceStage<'_>,
) -> (String, Vec<OverlayCell>) {
    let case = library
        .for_tracker(tracker)
        .map(|rules| rules.case.id.as_str())
        .unwrap_or("2nd PC");
    let setup = match stage {
        GuidanceStage::Inactive(message) => return (message, Vec::new()),
        GuidanceStage::SetupComplete {
            setup,
            solve,
            progress,
        } => {
            if !tracker.guidance().show_solve {
                return (
                    format!(
                        "PC Overlay: {case} setup complete ({}); press S to show the solve",
                        setup.id
                    ),
                    Vec::new(),
                );
            }
            let plan = pc_overlay_core::OverlayPlan::from_progress(&progress);
            return (
                format!("PC Overlay: {case} solve active ({})", solve.id),
                plan_cells(plan, snapshot.observation.current),
            );
        }
        GuidanceStage::Setup { setup } => setup,
    };

    let progress =
        pc_overlay_core::evaluate_setup_progress(&setup.placements, &snapshot.observation.board);
    let plan = pc_overlay_core::OverlayPlan::from_progress(&progress);
    let cells = plan_cells(plan, snapshot.observation.current);
    let status = if setup.review_status == pc_overlay_core::rules::ReviewStatus::Verified {
        format!("PC Overlay: {case} guidance active ({})", setup.id)
    } else {
        format!(
            "PC Overlay: {case} reference guidance active ({}, unreviewed)",
            setup.id
        )
    };
    (status, cells)
}

fn guidance_stage<'a>(
    library: &'a Library,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    reviewed_only: bool,
) -> GuidanceStage<'a> {
    if snapshot.pc_ordinal.get() != 2 {
        return GuidanceStage::Inactive(format!(
            "PC Overlay: tracking PC {}; 2nd-PC guidance activates on PC 2",
            snapshot.pc_ordinal.get()
        ));
    }
    let Some(rules) = library.for_tracker(tracker) else {
        let case = tracker
            .current_pc_start()
            .map(|start| observation_case(&start.observation))
            .unwrap_or_default();
        return GuidanceStage::Inactive(format!(
            "PC Overlay: PC 2 detected, no data for the four-piece case {case}"
        ));
    };
    let case = rules.case.id.as_str();

    let Some(context) = tracker.following_bag_context() else {
        return GuidanceStage::Inactive("PC Overlay: PC 2 start was not observed".to_string());
    };
    let report = if reviewed_only {
        rules.select_for_board(&context, &snapshot.observation.board)
    } else {
        rules.select_preview_for_board(&context, &snapshot.observation.board)
    };
    let Some(selected) = report.selected else {
        let qualifier = if reviewed_only {
            "reviewed"
        } else {
            "matching"
        };
        return GuidanceStage::Inactive(format!(
            "PC Overlay: {case} detected, no {qualifier} setup candidate"
        ));
    };
    let Some(setup) = rules.setup(&selected.method_id, &selected.setup_id) else {
        return GuidanceStage::Inactive(format!(
            "PC Overlay: selected {case} setup data is missing"
        ));
    };
    let board = &snapshot.observation.board;
    // Verified solves are evaluated even while display is OFF, so that a
    // finished setup followed by the player's own solve is not flagged as a
    // deviation; their cells are drawn only when display is ON.
    let candidates = setup.solve_candidates(&context, true);
    let rotation = if candidates.is_empty() {
        0
    } else {
        tracker.guidance().solve_index % candidates.len()
    };
    candidates
        .iter()
        .cycle()
        .skip(rotation)
        .take(candidates.len())
        .find_map(|solve| {
            match pc_overlay_core::evaluate_solve_progress(&setup.placements, &solve.target, board)
            {
                SolveProgress::InProgress {
                    setup_complete: true,
                    progress,
                    ..
                } => Some(GuidanceStage::SetupComplete {
                    setup,
                    solve,
                    progress,
                }),
                _ => None,
            }
        })
        .unwrap_or(GuidanceStage::Setup { setup })
}

fn plan_cells(plan: pc_overlay_core::OverlayPlan, current: Piece) -> Vec<OverlayCell> {
    let mut cells = Vec::new();
    for expected in plan.dim_cells {
        cells.push(OverlayCell {
            cell: expected.cell,
            piece: Some(expected.expected),
            kind: if expected.expected == current {
                OverlayCellKind::Current
            } else {
                OverlayCellKind::Missing
            },
        });
    }
    for expected in plan.warning_cells {
        cells.push(OverlayCell {
            cell: expected.cell,
            piece: Some(expected.expected),
            kind: OverlayCellKind::Warning,
        });
    }
    for cell in plan.unexpected_fixed_cells {
        cells.push(OverlayCell {
            cell,
            piece: None,
            kind: OverlayCellKind::Unexpected,
        });
    }
    cells
}

/// The 2nd-PC case consists of HOLD plus the first pieces that can be played,
/// independent of their order: returned sorted, as `case_key` sorts ids.
fn observation_case(observation: &pc_overlay_core::Observation) -> String {
    let mut pieces = Vec::with_capacity(4);
    if let Some(hold) = observation.hold {
        pieces.push(hold);
    }
    pieces.push(observation.current);
    pieces.extend(
        observation
            .next5
            .iter()
            .copied()
            .take(4usize.saturating_sub(pieces.len())),
    );
    let mut letters: Vec<char> = pieces.into_iter().map(piece_code).collect();
    letters.sort_unstable();
    letters.into_iter().collect()
}

fn commit_name(commit: &CommitOutcome) -> &'static str {
    match commit {
        CommitOutcome::Accepted { .. } => "accepted",
        CommitOutcome::Unchanged => "unchanged",
        CommitOutcome::IgnoredLowConfidence { .. } => "ignored_low_confidence",
        CommitOutcome::RejectedHistoryDiscontinuity => "history_discontinuity",
        CommitOutcome::RolledBack { .. } => "rolled_back",
    }
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args::with_defaults();
    let mut values = std::env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--once" => args.once = true,
            "--verbose" => args.verbose = true,
            "--reviewed-only" => args.reviewed_only = true,
            "--interval-ms" => {
                args.interval_ms = values
                    .next()
                    .ok_or("--interval-ms requires a value")?
                    .parse()?;
            }
            "--rules" => {
                args.rules = PathBuf::from(values.next().ok_or("--rules requires a path")?);
            }
            "--cases" => {
                args.cases = PathBuf::from(values.next().ok_or("--cases requires a folder")?);
            }
            "--diagrams" => {
                args.diagrams = PathBuf::from(values.next().ok_or("--diagrams requires a path")?);
            }
            "--calibration" => {
                args.calibration =
                    PathBuf::from(values.next().ok_or("--calibration requires a path")?);
            }
            "--konbini" => {
                args.konbini = PathBuf::from(values.next().ok_or("--konbini requires a folder")?);
            }
            "--help" | "-h" => {
                println!(
                    "run-overlay [--once] [--verbose] [--reviewed-only] [--interval-ms N] [--rules PATH] [--diagrams PATH] [--cases DIR] [--calibration PATH] [--konbini DIR]"
                );
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    Ok(args)
}

fn load_calibration(path: &Path) -> Result<BoardCalibration, Box<dyn Error>> {
    if !path.exists() {
        eprintln!(
            "warning: calibration file {} not found; using the default 1920x1009 profile",
            path.display()
        );
        return Ok(BoardCalibration::default());
    }
    let text = fs::read_to_string(path)?;
    let calibration: BoardCalibration = serde_json::from_str(&text)?;
    calibration
        .validate()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(calibration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_overlay_core::{Board, Cell, Observation, Placement, empty_board};
    use std::collections::BTreeSet;

    fn observation(hold: Option<Piece>, current: Piece, next5: [Piece; 5]) -> Observation {
        Observation {
            board: empty_board(),
            hold,
            current,
            next5,
        }
    }

    #[test]
    fn case_uses_hold_current_and_earliest_next_pieces() {
        let ijlo = case_key("IJLO");
        assert_eq!(
            observation_case(&observation(
                Some(Piece::I),
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
            )),
            ijlo
        );
        assert_eq!(
            observation_case(&observation(
                Some(Piece::I),
                Piece::J,
                [Piece::L, Piece::S, Piece::O, Piece::Z, Piece::T],
            )),
            case_key("IJLS")
        );
        assert_eq!(
            observation_case(&observation(
                None,
                Piece::O,
                [Piece::L, Piece::I, Piece::J, Piece::S, Piece::Z],
            )),
            ijlo
        );
        // Duplicate pieces from two bags form cases such as TJJO.
        assert_eq!(
            observation_case(&observation(
                None,
                Piece::J,
                [Piece::T, Piece::O, Piece::J, Piece::S, Piece::Z],
            )),
            case_key("TJJO")
        );
    }

    #[test]
    fn generated_cases_load_next_to_ijlo() {
        let library = Library::load(ijlo_only(), &root().join("data/cases")).unwrap();
        assert_eq!(library.cases[0].case.id, "IJLO");
        assert!(library.cases.len() >= 40, "{}", library.case_ids());
        // The hand-checked IJLO wins over the generated one.
        assert_eq!(
            library
                .cases
                .iter()
                .filter(|rules| case_key(&rules.case.id) == case_key("IJLO"))
                .count(),
            1
        );
    }

    #[test]
    fn a_generated_case_gets_a_likely_setup_at_pc_two() {
        let library = Library::load(ijlo_only(), &root().join("data/cases")).unwrap();
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        let mut filled = observation(
            None,
            Piece::T,
            [Piece::I, Piece::O, Piece::S, Piece::J, Piece::L],
        );
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        let cleared = observation(
            None,
            Piece::T,
            [Piece::I, Piece::O, Piece::S, Piece::J, Piece::L],
        );
        tracker.commit(cleared, 0.75, 2);
        let snapshot = tracker.snapshots().last().unwrap();
        let (status, cells) = guidance_for_snapshot(&library, &tracker, snapshot, true);
        assert!(status.contains("TIOS guidance active"), "{status}");
        assert!(!cells.is_empty());
    }

    #[test]
    fn first_clear_advances_to_pc_two_and_enables_ijlo_setup_cells() {
        let rules = ijlo_rules();

        let mut tracker = SessionTracker::new(TrackerConfig::default());
        let mut filled = observation(
            Some(Piece::I),
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
        );
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        let cleared = observation(
            Some(Piece::I),
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
        );
        tracker.commit(cleared, 0.75, 2);

        let snapshot = tracker.snapshots().last().unwrap();
        let (status, cells) = guidance_for_snapshot(&rules, &tracker, snapshot, false);
        assert_eq!(snapshot.pc_ordinal.get(), 2);
        // The bag after IJLO opens with S, so the verified S-queue setup applies.
        assert!(status.contains("IJLO guidance active (ijlo_s_queue)"));
        assert_eq!(cells.len(), 16);

        let mut progressed = observation(
            Some(Piece::J),
            Piece::L,
            [Piece::O, Piece::S, Piece::Z, Piece::T, Piece::I],
        );
        progressed.board[0][0] = Some(Piece::I);
        tracker.commit(progressed, 0.75, 3);
        assert!(rules.for_tracker(&tracker).is_some());
        let snapshot = tracker.snapshots().last().unwrap();
        let (status, _) = guidance_for_snapshot(&rules, &tracker, snapshot, false);
        assert!(!status.contains("no data for the four-piece case"));

        let (status, cells) = guidance_for_snapshot(&rules, &tracker, snapshot, true);
        assert!(status.contains("no reviewed setup candidate"));
        assert!(cells.is_empty());
    }

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn ijlo_only() -> RuleSet {
        let mut rules = RuleSet::load(root().join("data/ijlo.json")).unwrap();
        let diagrams = DiagramSet::load(root().join("data/ijlo/diagram_cells.json")).unwrap();
        rules.attach_setup_diagrams(&diagrams).unwrap();
        rules
    }

    /// IJLO alone, as the tests below exercise the hand-checked data.
    fn ijlo_rules() -> Library {
        Library {
            cases: vec![ijlo_only()],
        }
    }

    fn board_with(placements: &[Placement]) -> Board {
        let mut board = empty_board();
        for placement in placements {
            for cell in &placement.cells {
                board[cell.y as usize][cell.x as usize] = Some(placement.piece);
            }
        }
        board
    }

    fn cell_set(cells: &[OverlayCell]) -> BTreeSet<Cell> {
        cells.iter().map(|cell| cell.cell).collect()
    }

    /// PC 1 clears into a 2nd PC whose IJLO case is followed by an S-first bag.
    fn s_queue_pc_two() -> SessionTracker {
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        let mut filled = observation(
            Some(Piece::I),
            Piece::J,
            [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
        );
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        tracker.commit(
            observation(
                Some(Piece::I),
                Piece::J,
                [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
            ),
            0.75,
            2,
        );
        tracker
    }

    fn commit_board(tracker: &mut SessionTracker, board: Board, timestamp_ms: u64) {
        let mut next = observation(
            Some(Piece::J),
            Piece::L,
            [Piece::O, Piece::S, Piece::Z, Piece::T, Piece::I],
        );
        next.board = board;
        let outcome = tracker.commit(next, 0.75, timestamp_ms);
        assert!(
            matches!(
                outcome,
                CommitOutcome::Accepted { .. } | CommitOutcome::RolledBack { .. }
            ),
            "{outcome:?}"
        );
    }

    fn latest_guidance(rules: &Library, tracker: &SessionTracker) -> (String, Vec<OverlayCell>) {
        guidance_for_snapshot(rules, tracker, tracker.snapshots().last().unwrap(), true)
    }

    #[test]
    fn solve_cells_replace_setup_cells_once_the_setup_is_complete() {
        let rules = ijlo_rules();
        let setup = rules.setup("ijlo_source_method", "ijlo_s_queue").unwrap();
        let solve = &setup.solves[0];
        let mut tracker = s_queue_pc_two();
        assert!(tracker.toggle_show_solve());

        // An unfinished setup keeps showing the remaining setup cells.
        commit_board(&mut tracker, board_with(&setup.placements[..1]), 3);
        let (status, cells) = latest_guidance(&rules, &tracker);
        assert!(
            status.contains("IJLO guidance active (ijlo_s_queue)"),
            "{status}"
        );
        let remaining_setup: BTreeSet<Cell> = setup.placements[1..]
            .iter()
            .flat_map(|placement| placement.cells.iter().copied())
            .collect();
        assert_eq!(cell_set(&cells), remaining_setup);

        // A finished setup switches to the verified solve's cells.
        commit_board(&mut tracker, board_with(&setup.placements), 4);
        let (status, cells) = latest_guidance(&rules, &tracker);
        assert!(
            status.contains(&format!("solve active ({})", solve.id)),
            "{status}"
        );
        let solve_cells: BTreeSet<Cell> = solve
            .target
            .iter()
            .flat_map(|placement| placement.cells.iter().copied())
            .collect();
        assert_eq!(cell_set(&cells), solve_cells);
        assert!(cells.iter().all(|cell| matches!(
            cell.kind,
            OverlayCellKind::Missing | OverlayCellKind::Current
        )));
        assert!(
            cells
                .iter()
                .filter(|cell| cell.kind == OverlayCellKind::Current)
                .all(|cell| cell.piece == Some(Piece::L))
        );

        // A placed solve piece disappears from the overlay. Pick one that
        // completes no row, so the uncollapsed board is what the game shows.
        let placed = solve
            .target
            .iter()
            .map(|piece| {
                let mut placed = setup.placements.clone();
                placed.push(piece.clone());
                placed
            })
            .find(|placed| {
                matches!(
                    pc_overlay_core::evaluate_solve_progress(
                        &setup.placements,
                        &solve.target,
                        &board_with(placed),
                    ),
                    SolveProgress::InProgress {
                        cleared_row_count: 0,
                        ..
                    }
                )
            })
            .expect("some solve piece completes no row on its own");
        commit_board(&mut tracker, board_with(&placed), 5);
        let (_, cells) = latest_guidance(&rules, &tracker);
        assert_eq!(cells.len(), solve_cells.len() - 4);
    }

    #[test]
    fn solve_display_is_off_by_default_and_undo_restores_the_toggle() {
        let rules = ijlo_rules();
        let setup = rules.setup("ijlo_source_method", "ijlo_s_queue").unwrap();
        let mut tracker = s_queue_pc_two();
        assert!(!tracker.guidance().show_solve);

        commit_board(&mut tracker, board_with(&setup.placements[..3]), 3);
        commit_board(&mut tracker, board_with(&setup.placements), 4);
        let (status, cells) = latest_guidance(&rules, &tracker);
        assert!(status.contains("press S to show the solve"), "{status}");
        assert!(
            cells.is_empty(),
            "OFF draws neither solve nor deviation cells"
        );

        assert!(tracker.toggle_show_solve());
        let (status, cells) = latest_guidance(&rules, &tracker);
        assert!(status.contains("solve active"), "{status}");
        assert_eq!(cells.len(), 4 * setup.solves[0].target.len());

        // Undo to the state before the toggle brings back its OFF setting.
        commit_board(&mut tracker, board_with(&setup.placements[..3]), 5);
        assert!(!tracker.guidance().show_solve);
        commit_board(&mut tracker, board_with(&setup.placements), 6);
        let (_, cells) = latest_guidance(&rules, &tracker);
        assert!(cells.is_empty());
    }

    #[test]
    fn solve_hotkeys_show_the_solve_only_once_the_setup_is_complete() {
        let rules = ijlo_rules();
        let setup = rules.setup("ijlo_source_method", "ijlo_s_queue").unwrap();
        let mut tracker = s_queue_pc_two();

        // Before completion both keys are ignored and the setup cells stay.
        commit_board(&mut tracker, board_with(&setup.placements[..3]), 3);
        assert!(!latest_setup_complete(&rules, &tracker, true));
        for hotkey in [SolveHotkey::Show, SolveHotkey::Toggle] {
            assert_eq!(
                apply_solve_hotkey(&mut tracker, hotkey, false),
                Some("solve hotkey ignored: setup not complete")
            );
            assert!(!tracker.guidance().show_solve);
        }
        let (status, _) = latest_guidance(&rules, &tracker);
        assert!(status.contains("IJLO guidance active"), "{status}");

        // Completion alone does not reveal the solve.
        commit_board(&mut tracker, board_with(&setup.placements), 4);
        assert!(latest_setup_complete(&rules, &tracker, true));
        let (_, cells) = latest_guidance(&rules, &tracker);
        assert!(cells.is_empty());

        // Plain S shows it and never hides it again.
        assert_eq!(
            apply_solve_hotkey(&mut tracker, SolveHotkey::Show, true),
            Some("solve shown")
        );
        assert_eq!(
            apply_solve_hotkey(&mut tracker, SolveHotkey::Show, true),
            None
        );
        let (status, _) = latest_guidance(&rules, &tracker);
        assert!(status.contains("solve active"), "{status}");

        // Ctrl+Alt+S hides and shows it.
        assert_eq!(
            apply_solve_hotkey(&mut tracker, SolveHotkey::Toggle, true),
            Some("solve hidden")
        );
        assert_eq!(
            apply_solve_hotkey(&mut tracker, SolveHotkey::Toggle, true),
            Some("solve shown")
        );
        assert!(tracker.guidance().show_solve);
    }

    #[test]
    fn revealed_solve_is_hidden_again_on_the_next_pc() {
        let rules = ijlo_rules();
        let setup = rules.setup("ijlo_source_method", "ijlo_s_queue").unwrap();
        let mut tracker = s_queue_pc_two();
        commit_board(&mut tracker, board_with(&setup.placements), 3);
        let complete = latest_setup_complete(&rules, &tracker, true);
        apply_solve_hotkey(&mut tracker, SolveHotkey::Show, complete);
        assert!(tracker.guidance().show_solve);

        // The board empties: the PC is cleared and the next one starts OFF.
        commit_board(&mut tracker, empty_board(), 4);
        assert_eq!(tracker.pc_ordinal().get(), 3);
        assert!(!tracker.guidance().show_solve);
    }

    /// Two 2nd-PC setups of the TILJ case, as the sheet's CSV has them.
    const KONBINI_SECOND: &str = "\"KNEWJADE\",\"ID2\",\"SOL\",\"CASES\",\"FUMEN\"\n\
        \"\",\"TILJ\",\"98.33%\",\"4956/5040\",\"v115@Ghg0DeglAewwBeg0Deglywh0DehlzhJeAgH\"\n\
        \"\",\"TILJ\",\"100.00%\",\"5040/5040\",\"v115@GhwhDeglCeg0whDeglywg0whDehlwwh0whJeAgH\"\n";

    fn konbini_library() -> KonbiniLibrary {
        let mut library = KonbiniLibrary::default();
        library.add_stage_csv(2, KONBINI_SECOND);
        library
    }

    /// PC 1 clears into a 2nd PC that starts with T, I, L, J.
    fn tilj_pc_two() -> SessionTracker {
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        let queue = [Piece::I, Piece::L, Piece::J, Piece::O, Piece::S];
        let mut filled = observation(None, Piece::T, queue);
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        tracker.commit(observation(None, Piece::T, queue), 0.75, 2);
        tracker
    }

    #[test]
    fn konbini_shows_the_best_setup_and_cycles_through_candidates() {
        let library = konbini_library();
        let tracker = tilj_pc_two();
        let snapshot = tracker.snapshots().last().unwrap();
        assert_eq!(
            pc_pieces(&tracker, snapshot)[..4],
            [Piece::T, Piece::I, Piece::L, Piece::J]
        );

        let mut index = 0;
        let (status, cells) = konbini_guidance(&library, &tracker, snapshot, &mut index);
        assert!(
            status.contains("[Konbini 2nd]: TILJ 100.00% [KNEWJADE] (1/2)"),
            "{status}"
        );
        assert_eq!(cells.len(), 16);
        assert!(
            cells
                .iter()
                .filter(|cell| cell.kind == OverlayCellKind::Current)
                .all(|cell| cell.piece == Some(Piece::T))
        );

        // Previous from the first wraps to the last.
        let mut index = -1;
        let (status, _) = konbini_guidance(&library, &tracker, snapshot, &mut index);
        assert!(status.contains("98.33% [KNEWJADE] (2/2)"), "{status}");
        assert_eq!(index, 1);
    }

    #[test]
    fn konbini_drops_setups_the_board_contradicts() {
        let library = konbini_library();
        let mut tracker = tilj_pc_two();
        let mut board = empty_board();
        board[19][0] = Some(Piece::I);
        let mut next = observation(
            None,
            Piece::I,
            [Piece::L, Piece::J, Piece::O, Piece::S, Piece::Z],
        );
        next.board = board;
        tracker.commit(next, 0.75, 3);
        let snapshot = tracker.snapshots().last().unwrap();
        let (status, cells) = konbini_guidance(&library, &tracker, snapshot, &mut 0);
        assert!(
            status.contains("no buildable setup matches the board (2 in sheet)"),
            "{status}"
        );
        assert!(cells.is_empty());
    }

    #[test]
    fn konbini_guide_cycles_and_resets_per_pc() {
        let mut mode = KonbiniGuide::new(PathBuf::from("unused"));
        mode.library = Some(konbini_library());
        let tracker = tilj_pc_two();
        let snapshot = tracker.snapshots().last().unwrap();
        assert!(mode.guide(&tracker, snapshot).status.contains("(1/2)"));
        mode.apply(KonbiniHotkey::Next);
        let guide = mode.guide(&tracker, snapshot);
        assert!(guide.status.contains("(2/2)"), "{}", guide.status);
        assert_eq!(guide.cells.len(), 16);
        assert_eq!(guide.origin, Origin::Konbini);

        // A new PC starts again from the best setup.
        let mut tracker = tracker;
        let mut filled = observation(
            None,
            Piece::T,
            [Piece::I, Piece::L, Piece::J, Piece::O, Piece::S],
        );
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 3);
        tracker.resync(pc_overlay_core::PcOrdinal::new(2).unwrap());
        tracker.commit(
            observation(
                None,
                Piece::T,
                [Piece::I, Piece::L, Piece::J, Piece::O, Piece::S],
            ),
            0.75,
            4,
        );
        let snapshot = tracker.snapshots().last().unwrap();
        let guide = mode.guide(&tracker, snapshot);
        assert!(guide.status.contains("(1/2)"), "{}", guide.status);
    }

    #[test]
    fn konbini_guide_explains_missing_data() {
        let mut mode = KonbiniGuide::new(PathBuf::from("unused"));
        let tracker = tilj_pc_two();
        let guide = mode.guide(&tracker, tracker.snapshots().last().unwrap());
        assert_eq!(
            guide.status,
            "PC Overlay [Konbini]: no data; press Ctrl+Alt+F to fetch"
        );
        assert!(guide.cells.is_empty() && !guide.setup_complete);
    }

    fn unified(
        library: &Library,
        konbini: &mut KonbiniGuide,
        tracker: &SessionTracker,
        prefer_konbini: bool,
    ) -> Guide {
        unified_guidance(
            library,
            konbini,
            tracker,
            tracker.snapshots().last().unwrap(),
            true,
            prefer_konbini,
        )
    }

    fn konbini_guide() -> KonbiniGuide {
        let mut konbini = KonbiniGuide::new(PathBuf::from("unused"));
        let mut library = konbini_library();
        // One 3rd-PC setup, so a later PC has a sheet of its own.
        library.add_stage_csv(3, KONBINI_SECOND);
        konbini.library = Some(library);
        konbini
    }

    #[test]
    fn unified_guidance_prefers_the_source_rules_on_the_second_pc() {
        let library = ijlo_rules();
        let mut konbini = konbini_guide();
        let tracker = s_queue_pc_two();
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_eq!(guide.origin, Origin::Source);
        assert!(
            guide
                .status
                .starts_with("PC Overlay [source]: IJLO guidance active (ijlo_s_queue)"),
            "{}",
            guide.status
        );
        assert_eq!(guide.cells.len(), 16);
        assert!(!guide.setup_complete && guide.note.is_none());

        // Ctrl+Alt+K hands the same state to Setup Konbini.
        let guide = unified(&library, &mut konbini, &tracker, true);
        assert_eq!(guide.origin, Origin::Konbini);
        assert!(guide.status.starts_with("PC Overlay [Konbini 2nd]"));
        assert_eq!(
            guide.note.as_deref(),
            Some("source rules skipped (Ctrl+Alt+K)")
        );
    }

    #[test]
    fn unified_guidance_marks_a_completed_source_setup() {
        let library = ijlo_rules();
        let setup = library.setup("ijlo_source_method", "ijlo_s_queue").unwrap();
        let mut konbini = konbini_guide();
        let mut tracker = s_queue_pc_two();
        commit_board(&mut tracker, board_with(&setup.placements), 3);
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_eq!(guide.origin, Origin::Source);
        assert!(guide.setup_complete);
        assert!(
            guide.status.contains("press S to show the solve"),
            "{}",
            guide.status
        );
    }

    #[test]
    fn unified_guidance_falls_back_to_konbini_without_source_rules() {
        // IJLO only: the TILJ case has no source rules.
        let library = ijlo_rules();
        let mut konbini = konbini_guide();
        let tracker = tilj_pc_two();
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_eq!(guide.origin, Origin::Konbini);
        assert!(
            guide
                .status
                .contains("[Konbini 2nd]: TILJ 100.00% [KNEWJADE] (1/2)"),
            "{}",
            guide.status
        );
        assert_eq!(guide.cells.len(), 16);
        let note = guide.note.unwrap();
        assert!(note.contains("no data for the four-piece case"), "{note}");
    }

    #[test]
    fn unified_guidance_uses_konbini_on_other_pcs() {
        let library = ijlo_rules();
        let mut konbini = konbini_guide();
        let mut tracker = tilj_pc_two();
        tracker.resync(pc_overlay_core::PcOrdinal::new(3).unwrap());
        tracker.commit(
            observation(
                None,
                Piece::T,
                [Piece::I, Piece::L, Piece::J, Piece::O, Piece::S],
            ),
            0.75,
            3,
        );
        assert_eq!(tracker.pc_ordinal().get(), 3);
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_eq!(guide.origin, Origin::Konbini);
        assert!(
            guide.status.starts_with("PC Overlay [Konbini 3rd]"),
            "{}",
            guide.status
        );
        assert!(guide.note.is_none());
    }

    /// The 2nd PC after the 100% TILJ setup is built, with this HOLD and queue.
    fn tilj_setup_built(hold: Option<Piece>, current: Piece, next5: [Piece; 5]) -> SessionTracker {
        let library = konbini_library();
        let setup = library
            .setups
            .iter()
            .find(|setup| setup.sol_percent == Some(100.0))
            .unwrap();
        let mut tracker = tilj_pc_two();
        let mut built = observation(hold, current, next5);
        for cell in &setup.cells {
            built.board[usize::from(cell.cell.y)][usize::from(cell.cell.x)] = cell.piece;
        }
        assert!(matches!(
            tracker.commit(built, 0.75, 3),
            CommitOutcome::Accepted { .. }
        ));
        tracker
    }

    #[test]
    fn a_built_konbini_setup_gets_searched_perfect_clears() {
        let mut mode = KonbiniGuide::new(PathBuf::from("unused"));
        mode.library = Some(konbini_library());
        let queue = [Piece::S, Piece::Z, Piece::I, Piece::T, Piece::L];
        let mut tracker = tilj_setup_built(Some(Piece::J), Piece::O, queue);
        let snapshot = tracker.snapshots().last().unwrap().clone();
        let guide = mode.guide(&tracker, &snapshot);
        assert!(
            guide
                .status
                .contains("perfect clear(s) found (4 lines); press S"),
            "{}",
            guide.status
        );
        assert!(guide.setup_complete && guide.cells.is_empty());
        let count = mode.solve.as_ref().unwrap().solves.len();
        assert!(count >= 1);

        // S shows the first solve: 24 cells over the 16 of the setup.
        apply_solve_hotkey(&mut tracker, SolveHotkey::Show, guide.setup_complete);
        let guide = mode.guide(&tracker, &snapshot);
        assert!(
            guide
                .status
                .contains(&format!("perfect clear 1/{count} (search, SRS+, 4 lines)")),
            "{}",
            guide.status
        );
        assert_eq!(guide.cells.len(), 24);
        let coloured = |cells: &[OverlayCell]| -> BTreeSet<(Cell, Option<Piece>)> {
            cells.iter().map(|cell| (cell.cell, cell.piece)).collect()
        };
        let first = coloured(&guide.cells);

        // N switches to the next solve when there is one.
        mode.apply(KonbiniHotkey::Next);
        let guide = mode.guide(&tracker, &snapshot);
        if count > 1 {
            assert!(
                guide.status.contains(&format!("2/{count}")),
                "{}",
                guide.status
            );
            assert_ne!(coloured(&guide.cells), first);
        }

        // Placing the solve's first piece keeps the same solve, minus it.
        let session = mode.solve.as_ref().unwrap();
        let solve = &session.solves[session.index as usize];
        let (piece, cells) = solve.steps[0];
        let mut board = snapshot.observation.board;
        for index in 0..60u32 {
            if cells & (1 << index) != 0 {
                board[(index / 10) as usize][(index % 10) as usize] = Some(piece);
            }
        }
        let shown = guide.cells.len();
        let mut placed = observation(
            None,
            Piece::S,
            [Piece::Z, Piece::I, Piece::T, Piece::L, Piece::J],
        );
        placed.board = board;
        tracker.commit(placed, 0.75, 4);
        let snapshot = tracker.snapshots().last().unwrap().clone();
        let guide = mode.guide(&tracker, &snapshot);
        assert!(guide.status.contains("perfect clear"), "{}", guide.status);
        assert_eq!(guide.cells.len(), shown - 4, "{}", guide.status);
    }
}
