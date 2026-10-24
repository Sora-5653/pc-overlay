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
    /// Drawn large just above the board, such as the setup's PC rate.
    pub badge: String,
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

/// A 32-bit top-down image in the premultiplied `0xAARRGGBB` layout that
/// `UpdateLayeredWindow` blends per pixel. Kept free of Win32 so the cell
/// drawing is testable on any platform.
struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

/// Setup cells are a light tint with an opaque outline around each piece, so
/// the falling piece and the board stay visible through them.
const MISSING_FILL_ALPHA: u8 = 70;
/// The cells of the piece to place now are tinted more strongly.
const CURRENT_FILL_ALPHA: u8 = 125;
const WARNING_FILL_ALPHA: u8 = 60;
const OUTLINE_ALPHA: u8 = 235;
const BOARD_FRAME_ALPHA: u8 = 200;

impl Canvas<'_> {
    /// Draws `colour` at `alpha` over the half-open rectangle, clipped.
    fn blend_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, colour: Rgb, alpha: u8) {
        let (left, right) = (left.max(0), right.min(self.width));
        let (top, bottom) = (top.max(0), bottom.min(self.height));
        for y in top..bottom {
            let row = (y * self.width) as usize;
            for pixel in &mut self.pixels[row + left as usize..row + right.max(left) as usize] {
                *pixel = over(*pixel, colour, alpha);
            }
        }
    }

    fn outline_rect(&mut self, rect: [i32; 4], thickness: i32, colour: Rgb, alpha: u8) {
        let [left, top, right, bottom] = rect;
        self.blend_rect(left, top, right, top + thickness, colour, alpha);
        self.blend_rect(left, bottom - thickness, right, bottom, colour, alpha);
        self.blend_rect(
            left,
            top + thickness,
            left + thickness,
            bottom - thickness,
            colour,
            alpha,
        );
        self.blend_rect(
            right - thickness,
            top + thickness,
            right,
            bottom - thickness,
            colour,
            alpha,
        );
    }

    /// Opaque alpha for pixels GDI drew text into. GDI leaves alpha at 0, which
    /// a per-pixel-alpha window would show as invisible.
    fn make_drawn_pixels_opaque(&mut self) {
        for pixel in self.pixels.iter_mut() {
            if *pixel >> 24 == 0 && *pixel & 0x00FF_FFFF != 0 {
                *pixel |= 0xFF00_0000;
            }
        }
    }

    fn draw_board(&mut self, board: OverlayBoard, cells: &[OverlayCell]) {
        let rect = board.rect;
        self.outline_rect(
            [
                rect.left as i32,
                rect.top as i32,
                rect.left.saturating_add(rect.width) as i32,
                rect.top.saturating_add(rect.height) as i32,
            ],
            1,
            Rgb(90, 190, 255),
            BOARD_FRAME_ALPHA,
        );

        // Neighbouring cells of the same piece and kind share one outline, so
        // a setup reads as pieces rather than a grid. Two touching pieces of
        // the same type merge, as the cells carry no placement identity.
        let mut groups = [[None; 10]; 20];
        for overlay_cell in cells.iter().filter(|cell| cell.cell.in_bounds()) {
            groups[usize::from(overlay_cell.cell.y)][usize::from(overlay_cell.cell.x)] =
                Some((overlay_cell.piece, overlay_cell.kind));
        }
        let same_group = |x: i32, y: i32, group| {
            (0..10).contains(&x)
                && (0..20).contains(&y)
                && groups[y as usize][x as usize] == Some(group)
        };

        let cell_width = rect.width as f32 / 10.0;
        let cell_height = rect.height as f32 / 20.0;
        let thickness = if cell_width.min(cell_height) >= 16.0 {
            2
        } else {
            1
        };
        for overlay_cell in cells.iter().filter(|cell| cell.cell.in_bounds()) {
            let (x, y) = (
                i32::from(overlay_cell.cell.x),
                i32::from(overlay_cell.cell.y),
            );
            let image_row = 19 - y;
            let left = (rect.left as f32 + x as f32 * cell_width).round() as i32;
            let right = (rect.left as f32 + (x + 1) as f32 * cell_width).round() as i32;
            let top = (rect.top as f32 + image_row as f32 * cell_height).round() as i32;
            let bottom = (rect.top as f32 + (image_row + 1) as f32 * cell_height).round() as i32;
            let (colour, fill_alpha) = match overlay_cell.kind {
                OverlayCellKind::Warning | OverlayCellKind::Unexpected => {
                    (Rgb(255, 80, 80), WARNING_FILL_ALPHA)
                }
                OverlayCellKind::Missing => (cell_colour(overlay_cell.piece), MISSING_FILL_ALPHA),
                OverlayCellKind::Current => (cell_colour(overlay_cell.piece), CURRENT_FILL_ALPHA),
            };
            self.blend_rect(left, top, right, bottom, colour, fill_alpha);

            let group = (overlay_cell.piece, overlay_cell.kind);
            // Board y grows upwards, image rows downwards.
            if !same_group(x, y + 1, group) {
                self.blend_rect(left, top, right, top + thickness, colour, OUTLINE_ALPHA);
            }
            if !same_group(x, y - 1, group) {
                self.blend_rect(
                    left,
                    bottom - thickness,
                    right,
                    bottom,
                    colour,
                    OUTLINE_ALPHA,
                );
            }
            if !same_group(x - 1, y, group) {
                self.blend_rect(left, top, left + thickness, bottom, colour, OUTLINE_ALPHA);
            }
            if !same_group(x + 1, y, group) {
                self.blend_rect(right - thickness, top, right, bottom, colour, OUTLINE_ALPHA);
            }
        }
    }
}

/// Porter-Duff "over" of a straight colour onto a premultiplied pixel.
fn over(destination: u32, colour: Rgb, alpha: u8) -> u32 {
    let alpha = u32::from(alpha);
    let inverse = 255 - alpha;
    let channel = |source: u8, shift: u32| {
        let below = (destination >> shift) & 0xFF;
        ((u32::from(source) * alpha + below * inverse + 127) / 255) << shift
    };
    let below_alpha = destination >> 24;
    let out_alpha = alpha + (below_alpha * inverse + 127) / 255;
    (out_alpha << 24) | channel(colour.0, 16) | channel(colour.1, 8) | channel(colour.2, 0)
}

fn cell_colour(piece: Option<Piece>) -> Rgb {
    match piece {
        Some(Piece::I) => Rgb(50, 210, 170),
        Some(Piece::J) => Rgb(100, 120, 240),
        Some(Piece::L) => Rgb(240, 150, 80),
        Some(Piece::O) => Rgb(240, 210, 70),
        Some(Piece::S) => Rgb(150, 220, 80),
        Some(Piece::T) => Rgb(150, 100, 250),
        Some(Piece::Z) => Rgb(240, 90, 100),
        None => Rgb(100, 210, 255),
    }
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
    use super::*;
    use windows_sys::Win32::{
        Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_BRUSH,
            BLENDFUNCTION, BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW,
            DEFAULT_CHARSET, DEFAULT_GUI_FONT, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
            FW_BOLD, GdiFlush, GetDC, GetStockObject, HDC, NONANTIALIASED_QUALITY, PAINTSTRUCT,
            ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HTTRANSPARENT,
            HWND_TOPMOST, MA_NOACTIVATE, PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowPos, ShowWindow, TranslateMessage,
            ULW_ALPHA, UpdateLayeredWindow, WM_DESTROY, WM_ERASEBKGND, WM_MOUSEACTIVATE,
            WM_NCHITTEST, WM_PAINT, WM_QUIT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    };

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
                // Per-pixel alpha comes from UpdateLayeredWindow in `update`,
                // so the window is shown only once it has content.
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
            let Some(client) = client else {
                unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                return Ok(());
            };
            let width = i32::try_from(client.width)
                .map_err(|_| OverlayError::Win32("client width overflow"))?;
            let height = i32::try_from(client.height)
                .map_err(|_| OverlayError::Win32("client height overflow"))?;
            if width <= 0 || height <= 0 {
                unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                return Ok(());
            }
            unsafe {
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
                self.render(client.left, client.top, width, height, &state)?;
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            Ok(())
        }

        /// Draws the state into a fresh premultiplied DIB and hands it to the
        /// layered window. Transparent pixels show the game unchanged.
        unsafe fn render(
            &self,
            left: i32,
            top: i32,
            width: i32,
            height: i32,
            state: &OverlayState,
        ) -> Result<(), OverlayError> {
            unsafe {
                let screen = GetDC(std::ptr::null_mut());
                if screen.is_null() {
                    return Err(OverlayError::Win32("GetDC"));
                }
                let memory = CreateCompatibleDC(screen);
                if memory.is_null() {
                    ReleaseDC(std::ptr::null_mut(), screen);
                    return Err(OverlayError::Win32("CreateCompatibleDC"));
                }
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width,
                        // Negative: rows run top-down, as the canvas expects.
                        biHeight: -height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        ..BITMAPINFOHEADER::default()
                    },
                    ..BITMAPINFO::default()
                };
                let mut bits = std::ptr::null_mut();
                let bitmap = CreateDIBSection(
                    memory,
                    &info,
                    DIB_RGB_COLORS,
                    &mut bits,
                    std::ptr::null_mut(),
                    0,
                );
                let result = if bitmap.is_null() || bits.is_null() {
                    Err(OverlayError::Win32("CreateDIBSection"))
                } else {
                    let old_bitmap = SelectObject(memory, bitmap);
                    let pixels = std::slice::from_raw_parts_mut(
                        bits.cast::<u32>(),
                        width as usize * height as usize,
                    );
                    pixels.fill(0);
                    draw_status(memory, state);
                    GdiFlush();
                    let mut canvas = Canvas {
                        pixels,
                        width,
                        height,
                    };
                    canvas.make_drawn_pixels_opaque();
                    if let Some(board) = state.board {
                        canvas.draw_board(board, &state.cells);
                    }
                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: 255,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                    };
                    let position = POINT { x: left, y: top };
                    let size = SIZE {
                        cx: width,
                        cy: height,
                    };
                    let source = POINT { x: 0, y: 0 };
                    let updated = UpdateLayeredWindow(
                        self.hwnd, screen, &position, &size, memory, &source, 0, &blend, ULW_ALPHA,
                    );
                    SelectObject(memory, old_bitmap);
                    DeleteObject(bitmap);
                    if updated == 0 {
                        Err(OverlayError::Win32("UpdateLayeredWindow"))
                    } else {
                        Ok(())
                    }
                };
                DeleteDC(memory);
                ReleaseDC(std::ptr::null_mut(), screen);
                result
            }
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
                // Content comes from UpdateLayeredWindow; only validate.
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut paint);
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

    unsafe fn draw_status(hdc: HDC, state: &OverlayState) {
        unsafe {
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let old_font = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, colorref(Rgb(240, 240, 240)));
            draw_text(hdc, 12, 10, &state.status);
            SetTextColor(hdc, colorref(Rgb(190, 190, 190)));
            draw_text(hdc, 12, 28, &state.detail);
            SelectObject(hdc, old_font);
            if let Some(board) = state.board {
                draw_badge(hdc, board, &state.badge);
            }
        }
    }

    /// The badge in a bold font about one cell high, at the board's top
    /// left, just above it. Unsmoothed, as every drawn pixel turns opaque.
    unsafe fn draw_badge(hdc: HDC, board: OverlayBoard, text: &str) {
        if text.is_empty() {
            return;
        }
        let rect = board.rect;
        let size = (rect.height as i32 / 20).clamp(16, 40);
        // Below the status and detail lines.
        let y = (rect.top as i32 - size - 4).max(46);
        let x = rect.left as i32;
        unsafe {
            let face: Vec<u16> = "Segoe UI ".encode_utf16().collect();
            let font = CreateFontW(
                -size,
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                0,
                0,
                u32::from(NONANTIALIASED_QUALITY),
                0,
                face.as_ptr(),
            );
            if font.is_null() {
                return;
            }
            let old_font = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);
            // A dark shadow keeps it readable on a light background.
            SetTextColor(hdc, colorref(Rgb(16, 16, 16)));
            draw_text(hdc, x + 2, y + 2, text);
            SetTextColor(hdc, colorref(Rgb(255, 220, 90)));
            draw_text(hdc, x, y, text);
            SelectObject(hdc, old_font);
            DeleteObject(font);
        }
    }

    unsafe fn draw_text(hdc: HDC, x: i32, y: i32, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        unsafe {
            TextOutW(hdc, x, y, wide.as_ptr(), wide.len() as i32);
        }
    }

    fn colorref(colour: Rgb) -> u32 {
        u32::from(colour.0) | (u32::from(colour.1) << 8) | (u32::from(colour.2) << 16)
    }
}

#[cfg(windows)]
pub use win32::OverlayWindow;

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10x20 board of 20 px cells at the canvas origin.
    fn board() -> OverlayBoard {
        OverlayBoard {
            frame: FrameSize {
                width: 200,
                height: 400,
            },
            rect: PixelRect {
                left: 0,
                top: 0,
                width: 200,
                height: 400,
            },
        }
    }

    fn missing(x: u8, y: u8, piece: Piece) -> OverlayCell {
        OverlayCell {
            cell: Cell { x, y },
            piece: Some(piece),
            kind: OverlayCellKind::Missing,
        }
    }

    fn draw(cells: &[OverlayCell]) -> Vec<u32> {
        let mut pixels = vec![0; 200 * 400];
        Canvas {
            pixels: &mut pixels,
            width: 200,
            height: 400,
        }
        .draw_board(board(), cells);
        pixels
    }

    fn alpha(pixels: &[u32], x: usize, y: usize) -> u32 {
        pixels[y * 200 + x] >> 24
    }

    #[test]
    fn setup_cells_are_a_light_tint_outlined_around_the_piece_only() {
        // Two horizontally adjacent O cells on the bottom row (image y 380..400).
        let pixels = draw(&[missing(3, 0, Piece::O), missing(4, 0, Piece::O)]);
        assert_eq!(alpha(&pixels, 70, 390), u32::from(MISSING_FILL_ALPHA));
        assert!(
            alpha(&pixels, 60, 390) >= u32::from(OUTLINE_ALPHA),
            "outer left edge"
        );
        assert!(
            alpha(&pixels, 70, 380) >= u32::from(OUTLINE_ALPHA),
            "outer top edge"
        );
        // The shared edge at x = 80 is not outlined, so no grid line splits the piece.
        assert_eq!(alpha(&pixels, 79, 390), u32::from(MISSING_FILL_ALPHA));
        assert_eq!(alpha(&pixels, 80, 390), u32::from(MISSING_FILL_ALPHA));
        // Outside the cells and the board frame stays fully transparent.
        assert_eq!(alpha(&pixels, 120, 390), 0);
    }

    #[test]
    fn different_pieces_keep_their_own_outlines_and_current_is_stronger() {
        let mut current = missing(4, 0, Piece::T);
        current.kind = OverlayCellKind::Current;
        let pixels = draw(&[missing(3, 0, Piece::O), current]);
        assert!(alpha(&pixels, 79, 390) >= u32::from(OUTLINE_ALPHA));
        assert!(alpha(&pixels, 80, 390) >= u32::from(OUTLINE_ALPHA));
        assert_eq!(alpha(&pixels, 90, 390), u32::from(CURRENT_FILL_ALPHA));
    }

    #[test]
    fn over_keeps_premultiplied_channels_within_alpha() {
        let once = over(0, Rgb(240, 210, 70), 70);
        let twice = over(once, Rgb(240, 210, 70), 235);
        for pixel in [once, twice] {
            let alpha = pixel >> 24;
            assert!((0..3).all(|channel| (pixel >> (channel * 8)) & 0xFF <= alpha));
        }
    }
}
