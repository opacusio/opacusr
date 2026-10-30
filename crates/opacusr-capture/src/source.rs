//! The capture abstraction implemented by every backend.

use crate::{CaptureError, Frame};

/// Default streaming frame rate.
pub const CAPTURE_FPS_DEFAULT: u8 = 15;

/// A live screen source. `capture()` returns the current full-screen frame;
/// backends may cache resources between calls.
pub trait ScreenCapture: Send {
    /// Grab one RGBA frame.
    fn capture(&mut self) -> Result<Frame, CaptureError>;
    /// Current screen size (queried fresh — screens can be reconfigured).
    fn size(&mut self) -> Result<(u32, u32), CaptureError>;
    /// Human-readable backend name for logs.
    fn backend(&self) -> &'static str {
        "unknown"
    }
}
