//! Transparent, click-through Win32 overlay surface.
//!
//! The overlay is deliberately a rendering boundary.  Recognition and rule
//! selection remain in the pure core; this module only follows the discovered
//! TETR.IO client rectangle and draws a status line plus any cells the caller
//! explicitly authorizes.  In particular, an unknown frame can show a warning
//! but cannot accidentally reuse a stale setup plan.

use crate::{
    model::{Cell, Piece},
    roi::{FrameSize, PixelRect},
    window::WindowRect,
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayCellKind {
    Missing,
    Current,
    Warning,
    Unexpected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayCell {
    pub cell: Cell,
    pub piece: Option<Piece>,
    pub kind: OverlayCellKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayBoard {
    pub frame: FrameSize,
    pub rect: PixelRect,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayState {
    pub status: String,
    pub detail: String,
    pub board: Option<OverlayBoard>,
    pub cells: Vec<OverlayCell>,
}

#[derive(Debug, Error)]
pub enum OverlayError {
    #[error("transparent overlay is only implemented on Windows")]
    UnsupportedPlatform,
    #[cfg(windows)]
    #[error("Win32 overlay operation failed: {0}")]
    Win32(&'static str),
}

#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct OverlayWindow;

#[cfg(not(windows))]
impl OverlayWindow {
    pub fn new() -> Result<Self, OverlayError> {
        Err(OverlayError::UnsupportedPlatform)
    }

    pub fn update(
        &mut self,
        _client: Option<WindowRect>,
        _state: OverlayState,
    ) -> Result<(), OverlayError> {
        Err(OverlayError::UnsupportedPlatform)
    }

    pub fn pump(&mut self) -> bool {
        false
    }
}

#[cfg(windows)]
mod win32 {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use windows_sys::Win32::{
        Foundation::{
            COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, RECT, WPARAM,
        },
        Graphics::Gdi::{
            BLACK_BRUSH, BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, EndPaint,
            FillRect, FrameRect, GetStockObject, HGDIOBJ, InvalidateRect, PAINTSTRUCT,
            SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
            HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, LWA_COLORKEY, MA_NOACTIVATE, PM_REMOVE,
            PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
            SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, WM_DESTROY,
            WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WM_QUIT, WNDCLASSW,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
            WS_POPUP,
        },
    };

    static STATE: OnceLock<Mutex<OverlayState>> = OnceLock::new();
    const CLASS_NAME: &[u16] = &[
        'P' as u16, 'c' as u16, 'O' as u16, 'v' as u16, 'e' as u16, 'r' as u16, 'l' as u16,
        'a' as u16, 'y' as u16, 'W' as u16, 'i' as u16, 'n' as u16, 'd' as u16, 'o' as u16,
        'w' as u16, 0,
    ];

    pub struct OverlayWindow {
        hwnd: HWND,
        closed: bool,
    }

    impl OverlayWindow {
        pub fn new() -> Result<Self, OverlayError> {
            STATE.get_or_init(|| Mutex::new(OverlayState::default()));
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                if instance.is_null() {
                    return Err(OverlayError::Win32("GetModuleHandleW"));
                }
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    hbrBackground: GetStockObject(BLACK_BRUSH) as _,
                    lpszClassName: CLASS_NAME.as_ptr(),
                    ..WNDCLASSW::default()
                };
                if RegisterClassW(&class) == 0 {
                    let error = GetLastError();
                    if error != ERROR_CLASS_ALREADY_EXISTS {
                        return Err(OverlayError::Win32("RegisterClassW"));
                    }
                }
                let hwnd = CreateWindowExW(
                    WS_EX_LAYERED
                        | WS_EX_TRANSPARENT
                        | WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE,
                    CLASS_NAME.as_ptr(),
                    CLASS_NAME.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    return Err(OverlayError::Win32("CreateWindowExW"));
                }
                if SetLayeredWindowAttributes(hwnd, 0, 210, LWA_COLORKEY | LWA_ALPHA) == 0 {
                    DestroyWindow(hwnd);
                    return Err(OverlayError::Win32("SetLayeredWindowAttributes"));
                }
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                Ok(Self {
                    hwnd,
                    closed: false,
                })
            }
        }

        pub fn update(
            &mut self,
            client: Option<WindowRect>,
            state: OverlayState,
        ) -> Result<(), OverlayError> {
            if self.closed {
                return Ok(());
            }
            if let Some(mutex) = STATE.get() {
                let mut current = mutex
                    .lock()
                    .map_err(|_| OverlayError::Win32("overlay state lock"))?;
                *current = state;
            }
            unsafe {
                if let Some(client) = client {
                    let width = i32::try_from(client.width)
                        .map_err(|_| OverlayError::Win32("client width overflow"))?;
                    let height = i32::try_from(client.height)
                        .map_err(|_| OverlayError::Win32("client height overflow"))?;
                    if width <= 0 || height <= 0 {
                        ShowWindow(self.hwnd, SW_HIDE);
                    } else {
                        if SetWindowPos(
                            self.hwnd,
                            HWND_TOPMOST,
                            client.left,
                            client.top,
                            width,
                            height,
                            SWP_NOACTIVATE,
                        ) == 0
                        {
                            return Err(OverlayError::Win32("SetWindowPos"));
                        }
                        ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    }
                } else {
                    ShowWindow(self.hwnd, SW_HIDE);
                }
                InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
            Ok(())
        }

        pub fn pump(&mut self) -> bool {
            if self.closed {
                return false;
            }
            unsafe {
                let mut message = windows_sys::Win32::UI::WindowsAndMessaging::MSG::default();
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if message.message == WM_QUIT {
                        self.closed = true;
                        return false;
                    }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            true
        }
    }

    impl Drop for OverlayWindow {
        fn drop(&mut self) {
            if !self.closed {
                unsafe {
                    DestroyWindow(self.hwnd);
                }
                self.closed = true;
            }
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        unsafe {
            match message {
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut paint);
                    paint_window(hdc, hwnd);
                    EndPaint(hwnd, &paint);
                    0
                }
                WM_ERASEBKGND => 1,
                WM_NCHITTEST => HTTRANSPARENT as isize,
                WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
                WM_DESTROY => 0,
                _ => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
    }

    unsafe fn paint_window(hdc: HGDIOBJ, hwnd: HWND) {
        unsafe {
            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client);
            let black = CreateSolidBrush(rgb(0, 0, 0));
            if !black.is_null() {
                FillRect(hdc, &client, black);
                DeleteObject(black as _);
            }

            let Some(mutex) = STATE.get() else {
                return;
            };
            let Ok(state) = mutex.lock() else {
                return;
            };
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let old_font = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, rgb(240, 240, 240));
            draw_text(hdc, 12, 10, &state.status);
            SetTextColor(hdc, rgb(190, 190, 190));
            draw_text(hdc, 12, 28, &state.detail);

            if let Some(board) = state.board {
                draw_board_frame(hdc, board.rect);
                draw_cells(hdc, board, &state.cells);
            }
            SelectObject(hdc, old_font);
        }
    }

    unsafe fn draw_text(hdc: HGDIOBJ, x: i32, y: i32, text: &str) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        unsafe {
            TextOutW(hdc, x, y, wide.as_mut_ptr(), wide.len() as i32);
        }
    }

    unsafe fn draw_cells(hdc: HGDIOBJ, board: OverlayBoard, cells: &[OverlayCell]) {
        unsafe {
            let cell_width = board.rect.width as f32 / 10.0;
            let cell_height = board.rect.height as f32 / 20.0;
            for overlay_cell in cells {
                if !overlay_cell.cell.in_bounds() {
                    continue;
                }
                let image_row = 19 - usize::from(overlay_cell.cell.y);
                let left = board.rect.left as f32 + f32::from(overlay_cell.cell.x) * cell_width;
                let top = board.rect.top as f32 + image_row as f32 * cell_height;
                let right = left + cell_width;
                let bottom = top + cell_height;
                let outer = RECT {
                    left: left.round() as i32,
                    top: top.round() as i32,
                    right: right.round() as i32,
                    bottom: bottom.round() as i32,
                };
                let colour = match overlay_cell.kind {
                    OverlayCellKind::Warning | OverlayCellKind::Unexpected => rgb(255, 80, 80),
                    OverlayCellKind::Missing | OverlayCellKind::Current => overlay_cell
                        .piece
                        .map(piece_colour)
                        .unwrap_or(rgb(100, 210, 255)),
                };
                let brush = CreateSolidBrush(colour);
                if brush.is_null() {
                    continue;
                }
                match overlay_cell.kind {
                    OverlayCellKind::Current => {
                        FillRect(hdc, &outer, brush);
                    }
                    OverlayCellKind::Missing
                    | OverlayCellKind::Warning
                    | OverlayCellKind::Unexpected => {
                        FrameRect(hdc, &outer, brush);
                    }
                }
                DeleteObject(brush as _);
            }
        }
    }

    unsafe fn draw_board_frame(hdc: HGDIOBJ, board: PixelRect) {
        unsafe {
            let rect = RECT {
                left: board.left as i32,
                top: board.top as i32,
                right: board.left.saturating_add(board.width) as i32,
                bottom: board.top.saturating_add(board.height) as i32,
            };
            let brush = CreateSolidBrush(rgb(90, 190, 255));
            if !brush.is_null() {
                FrameRect(hdc, &rect, brush);
                DeleteObject(brush as _);
            }
        }
    }

    fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
        u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
    }

    fn piece_colour(piece: Piece) -> COLORREF {
        match piece {
            Piece::I => rgb(50, 210, 170),
            Piece::J => rgb(100, 120, 240),
            Piece::L => rgb(240, 150, 80),
            Piece::O => rgb(240, 210, 70),
            Piece::S => rgb(150, 220, 80),
            Piece::T => rgb(150, 100, 250),
            Piece::Z => rgb(240, 90, 100),
        }
    }
}

#[cfg(windows)]
pub use win32::OverlayWindow;
