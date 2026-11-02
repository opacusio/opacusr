//! OpacusR screen capture.
//!
//! One frame = tightly-packed RGBA8 (`4 * width * height` bytes).
//!
//! Backends:
//! * Linux/X11 — full-screen `GetImage` (simple, correct; XShm/Damage and
//!   Wayland/PipeWire are roadmap items, see docs/roadmap.md);
//! * Windows — GDI `BitBlt` of the primary screen (DXGI Desktop Duplication
//!   is the roadmap upgrade);
//! * macOS / Android / iOS — stub returning [`CaptureError::Unsupported`]
//!   (viewer-only use on those platforms for now);
//! * [`test::TestSource`] — deterministic animated pattern used by tests
//!   and `opacusr-agent --demo` (no real screen required).

pub mod cursor;
/// Byte-level `spa_meta_cursor` reader for the PipeWire/portal backend.
pub mod cursor_meta;
pub mod source;

#[cfg(feature = "x11")]
pub mod x11;

#[cfg(windows)]
pub mod dxgi;

#[cfg(windows)]
pub mod win32;

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod wayland;

#[cfg(any(
    target_os = "macos",
    target_os = "android",
    target_os = "ios",
    all(not(target_os = "windows"), not(feature = "x11"))
))]
pub mod stub;

pub mod test;

pub use cursor::{
    platform_cursor, CursorBitmap, CursorPoller, CursorPosition, CursorSource, TestCursor,
};
pub use source::{ScreenCapture, CAPTURE_FPS_DEFAULT};

/// True when the process looks like it runs inside a Wayland session:
/// `WAYLAND_DISPLAY`/`WAYLAND_SOCKET` is set, or `XDG_SESSION_TYPE` is
/// `wayland`.
pub fn is_wayland_session() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
    {
        return true;
    }
    matches!(std::env::var("XDG_SESSION_TYPE").as_deref(), Ok("wayland"))
}

/// Non-fatal startup hint for hosts: under a Wayland session without the
/// PipeWire capture feature, screen sharing cannot work. Surfaced to the
/// UI / CLI when the service starts; `None` on other platforms.
pub fn wayland_startup_warning() -> Option<String> {
    #[cfg(all(target_os = "linux", not(feature = "pipewire")))]
    {
        if is_wayland_session() {
            return Some(
                "Wayland session detected: screen capture needs the PipeWire portal (build with --features opacusr-capture/pipewire) or an X11 session."
                    .to_string(),
            );
        }
    }
    None
}

use thiserror::Error;

/// Capture failures, unified across backends.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("no capture backend on this platform (viewer-only)")]
    Unsupported,
    #[error("x11 error: {0}")]
    X11(String),
    #[error("wayland: {0}")]
    Wayland(String),
    #[error("win32 error: {0}")]
    Win32(String),
    #[error("screen changed shape mid-capture")]
    Resized,
    #[error("{0}")]
    Other(String),
}

/// One captured frame in RGBA8.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, `RGBA`.
    pub rgba: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        debug_assert_eq!(rgba.len(), (width as usize) * (height as usize) * 4);
        Self {
            width,
            height,
            rgba,
        }
    }

    /// Black frame of the given size.
    pub fn black(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; (width as usize) * (height as usize) * 4],
        }
    }
}

/// Convert tightly-packed BGRA (X11 `ZPixmap` 32bpp, Windows `GetDIBits`)
/// to RGBA in place.
pub fn bgra_to_rgba(buf: &mut [u8]) {
    for px in buf.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Nearest-neighbour downscale — fast and dependency-free; good enough for
/// the viewer's quality slider (bilinear/Lanczos on the roadmap).
pub fn resize_nearest(frame: &Frame, new_w: u32, new_h: u32) -> Frame {
    if (frame.width, frame.height) == (new_w, new_h) {
        return frame.clone();
    }
    if new_w == 0 || new_h == 0 {
        return Frame::black(frame.width, frame.height);
    }
    let mut out = Vec::with_capacity((new_w as usize) * (new_h as usize) * 4);
    let x_ratio = frame.width as f64 / new_w as f64;
    let y_ratio = frame.height as f64 / new_h as f64;
    for y in 0..new_h {
        let sy = ((y as f64 * y_ratio) as u32).min(frame.height - 1) as usize;
        for x in 0..new_w {
            let sx = ((x as f64 * x_ratio) as u32).min(frame.width - 1) as usize;
            let src = (sy * frame.width as usize + sx) * 4;
            out.extend_from_slice(&frame.rgba[src..src + 4]);
        }
    }
    Frame::new(new_w, new_h, out)
}

/// Which capture backend to use on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinBackend {
    /// Try DXGI Desktop Duplication, fall back to GDI.
    Auto,
    /// GDI `BitBlt` (always available, slower).
    Gdi,
    /// DXGI Desktop Duplication (fails on service sessions).
    Dxgi,
}

impl WinBackend {
    /// Parse the config value (`auto` | `gdi` | `dxgi`).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "gdi" => Self::Gdi,
            "dxgi" => Self::Dxgi,
            _ => Self::Auto,
        }
    }
}

/// Best backend for the current platform. Falls back to the test source
/// **only** when `allow_test_fallback` is set (used by `--demo` mode and
/// tests); production use should surface the error to the user instead.
/// `backend` selects the Windows capture backend (`auto` | `gdi` | `dxgi`).
pub fn platform_capture(
    allow_test_fallback: bool,
    backend: WinBackend,
) -> Result<Box<dyn ScreenCapture>, CaptureError> {
    // `backend` only matters on Windows; reference it unconditionally so
    // the unused_variables lint is satisfied on other platforms.
    let _ = backend;
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    {
        // Native Wayland session → the PipeWire portal (X11 would only see
        // XWayland windows). Fall back to X11 when the portal is
        // unavailable or denied.
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match crate::wayland::PipeWireCapture::connect() {
                Ok(c) => {
                    tracing::info!(target: "opacusr", "using PipeWire portal capture (Wayland)");
                    return Ok(Box::new(c));
                }
                Err(e) => {
                    tracing::warn!(target: "opacusr", "PipeWire capture unavailable ({e}); falling back to X11/XWayland");
                }
            }
        }
    }
    #[cfg(all(feature = "x11", target_os = "linux"))]
    {
        // Native Wayland session without the PipeWire feature: an X11
        // capture would only see XWayland windows (native apps stay
        // blank), which is worse than failing with a clear message.
        // Escape hatch: OPACUSR_ALLOW_X11_ON_WAYLAND=1.
        if crate::is_wayland_session() && std::env::var_os("OPACUSR_ALLOW_X11_ON_WAYLAND").is_none()
        {
            return Err(CaptureError::Wayland(
                "Wayland session detected: screen capture needs the PipeWire portal \
                (build with --features opacusr-capture/pipewire) or an X11 session \
                (e.g. select 'Ubuntu on Xorg' at the login screen). \
                Set OPACUSR_ALLOW_X11_ON_WAYLAND=1 to force the limited XWayland capture."
                    .to_string(),
            ));
        }

        match crate::x11::X11Capture::connect() {
            Ok(c) => return Ok(Box::new(c)),
            Err(e) => {
                if allow_test_fallback {
                    tracing::warn!(target: "opacusr", "X11 capture unavailable ({e}); using test source");
                    return Ok(Box::new(crate::test::TestSource::new(1280, 720)));
                }
                if crate::is_wayland_session() {
                    return Err(CaptureError::Wayland(format!(
                        "Wayland session: PipeWire portal unavailable and X11/XWayland capture failed ({e}). \
                        Use an X11 session or build with --features opacusr-capture/pipewire."
                    )));
                }
                return Err(e);
            }
        }
    }
    #[cfg(windows)]
    {
        if backend != WinBackend::Gdi {
            match crate::dxgi::DxgiCapture::new() {
                Ok(c) => {
                    tracing::info!(target: "opacusr", "using DXGI Desktop Duplication");
                    return Ok(Box::new(c));
                }
                Err(e) => {
                    if backend == WinBackend::Dxgi {
                        return Err(e);
                    }
                    tracing::warn!(target: "opacusr", "DXGI capture unavailable ({e}); falling back to GDI");
                }
            }
        }
        // GDI capture is always available on Windows; the fallback flag
        // only governs the Unix path below.
        let _ = allow_test_fallback;
        return Ok(Box::new(crate::win32::Win32Capture::new()));
    }
    #[allow(unreachable_code)]
    {
        let _ = backend;
        if allow_test_fallback {
            return Ok(Box::new(crate::test::TestSource::new(1280, 720)));
        }
        Err(CaptureError::Unsupported)
    }
}
