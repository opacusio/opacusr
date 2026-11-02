//! Remote cursor polling — the pointer position and appearance, captured
//! separately from the screen content (capture APIs drop the cursor, so it
//! has to travel to the viewer as its own overlay).
//!
//! One session uses one [`CursorPoller`]: [`CursorSource::poll`] is cheap
//! enough to call every streamed frame, while [`CursorSource::shape`] runs
//! only when the cursor's appearance changes (the position carries a
//! `serial` that changes with the shape). Backends without cursor support
//! (e.g. macOS) return `None` from `poll` and the host simply never sends
//! cursor messages.
//!
//! Wayland: the portal screencast stream carries `spa_meta_cursor`
//! metadata (see [`crate::wayland`]); [`CursorPoller`] picks that backend
//! up when built with the `pipewire` feature inside a Wayland session.

/// Cursor position (the hotspot) in *global* screen coordinates.
///
/// The host translates this by the streamed monitor's origin before it
/// goes on the wire, so the viewer sees coordinates relative to the
/// streamed frame ([`crate::source::ScreenCapture::origin`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub x: i32,
    pub y: i32,
    /// `false` = the cursor is logically hidden (e.g. during playback or
    /// when an app called `SetCursor(None)`).
    pub visible: bool,
    /// Identifies the current shape; two equal serials mean the same
    /// bitmap. X11: the XFixes cursor serial. Windows: the `HCURSOR` value.
    pub serial: u64,
}

/// One cursor bitmap in straight-alpha RGBA plus its hotspot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorBitmap {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    /// `width * height * 4` bytes, row-major RGBA.
    pub rgba: Vec<u8>,
}

/// Per-platform cursor source. Everything is `None` when the platform
/// cannot provide the information — the session degrades gracefully.
pub trait CursorSource: Send {
    /// Latest cursor position; `None` = no cursor support on this
    /// platform / connection failed.
    fn poll(&mut self) -> Option<CursorPosition>;
    /// Bitmap for the given serial. `None` = shape unavailable (the
    /// viewer keeps showing the previous one).
    fn shape(&mut self, serial: u64) -> Option<CursorBitmap>;
    /// Human-readable backend name for logs.
    fn backend(&self) -> &'static str {
        "unknown"
    }
}

/// Best cursor source for the current platform (a no-op poller where
/// unsupported).
pub fn platform_cursor() -> Box<dyn CursorSource> {
    Box::new(CursorPoller::new())
}

// ---------------------------------------------------------------------------
// Real backends
// ---------------------------------------------------------------------------

/// Platform cursor poller: Win32 (`GetCursorInfo` + `GetIconInfo`) on
/// Windows, XFixes on X11, PipeWire cursor metadata in Wayland sessions,
/// empty elsewhere.
pub struct CursorPoller {
    inner: Option<Inner>,
}

enum Inner {
    #[cfg(windows)]
    Win32,
    #[cfg(all(target_os = "linux", feature = "x11"))]
    X11(Box<X11Cursor>),
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    PipeWire(crate::wayland::PwCursor),
}

#[cfg(all(target_os = "linux", feature = "x11"))]
struct X11Cursor {
    conn: x11rb::rust_connection::RustConnection,
    /// `true` after a successful XFixes version negotiation.
    ready: bool,
}

impl CursorPoller {
    /// Create a poller for the current platform. Never fails: platforms
    /// without support yield a poller that always returns `None`.
    pub fn new() -> Self {
        let inner = Self::connect();
        if inner.is_none() {
            tracing::debug!(target: "opacusr", "cursor polling unavailable on this platform");
        }
        Self { inner }
    }

    fn connect() -> Option<Inner> {
        #[cfg(windows)]
        {
            Some(Inner::Win32)
        }
        #[cfg(all(target_os = "linux", feature = "pipewire"))]
        {
            // Native Wayland sessions: the cursor metadata rides on the
            // portal's PipeWire stream (XFixes would only see the
            // XWayland cursor, not the real one).
            if crate::is_wayland_session() {
                return Some(Inner::PipeWire(crate::wayland::PwCursor::new()));
            }
        }
        #[cfg(all(target_os = "linux", feature = "x11"))]
        {
            // Native Wayland sessions without the pipewire feature:
            // XFixes would only see the XWayland cursor, not the real
            // one — report unsupported instead.
            if crate::is_wayland_session() {
                return None;
            }
            let (conn, _screen) = x11rb::rust_connection::RustConnection::connect(None).ok()?;
            Some(Inner::X11(Box::new(X11Cursor { conn, ready: false })))
        }
        #[cfg(not(any(
            windows,
            all(target_os = "linux", feature = "x11"),
            all(target_os = "linux", feature = "pipewire")
        )))]
        {
            None
        }
    }
}

impl Default for CursorPoller {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorSource for CursorPoller {
    fn poll(&mut self) -> Option<CursorPosition> {
        match self.inner.as_mut()? {
            #[cfg(windows)]
            Inner::Win32 => win32::poll(),
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Inner::X11(c) => c.poll(),
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Inner::PipeWire(c) => c.poll(),
            #[cfg(not(any(
                windows,
                all(target_os = "linux", feature = "x11"),
                all(target_os = "linux", feature = "pipewire")
            )))]
            _ => None,
        }
    }

    #[cfg_attr(
        not(any(
            windows,
            all(target_os = "linux", feature = "x11"),
            all(target_os = "linux", feature = "pipewire")
        )),
        allow(unused_variables)
    )]
    fn shape(&mut self, serial: u64) -> Option<CursorBitmap> {
        match self.inner.as_mut()? {
            #[cfg(windows)]
            Inner::Win32 => win32::shape(serial),
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Inner::X11(c) => c.shape(serial),
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Inner::PipeWire(c) => c.shape(serial),
            #[cfg(not(any(
                windows,
                all(target_os = "linux", feature = "x11"),
                all(target_os = "linux", feature = "pipewire")
            )))]
            _ => None,
        }
    }

    fn backend(&self) -> &'static str {
        match &self.inner {
            #[cfg(windows)]
            Some(Inner::Win32) => "win32-cursor",
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Some(Inner::X11(_)) => "xfixes-cursor",
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Some(Inner::PipeWire(_)) => "pipewire-cursor",
            None => "none",
            #[cfg(not(any(
                windows,
                all(target_os = "linux", feature = "x11"),
                all(target_os = "linux", feature = "pipewire")
            )))]
            _ => "none",
        }
    }
}

// ---------------------------------------------------------------------------
// Windows (win32)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win32 {
    use super::{CursorBitmap, CursorPosition};

    use windows_sys::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
        HBITMAP, RGBQUAD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, HCURSOR, ICONINFO,
    };

    /// `Ok((hcursor, (x, y), visible))` or `None` when the call fails
    /// (which does not happen in practice; a hidden cursor is reported
    /// with flags missing `CURSOR_SHOWING`).
    fn info() -> Option<(HCURSOR, (i32, i32), bool)> {
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            flags: 0,
            hCursor: std::ptr::null_mut(),
            ptScreenPos: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
        };
        let ok = unsafe { GetCursorInfo(&mut ci) };
        if ok == 0 {
            return None;
        }
        Some((
            ci.hCursor,
            (ci.ptScreenPos.x, ci.ptScreenPos.y),
            ci.flags & CURSOR_SHOWING != 0,
        ))
    }

    pub fn poll() -> Option<CursorPosition> {
        let (hcursor, (x, y), visible) = info()?;
        Some(CursorPosition {
            x,
            y,
            visible,
            serial: hcursor as usize as u64,
        })
    }

    pub fn shape(serial: u64) -> Option<CursorBitmap> {
        // The serial comes from a live HCURSOR seen in `poll`; if the
        // cursor changed between poll and shape the handle may no longer
        // resolve — `None` then, and the viewer keeps its previous bitmap.
        let hcursor = serial as usize as HCURSOR;
        let mut ii = ICONINFO {
            fIcon: 0,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: std::ptr::null_mut(),
            hbmColor: std::ptr::null_mut(),
        };
        if unsafe { GetIconInfo(hcursor, &mut ii) } == 0 {
            return None;
        }
        let out = bitmap_from_iconinfo(&ii);
        unsafe {
            if !ii.hbmMask.is_null() {
                DeleteObject(ii.hbmMask as _);
            }
            if !ii.hbmColor.is_null() {
                DeleteObject(ii.hbmColor as _);
            }
        }
        out
    }

    fn bitmap_from_iconinfo(ii: &ICONINFO) -> Option<CursorBitmap> {
        let (w, h, mono) = dims(ii)?;
        let rgba = if mono {
            mono_to_rgba(ii.hbmMask, w, h)?
        } else {
            color_to_rgba(ii.hbmColor, w, h)?
        };
        Some(CursorBitmap {
            width: w,
            height: h,
            hotspot_x: ii.xHotspot.min(w),
            hotspot_y: ii.yHotspot.min(h),
            rgba,
        })
    }

    /// Dimensions of the icon/cursor; `mono = hbmColor is NULL` (mask-only
    /// cursors store both masks in one bitmap of double height).
    fn dims(ii: &ICONINFO) -> Option<(u32, u32, bool)> {
        if ii.hbmMask.is_null() {
            return None;
        }
        let hdc = unsafe { GetDC(std::ptr::null_mut()) };
        let mut bmi = BITMAPINFO {
            bmiHeader: header(0, 0),
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        // Ask for the header only: `biHeight` of the (possibly double-high)
        // mask bitmap.
        let ok = unsafe {
            GetDIBits(
                hdc,
                ii.hbmMask as _,
                0,
                0,
                std::ptr::null_mut(),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };
        if ok == 0 {
            return None;
        }
        let h_full = bmi.bmiHeader.biHeight.unsigned_abs();
        if h_full == 0 || bmi.bmiHeader.biWidth <= 0 {
            return None;
        }
        if ii.hbmColor.is_null() {
            Some((bmi.bmiHeader.biWidth as u32, h_full / 2, true))
        } else {
            Some((bmi.bmiHeader.biWidth as u32, h_full, false))
        }
    }

    fn header(w: i32, h: i32) -> BITMAPINFOHEADER {
        BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        }
    }

    fn color_to_rgba(hbm: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut bmi = BITMAPINFO {
            bmiHeader: header(w as i32, h as i32),
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let hdc = unsafe { GetDC(std::ptr::null_mut()) };
        let ok = unsafe {
            GetDIBits(
                hdc,
                hbm as _,
                0,
                h,
                rgba.as_mut_ptr() as _,
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };
        if ok == 0 {
            return None;
        }
        // GetDIBits gives BGRA; swap channels. Some legacy cursors ship a
        // zero alpha channel — synthesize 255 wherever the pixel is not
        // fully black.
        let mut any_alpha = false;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            any_alpha |= px[3] != 0;
        }
        if !any_alpha {
            for px in rgba.as_chunks_mut::<4>().0 {
                if px[0] != 0 || px[1] != 0 || px[2] != 0 {
                    px[3] = 255;
                }
            }
        }
        Some(rgba)
    }

    /// Monochrome cursors: one 1-bpp bitmap, top half AND mask (1 =
    /// transparent), bottom half XOR mask (1 = white/inverted).
    fn mono_to_rgba(hbm: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
        let stride = (w as usize).div_ceil(8).next_multiple_of(4);
        let full_h = h as usize * 2;
        let mut bits = vec![0u8; stride * full_h];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biBitCount: 1,
                biHeight: -(full_h as i32), // top-down
                ..header(w as i32, full_h as i32)
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let hdc = unsafe { GetDC(std::ptr::null_mut()) };
        let ok = unsafe {
            GetDIBits(
                hdc,
                hbm as _,
                0,
                full_h as u32,
                bits.as_mut_ptr() as _,
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };
        if ok == 0 {
            return None;
        }
        let bit = |bytes: &[u8], x: usize| bytes[x / 8] & (0x80 >> (x % 8)) != 0;
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let and = bit(&bits[y * stride..], x);
                let xor = bit(&bits[(h as usize + y) * stride..], x);
                let px = &mut rgba[(y * w as usize + x) * 4..][..4];
                // AND=0,XOR=0 → black; AND=0,XOR=1 → white;
                // AND=1,XOR=0 → transparent; AND=1,XOR=1 → screen-invert
                // (approximated as opaque white).
                let (luma, alpha) = match (and, xor) {
                    (false, false) => (0u8, 255u8),
                    (false, true) => (255, 255),
                    (true, false) => (0, 0),
                    (true, true) => (255, 255),
                };
                px.copy_from_slice(&[luma, luma, luma, alpha]);
            }
        }
        Some(rgba)
    }
}

// ---------------------------------------------------------------------------
// X11 (XFixes)
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "linux", feature = "x11"))]
impl X11Cursor {
    /// Negotiate XFixes once per connection; both `poll` and `shape` need
    /// it before any cursor request.
    fn ensure(&mut self) -> bool {
        if !self.ready {
            use x11rb::protocol::xfixes::ConnectionExt as _;
            self.ready = self
                .conn
                .xfixes_query_version(4, 0)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some();
        }
        self.ready
    }

    fn poll(&mut self) -> Option<CursorPosition> {
        use x11rb::protocol::xfixes::ConnectionExt as _;
        if !self.ensure() {
            return None;
        }
        let img = self.conn.xfixes_get_cursor_image().ok()?.reply().ok()?;
        Some(CursorPosition {
            x: img.x as i32,
            y: img.y as i32,
            visible: true,
            serial: img.cursor_serial as u64,
        })
    }

    fn shape(&mut self, _serial: u64) -> Option<CursorBitmap> {
        use x11rb::protocol::xfixes::ConnectionExt as _;
        if !self.ensure() {
            return None;
        }
        // XFixesGetCursorImage always returns the *current* cursor; the
        // caller asks right after seeing a new serial in `poll`, so the
        // bitmap matches the serial it is filed under.
        let img = self.conn.xfixes_get_cursor_image().ok()?.reply().ok()?;
        let (w, h) = (img.width as usize, img.height as usize);
        if w == 0 || h == 0 || img.cursor_image.len() != w * h {
            return None;
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for p in &img.cursor_image {
            // ARGB u32 → RGBA bytes.
            rgba.extend_from_slice(&[
                ((p >> 16) & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                (p & 0xFF) as u8,
                ((p >> 24) & 0xFF) as u8,
            ]);
        }
        Some(CursorBitmap {
            width: img.width as u32,
            height: img.height as u32,
            hotspot_x: img.xhot as u32,
            hotspot_y: img.yhot as u32,
            rgba,
        })
    }
}

// ---------------------------------------------------------------------------
// Deterministic test/demo cursor
// ---------------------------------------------------------------------------

/// A synthetic cursor for `--demo`, tests and the e2e harness: moves on a
/// Lissajous path and alternates between two shapes every second. Shape
/// bitmaps are deterministic so tests can assert exact bytes.
pub struct TestCursor {
    t0: std::time::Instant,
    area: (u32, u32),
}

impl TestCursor {
    /// Cursor roams inside `area` (match the test source's frame size).
    pub fn new(area: (u32, u32)) -> Self {
        Self {
            t0: std::time::Instant::now(),
            area,
        }
    }
}

impl CursorSource for TestCursor {
    fn poll(&mut self) -> Option<CursorPosition> {
        let el = self.t0.elapsed().as_secs_f32();
        let cx = self.area.0 as f32;
        let cy = self.area.1 as f32;
        let x = (cx * 0.5 + cx * 0.3 * (el * 0.9).sin()) as i32;
        let y = (cy * 0.5 + cy * 0.3 * (el * 0.6).cos()) as i32;
        Some(CursorPosition {
            x,
            y,
            visible: true,
            serial: (el as u64) % 2,
        })
    }

    fn shape(&mut self, serial: u64) -> Option<CursorBitmap> {
        match serial {
            0 => Some(test_arrow()),
            1 => Some(test_caret()),
            _ => None,
        }
    }

    fn backend(&self) -> &'static str {
        "test-cursor"
    }
}

/// 10×14 filled arrow, hotspot (0, 0).
pub fn test_arrow() -> CursorBitmap {
    let (w, h) = (10u32, 14u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let put = |rgba: &mut [u8], x: u32, y: u32, c: [u8; 4]| {
        if x < w && y < h {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&c);
        }
    };
    for row in 0..h {
        // Triangle: the row widens as we go down, plus a vertical tail.
        let tip = (row as f32 * 0.6) as u32;
        for x in 0..=tip.min(6) {
            put(&mut rgba, x, row, [20, 20, 20, 255]);
        }
        if row >= 8 {
            put(&mut rgba, 2, row, [255, 255, 255, 255]);
        }
    }
    CursorBitmap {
        width: w,
        height: h,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba,
    }
}

/// 4×14 text caret, hotspot (1, 0).
pub fn test_caret() -> CursorBitmap {
    let (w, h) = (4u32, 14u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for row in 0..h {
        for x in 1..3 {
            let i = ((row * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    CursorBitmap {
        width: w,
        height: h,
        hotspot_x: 1,
        hotspot_y: 0,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_switches_shapes_and_moves() {
        let mut c = TestCursor::new((320, 200));
        let mut shapes = std::collections::HashSet::new();
        let mut positions = Vec::new();
        for _ in 0..80 {
            let p = c.poll().unwrap();
            assert!(p.visible);
            assert!((-1..=320).contains(&p.x));
            assert!((-1..=200).contains(&p.y));
            positions.push((p.x, p.y));
            if let Some(s) = c.shape(p.serial) {
                assert_eq!(s.rgba.len(), s.width as usize * s.height as usize * 4);
                shapes.insert(p.serial);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        // ~1.2 s of polling: the cursor moved and used both shapes.
        assert!(shapes.contains(&0) && shapes.contains(&1));
        assert!(positions.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn test_shapes_are_deterministic() {
        let a = test_arrow();
        let b = test_arrow();
        assert_eq!(a, b);
        let c = test_caret();
        assert_eq!((c.width, c.height), (4, 14));
        assert_eq!(c.hotspot_x, 1);
    }
}
