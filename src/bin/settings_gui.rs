//! A small settings and status window for the overlay (`pc-overlay-gui`).
//!
//! It starts and stops `run-overlay`, runs the board calibration
//! (`configure-overlay`), downloads the Setup Konbini sheets, lists the
//! hotkeys, and shows what data is in place. It is plain Win32 through
//! `windows-sys`, which the overlay already uses, so it adds no dependency.
//! The overlay keeps its own status line; this window only knows whether the
//! process runs.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc,
    thread,
};

use pc_overlay_core::{
    BoardCalibration,
    konbini::{self, KonbiniLibrary},
};

/// The hotkeys of `run-overlay`, as listed in the README and docs/technical.md.
const HOTKEYS: &str = "\
Hotkeys while the overlay runs:
S  -  show the solve once the setup is complete
Ctrl+Alt+S  -  seed queue ON / OFF: also search with the pieces after NEXT, from the seed estimated from the pieces seen
Ctrl+Alt+N / B  -  next / previous setup (or solve, once found)
Ctrl+Alt+K  -  2nd PC: use Setup Konbini instead of the source rules
Ctrl+Alt+F  -  download the Setup Konbini sheets again
Ctrl+Alt+1..7  -  set the PC number (if it was miscounted)
R / Ctrl+Z / Ctrl+Y  -  TETR.IO's retry / undo / redo, read as hints (run-overlay --retry-key for another retry key)";

/// Paths the overlay reads, relative to the working directory.
struct DataPaths {
    calibration: PathBuf,
    rules: PathBuf,
    cases: PathBuf,
    konbini: PathBuf,
}

impl DataPaths {
    fn under(root: &Path) -> Self {
        Self {
            calibration: root.join("data/overlay-calibration.json"),
            rules: root.join("data/ijlo.json"),
            cases: root.join("data/cases"),
            konbini: root.join("data/konbini"),
        }
    }
}

/// What the window knows about a child process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessState {
    Stopped,
    Running(u32),
    Exited(Option<i32>),
}

fn describe(name: &str, state: &ProcessState) -> String {
    match state {
        ProcessState::Stopped => format!("{name}: not running"),
        ProcessState::Running(pid) => format!("{name}: running (pid {pid})"),
        ProcessState::Exited(Some(0)) => format!("{name}: finished"),
        ProcessState::Exited(Some(code)) => format!("{name}: exited with code {code}"),
        ProcessState::Exited(None) => format!("{name}: stopped"),
    }
}

/// The data lines of the status text.
fn data_status(paths: &DataPaths) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(match fs::read_to_string(&paths.calibration) {
        Ok(text) => match serde_json::from_str::<BoardCalibration>(&text) {
            Ok(calibration) => format!(
                "Calibration: centre x {:.4}, top {:.4}, width {:.4} (client-area ratios)",
                calibration.center_x_ratio, calibration.top_ratio, calibration.board_width_ratio
            ),
            Err(error) => format!("Calibration: unreadable ({error})"),
        },
        Err(_) => "Calibration: none; the default 1920x1009 profile is used".to_string(),
    });
    let cases = fs::read_dir(&paths.cases)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        })
        .unwrap_or(0);
    lines.push(format!(
        "Source rules: {} + {cases} generated cases",
        if paths.rules.exists() {
            "IJLO"
        } else {
            "IJLO missing"
        }
    ));
    let sheets = (1..=konbini::STAGE_SHEETS.len() as u8)
        .filter(|stage| {
            paths
                .konbini
                .join(konbini::cache_file_name(*stage))
                .exists()
        })
        .count();
    lines.push(format!(
        "Setup Konbini: {sheets} of {} sheets cached",
        konbini::STAGE_SHEETS.len()
    ));
    lines
}

/// The executable `name` next to this one (`target/release` after
/// `cargo build --release`).
fn sibling_exe(name: &str) -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    path.exists().then_some(path)
}

/// The window's state: child processes, the fetch, and the last message.
struct App {
    paths: DataPaths,
    overlay: Option<Child>,
    overlay_state: ProcessState,
    calibration: Option<Child>,
    calibration_state: ProcessState,
    fetching: Option<mpsc::Receiver<Result<usize, String>>>,
    message: String,
}

impl App {
    fn new(root: &Path) -> Self {
        Self {
            paths: DataPaths::under(root),
            overlay: None,
            overlay_state: ProcessState::Stopped,
            calibration: None,
            calibration_state: ProcessState::Stopped,
            fetching: None,
            message: "Ready.".to_string(),
        }
    }

    fn spawn(name: &str) -> Result<Child, String> {
        let exe = sibling_exe(name).ok_or_else(|| {
            format!("{name} not found next to this program; run `cargo build --release` first")
        })?;
        Command::new(exe)
            .spawn()
            .map_err(|error| format!("{name}: {error}"))
    }

    fn start_overlay(&mut self) {
        if self.overlay.is_some() {
            self.message = "The overlay is already running.".to_string();
            return;
        }
        match Self::spawn("run-overlay") {
            Ok(child) => {
                self.overlay_state = ProcessState::Running(child.id());
                self.overlay = Some(child);
                self.message = "Overlay started.".to_string();
            }
            Err(error) => self.message = error,
        }
    }

    fn stop_overlay(&mut self) {
        match self.overlay.take() {
            Some(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                self.overlay_state = ProcessState::Exited(None);
                self.message = "Overlay stopped.".to_string();
            }
            None => self.message = "The overlay is not running.".to_string(),
        }
    }

    fn calibrate(&mut self) {
        if self.calibration.is_some() {
            self.message = "Calibration is already open.".to_string();
            return;
        }
        match Self::spawn("configure-overlay") {
            Ok(child) => {
                self.calibration_state = ProcessState::Running(child.id());
                self.calibration = Some(child);
                self.message = "Drag over the TETR.IO board, then press Enter (Esc cancels). \
                    Restart the overlay afterwards."
                    .to_string();
            }
            Err(error) => self.message = error,
        }
    }

    fn fetch(&mut self) {
        if self.fetching.is_some() {
            return;
        }
        let dir = self.paths.konbini.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = konbini::fetch_all(&dir)
                .and_then(|()| KonbiniLibrary::load_dir(&dir))
                .map(|library| library.setups.len())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.fetching = Some(receiver);
        self.message = "Downloading Setup Konbini...".to_string();
    }

    /// Picks up finished children and fetches.
    fn poll(&mut self) {
        for (child, state) in [
            (&mut self.overlay, &mut self.overlay_state),
            (&mut self.calibration, &mut self.calibration_state),
        ] {
            if let Some(running) = child
                && let Ok(Some(status)) = running.try_wait()
            {
                *state = ProcessState::Exited(status.code());
                *child = None;
            }
        }
        if let Some(receiver) = &self.fetching
            && let Ok(result) = receiver.try_recv()
        {
            self.message = match result {
                Ok(count) => format!("Setup Konbini: {count} setups downloaded."),
                Err(error) => format!("Setup Konbini download failed: {error}"),
            };
            self.fetching = None;
        }
    }

    fn status_text(&self) -> String {
        let mut lines = vec![
            describe("Overlay", &self.overlay_state),
            describe("Calibration", &self.calibration_state),
        ];
        lines.extend(data_status(&self.paths));
        lines.push(String::new());
        lines.push(self.message.clone());
        lines.join("\r\n")
    }

    fn shutdown(&mut self) {
        if let Some(mut child) = self.overlay.take() {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
mod gui {
    use std::cell::RefCell;

    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{COLOR_BTNFACE, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            BS_PUSHBUTTON, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW,
            GetMessageW, IDC_ARROW, LoadCursorW, MSG, PostQuitMessage, RegisterClassW,
            SendMessageW, SetTimer, SetWindowTextW, TranslateMessage, WM_COMMAND, WM_DESTROY,
            WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED,
            WS_SYSMENU, WS_VISIBLE,
        },
    };

    use super::App;

    const START: usize = 1;
    const STOP: usize = 2;
    const CALIBRATE: usize = 3;
    const FETCH: usize = 4;
    const TIMER: usize = 1;

    thread_local! {
        static APP: RefCell<Option<App>> = const { RefCell::new(None) };
        static STATUS: RefCell<HWND> = const { RefCell::new(std::ptr::null_mut()) };
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn refresh() {
        let text = APP.with_borrow_mut(|app| {
            app.as_mut().map(|app| {
                app.poll();
                app.status_text()
            })
        });
        if let Some(text) = text {
            let status = STATUS.with_borrow(|status| *status);
            let text = wide(&text);
            // SAFETY: `status` is this thread's static control (or null,
            // which SetWindowTextW rejects), and `text` is NUL-terminated.
            unsafe { SetWindowTextW(status, text.as_ptr()) };
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_COMMAND => {
                APP.with_borrow_mut(|app| {
                    if let Some(app) = app.as_mut() {
                        match wparam & 0xffff {
                            START => app.start_overlay(),
                            STOP => app.stop_overlay(),
                            CALIBRATE => app.calibrate(),
                            FETCH => app.fetch(),
                            _ => {}
                        }
                    }
                });
                refresh();
                0
            }
            WM_TIMER => {
                refresh();
                0
            }
            WM_DESTROY => {
                APP.with_borrow_mut(|app| {
                    if let Some(app) = app.as_mut() {
                        app.shutdown();
                    }
                });
                // SAFETY: called on the window's thread.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: unhandled messages go to the default procedure with
            // the arguments Windows passed in.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    /// A child control of `parent` at `(x, y, width, height)`.
    ///
    /// # Safety
    /// `parent` must be a live window of this thread.
    unsafe fn control(
        parent: HWND,
        class: &str,
        text: &str,
        style: u32,
        id: usize,
        rect: (i32, i32, i32, i32),
    ) -> HWND {
        let class = wide(class);
        let text = wide(text);
        // SAFETY: the strings are NUL-terminated and outlive the call; the
        // menu argument of a child window is its control id.
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                parent,
                std::ptr::without_provenance_mut(id),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            SendMessageW(
                hwnd,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
            hwnd
        }
    }

    pub fn run(app: App) -> Result<(), String> {
        let status_text = app.status_text();
        APP.with_borrow_mut(|slot| *slot = Some(app));
        let class_name = wide("PcOverlaySettings");
        let title = wide("PC Overlay settings");
        // SAFETY: plain Win32 window setup on this thread; every string is
        // NUL-terminated and outlives the call that reads it.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
                lpszClassName: class_name.as_ptr(),
                ..WNDCLASSW::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err("RegisterClassW failed".to_string());
            }
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                620,
                400,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err("CreateWindowExW failed".to_string());
            }
            let button = BS_PUSHBUTTON as u32;
            let buttons = [
                (START, "Start overlay"),
                (STOP, "Stop overlay"),
                (CALIBRATE, "Calibrate board"),
                (FETCH, "Download Setup Konbini"),
            ];
            for (index, (id, label)) in buttons.iter().enumerate() {
                control(
                    hwnd,
                    "BUTTON",
                    label,
                    button,
                    *id,
                    (12 + index as i32 * 148, 12, 140, 28),
                );
            }
            let status = control(hwnd, "STATIC", &status_text, 0, 0, (12, 52, 580, 130));
            STATUS.with_borrow_mut(|slot| *slot = status);
            control(hwnd, "STATIC", super::HOTKEYS, 0, 0, (12, 190, 580, 160));
            SetTimer(hwnd, TIMER, 1000, None);

            let mut message = MSG::default();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), String> {
    let root = std::env::current_dir().map_err(|error| error.to_string())?;
    gui::run(App::new(&root))
}

#[cfg(not(windows))]
fn main() {
    eprintln!("pc-overlay-gui runs on Windows only");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn data_status_reads_the_repository_data() {
        let lines = data_status(&DataPaths::under(&root()));
        assert!(lines[1].starts_with("Source rules: IJLO + "), "{lines:?}");
        assert!(lines[1].ends_with(" generated cases"));
        assert!(lines[2].starts_with("Setup Konbini: "), "{lines:?}");
    }

    #[test]
    fn data_status_explains_missing_data() {
        let lines = data_status(&DataPaths::under(Path::new("no such folder")));
        assert_eq!(
            lines,
            vec![
                "Calibration: none; the default 1920x1009 profile is used",
                "Source rules: IJLO missing + 0 generated cases",
                "Setup Konbini: 0 of 7 sheets cached",
            ]
        );
    }

    #[test]
    fn status_text_names_the_processes_and_the_last_message() {
        let mut app = App::new(Path::new("no such folder"));
        app.overlay_state = ProcessState::Running(42);
        app.calibration_state = ProcessState::Exited(Some(1));
        let text = app.status_text();
        assert!(
            text.starts_with("Overlay: running (pid 42)\r\nCalibration: exited with code 1\r\n")
        );
        assert!(text.ends_with("\r\n\r\nReady."));
    }

    #[test]
    fn hotkey_list_matches_the_overlay() {
        for key in [
            "Ctrl+Alt+S",
            "Ctrl+Alt+N / B",
            "Ctrl+Alt+K",
            "Ctrl+Alt+F",
            "Ctrl+Alt+1..7",
        ] {
            assert!(HOTKEYS.contains(key), "{key}");
        }
    }
}
