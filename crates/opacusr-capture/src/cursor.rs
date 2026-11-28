//! Remote cursor polling — the pointer position and appearance, captured
//! separately from the screen content (capture APIs drop the cursor, so it
//! has to travel to the viewer as its own overlay).
//!
//! One session uses one [`CursorPoller`]: [`CursorSource::poll`] is cheap
//! enough to call every streamed frame, while [`CursorSource::shape`] runs
//! only when the cursor's appearance changes (the position carries a
//! `serial` that changes with the shape). Backends without cursor support
//! return `None` from `poll` and the host simply never sends cursor
//! messages.
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

/// Convert PREMULTIPLIED-alpha RGBA (what AppKit/CoreGraphics bitmap
/// contexts composite to — the macOS cursor shim ships this) into the
/// straight alpha the wire speaks ([`CursorBitmap::rgba`]). No-op on
/// fully opaque and fully transparent pixels. Public for the same
/// reason as [`crate::bgra_to_rgba`]: a generally useful conversion
/// (and it keeps the Linux unit tests alive — the macOS caller is
/// cfg-gated out there).
pub fn unpremultiply_rgba(buf: &mut [u8]) {
    for px in buf.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        // c' = round(c * 255 / a); premultiplied guarantees c <= a, so
        // the u16 math never overflows and the result never exceeds 255.
        let a16 = u16::from(a);
        for c in px[..3].iter_mut() {
            *c = ((u16::from(*c) * 255 + a16 / 2) / a16) as u8;
        }
    }
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
/// real NSCursor shapes through the C shim on macOS.
pub struct CursorPoller {
    inner: Option<Inner>,
}

enum Inner {
    #[cfg(windows)]
    Win32,
    #[cfg(target_os = "macos")]
    Mac,
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
        #[cfg(target_os = "macos")]
        {
            Some(Inner::Mac)
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
            target_os = "macos",
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
            #[cfg(target_os = "macos")]
            Inner::Mac => macos::poll(),
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Inner::X11(c) => c.poll(),
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Inner::PipeWire(c) => c.poll(),
            #[cfg(not(any(
                windows,
                target_os = "macos",
                all(target_os = "linux", feature = "x11"),
                all(target_os = "linux", feature = "pipewire")
            )))]
            _ => None,
        }
    }

    #[cfg_attr(
        not(any(
            windows,
            target_os = "macos",
            all(target_os = "linux", feature = "x11"),
            all(target_os = "linux", feature = "pipewire")
        )),
        allow(unused_variables)
    )]
    fn shape(&mut self, serial: u64) -> Option<CursorBitmap> {
        match self.inner.as_mut()? {
            #[cfg(windows)]
            Inner::Win32 => win32::shape(serial),
            #[cfg(target_os = "macos")]
            Inner::Mac => macos::shape(serial),
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Inner::X11(c) => c.shape(serial),
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Inner::PipeWire(c) => c.shape(serial),
            #[cfg(not(any(
                windows,
                target_os = "macos",
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
            #[cfg(target_os = "macos")]
            Some(Inner::Mac) => "macos-cursor",
            #[cfg(all(target_os = "linux", feature = "x11"))]
            Some(Inner::X11(_)) => "xfixes-cursor",
            #[cfg(all(target_os = "linux", feature = "pipewire"))]
            Some(Inner::PipeWire(_)) => "pipewire-cursor",
            None => "none",
            #[cfg(not(any(
                windows,
                target_os = "macos",
                all(target_os = "linux", feature = "x11"),
                all(target_os = "linux", feature = "pipewire")
            )))]
            _ => "none",
        }
    }
}

// ---------------------------------------------------------------------------
// macOS (CoreGraphics events + real NSCursor shapes via the C shim)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{unpremultiply_rgba, CursorBitmap, CursorPosition};

    /// Serial of the FALLBACK arrow sprite — used only when the NSCursor
    /// shim has never seen a cursor (serial 0: no window server access).
    /// With the shim live, the serial comes from it and changes with the
    /// real per-app shape (I-beam, resize, grab — whatever the system
    /// shows). The viewer adds a contrasting outline around the sprite,
    /// so a flat black arrow reads fine on light AND dark desktops.
    const ARROW_SERIAL: u64 = 1;

    const ARROW_W: u32 = 16;
    const ARROW_H: u32 = 24;

    /// Buffer for the shim's rendered cursor (256x256 RGBA max, the
    /// shim's own sanity cap).
    const SHIM_BUF_CAP: usize = 256 * 256 * 4;

    /// Device px per point of the display under the cursor, cached by
    /// the last poll() — the shim renders the shape at exactly this
    /// scale, so poll and shape always agree (a scale drift would bump
    /// the shim's serial and re-download the bitmap).
    static LAST_SCALE100: AtomicU32 = AtomicU32::new(100);

    /// C bridge to shim/cursor_shim.m (the only ObjC — compiled by
    /// build.rs on macOS targets only).
    mod nscursor {
        use std::ffi::c_int;

        #[link(name = "opacusr_cursor_shim", kind = "static")]
        extern "C" {
            pub(super) fn opacusr_cursor_serial(scale: f32) -> u64;
            pub(super) fn opacusr_cursor_image(
                scale: f32,
                dst: *mut u8,
                cap: u32,
                w: *mut u32,
                h: *mut u32,
                hot_x: *mut u32,
                hot_y: *mut u32,
                serial: *mut u64,
            ) -> c_int;
        }
    }

    /// Outline of the standard macOS arrow, clockwise, in sprite pixels
    /// (x right, y down): vertical left edge → tail notch → tail → back
    /// up to the notch → horizontal ledge → long hypotenuse home.
    const ARROW_POLY: &[(f32, f32)] = &[
        (0.0, 0.0),
        (0.0, 16.0),
        (4.5, 12.0),
        (7.0, 19.0),
        (9.5, 18.0),
        (6.5, 11.5),
        (11.0, 11.5),
    ];

    /// Even-odd ray casting: is the point inside the arrow polygon?
    fn inside_arrow(px: f32, py: f32) -> bool {
        let mut inside = false;
        let mut prev = ARROW_POLY[ARROW_POLY.len() - 1];
        for &(xi, yi) in ARROW_POLY {
            let (xj, yj) = prev;
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            prev = (xi, yi);
        }
        inside
    }

    /// Rasterize the arrow: opaque black fill, transparent background,
    /// hotspot at the tip (0, 0). 1-px sampling of pixel centers.
    fn arrow_bitmap() -> CursorBitmap {
        let mut rgba = vec![0u8; (ARROW_W * ARROW_H * 4) as usize];
        for y in 0..ARROW_H {
            for x in 0..ARROW_W {
                if inside_arrow(x as f32 + 0.5, y as f32 + 0.5) {
                    let i = ((y * ARROW_W + x) * 4) as usize;
                    rgba[i] = 10; // near-black like the real pointer
                    rgba[i + 1] = 10;
                    rgba[i + 2] = 10;
                    rgba[i + 3] = 255;
                }
            }
        }
        CursorBitmap {
            width: ARROW_W,
            height: ARROW_H,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba,
        }
    }

    pub fn poll() -> Option<CursorPosition> {
        // Quartz events: global top-left origin, POINTS. Scale to the
        // device pixels the streamed frame speaks (2x on Retina).
        let (x_pt, y_pt) = crate::macos::cursor_point()?;
        let s = crate::macos::px_per_pt_at_point(x_pt, y_pt);
        LAST_SCALE100.store((s * 100.0).round().max(1.0) as u32, Ordering::Relaxed);
        // The shim's serial identifies the REAL system cursor shape and
        // bumps when the rendered content changes (per-app shapes, or a
        // scale change — dims are part of the identity). Serial 0 means
        // the shim has never seen a cursor: fall back to the arrow.
        let serial = unsafe { nscursor::opacusr_cursor_serial(s) };
        let serial = if serial == 0 { ARROW_SERIAL } else { serial };
        Some(CursorPosition {
            x: (x_pt * s).round() as i32,
            y: (y_pt * s).round() as i32,
            // Visibility needs window-server state the public AppKit API
            // does not expose — always report visible; a hidden remote
            // pointer draws an arrow that sits still, which is less
            // confusing than no pointer at all.
            visible: true,
            serial,
        })
    }

    pub fn shape(serial: u64) -> Option<CursorBitmap> {
        // Render the CURRENT system cursor at the cached scale. The
        // bitmap comes back PREMULTIPLIED (AppKit bitmaps) — convert to
        // the straight alpha the wire speaks.
        let scale = LAST_SCALE100.load(Ordering::Relaxed).max(1) as f32 / 100.0;
        let mut buf = vec![0u8; SHIM_BUF_CAP];
        let (mut w, mut h, mut hx, mut hy, mut shim_serial) = (0u32, 0u32, 0u32, 0u32, 0u64);
        let rc = unsafe {
            nscursor::opacusr_cursor_image(
                scale,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut w,
                &mut h,
                &mut hx,
                &mut hy,
                &mut shim_serial,
            )
        };
        if rc == 0 && w > 0 && h > 0 && w <= 256 && h <= 256 {
            if shim_serial == serial {
                let n = w as usize * h as usize * 4;
                let mut rgba = buf[..n].to_vec();
                unpremultiply_rgba(&mut rgba);
                return Some(CursorBitmap {
                    width: w,
                    height: h,
                    hotspot_x: hx,
                    hotspot_y: hy,
                    rgba,
                });
            }
            // The shape changed between poll and shape (stale serial):
            // None — the session retries next frame, or gives up on the
            // serial after 5 tries; the viewer keeps its last bitmap.
            return None;
        }
        // Shim unavailable (never saw a cursor / render failed): the
        // arrow sprite keeps the overlay alive.
        if serial == ARROW_SERIAL {
            return Some(arrow_bitmap());
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn arrow_sprite_has_expected_geometry_and_fill() {
            let cur = arrow_bitmap();
            assert_eq!((cur.width, cur.height), (ARROW_W, ARROW_H));
            assert_eq!((cur.hotspot_x, cur.hotspot_y), (0, 0));
            let alpha_at = |x: u32, y: u32| -> u8 {
                let i = ((y * ARROW_W + x) * 4) as usize;
                cur.rgba[i + 3]
            };
            // The 1px tip of the hypotenuse passes just left of the
            // (0,0) pixel center — the first fully-opaque pixel is one
            // row down. The far corner is transparent, the body is
            // opaque, nothing beyond the ledge/tail is filled.
            assert_eq!(alpha_at(0, 1), 255);
            assert_eq!(alpha_at(15, 23), 0);
            assert_eq!(alpha_at(2, 10), 255);
            assert_eq!(alpha_at(12, 20), 0);
        }

        #[test]
        fn arrow_serial_is_stable() {
            // Same serial → same bitmap; unknown serials → None (the
            // host retries 5× then gives up — never triggered here).
            assert_eq!(arrow_bitmap(), arrow_bitmap());
            assert!(shape(ARROW_SERIAL).is_some());
            assert!(shape(42).is_none());
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

    #[test]
    fn unpremultiply_leaves_edges_untouched() {
        // Fully opaque: identity. Fully transparent: untouched garbage
        // RGB (invisible anyway).
        let mut buf = vec![255u8, 200, 100, 255, 90, 60, 30, 0];
        unpremultiply_rgba(&mut buf);
        assert_eq!(buf, vec![255, 200, 100, 255, 90, 60, 30, 0]);
    }

    #[test]
    fn unpremultiply_restores_composited_values() {
        // Straight 64,128,192 at alpha 128 composites to 32,64,96
        // (truncating premultiply). The inverse is exact for 64 and 128,
        // and ±1 for 192 (96.37 quantized to 96 — information loss is
        // inherent, invisible on a cursor bitmap).
        let mut buf = vec![32u8, 64, 96, 128];
        unpremultiply_rgba(&mut buf);
        assert_eq!(buf, vec![64, 128, 191, 128]);
    }

    #[test]
    fn unpremultiply_stays_in_range_and_saturates() {
        // Premultiplied c == a inverts to exactly 255 (no overflow).
        let mut buf = vec![128u8, 128, 128, 128, 254, 254, 254, 255];
        unpremultiply_rgba(&mut buf);
        assert_eq!(buf[0..4], vec![255u8, 255, 255, 128]);
        assert_eq!(buf[4..8], vec![254u8, 254, 254, 255]);
    }

    #[test]
    fn unpremultiply_round_trip_stays_within_bound() {
        // The premultiply quantization is lossy (unpremultiply cannot be
        // exact in general): the straight value is recoverable only to
        // within ceil(255/a) + 1 — the tighter the alpha, the more bits
        // the composite destroyed. Verified numerically over the full
        // sweep; deviations are invisible on a cursor bitmap.
        let straight: Vec<u8> = (0..=255u8)
            .flat_map(|i| [i, 255 - i, i / 2, i.wrapping_add(55).max(1)])
            .collect();
        let mut prem = straight.clone();
        for px in prem.as_chunks_mut::<4>().0 {
            let a = u16::from(px[3]);
            for c in px[..3].iter_mut() {
                *c = (u16::from(*c) * a / 255) as u8;
            }
        }
        unpremultiply_rgba(&mut prem);
        for (got, want) in prem
            .as_chunks::<4>()
            .0
            .iter()
            .zip(straight.as_chunks::<4>().0)
        {
            let a = u16::from(want[3]).max(1);
            let bound = (255u16.div_ceil(a) + 1) as i16;
            for ch in 0..3 {
                let d = i16::from(got[ch]) - i16::from(want[ch]);
                assert!(
                    d.abs() <= bound,
                    "straight {want:?} → got {got:?} (bound {bound})"
                );
            }
        }
    }
}
