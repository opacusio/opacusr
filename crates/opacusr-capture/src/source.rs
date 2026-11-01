//! The capture abstraction implemented by every backend.

use crate::{CaptureError, Frame};

/// Default streaming frame rate.
pub const CAPTURE_FPS_DEFAULT: u8 = 15;

/// One selectable monitor of the host (multi-monitor support).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDesc {
    /// Stable within one capture instance; used with [`ScreenCapture::select_screen`].
    pub id: u32,
    pub name: String,
    pub w: u32,
    pub h: u32,
}

/// A live screen source. `capture()` returns the current frame of the
/// *selected* monitor; backends may cache resources between calls.
pub trait ScreenCapture: Send {
    /// Grab one RGBA frame.
    fn capture(&mut self) -> Result<Frame, CaptureError>;
    /// Current screen size (queried fresh — screens can be reconfigured).
    fn size(&mut self) -> Result<(u32, u32), CaptureError>;
    /// Monitors available for streaming. The first entry is the default.
    fn screens(&mut self) -> Vec<ScreenDesc> {
        match self.size() {
            Ok((w, h)) => vec![ScreenDesc {
                id: 0,
                name: "Screen".into(),
                w,
                h,
            }],
            Err(_) => Vec::new(),
        }
    }
    /// Switch the streamed monitor. `Ok(true)` = selection changed,
    /// `Ok(false)` = already selected; errors on unknown ids.
    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        if id == 0 {
            Ok(false)
        } else {
            Err(CaptureError::Unsupported)
        }
    }
    /// Top-left of the captured region in global screen coordinates.
    /// Cursor positions ([`crate::cursor::CursorPosition`]) are global;
    /// the host subtracts this origin so the viewer gets coordinates
    /// relative to the streamed frame. Single-monitor captures are `(0,0)`.
    fn origin(&mut self) -> (i32, i32) {
        (0, 0)
    }
    /// Human-readable backend name for logs.
    fn backend(&self) -> &'static str {
        "unknown"
    }
}
