//! Deterministic animated test source — no display server required.
//!
//! Used by the integration test suite and by `opacusr-agent --demo` so a
//! full session can be exercised anywhere (CI, containers, phones acting as
//! viewers of a headless box).
//!
//! The picture is a moving gradient with a stable color band per frame; the
//! frame counter is stored in the first 8 pixels so end-to-end tests can
//! assert frame identity.

use crate::source::ScreenCapture;
use crate::{CaptureError, Frame};

pub struct TestSource {
    width: u32,
    height: u32,
    frame: u64,
}

impl TestSource {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            frame: 0,
        }
    }

    pub fn frame_count(&self) -> u64 {
        self.frame
    }
}

impl ScreenCapture for TestSource {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let (w, h) = (self.width, self.height);
        let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
        let t = (self.frame % 256) as u8;
        for y in 0..h {
            for x in 0..w {
                let r = ((x * 255) / w.max(1)) as u8;
                let g = ((y * 255) / h.max(1)) as u8;
                let b = t;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        // Encode the frame number in the first 8 pixels' red channel.
        let seq = self.frame.to_le_bytes();
        for (i, byte) in seq.iter().enumerate() {
            rgba[i * 4] = *byte;
        }
        self.frame += 1;
        Ok(Frame::new(w, h, rgba))
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        Ok((self.width, self.height))
    }

    fn backend(&self) -> &'static str {
        "test-source"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_deterministic_and_sequenced() {
        let mut src = TestSource::new(64, 32);
        let f0 = src.capture().unwrap();
        assert_eq!((f0.width, f0.height), (64, 32));
        assert_eq!(f0.rgba.len(), 64 * 32 * 4);
        // Frame number embedded in first 8 pixels.
        let mut seq = [0u8; 8];
        for (i, slot) in seq.iter_mut().enumerate() {
            *slot = f0.rgba[i * 4];
        }
        assert_eq!(u64::from_le_bytes(seq), 0);
        src.capture().unwrap();
        src.capture().unwrap();
        let f3 = src.capture().unwrap();
        let mut seq = [0u8; 8];
        for (i, slot) in seq.iter_mut().enumerate() {
            *slot = f3.rgba[i * 4];
        }
        assert_eq!(u64::from_le_bytes(seq), 3);
    }
}
