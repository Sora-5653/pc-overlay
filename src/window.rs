//! Minimal Windows window discovery boundary.
//!
//! The overlay must identify a visible TETR.IO client rectangle, but it must
//! not depend on TETR.IO internals. The platform-specific enumeration lives in
//! this module; ROI and recognition remain pure and testable elsewhere.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowRect {
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Platform handle converted to an opaque integer. It is only valid for the
    /// current desktop session and is never persisted.
    pub handle: u64,
    pub process_id: u32,
    pub title: String,
    pub client_rect: WindowRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowLocator {
    pub minimum_client_width: u32,
    pub minimum_client_height: u32,
}

impl Default for WindowLocator {
    fn default() -> Self {
        Self {
            minimum_client_width: 640,
            minimum_client_height: 360,
        }
    }
}

#[derive(Debug, Error)]
pub enum WindowLocatorError {
    #[error("TETR.IO window discovery is only implemented on Windows")]
    UnsupportedPlatform,
    #[cfg(windows)]
    #[error("Win32 window enumeration failed")]
    EnumerationFailed,
}

impl WindowLocator {
    pub fn locate(&self) -> Result<Option<WindowInfo>, WindowLocatorError> {
        #[cfg(windows)]
        {
            let mut candidates = enumerate_visible_windows()?;
            candidates.retain(|candidate| {
                candidate.client_rect.width >= self.minimum_client_width
                    && candidate.client_rect.height >= self.minimum_client_height
                    && is_tetrio_title(&candidate.title)
            });
            candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.client_rect.area()));
            Ok(candidates.into_iter().next())
        }
        #[cfg(not(windows))]
        {
            let _ = self;
            Err(WindowLocatorError::UnsupportedPlatform)
        }
    }
}

pub fn is_tetrio_title(title: &str) -> bool {
    title.to_ascii_lowercase().contains("tetr.io")
}

/// Whether a process image is TETR.IO Desktop (`TETR.IO.exe`). The title
/// alone also matches a browser showing tetr.io or any page titled with it,
/// and the largest such window would win over the game.
pub fn is_tetrio_desktop_image(path: &str) -> bool {
    path.rsplit(['\\', '/'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("TETR.IO.exe"))
}

#[cfg(windows)]
fn enumerate_visible_windows() -> Result<Vec<WindowInfo>, WindowLocatorError> {
    use windows_sys::{
        Win32::{
            Foundation::{HWND, LPARAM, POINT, RECT},
            Graphics::Gdi::ClientToScreen,
            UI::WindowsAndMessaging::{
                EnumWindows, GetClientRect, GetWindowTextLengthW, GetWindowTextW,
                GetWindowThreadProcessId, IsWindowVisible,
            },
        },
        core::BOOL,
    };

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let length = GetWindowTextLengthW(hwnd);
            if length <= 0 {
                return 1;
            }
            let mut buffer = vec![0u16; length as usize + 1];
            let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            if copied <= 0 {
                return 1;
            }
            let title = String::from_utf16_lossy(&buffer[..copied as usize]);
            if !is_tetrio_title(&title) {
                return 1;
            }

            let mut client = RECT::default();
            if GetClientRect(hwnd, &mut client) == 0 {
                return 1;
            }
            let mut origin = POINT {
                x: client.left,
                y: client.top,
            };
            if ClientToScreen(hwnd, &mut origin) == 0 {
                return 1;
            }
            let width = client.right.saturating_sub(client.left);
            let height = client.bottom.saturating_sub(client.top);
            if width <= 0 || height <= 0 {
                return 1;
            }

            let mut process_id = 0u32;
            GetWindowThreadProcessId(hwnd, &mut process_id);
            if !process_image(process_id).is_some_and(|path| is_tetrio_desktop_image(&path)) {
                return 1;
            }
            let candidates = &mut *(lparam as *mut Vec<WindowInfo>);
            candidates.push(WindowInfo {
                handle: hwnd as usize as u64,
                process_id,
                title,
                client_rect: WindowRect {
                    left: origin.x,
                    top: origin.y,
                    width: width as u32,
                    height: height as u32,
                },
            });
            1
        }
    }

    /// The full path of a process's executable, readable without elevation.
    fn process_image(process_id: u32) -> Option<String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        };
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
            if process.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let mut length = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut length,
            );
            CloseHandle(process);
            (ok != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
        }
    }

    let mut candidates = Vec::new();
    let result = unsafe {
        EnumWindows(
            Some(callback),
            (&mut candidates as *mut Vec<WindowInfo>) as LPARAM,
        )
    };
    if result == 0 {
        Err(WindowLocatorError::EnumerationFailed)
    } else {
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_filter_is_case_insensitive_and_does_not_match_other_games() {
        assert!(is_tetrio_title("TETR.IO"));
        assert!(is_tetrio_title("TETR.IO Desktop - ZEN"));
        assert!(!is_tetrio_title("Tetris Effect"));
    }

    #[test]
    fn only_the_desktop_client_process_counts_not_a_browser() {
        assert!(is_tetrio_desktop_image(
            r"C:\Users\me\AppData\Local\Programs\tetrio-desktop\TETR.IO.exe"
        ));
        assert!(is_tetrio_desktop_image("tetr.io.EXE"));
        assert!(!is_tetrio_desktop_image(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        ));
        assert!(!is_tetrio_desktop_image(r"C:\tools\TETR.IO.exe.bak"));
    }

    #[test]
    fn area_is_width_times_height() {
        assert_eq!(
            WindowRect {
                left: 0,
                top: 0,
                width: 100,
                height: 50
            }
            .area(),
            5000
        );
    }
}
