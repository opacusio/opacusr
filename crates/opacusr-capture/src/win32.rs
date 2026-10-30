//! Windows backend: GDI `BitBlt` of one monitor.
//!
//! Monitors are enumerated with `EnumDisplayMonitors`; the primary is
//! streamed by default and [`ScreenCapture::select_screen`] switches the
//! blit rect. Works on every Windows version without extra privileges.
//! DXGI Desktop Duplication (faster, HDR-aware) is a separate backend —
//! see `dxgi.rs` / the roadmap.

#![cfg(windows)]

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    MONITORINFO, MONITORINFOEXW, RGBQUAD, SRCCOPY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics};

use crate::source::{ScreenCapture, ScreenDesc};
use crate::{bgra_to_rgba, CaptureError, Frame};

/// One physical monitor in root coordinates.
#[derive(Debug, Clone)]
struct MonRect {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    primary: bool,
}

pub struct Win32Capture {
    monitors: Vec<MonRect>,
    selected: usize,
    desktop: HWND,
}

// SAFETY: `Win32Capture` only carries monitor rects and an `HWND` (a
// process-wide window handle, not thread-affine); every GDI resource used
// in [`capture`] is acquired and released within that one call. The struct
// is moved between threads only by the agent's host loop, which uses it
// from a single thread at a time.
unsafe impl Send for Win32Capture {}

impl Default for Win32Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32Capture {
    pub fn new() -> Self {
        let mut cap = Self {
            monitors: Vec::new(),
            selected: 0,
            desktop: unsafe { GetDesktopWindow() },
        };
        cap.refresh_monitors();
        cap
    }

    /// Re-enumerate monitors (hotplug / reconfiguration support). The
    /// primary monitor sorts first for stable ids.
    fn refresh_monitors(&mut self) {
        let mut list: Vec<MonRect> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(enum_proc),
                &mut list as *mut Vec<MonRect> as isize,
            );
        }
        list.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.name.cmp(&b.name)));
        self.monitors = list;
    }

    fn current(&self) -> MonRect {
        self.monitors
            .get(self.selected)
            .cloned()
            .unwrap_or(MonRect {
                name: "Primary".into(),
                x: 0,
                y: 0,
                w: unsafe { GetSystemMetrics(SM_CXSCREEN) },
                h: unsafe { GetSystemMetrics(SM_CYSCREEN) },
                primary: true,
            })
    }
}

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

/// `EnumDisplayMonitors` callback: collect `MONITORINFOEXW` for each monitor.
unsafe extern "system" fn enum_proc(
    hmonitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
    _hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    _rect: *mut RECT,
    lparam: isize,
) -> i32 {
    let list = unsafe { &mut *(lparam as *mut Vec<MonRect>) };
    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut MONITORINFO) };
    if ok != 0 {
        let rc = info.monitorInfo.rcMonitor;
        list.push(MonRect {
            name: String::from_utf16_lossy(&info.szDevice),
            x: rc.left,
            y: rc.top,
            w: rc.right - rc.left,
            h: rc.bottom - rc.top,
            primary: info.monitorInfo.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
        });
    }
    1 // continue enumeration
}

use windows_sys::Win32::Graphics::Gdi::EnumDisplayMonitors;

impl ScreenCapture for Win32Capture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let mon = self.current();
        let (w, h) = (mon.w.max(0) as u32, mon.h.max(0) as u32);
        if w == 0 || h == 0 {
            return Err(CaptureError::Win32("empty monitor rect".into()));
        }

        unsafe {
            let hdc = GetDC(self.desktop);
            if hdc.is_null() {
                return Err(CaptureError::Win32("GetDC failed".into()));
            }
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w as i32, h as i32);
            let old = SelectObject(mem, bmp);

            let blit_ok = BitBlt(mem, 0, 0, w as i32, h as i32, hdc, mon.x, mon.y, SRCCOPY);

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
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
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
        let mon = self.current();
        let (w, h) = (mon.w.max(0) as u32, mon.h.max(0) as u32);
        if w == 0 || h == 0 {
            return Err(CaptureError::Win32("empty monitor metrics".into()));
        }
        Ok((w, h))
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        self.refresh_monitors();
        if self.monitors.is_empty() {
            self.refresh_monitors();
        }
        self.monitors
            .iter()
            .enumerate()
            .map(|(i, m)| ScreenDesc {
                id: i as u32,
                name: if m.primary {
                    format!("{} (primary)", m.name)
                } else {
                    m.name.clone()
                },
                w: m.w.max(0) as u32,
                h: m.h.max(0) as u32,
            })
            .collect()
    }

    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        self.refresh_monitors();
        let idx = id as usize;
        if idx >= self.monitors.len() {
            return Err(CaptureError::Win32(format!(
                "unknown screen id {id} ({} monitors)",
                self.monitors.len()
            )));
        }
        let changed = idx != self.selected;
        self.selected = idx;
        Ok(changed)
    }

    fn backend(&self) -> &'static str {
        "win32-gdi"
    }
}
