//! Stub backend for platforms without capture support yet (macOS, Android,
//! iOS). The viewer role works there; acting as a host is a roadmap item.

#![allow(dead_code)]

use crate::source::ScreenCapture;
use crate::{CaptureError, Frame};

pub struct StubCapture;

impl ScreenCapture for StubCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unsupported)
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        Err(CaptureError::Unsupported)
    }

    fn backend(&self) -> &'static str {
        "stub"
    }
}
