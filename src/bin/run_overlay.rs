use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use pc_overlay_core::{
    BoardCalibration, CommitOutcome, DiagramSet, FrameSize, OverlayBoard, OverlayCell,
    OverlayCellKind, OverlayState, OverlayWindow, PcOrdinal, Piece, RecognizerConfig, RuleSet,
    SessionTracker, SolveProgress, SourceSections, StabilityConfig, StabilityFilter,
    TetrioRecognizer, TrackerConfig, WindowInfo, WindowLocator, board_is_empty,
    finder::{Buildable, Finder},
    konbini,
    konbini::{BuildQuery, FoundSolve, KonbiniLibrary, KonbiniProgress},
    piece_code,
    tracker::case_size,
    window::foreground_handle,
};

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
    let library = Library::load(rules, &args.cases)?.with_source_sections(&args.rules);
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
    let mut pieces_reading = PiecesReading::default();
    let hotkeys = keyboard_hook::start();
    let mut solve_notice: Option<(&'static str, Instant)> = None;
    let mut konbini = KonbiniGuide::new(args.konbini.clone());
    konbini.load_or_fetch();
    // Ctrl+Alt+K: the 2nd and 7th PCs use Setup Konbini even when the source has rules.
    let mut prefer_konbini = false;
    // Whether the last stable frame showed a completed setup with a solve.
    let mut solve_available = false;
    let mut overlay = OverlayWindow::new()?;
    println!(
        "run-overlay started; Ctrl+Alt+1..7 resynchronizes the PC number; S shows / Ctrl+Alt+S toggles the solve once the setup is complete; Ctrl+Alt+N / Ctrl+Alt+B switch setups; Ctrl+Alt+K makes the 2nd and 7th PCs use Setup Konbini; Ctrl+Alt+F refetches Setup Konbini; press Ctrl+C to stop"
    );

    loop {
        if !overlay.pump() {
            break;
        }
        for hotkey in hotkeys.try_iter() {
            match hotkey {
                Hotkey::Resync(ordinal) => {
                    tracker.resync(ordinal);
                    stability.reset();
                    eprintln!("PC number manually resynchronized to {}", ordinal.get());
                }
                Hotkey::Konbini(KonbiniHotkey::PreferKonbini) => {
                    prefer_konbini = !prefer_konbini;
                    let notice = if prefer_konbini {
                        "2nd/7th PC: Setup Konbini"
                    } else {
                        "2nd/7th PC: source rules first"
                    };
                    eprintln!("{notice}");
                    solve_notice = Some((notice, Instant::now()));
                }
                Hotkey::Konbini(hotkey) => konbini.apply(hotkey),
                Hotkey::Solve(hotkey) => {
                    if let Some(notice) = apply_solve_hotkey(&mut tracker, hotkey, solve_available)
                    {
                        eprintln!("{notice}");
                        solve_notice = Some((notice, Instant::now()));
                    }
                }
            }
        }
        konbini.poll_fetch();
        let active_notice = solve_notice
            .filter(|(_, at)| at.elapsed() < SOLVE_NOTICE_DURATION)
            .map(|(notice, _)| notice);
        let located = locator.locate();
        keyboard_hook::set_target(
            located
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .map(|window| window.handle),
        );
        match located {
            // TETR.IO is in the background: hide the overlay and skip
            // recognition until it has focus again.
            Ok(Some(window)) if foreground_handle() != Some(window.handle) => {
                stability.clear_pending();
                overlay.update(None, OverlayState::default())?;
            }
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
                // The PIECES counter, when shown, decides the PC number; the
                // tracker's own count is the fallback when it is unreadable.
                let pieces = pieces_reading.confirm(recognition.pieces_placed);
                // Only when the latest snapshot is this frame's state, so the
                // pieces placed in the PC match the counter.
                let tracked_now = commit.as_ref().is_some_and(|commit| {
                    !matches!(
                        commit,
                        CommitOutcome::IgnoredLowConfidence { .. }
                            | CommitOutcome::RejectedHistoryDiscontinuity
                    )
                });
                if tracked_now
                    && let Some(pieces) = pieces
                    && let Some(ordinal) = tracker.ordinal_from_pieces(pieces)
                    && tracker.align_pc_ordinal(ordinal)
                {
                    eprintln!(
                        "PC number aligned to {} from PIECES {pieces}",
                        tracker.pc_ordinal().get()
                    );
                }
                // Always the guidance for the last tracked state, so it stays
                // on screen while recognition pauses (a piece falling below
                // the spawn rows, or frames settling after a lock).
                let guide = tracker.snapshots().last().map(|snapshot| {
                    unified_guidance(
                        &library,
                        &mut konbini,
                        &tracker,
                        snapshot,
                        args.reviewed_only,
                        prefer_konbini,
                    )
                });
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
                    pieces,
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

/// The PIECES counter, trusted once two consecutive frames read the same
/// value, so a frame caught mid-redraw cannot move the PC number.
#[derive(Debug, Default)]
struct PiecesReading {
    last: Option<u32>,
}

impl PiecesReading {
    fn confirm(&mut self, reading: Option<u32>) -> Option<u32> {
        let confirmed = reading.filter(|_| self.last == reading);
        self.last = reading;
        confirmed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SolveHotkey {
    /// Ctrl+Alt+S: show or hide.
    Toggle,
    /// Plain S: only shows, so S doubling as a game key never hides a solve.
    Show,
}

/// A hotkey press, as delivered by the keyboard hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    /// Ctrl+Alt+1..7: resynchronize the PC number.
    Resync(PcOrdinal),
    /// S or Ctrl+Alt+S.
    Solve(SolveHotkey),
    /// Ctrl+Alt+K / N / B / F.
    Konbini(KonbiniHotkey),
}

/// The hotkey a key-down of virtual key `key` makes, given whether Ctrl and
/// Alt are both held.
fn hotkey_for(key: u32, ctrl_alt: bool) -> Option<Hotkey> {
    match (key, ctrl_alt) {
        (0x53, true) => Some(Hotkey::Solve(SolveHotkey::Toggle)),
        (0x53, false) => Some(Hotkey::Solve(SolveHotkey::Show)),
        (_, false) => None,
        (0x31..=0x37, true) => PcOrdinal::new((key - 0x30) as u8).map(Hotkey::Resync),
        (0x4B, true) => Some(Hotkey::Konbini(KonbiniHotkey::PreferKonbini)),
        (0x4E, true) => Some(Hotkey::Konbini(KonbiniHotkey::Next)),
        (0x42, true) => Some(Hotkey::Konbini(KonbiniHotkey::Previous)),
        (0x46, true) => Some(Hotkey::Konbini(KonbiniHotkey::Refetch)),
        _ => None,
    }
}

/// Hotkeys from a low-level keyboard hook on a thread of its own. Polling
/// the key state once per recognition loop missed about a third of presses,
/// since a tap shorter than one loop was never seen down; the hook receives
/// every key-down as it happens. It only observes: each key is passed on,
/// so the game still receives S and the rest. Presses count only while
/// TETR.IO has focus, so typing S elsewhere does not reveal a solve.
#[cfg(windows)]
mod keyboard_hook {
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    };

    use pc_overlay_core::window::foreground_handle;

    use windows_sys::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::GetAsyncKeyState,
            WindowsAndMessaging::{
                CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
                WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            },
        },
    };

    use super::{Hotkey, hotkey_for};

    struct HookState {
        sender: Sender<Hotkey>,
        /// Keys currently down, so auto-repeated key-downs count once.
        down: [bool; 256],
    }

    static STATE: OnceLock<Mutex<HookState>> = OnceLock::new();
    /// The TETR.IO window's handle, or 0 while there is none.
    static TARGET: AtomicU64 = AtomicU64::new(0);

    /// Sets the window that must have focus for a press to count.
    pub fn set_target(handle: Option<u64>) {
        TARGET.store(handle.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn start() -> Receiver<Hotkey> {
        let (sender, receiver) = mpsc::channel();
        let state = HookState {
            sender,
            down: [false; 256],
        };
        if STATE.set(Mutex::new(state)).is_ok() {
            std::thread::spawn(|| {
                // SAFETY: `hook_proc` matches HOOKPROC and stays valid for the
                // whole process. A low-level hook is called on the installing
                // thread, which therefore runs a message loop until exit.
                unsafe {
                    let module = GetModuleHandleW(std::ptr::null());
                    let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0);
                    if hook.is_null() {
                        eprintln!("warning: keyboard hook unavailable; hotkeys are disabled");
                        return;
                    }
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {}
                }
            });
        }
        receiver
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for HC_ACTION, `lparam` points to a KBDLLHOOKSTRUCT.
            let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            record(event.vkCode, wparam as u32);
        }
        // SAFETY: passes the event on unchanged; the hook handle is optional.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    fn record(key: u32, message: u32) {
        let Some(Ok(mut state)) = STATE.get().map(Mutex::lock) else {
            return;
        };
        let Some(down) = state.down.get_mut(key as usize) else {
            return;
        };
        match message {
            WM_KEYDOWN | WM_SYSKEYDOWN if !*down => {
                *down = true;
                const VK_CONTROL: i32 = 0x11;
                const VK_MENU: i32 = 0x12;
                // SAFETY: GetAsyncKeyState takes a virtual key code only. In
                // the hook it reflects every key but the one being pressed.
                let ctrl_alt =
                    unsafe { GetAsyncKeyState(VK_CONTROL) < 0 && GetAsyncKeyState(VK_MENU) < 0 };
                let target = TARGET.load(Ordering::Relaxed);
                let focused = target != 0 && foreground_handle() == Some(target);
                if focused && let Some(hotkey) = hotkey_for(key, ctrl_alt) {
                    // The receiver lives as long as the overlay loop.
                    let _ = state.sender.send(hotkey);
                }
            }
            WM_KEYUP | WM_SYSKEYUP => *down = false,
            _ => {}
        }
    }
}

#[cfg(not(windows))]
mod keyboard_hook {
    use std::sync::mpsc::{self, Receiver};

    pub fn start() -> Receiver<super::Hotkey> {
        mpsc::channel().1
    }

    pub fn set_target(_handle: Option<u64>) {}
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
    /// Ctrl+Alt+K: the 2nd and 7th PCs use Setup Konbini even when the source has
    /// rules for its case (press again to go back).
    PreferKonbini,
    /// Ctrl+Alt+N: the next candidate setup.
    Next,
    /// Ctrl+Alt+B: the previous candidate setup.
    Previous,
    /// Ctrl+Alt+F: download the sheets again.
    Refetch,
}

/// Where the guidance on screen comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The verified 2nd-PC rules from "Algebruh's 2nd Methods".
    Source,
    /// A Setup Konbini setup the live queue can build.
    Konbini,
    /// A 2-line perfect clear the shown pieces can play, before either.
    TwoLine,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Source => "source",
            Origin::Konbini => "Konbini",
            Origin::TwoLine => "2L PC",
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
    /// The Setup Konbini PC rate (`SOL`) of the setup shown or built.
    pc_rate: Option<f32>,
}

/// One guidance for every PC. A 2-line perfect clear the shown pieces can
/// play comes first. The 2nd and 7th PCs then use the source rules when they
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
    if let Some(guide) = konbini.two_line_guide(tracker, snapshot) {
        return guide;
    }
    let mut note = None;
    if case_size(snapshot.pc_ordinal).is_some() {
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
                        pc_rate: None,
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
    /// The PC rate of the setup last seen complete in this PC, kept for the
    /// solves searched from it.
    built_rate: Option<f32>,
    /// 2-line perfect clears of the last state searched, while the board
    /// follows one of them.
    two_line: Option<SolveSession>,
    /// Whether the last guide was a 2-line PC, so Ctrl+Alt+N / B cycle it.
    two_line_shown: bool,
}

/// Perfect clears found by the solve search, cycled by Ctrl+Alt+N / B.
#[derive(Debug, Clone)]
struct SolveSession {
    solves: Vec<FoundSolve>,
    index: isize,
    /// The PC rate of the setup the search started from.
    pc_rate: Option<f32>,
    /// The state `solves` were last checked against.
    checked: pc_overlay_core::Observation,
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
    /// The PC rate for the state on screen (`konbini::state_pc_rate`):
    /// outer `None` until rated, inner `None` when it cannot be rated.
    rate: Option<Option<f32>>,
}

/// Time spent rating candidates per frame; the rest wait for the next frame.
const RATE_BUDGET: Duration = Duration::from_millis(40);

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
            built_rate: None,
            two_line: None,
            two_line_shown: false,
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
            KonbiniHotkey::Next => match self.cycled_solve() {
                Some(session) => session.index += 1,
                None => self.index += 1,
            },
            // Wraps to the last candidate once the count is known.
            KonbiniHotkey::Previous => match self.cycled_solve() {
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

    /// The perfect clears Ctrl+Alt+N / B switch: the 2-line PCs while one is
    /// shown, else those found for a built setup.
    fn cycled_solve(&mut self) -> Option<&mut SolveSession> {
        if self.two_line_shown {
            self.two_line.as_mut()
        } else {
            self.solve.as_mut()
        }
    }

    /// Starts the candidate index and the searches over at a new PC.
    fn sync_pc(&mut self, tracker: &SessionTracker) {
        let pc_key = tracker
            .current_pc_start()
            .map(|start| (start.pc_ordinal.get(), start.timestamp_ms));
        if pc_key != self.pc_key {
            self.pc_key = pc_key;
            self.index = 0;
            self.solve = None;
            self.searched = None;
            self.built_rate = None;
            self.two_line = None;
        }
    }

    /// A 2-line perfect clear the board follows and HOLD, Current, and NEXT
    /// can play, shown directly: the shape is the whole PC. `None` once the
    /// board leaves every one, and the usual guidance takes over.
    fn two_line_guide(
        &mut self,
        tracker: &SessionTracker,
        snapshot: &pc_overlay_core::tracker::Snapshot,
    ) -> Option<Guide> {
        self.sync_pc(tracker);
        let observation = &snapshot.observation;
        let kept = self.two_line.as_mut().is_some_and(|session| {
            if session.checked != *observation {
                session
                    .solves
                    .retain(|solve| solve.playable(&mut self.finder, observation));
                session.checked = observation.clone();
            }
            !session.solves.is_empty()
        });
        if !kept {
            let solves =
                konbini::find_two_line_perfect_clears(&mut self.finder, observation, SOLVE_LIMIT);
            self.two_line = (!solves.is_empty()).then(|| SolveSession {
                solves,
                index: 0,
                pc_rate: None,
                checked: observation.clone(),
            });
        }
        let guide = self
            .two_line
            .as_mut()
            .and_then(|session| two_line_render(session, snapshot));
        self.two_line_shown = guide.is_some();
        if !self.two_line_shown {
            self.two_line = None;
        }
        guide
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
        self.sync_pc(tracker);
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
                pc_rate: None,
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
        let shortlist = &mut self.shortlist.as_mut().expect("just computed").1;
        rate_shortlist(
            library,
            &mut self.finder,
            shortlist,
            snapshot,
            &mut self.index,
            RATE_BUDGET,
        );
        let shortlist = &*shortlist;
        let observation = &snapshot.observation;
        let show = tracker.guidance().show_solve;
        // Keep only the solves the pieces shown now can still finish: the
        // player may have used HOLD or an order the solve does not allow.
        if let Some(session) = self.solve.as_mut()
            && session.checked != *observation
        {
            session
                .solves
                .retain(|solve| solve.playable(&mut self.finder, observation));
            session.checked = observation.clone();
            if session.solves.is_empty() {
                self.solve = None;
            }
        }
        if let Some(guide) = solve_guide(&mut self.solve, snapshot, show) {
            return guide;
        }
        let guide = konbini_render(library, snapshot, shortlist, &mut self.index);
        let shown_complete = usize::try_from(self.index)
            .ok()
            .and_then(|index| shortlist.get(index))
            .is_some_and(|candidate| candidate.progress.complete());
        if shown_complete {
            self.built_rate = guide.pc_rate;
        }
        let board_off_setup = shortlist.is_empty() && !board_is_empty(&observation.board);
        if (shown_complete || board_off_setup) && self.searched.as_ref() != Some(observation) {
            // A built setup, or a board no setup explains (the player went
            // their own way): search the perfect clears from here.
            self.searched = Some(observation.clone());
            let solves = konbini::find_perfect_clears(&mut self.finder, observation, SOLVE_LIMIT);
            if !solves.is_empty() {
                self.solve = Some(SolveSession {
                    solves,
                    index: 0,
                    pc_rate: self.built_rate,
                    checked: observation.clone(),
                });
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
        pc_rate: session.pc_rate,
    })
}

/// The 2-line PC at the session's index, or the next one the board follows.
fn two_line_render(
    session: &mut SolveSession,
    snapshot: &pc_overlay_core::tracker::Snapshot,
) -> Option<Guide> {
    let observation = &snapshot.observation;
    let count = session.solves.len();
    let start = session.index.rem_euclid(count as isize) as usize;
    let (position, progress) =
        (0..count)
            .map(|offset| (start + offset) % count)
            .find_map(|position| {
                session.solves[position]
                    .progress(&observation.board)
                    .map(|progress| (position, progress))
            })?;
    session.index = position as isize;
    let mut pieces: Vec<char> = session.solves[position]
        .steps
        .iter()
        .map(|(piece, _)| piece_code(*piece))
        .collect();
    pieces.sort_unstable();
    let pieces: String = pieces.into_iter().collect();
    let status = format!(
        "PC Overlay [2L PC]: 2-line perfect clear {}/{count} ({pieces}); the next PC is the {}; Ctrl+Alt+N/B to switch",
        position + 1,
        konbini::stage_label(snapshot.pc_ordinal.after_pieces(5).get()),
    );
    let cells = progress
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
        .collect();
    Some(Guide {
        status,
        cells,
        origin: Origin::TwoLine,
        setup_complete: false,
        note: None,
        pc_rate: None,
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
            rate: None,
        })
        .collect()
}

/// Rates the shown candidate, then the others in order, until `budget` is
/// spent (at least one per call); the rest wait for the next frame. Once
/// every candidate is rated they are ordered by that rate (setups buildable
/// whatever the next pieces still first), and `index` follows the shown one.
fn rate_shortlist(
    library: &KonbiniLibrary,
    finder: &mut Finder,
    shortlist: &mut [Shortlisted],
    snapshot: &pc_overlay_core::tracker::Snapshot,
    index: &mut isize,
    budget: Duration,
) {
    if shortlist.iter().all(|candidate| candidate.rate.is_some()) {
        return;
    }
    let started = Instant::now();
    let observation = &snapshot.observation;
    // As for the build search: 10 pieces per earlier PC (the 7-bag position).
    let before = 10 * usize::from(snapshot.pc_ordinal.get() - 1);
    let shown = index.rem_euclid(shortlist.len() as isize) as usize;
    let order = std::iter::once(shown).chain((0..shortlist.len()).filter(|at| *at != shown));
    for at in order {
        if shortlist[at].rate.is_some() {
            continue;
        }
        if started.elapsed() >= budget && at != shown {
            return;
        }
        let candidate = &mut shortlist[at];
        let first_index =
            before + candidate.progress.placed_pieces() + usize::from(observation.hold.is_some());
        candidate.rate = Some(konbini::state_pc_rate(
            finder,
            &library.setups[candidate.setup],
            &candidate.progress,
            observation,
            first_index,
        ));
    }
    let shown_setup = shortlist[shown].setup;
    // Stable: ties keep the sheet's order.
    shortlist.sort_by(|a, b| {
        let key = |candidate: &Shortlisted| {
            (
                candidate.buildable != Buildable::Always,
                -candidate.rate.flatten().unwrap_or(-1.0),
            )
        };
        key(a).partial_cmp(&key(b)).expect("rates are finite")
    });
    if *index != 0 {
        // A candidate picked with Ctrl+Alt+N / B stays shown.
        *index = shortlist
            .iter()
            .position(|candidate| candidate.setup == shown_setup)
            .expect("still listed") as isize;
    }
}

#[cfg(test)]
fn konbini_guidance(
    library: &KonbiniLibrary,
    tracker: &SessionTracker,
    snapshot: &pc_overlay_core::tracker::Snapshot,
    index: &mut isize,
) -> (String, Vec<OverlayCell>) {
    let mut finder = Finder::new();
    let mut shortlist = konbini_shortlist(library, &mut finder, tracker, snapshot);
    rate_shortlist(
        library,
        &mut finder,
        &mut shortlist,
        snapshot,
        index,
        Duration::MAX,
    );
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
            pc_rate: None,
        };
    }
    let position = index.rem_euclid(candidates.len() as isize) as usize;
    *index = position as isize;
    let candidate = &candidates[position];
    let setup = &library.setups[candidate.setup];
    // The rate for the state on screen; the sheet's when they differ, as the
    // sheet assumes the pieces its ID names.
    let rate = candidate.rate.flatten();
    let sol = match (rate, setup.sol_percent) {
        (Some(rate), Some(sol)) if (rate - sol).abs() < 0.005 => format!(" {rate:.2}%"),
        (Some(rate), Some(sol)) => format!(" {rate:.2}% here (sheet {sol:.2}%)"),
        (Some(rate), None) => format!(" {rate:.2}% here"),
        (None, Some(sol)) if candidate.rate.is_none() => format!(" sheet {sol:.2}%, rating"),
        (None, Some(sol)) => format!(" sheet {sol:.2}%"),
        (None, None) => String::new(),
    };
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
        pc_rate: rate,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_overlay_state(
    window: &WindowInfo,
    frame: &pc_overlay_core::CapturedFrame,
    recognition: &pc_overlay_core::RecognitionResult,
    stable: Option<&pc_overlay_core::StabilityOutcome>,
    commit: Option<&CommitOutcome>,
    tracker: &SessionTracker,
    pieces: Option<u32>,
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

    // Why this frame did not update the tracked state, if it did not.
    let paused = if recognition.observation.is_none() {
        Some("recognition paused (unknown or unstable)".to_string())
    } else {
        match stable {
            Some(pc_overlay_core::StabilityOutcome::Stable { .. }) => None,
            Some(pc_overlay_core::StabilityOutcome::Pending { consecutive_frames }) => {
                Some(format!("waiting for stable frame ({consecutive_frames}/3)"))
            }
            Some(pc_overlay_core::StabilityOutcome::IgnoredLowConfidence { .. }) => {
                Some("recognition confidence too low".to_string())
            }
            None => Some("recognition paused (unknown or unstable)".to_string()),
        }
    };
    let mut badge = String::new();
    let (status, cells) = match (guide, paused) {
        (Some(guide), paused) => {
            detail.push_str(&format!("  guide={}", guide.origin.label()));
            if let Some(note) = &guide.note {
                detail.push_str(&format!(" ({note})"));
            }
            if let Some(rate) = guide.pc_rate {
                badge = format!("PC {rate:.2}%");
            }
            // The guidance of the last tracked state stays up while a piece
            // falls; recognition resumes once it is next stable.
            let status = match paused {
                Some(reason) => format!("{} | last stable state: {reason}", guide.status),
                None => guide.status,
            };
            (status, guide.cells)
        }
        (None, Some(reason)) => (format!("PC Overlay: {reason}"), Vec::new()),
        (None, None) => (
            "PC Overlay: waiting for first tracked state".to_string(),
            Vec::new(),
        ),
    };

    if let Some(commit) = commit {
        detail.push_str("  state=");
        detail.push_str(commit_name(commit));
    }
    detail.push_str(&format!("  PC {}", tracker.pc_ordinal().get()));
    match pieces {
        Some(pieces) => detail.push_str(&format!(" (PIECES {pieces})")),
        None => detail.push_str(" (own count)"),
    }
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
        badge,
        board: Some(board),
        cells,
    }
}

/// Every case's rules. The hand-checked IJLO data (`--rules`) comes first;
/// generated cases in `--cases` with the same id are skipped.
struct Library {
    cases: Vec<RuleSet>,
    /// Headings of each source document (2nd, Bad 2nd, 7th) by file name, to
    /// name each setup's section.
    sections: BTreeMap<String, SourceSections>,
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
        Ok(Self {
            cases,
            sections: BTreeMap::new(),
        })
    }

    /// Reads the headings of every source document the rules cite by file
    /// name; they sit in the repository root, the parent of the rules'
    /// `data/`. Without one, its setups are shown without their section.
    fn with_source_sections(mut self, rules_path: &Path) -> Self {
        let root = rules_path
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."));
        let documents: BTreeSet<String> = self
            .cases
            .iter()
            .map(|rules| rules.case.source.document.clone())
            .collect();
        for document in documents {
            let path = root.join(&document);
            match SourceSections::load(&path) {
                Ok(sections) => {
                    self.sections.insert(document, sections);
                }
                Err(error) => eprintln!(
                    "warning: {}: {error}; setups are shown without their source section",
                    path.display()
                ),
            }
        }
        self
    }

    /// ` · Odd JL, with T › TIJZ/TILS (mirrored)` for a setup, or empty.
    fn source_label(&self, setup: &pc_overlay_core::rules::SetupRule) -> String {
        let Some(section) = self
            .sections
            .get(&setup.source.document)
            .and_then(|sections| sections.section(setup.source.lines[0]))
        else {
            return String::new();
        };
        // Generated cases mirror the source's case with an `_m` id.
        let mirrored = if setup.id.ends_with("_m") {
            " (mirrored)"
        } else {
            ""
        };
        format!(" · {section}{mirrored}")
    }

    fn case_ids(&self) -> String {
        self.cases
            .iter()
            .map(|rules| rules.case.id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The rules for the case the tracked 2nd or 7th PC started with.
    fn for_tracker(&self, tracker: &SessionTracker) -> Option<&RuleSet> {
        let start = tracker.current_pc_start()?;
        let size = case_size(start.pc_ordinal)?;
        let key = observation_case(&start.observation, size);
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
        .unwrap_or("source PC");
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
                        "PC Overlay: {case} setup complete ({}{}); press S to show the solve",
                        setup.id,
                        library.source_label(setup)
                    ),
                    Vec::new(),
                );
            }
            let plan = pc_overlay_core::OverlayPlan::from_progress(&progress);
            return (
                format!(
                    "PC Overlay: {case} solve active ({}{})",
                    solve.id,
                    library.source_label(setup)
                ),
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
        format!(
            "PC Overlay: {case} guidance active ({}{})",
            setup.id,
            library.source_label(setup)
        )
    } else {
        format!(
            "PC Overlay: {case} reference guidance active ({}, unreviewed{})",
            setup.id,
            library.source_label(setup)
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
    let ordinal = snapshot.pc_ordinal.get();
    let Some(size) = case_size(snapshot.pc_ordinal) else {
        return GuidanceStage::Inactive(format!(
            "PC Overlay: tracking PC {ordinal}; source guidance activates on PC 2 and PC 7"
        ));
    };
    let Some(rules) = library.for_tracker(tracker) else {
        let case = tracker
            .current_pc_start()
            .map(|start| observation_case(&start.observation, size))
            .unwrap_or_default();
        let count = if size == 3 { "three" } else { "four" };
        return GuidanceStage::Inactive(format!(
            "PC Overlay: PC {ordinal} detected, no data for the {count}-piece case {case}"
        ));
    };
    let case = rules.case.id.as_str();

    let Some(context) = tracker.following_bag_context() else {
        return GuidanceStage::Inactive(format!("PC Overlay: PC {ordinal} start was not observed"));
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

/// A PC's case consists of HOLD plus the first pieces that can be played,
/// `size` in all (`case_size`), independent of their order: returned sorted,
/// as `case_key` sorts ids.
fn observation_case(observation: &pc_overlay_core::Observation, size: usize) -> String {
    let mut pieces = Vec::with_capacity(size);
    if let Some(hold) = observation.hold {
        pieces.push(hold);
    }
    pieces.push(observation.current);
    pieces.extend(
        observation
            .next5
            .iter()
            .copied()
            .take(size.saturating_sub(pieces.len())),
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
        CommitOutcome::NewGame { .. } => "new_game",
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
            observation_case(
                &observation(
                    Some(Piece::I),
                    Piece::J,
                    [Piece::L, Piece::O, Piece::S, Piece::Z, Piece::T],
                ),
                4
            ),
            ijlo
        );
        assert_eq!(
            observation_case(
                &observation(
                    Some(Piece::I),
                    Piece::J,
                    [Piece::L, Piece::S, Piece::O, Piece::Z, Piece::T],
                ),
                4
            ),
            case_key("IJLS")
        );
        assert_eq!(
            observation_case(
                &observation(
                    None,
                    Piece::O,
                    [Piece::L, Piece::I, Piece::J, Piece::S, Piece::Z],
                ),
                4
            ),
            ijlo
        );
        // Duplicate pieces from two bags form cases such as TJJO.
        assert_eq!(
            observation_case(
                &observation(
                    None,
                    Piece::J,
                    [Piece::T, Piece::O, Piece::J, Piece::S, Piece::Z],
                ),
                4
            ),
            case_key("TJJO")
        );
        // The 7th PC starts with a three-piece case.
        assert_eq!(
            observation_case(
                &observation(
                    Some(Piece::T),
                    Piece::O,
                    [Piece::I, Piece::J, Piece::S, Piece::Z, Piece::L],
                ),
                3
            ),
            case_key("TIO")
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

    /// The tracked PC `ordinal` starts on an empty board with this queue.
    fn tracker_at_pc_start(ordinal: u8, current: Piece, next5: [Piece; 5]) -> SessionTracker {
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        tracker.resync(pc_overlay_core::PcOrdinal::new(ordinal - 1).unwrap());
        let mut filled = observation(None, current, next5);
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        tracker.commit(observation(None, current, next5), 0.75, 2);
        assert_eq!(
            tracker.snapshots().last().unwrap().pc_ordinal.get(),
            ordinal
        );
        tracker
    }

    #[test]
    fn bad_second_and_seventh_cases_get_source_guidance() {
        let library = Library::load(ijlo_only(), &root().join("data/cases"))
            .unwrap()
            .with_source_sections(&root().join("data/ijlo.json"));

        // IJOS, from "Algebruh's Bad 2nd".
        let tracker = tracker_at_pc_start(
            2,
            Piece::I,
            [Piece::J, Piece::O, Piece::S, Piece::T, Piece::L],
        );
        let snapshot = tracker.snapshots().last().unwrap();
        let (status, cells) = guidance_for_snapshot(&library, &tracker, snapshot, true);
        assert!(status.contains("IJOS guidance active"), "{status}");
        assert!(status.contains("IJOS/ILOZ"), "{status}");
        assert!(!cells.is_empty());

        // TIO, a three-piece case from "Algebruh's 7th".
        let tracker = tracker_at_pc_start(
            7,
            Piece::T,
            [Piece::I, Piece::O, Piece::S, Piece::J, Piece::L],
        );
        let snapshot = tracker.snapshots().last().unwrap();
        let (status, cells) = guidance_for_snapshot(&library, &tracker, snapshot, true);
        assert!(status.contains("TIO guidance active"), "{status}");
        assert!(status.contains("With T"), "{status}");
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
            sections: BTreeMap::new(),
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
    fn key_downs_map_to_hotkeys_only_with_their_modifiers() {
        const VK_S: u32 = 0x53;
        assert_eq!(
            hotkey_for(VK_S, false),
            Some(Hotkey::Solve(SolveHotkey::Show))
        );
        assert_eq!(
            hotkey_for(VK_S, true),
            Some(Hotkey::Solve(SolveHotkey::Toggle))
        );
        assert_eq!(
            hotkey_for(0x33, true),
            Some(Hotkey::Resync(PcOrdinal::new(3).unwrap()))
        );
        assert_eq!(hotkey_for(0x33, false), None, "plain 3 is a game key");
        assert_eq!(hotkey_for(0x38, true), None, "there is no 8th PC");
        assert_eq!(
            hotkey_for(0x4E, true),
            Some(Hotkey::Konbini(KonbiniHotkey::Next))
        );
        assert_eq!(hotkey_for(0x4E, false), None);
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

    /// The 2nd PC with this HOLD and queue (the bag's last three after HOLD).
    fn pc_two_with_hold(hold: Piece, current: Piece, next5: [Piece; 5]) -> SessionTracker {
        let mut tracker = SessionTracker::new(TrackerConfig::default());
        let mut filled = observation(Some(hold), current, next5);
        filled.board[0][0] = Some(Piece::T);
        tracker.commit(filled, 0.75, 1);
        tracker.commit(observation(Some(hold), current, next5), 0.75, 2);
        assert_eq!(tracker.pc_ordinal().get(), 2);
        tracker
    }

    #[test]
    fn konbini_rates_a_setup_for_the_pieces_on_screen() {
        use Piece::*;
        // TILZ built from I, L, Z: the sheet's 100% keeps the T for the PC.
        let mut library = KonbiniLibrary::default();
        library.add_stage_csv(
            2,
            "\"KNEWJADE\",\"ID2\",\"SOL\",\"CASES\",\"FUMEN\"
             \"\",\"TILZ\",\"100.00%\",\"5040/5040\",\"v115@HhglIeglBtGehlBtAezhKeAgH\"
",
        );
        let named = pc_two_with_hold(T, I, [L, Z, J, O, S]);
        let snapshot = named.snapshots().last().unwrap();
        let (status, _) = konbini_guidance(&library, &named, snapshot, &mut 0);
        assert!(status.contains("TILZ 100.00% [KNEWJADE]"), "{status}");

        // O instead of T: O stays in HOLD, and only 1 in 4 orders of the
        // bag's rest leave a perfect clear.
        let other = pc_two_with_hold(O, I, [L, Z, J, O, S]);
        let snapshot = other.snapshots().last().unwrap();
        let (status, _) = konbini_guidance(&library, &other, snapshot, &mut 0);
        assert!(
            status.contains("TILZ 25.00% here (sheet 100.00%) [KNEWJADE]"),
            "{status}"
        );
    }

    #[test]
    fn rating_spreads_over_frames_and_then_orders_by_rate() {
        let library = konbini_library();
        let tracker = tilj_pc_two();
        let snapshot = tracker.snapshots().last().unwrap();
        let mut finder = Finder::new();
        let mut shortlist = konbini_shortlist(&library, &mut finder, &tracker, snapshot);
        assert_eq!(shortlist.len(), 2);
        // No budget: only the shown candidate is rated this frame.
        let mut index = 1;
        rate_shortlist(
            &library,
            &mut finder,
            &mut shortlist,
            snapshot,
            &mut index,
            Duration::ZERO,
        );
        assert!(shortlist[1].rate.is_some() && shortlist[0].rate.is_none());
        rate_shortlist(
            &library,
            &mut finder,
            &mut shortlist,
            snapshot,
            &mut index,
            Duration::MAX,
        );
        let rates: Vec<f32> = shortlist
            .iter()
            .map(|candidate| candidate.rate.flatten().unwrap())
            .collect();
        assert!(rates[0] >= rates[1], "{rates:?}");
        // The candidate picked with Ctrl+Alt+N stays shown after the reorder.
        let picked = konbini_shortlist(&library, &mut finder, &tracker, snapshot)[1].setup;
        assert_eq!(shortlist[index as usize].setup, picked);
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
    fn a_two_line_pc_comes_before_the_source_rules_and_konbini() {
        use Piece::*;
        let library = ijlo_rules();
        let mut konbini = konbini_guide();
        let mut tracker = tracker_at_pc_start(2, O, [I, L, J, O, S]);
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_eq!(guide.origin, Origin::TwoLine);
        assert!(
            guide
                .status
                .starts_with("PC Overlay [2L PC]: 2-line perfect clear 1/")
                && guide.status.contains("(IJLOO)")
                && guide.status.contains("the next PC is the 6th"),
            "{}",
            guide.status
        );
        // The whole PC is drawn, both O filled in as the Current piece (by
        // piece, as for Setup Konbini).
        assert_eq!(guide.cells.len(), 20);
        assert_eq!(
            guide
                .cells
                .iter()
                .filter(|cell| cell.kind == OverlayCellKind::Current)
                .count(),
            8
        );
        assert!(!guide.setup_complete);

        // A T on the board: no 2-line PC, the usual guidance again.
        let mut off = observation(None, I, [L, J, O, S, Z]);
        for (x, y) in [(0, 0), (1, 0), (2, 0), (1, 1)] {
            off.board[y][x] = Some(T);
        }
        tracker.commit(off, 0.75, 3);
        let guide = unified(&library, &mut konbini, &tracker, false);
        assert_ne!(guide.origin, Origin::TwoLine, "{}", guide.status);
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
        // HOLD stays J: an emptied HOLD would read as a restarted game.
        let mut placed = observation(
            Some(Piece::J),
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

    #[test]
    fn perfect_clears_use_only_hold_current_and_next() {
        let library = konbini_library();
        let setup = library
            .setups
            .iter()
            .find(|setup| setup.sol_percent == Some(100.0))
            .unwrap();
        let mut board = empty_board();
        for cell in &setup.cells {
            board[usize::from(cell.cell.y)][usize::from(cell.cell.x)] = cell.piece;
        }
        let mut finder = Finder::new();
        let mut without = 0;
        // Each piece of the bag left out, the rest shown in a fixed order.
        for last in pc_overlay_core::finder::ALL_PIECES {
            let first: Vec<Piece> = pc_overlay_core::finder::ALL_PIECES
                .into_iter()
                .filter(|piece| *piece != last)
                .collect();
            let observation = Observation {
                board,
                hold: None,
                current: first[0],
                next5: first[1..].try_into().unwrap(),
            };
            let solves = konbini::find_perfect_clears(&mut finder, &observation, SOLVE_LIMIT);
            if solves.is_empty() {
                without += 1;
            }
            for solve in &solves {
                assert!(
                    solve.steps.iter().all(|(piece, _)| *piece != last),
                    "{first:?} solved with the unseen {last:?}"
                );
                assert!(solve.playable(&mut finder, &observation));
            }
        }
        // The 6 pieces shown miss some orders; those wait for the 7th.
        assert!(without > 0);
    }

    #[test]
    fn a_solve_the_shown_pieces_cannot_finish_is_dropped() {
        let mut mode = KonbiniGuide::new(PathBuf::from("unused"));
        mode.library = Some(konbini_library());
        let queue = [Piece::S, Piece::Z, Piece::I, Piece::T, Piece::L];
        let tracker = tilj_setup_built(Some(Piece::J), Piece::O, queue);
        let snapshot = tracker.snapshots().last().unwrap().clone();
        let guide = mode.guide(&tracker, &snapshot);
        assert!(guide.status.contains("perfect clear(s) found"));
        assert_eq!(guide.pc_rate, mode.built_rate);
        assert!(mode.built_rate.is_some());
        // The same board with a HOLD and NEXT that hold none of the solve's
        // pieces: no solve is left to show.
        let mut other = snapshot.clone();
        other.observation.hold = Some(Piece::O);
        other.observation.current = Piece::O;
        other.observation.next5 = [Piece::O; 5];
        let guide = mode.guide(&tracker, &other);
        assert!(mode.solve.is_none(), "{}", guide.status);
        assert!(
            !guide.status.contains("perfect clear 1/"),
            "{}",
            guide.status
        );
    }

    #[test]
    fn setups_name_their_section_of_the_source_document() {
        let library = Library::load(ijlo_only(), &root().join("data/cases"))
            .unwrap()
            .with_source_sections(&root().join("data/ijlo.json"));
        let setup = |id: &str| {
            library
                .cases
                .iter()
                .flat_map(|rules| &rules.methods)
                .flat_map(|method| &method.setups)
                .find(|setup| setup.id == id)
                .unwrap()
        };
        assert_eq!(
            library.source_label(setup("tils_image434_m")),
            " · Odd JL, with T › TIJZ/TILS (mirrored)"
        );
        assert_eq!(
            library.source_label(setup("ijlo_s_queue")),
            " · Even JL, no T › IJLO"
        );
    }
}
