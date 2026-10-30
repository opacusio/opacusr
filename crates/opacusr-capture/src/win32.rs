//! Windows backend: GDI `BitBlt` of the primary monitor.
//!
//! Works on every Windows version without extra privileges. DXGI Desktop
//! Duplication (faster, HDR-aware, multi-monitor friendly) is a roadmap
//! upgrade — this backend is intentionally small and hard to get wrong.

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics};

use crate::source::ScreenCapture;
use crate::{bgra_to_rgba, CaptureError, Frame};

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

pub struct Win32Capture {
    desktop: HWND,
}

impl Win32Capture {
    pub fn new() -> Self {
        Self {
            desktop: unsafe { GetDesktopWindow() },
        }
    }
}

impl ScreenCapture for Win32Capture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        if w <= 0 || h <= 0 {
            return Err(CaptureError::Win32(
                "GetSystemMetrics returned empty screen".into(),
            ));
        }
        let (w, h) = (w as u32, h as u32);

        unsafe {
            let hdc = GetDC(self.desktop);
            if hdc.is_null() {
                return Err(CaptureError::Win32("GetDC failed".into()));
            }
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w as i32, h as i32);
            let old = SelectObject(mem, bmp);

            let blit_ok = BitBlt(mem, 0, 0, w as i32, h as i32, hdc, 0, 0, SRCCOPY);

            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [Default::default()],
            };
            let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
            let got = if blit_ok != 0 {
                GetDIBits(
                    mem,
                    bmp,
                    0,
                    h,
                    rgba.as_mut_ptr().cast(),
                    &mut bi,
                    DIB_RGB_COLORS,
                )
            } else {
                0
            };

            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(self.desktop, hdc);

            if got == 0 {
                return Err(CaptureError::Win32("BitBlt/GetDIBits failed".into()));
            }
            bgra_to_rgba(&mut rgba);
            Ok(Frame::new(w, h, rgba))
        }
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        if w <= 0 || h <= 0 {
            return Err(CaptureError::Win32("empty screen metrics".into()));
        }
        Ok((w as u32, h as u32))
    }

    fn backend(&self) -> &'static str {
        "win32-gdi"
    }
}
