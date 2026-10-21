//! Interactive mouse calibration for the 10x20 TETR.IO board.
//!
//! The calibration surface is separate from the click-through runtime
//! overlay. It accepts mouse input, draws a temporary rectangle over the
//! visible TETR.IO client, and returns client-relative coordinates only after
//! the user confirms with Enter.

use crate::{roi::PixelRect, window::WindowRect};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CalibrationError {
    #[error("interactive calibration is only implemented on Windows")]
    UnsupportedPlatform,
    #[cfg(windows)]
    #[error("Win32 calibration operation failed: {0}")]
    Win32(&'static str),
    #[error("calibration was cancelled")]
    Cancelled,
    #[error("no non-empty board rectangle was selected")]
    EmptySelection,
}

/// Let the user drag a rectangle over the visible TETR.IO board.
///
/// The returned rectangle is relative to the TETR.IO client area. The caller
/// is responsible for converting it to a `BoardCalibration` after checking the
/// captured client dimensions.
pub fn select_board(client: WindowRect) -> Result<PixelRect, CalibrationError> {
    #[cfg(windows)]
    {
        win32::select_board(client)
    }
    #[cfg(not(windows))]
    {
        let _ = client;
        Err(CalibrationError::UnsupportedPlatform)
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
            SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW, UpdateWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN},
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetMessageW,
            HTCLIENT, LWA_ALPHA, PostQuitMessage, RegisterClassW, SW_SHOW, SetForegroundWindow,
            SetLayeredWindowAttributes, ShowWindow, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
            WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT,
            WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    };

    const CLASS_NAME: &[u16] = &[
        'P' as u16, 'c' as u16, 'C' as u16, 'a' as u16, 'l' as u16, 'i' as u16, 'b' as u16,
        'r' as u16, 'a' as u16, 't' as u16, 'i' as u16, 'o' as u16, 'n' as u16, 'W' as u16,
        'i' as u16, 'n' as u16, 'd' as u16, 'o' as u16, 'w' as u16, 0,
    ];

    static STATE: OnceLock<Mutex<SelectionState>> = OnceLock::new();

    #[derive(Debug, Clone, Copy, Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[derive(Debug, Default, Clone)]
    struct SelectionState {
        start: Option<Point>,
        current: Point,
        dragging: bool,
        selection: Option<PixelRect>,
        result: Option<PixelRect>,
        cancelled: bool,
    }

    pub fn select_board(client: WindowRect) -> Result<PixelRect, CalibrationError> {
        if client.width == 0 || client.height == 0 {
            return Err(CalibrationError::EmptySelection);
        }
        let state = STATE.get_or_init(|| Mutex::new(SelectionState::default()));
        state
            .lock()
            .map_err(|_| CalibrationError::Win32("calibration state lock"))?
            .clone_from(&SelectionState::default());

        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            if instance.is_null() {
                return Err(CalibrationError::Win32("GetModuleHandleW"));
            }
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hbrBackground: GetStockObject(BLACK_BRUSH) as _,
                lpszClassName: CLASS_NAME.as_ptr(),
                ..WNDCLASSW::default()
            };
            if RegisterClassW(&class) == 0 {
                let error = GetLastError();
                if error != ERROR_CLASS_ALREADY_EXISTS {
                    return Err(CalibrationError::Win32("RegisterClassW"));
                }
            }
            let width = i32::try_from(client.width)
                .map_err(|_| CalibrationError::Win32("client width overflow"))?;
            let height = i32::try_from(client.height)
                .map_err(|_| CalibrationError::Win32("client height overflow"))?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                CLASS_NAME.as_ptr(),
                CLASS_NAME.as_ptr(),
                WS_POPUP,
                client.left,
                client.top,
                width,
                height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err(CalibrationError::Win32("CreateWindowExW"));
            }
            // A colour-keyed black background becomes mouse-transparent on a
            // layered window. Use a uniform semi-transparent dimming layer so
            // the entire TETR.IO client remains an interactive hit target.
            if SetLayeredWindowAttributes(hwnd, 0, 128, LWA_ALPHA) == 0 {
                DestroyWindow(hwnd);
                return Err(CalibrationError::Win32("SetLayeredWindowAttributes"));
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            SetForegroundWindow(hwnd);

            let mut message = windows_sys::Win32::UI::WindowsAndMessaging::MSG::default();
            loop {
                let status = GetMessageW(&mut message, std::ptr::null_mut(), 0, 0);
                if status == -1 {
                    DestroyWindow(hwnd);
                    return Err(CalibrationError::Win32("GetMessageW"));
                }
                if status == 0 {
                    break;
                }
                windows_sys::Win32::UI::WindowsAndMessaging::TranslateMessage(&message);
                windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(&message);
            }
            DestroyWindow(hwnd);
        }

        let state = STATE
            .get()
            .ok_or(CalibrationError::Win32("calibration state missing"))?
            .lock()
            .map_err(|_| CalibrationError::Win32("calibration state lock"))?;
        if state.cancelled {
            Err(CalibrationError::Cancelled)
        } else {
            state.result.ok_or(CalibrationError::EmptySelection)
        }
    }

    #[allow(clippy::collapsible_if)]
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
                    paint_window(hdc);
                    EndPaint(hwnd, &paint);
                    0
                }
                WM_ERASEBKGND => 1,
                WM_NCHITTEST => HTCLIENT as isize,
                WM_LBUTTONDOWN => {
                    let point = point_from_lparam(lparam);
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            state.start = Some(point);
                            state.current = point;
                            state.dragging = true;
                            state.selection = None;
                        }
                    }
                    SetCapture(hwnd);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                    0
                }
                WM_MOUSEMOVE => {
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            if state.dragging {
                                state.current = point_from_lparam(lparam);
                                state.selection = state
                                    .start
                                    .and_then(|start| normalize_selection(start, state.current));
                            }
                        }
                    }
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                    0
                }
                WM_LBUTTONUP => {
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            state.current = point_from_lparam(lparam);
                            state.dragging = false;
                            state.selection = state
                                .start
                                .and_then(|start| normalize_selection(start, state.current));
                        }
                    }
                    ReleaseCapture();
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                    0
                }
                WM_KEYDOWN if wparam == usize::from(VK_RETURN) => {
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            if let Some(selection) = state.selection {
                                state.result = Some(selection);
                                state.cancelled = false;
                                PostQuitMessage(0);
                            }
                        }
                    }
                    0
                }
                WM_KEYDOWN if wparam == usize::from(VK_ESCAPE) => {
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            state.cancelled = true;
                            state.result = None;
                            PostQuitMessage(0);
                        }
                    }
                    0
                }
                WM_CLOSE => {
                    if let Some(state) = STATE.get() {
                        if let Ok(mut state) = state.lock() {
                            state.cancelled = true;
                        }
                    }
                    PostQuitMessage(0);
                    0
                }
                WM_DESTROY => 0,
                _ => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
    }

    #[allow(clippy::collapsible_if)]
    unsafe fn paint_window(hdc: HGDIOBJ) {
        unsafe {
            let client = RECT {
                left: 0,
                top: 0,
                right: 100_000,
                bottom: 100_000,
            };
            let black = CreateSolidBrush(rgb(0, 0, 0));
            if !black.is_null() {
                FillRect(hdc, &client, black);
                DeleteObject(black as _);
            }

            let font = GetStockObject(DEFAULT_GUI_FONT);
            let old_font = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, rgb(245, 245, 245));
            draw_text(
                hdc,
                14,
                12,
                "Drag the 10x20 board, then press Enter to save (Esc cancels)",
            );
            SetTextColor(hdc, rgb(210, 210, 210));
            draw_text(
                hdc,
                14,
                31,
                "The rectangle is client-area relative; HOLD/NEXT follow it",
            );

            if let Some(state) = STATE.get().and_then(|state| state.lock().ok()) {
                if let Some(selection) = state.selection.or_else(|| {
                    state
                        .start
                        .and_then(|start| normalize_selection(start, state.current))
                }) {
                    let expected_height = selection.width.saturating_mul(2);
                    let colour = if selection.height.abs_diff(expected_height)
                        <= selection.width.max(8) / 4
                    {
                        rgb(80, 220, 150)
                    } else {
                        rgb(255, 190, 70)
                    };
                    let brush = CreateSolidBrush(colour);
                    if !brush.is_null() {
                        let rect = RECT {
                            left: selection.left as i32,
                            top: selection.top as i32,
                            right: selection.left.saturating_add(selection.width) as i32,
                            bottom: selection.top.saturating_add(selection.height) as i32,
                        };
                        FrameRect(hdc, &rect, brush);
                        DeleteObject(brush as _);
                    }
                    draw_text(
                        hdc,
                        14,
                        50,
                        &format!(
                            "selected x={} y={} w={} h={} (expected h={})",
                            selection.left,
                            selection.top,
                            selection.width,
                            selection.height,
                            expected_height
                        ),
                    );
                }
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

    fn point_from_lparam(lparam: LPARAM) -> Point {
        Point {
            x: (lparam as u32 & 0xffff) as i16 as i32,
            y: ((lparam as u32 >> 16) & 0xffff) as i16 as i32,
        }
    }

    fn normalize_selection(start: Point, current: Point) -> Option<PixelRect> {
        let left = start.x.min(current.x).max(0) as u32;
        let top = start.y.min(current.y).max(0) as u32;
        let right = start.x.max(current.x).max(0) as u32;
        let bottom = start.y.max(current.y).max(0) as u32;
        let width = right.saturating_sub(left);
        let height = bottom.saturating_sub(top);
        (width >= 10 && height >= 10).then_some(PixelRect {
            left,
            top,
            width,
            height,
        })
    }

    fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
        u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
    }
}
