//! Windows client-area capture boundary.
//!
//! This uses a conservative GDI BitBlt snapshot. It is intentionally a frame
//! source only; recognition decides whether a frame is usable and stable. A
//! future Desktop Duplication/Windows Graphics Capture source can implement the
//! same output type if a GPU-rendered TETR.IO window needs it.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::window::WindowInfo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// Pixel order is RGBA, row-major, top-to-bottom.
    pub rgba: Vec<u8>,
    pub captured_at_ms: u64,
}

impl CapturedFrame {
    pub fn expected_byte_len(&self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
    }

    pub fn is_well_formed(&self) -> bool {
        self.expected_byte_len() == Some(self.rgba.len())
    }

    /// Writes a diagnostic 32-bit BMP without adding an image-encoding
    /// dependency. This is opt-in and intended for ROI calibration only.
    pub fn write_bmp(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        use std::io::Write;

        if !self.is_well_formed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cannot write malformed frame",
            ));
        }
        let row_bytes = self.width as usize * 4;
        let pixel_bytes = row_bytes * self.height as usize;
        let file_size = 54usize + pixel_bytes;
        // Buffered: the pixel loop below issues one small write per pixel.
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        file.write_all(b"BM")?;
        file.write_all(&(file_size as u32).to_le_bytes())?;
        file.write_all(&[0u8; 4])?;
        file.write_all(&(54u32).to_le_bytes())?;
        file.write_all(&(40u32).to_le_bytes())?;
        file.write_all(&(self.width as i32).to_le_bytes())?;
        file.write_all(&(self.height as i32).to_le_bytes())?;
        file.write_all(&(1u16).to_le_bytes())?;
        file.write_all(&(32u16).to_le_bytes())?;
        file.write_all(&(0u32).to_le_bytes())?;
        file.write_all(&(pixel_bytes as u32).to_le_bytes())?;
        file.write_all(&[0u8; 16])?;

        for row in (0..self.height as usize).rev() {
            let start = row * row_bytes;
            for pixel in self.rgba[start..start + row_bytes].chunks_exact(4) {
                file.write_all(&[pixel[2], pixel[1], pixel[0], 0])?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("screen capture is only implemented on Windows")]
    UnsupportedPlatform,
    #[error("TETR.IO client rectangle is too large or empty")]
    InvalidSize,
    #[cfg(windows)]
    #[error("Win32 capture operation failed: {0}")]
    Win32(&'static str),
}

pub fn capture_client(window: &WindowInfo) -> Result<CapturedFrame, CaptureError> {
    #[cfg(windows)]
    {
        capture_client_win32(window)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err(CaptureError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
fn capture_client_win32(window: &WindowInfo) -> Result<CapturedFrame, CaptureError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::{
        Foundation::HWND,
        Graphics::Gdi::{GetDC, ReleaseDC},
    };

    let width = window.client_rect.width;
    let height = window.client_rect.height;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(CaptureError::InvalidSize);
    }
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(CaptureError::InvalidSize)?;
    let byte_count = pixel_count
        .checked_mul(4)
        .ok_or(CaptureError::InvalidSize)?;

    unsafe {
        let hwnd = window.handle as usize as HWND;
        let source_dc = GetDC(hwnd);
        if source_dc.is_null() {
            return Err(CaptureError::Win32("GetDC"));
        }
        let result = capture_from_dc(source_dc, width, height, byte_count);
        ReleaseDC(hwnd, source_dc);
        let mut frame = result?;
        for pixel in frame.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let captured_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or_default();
        Ok(CapturedFrame {
            width,
            height,
            rgba: frame,
            captured_at_ms,
        })
    }
}

#[cfg(windows)]
unsafe fn capture_from_dc(
    source_dc: *mut core::ffi::c_void,
    width: u32,
    height: u32,
    byte_count: usize,
) -> Result<Vec<u8>, CaptureError> {
    use std::{ffi::c_void, mem::size_of};

    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
        SelectObject,
    };

    unsafe {
        let memory_dc: HDC = CreateCompatibleDC(source_dc);
        if memory_dc.is_null() {
            return Err(CaptureError::Win32("CreateCompatibleDC"));
        }
        let bitmap: HBITMAP = CreateCompatibleBitmap(source_dc, width as i32, height as i32);
        if bitmap.is_null() {
            DeleteDC(memory_dc);
            return Err(CaptureError::Win32("CreateCompatibleBitmap"));
        }
        let old_object: HGDIOBJ = SelectObject(memory_dc, bitmap as HGDIOBJ);
        if old_object.is_null() {
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(memory_dc);
            return Err(CaptureError::Win32("SelectObject"));
        }

        let mut pixels = vec![0u8; byte_count];
        let copied = BitBlt(
            memory_dc,
            0,
            0,
            width as i32,
            height as i32,
            source_dc,
            0,
            0,
            SRCCOPY,
        );
        if copied == 0 {
            SelectObject(memory_dc, old_object);
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(memory_dc);
            return Err(CaptureError::Win32("BitBlt"));
        }

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height requests a top-down DIB.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let scanlines = GetDIBits(
            memory_dc,
            bitmap,
            0,
            height,
            pixels.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory_dc, old_object);
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(memory_dc);
        if scanlines == 0 {
            Err(CaptureError::Win32("GetDIBits"))
        } else {
            Ok(pixels)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_shape_is_checked_before_recognition() {
        let frame = CapturedFrame {
            width: 2,
            height: 1,
            rgba: vec![0; 8],
            captured_at_ms: 0,
        };
        assert!(frame.is_well_formed());
        assert_eq!(frame.expected_byte_len(), Some(8));
    }

    #[test]
    fn truncated_frame_is_rejected() {
        let frame = CapturedFrame {
            width: 2,
            height: 1,
            rgba: vec![0; 4],
            captured_at_ms: 0,
        };
        assert!(!frame.is_well_formed());
    }

    #[test]
    fn malformed_frame_is_not_written() {
        let frame = CapturedFrame {
            width: 2,
            height: 1,
            rgba: vec![0; 4],
            captured_at_ms: 0,
        };
        let path = std::env::temp_dir().join("pc-overlay-malformed-frame.bmp");
        let error = frame.write_bmp(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
