//! ScreenCaptureKit stream — the hardware-backed primary macOS capture
//! path (`SCStream` via the C shim `shim/sck_shim.m`), with the
//! CoreGraphics snapshot backend in [`super::macos`] as the fallback.
//!
//! The push/pull bridge: the stream delivers frames on its own queue at
//! [`STREAM_FPS`]; [`SckStream::frame`] copies the latest one out (the
//! session pulls at its own adaptive pace — a static screen costs
//! nothing, SCK only wakes on content change).
//!
//! Every failure that makes SCK unusable (OS < 12.3, missing Screen
//! Recording permission, unexpected pixel format, delegate stop) comes
//! back as an error; `macos::MacCapture` then falls back to
//! CoreGraphics snapshots — the pre-2 ladder that still works on every
//! macOS version.

use std::ffi::{c_char, c_int};

use crate::{CaptureError, Frame};

/// Stream frame rate. The session pulls at its own (adaptive, ≤ 30)
/// pace; running the stream ahead of that keeps every pull fresh, and
/// SCK delivers only on content change — an idle desktop costs nothing.
const STREAM_FPS: u32 = 60;

/// Error buffer capacity for the shim's C-string messages (bytes).
const ERRBUF_CAP: usize = 512;

/// Shim return codes (must match shim/sck_shim.m).
const RC_OK: c_int = 0;
const RC_WRONG_FORMAT: c_int = 1;
const RC_ERR: c_int = -2;
const RC_NO_FRAME: c_int = -4;
const RC_STOPPED: c_int = -5;

#[link(name = "opacusr_sck_shim", kind = "static")]
extern "C" {
    fn opacusr_sck_supported() -> c_int;
    fn opacusr_sck_start(
        display_id: u32,
        width: u32,
        height: u32,
        fps: u32,
        errbuf: *mut c_char,
        errbuf_cap: u32,
    ) -> usize;
    fn opacusr_sck_poll(
        handle: usize,
        dst: *mut u8,
        dst_cap: u32,
        w: *mut u32,
        h: *mut u32,
        seq: *mut u64,
    ) -> c_int;
    fn opacusr_sck_stopmsg(handle: usize, buf: *mut c_char, cap: u32) -> c_int;
    fn opacusr_sck_stop(handle: usize);
}

/// Why a live stream stopped being usable — ScreenCaptureKit must be
/// dropped (for the rest of the session or until the next display
/// switch) and the CoreGraphics path takes over.
pub(crate) enum SckDeath {
    /// The delivered pixel format is not BGRA (unexpected OS behavior
    /// on some macOS versions that ignore the pixelFormat setting).
    WrongFormat,
    /// Any other fatal condition — delegate stop error, poll failure —
    /// with the human-readable reason for the log.
    Failed(String),
}

/// One live `SCStream`. `Drop` stops it; handles are object pointers in
/// the shim, so a dropped stream can never tear down a freshly started
/// one (the reason [`SckStream`] is moved out of `MacCapture` on error
/// instead of being replaced in place).
pub(crate) struct SckStream {
    handle: usize,
    /// Packed BGRA straight from the shim (stride-compacted rows).
    buf: Vec<u8>,
    /// Device-pixel size the stream was configured with.
    cfg: (u32, u32),
}

impl SckStream {
    /// True when this macOS can run ScreenCaptureKit at all (12.3+ and
    /// the SCStream class actually present).
    pub(crate) fn available() -> bool {
        unsafe { opacusr_sck_supported() == 1 }
    }

    /// Start a display stream and block until the first frame lands
    /// (bounded by the shim's 5 s timeout): a stream that never
    /// delivers is an error, not a black stream. `w`/`h` are the
    /// display's device-pixel size — SCK output is fixed to it.
    pub(crate) fn start(display_id: u32, w: u32, h: u32) -> Result<Self, String> {
        let mut err = [0u8; ERRBUF_CAP];
        let handle = unsafe {
            opacusr_sck_start(
                display_id,
                w,
                h,
                STREAM_FPS,
                err.as_mut_ptr().cast::<c_char>(),
                (ERRBUF_CAP - 1) as u32,
            )
        };
        if handle == 0 {
            let msg = take_cstring(&mut err);
            return Err(if msg.is_empty() {
                "ScreenCaptureKit start failed".into()
            } else {
                msg
            });
        }
        Ok(Self {
            handle,
            buf: vec![0; w as usize * h as usize * 4],
            cfg: (w, h),
        })
    }

    /// Device-pixel size the stream was configured with.
    pub(crate) fn cfg_size(&self) -> (u32, u32) {
        self.cfg
    }

    /// Latest frame as RGBA. The shim reports the frame geometry even
    /// when the destination buffer is too small, so a mid-stream
    /// geometry change (the shim reallocs under its lock) resizes and
    /// retries once instead of failing.
    pub(crate) fn frame(&mut self) -> Result<Frame, SckDeath> {
        for _ in 0..2 {
            let (mut w, mut h, mut seq) = (0u32, 0u32, 0u64);
            let rc = unsafe {
                opacusr_sck_poll(
                    self.handle,
                    self.buf.as_mut_ptr(),
                    self.buf.len() as u32,
                    &mut w,
                    &mut h,
                    &mut seq,
                )
            };
            match rc {
                RC_OK => {
                    // One C→Rust copy per frame (the same price as the
                    // CG path's CFData copy); the shim-side buffer
                    // stays persistent between pulls.
                    let mut rgba = self.buf.clone();
                    crate::bgra_to_rgba(&mut rgba);
                    return Ok(Frame::new(w, h, rgba));
                }
                // Destination too small (a mid-stream geometry change
                // grew the shim buffer): resize to the reported
                // geometry and retry once.
                RC_ERR if w > 0 && h > 0 => self.buf = vec![0; w as usize * h as usize * 4],
                RC_WRONG_FORMAT => return Err(SckDeath::WrongFormat),
                RC_STOPPED => return Err(SckDeath::Failed(self.stop_reason())),
                RC_NO_FRAME => return Err(SckDeath::Failed("no frame from the stream".into())),
                _ => {
                    return Err(SckDeath::Failed(format!(
                        "ScreenCaptureKit poll failed (rc {rc})"
                    )))
                }
            }
        }
        Err(SckDeath::Failed(
            "frame buffer did not settle after resize".into(),
        ))
    }

    /// Reason the stream stopped (the shim's delegate message), or a
    /// generic note when the shim has nothing better.
    fn stop_reason(&self) -> String {
        let mut buf = [0u8; 320];
        let rc = unsafe {
            opacusr_sck_stopmsg(
                self.handle,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len() as u32,
            )
        };
        if rc == RC_OK {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let s = String::from_utf8_lossy(&buf[..end]).into_owned();
            if !s.is_empty() {
                return s;
            }
        }
        "ScreenCaptureKit stream stopped".into()
    }
}

impl Drop for SckStream {
    fn drop(&mut self) {
        // The shim's stop is identity-checked and idempotent-safe: a
        // stale handle (stream already replaced) tears nothing down.
        unsafe { opacusr_sck_stop(self.handle) };
    }
}

/// Read the shim's NUL-terminated message out of a zeroed buffer.
fn take_cstring(buf: &mut [u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Map a stream death to the `CaptureError` the session surfaces when
/// even the CoreGraphics fallback fails right after (the CG error is
/// the one the user sees; this keeps the SCK reason in the logs).
impl From<SckDeath> for CaptureError {
    fn from(d: SckDeath) -> Self {
        let why = match d {
            SckDeath::WrongFormat => "unexpected pixel format".to_string(),
            SckDeath::Failed(m) => m,
        };
        CaptureError::Other(format!("ScreenCaptureKit: {why}"))
    }
}
