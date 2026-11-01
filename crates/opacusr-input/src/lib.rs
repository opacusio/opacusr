//! OpacusR input injection.
//!
//! Desktop backends use [`enigo`] (X11 / Windows / macOS). Mobile targets
//! and tests use the stub / mock implementations. The host scales incoming
//! coordinates from the *viewer's* view of the remote screen to its own
//! real screen size before calling [`InputInjector`] — see
//! `opacusr-agent::session::InputExecutor`.

use opacusr_proto::message::{mods, MouseButtonId, OpKey};
use thiserror::Error;

/// Injection failures.
#[derive(Debug, Error)]
pub enum InputError {
    #[error("no injection backend on this platform")]
    Unsupported,
    #[error("backend error: {0}")]
    Backend(String),
    #[error("unsupported key: {0:?}")]
    UnsupportedKey(OpKey),
}

/// Executes remote input events on the local machine.
pub trait InputInjector: Send {
    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError>;
    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        button: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError>;
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
    /// Press/release a (possibly modified) key.
    fn key(&mut self, key: OpKey, m: u8, down: bool) -> Result<(), InputError>;
    /// Type a text string (mobile / IME path).
    fn text(&mut self, text: &str) -> Result<(), InputError>;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use desktop::DesktopInjector;

pub mod clipboard;

/// TeamViewer-style physical input lock (Windows LL hooks / Linux evdev
/// grabs). Engaged by full-control sessions when `lock_input` is set.
pub mod lock;

/// No-op injector (mobile viewer-side builds, tests).
#[derive(Debug, Default)]
pub struct NullInjector;

impl InputInjector for NullInjector {
    fn mouse_move(&mut self, _x: u32, _y: u32) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
    fn mouse_button(
        &mut self,
        _x: u32,
        _y: u32,
        _b: MouseButtonId,
        _down: bool,
    ) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
    fn wheel(&mut self, _dx: i32, _dy: i32) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
    fn key(&mut self, _k: OpKey, _m: u8, _down: bool) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
    fn text(&mut self, _t: &str) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
}

/// Records events instead of executing them (used by the agent integration
/// test to verify the full pipeline without touching a real desktop).
#[derive(Debug, Default)]
pub struct RecordingInjector {
    pub events: Vec<String>,
}

impl RecordingInjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputInjector for RecordingInjector {
    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError> {
        self.events.push(format!("move {x} {y}"));
        Ok(())
    }

    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        b: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError> {
        self.events.push(format!("button {x} {y} {b:?} {down}"));
        Ok(())
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.events.push(format!("wheel {dx} {dy}"));
        Ok(())
    }

    fn key(&mut self, k: OpKey, m: u8, down: bool) -> Result<(), InputError> {
        self.events.push(format!("key {k:?} {m} {down}"));
        Ok(())
    }

    fn text(&mut self, t: &str) -> Result<(), InputError> {
        self.events.push(format!("text {t:?}"));
        Ok(())
    }
}

/// Best-effort desktop backend for the current platform.
pub fn platform_injector() -> Box<dyn InputInjector> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        match desktop::DesktopInjector::new() {
            Ok(i) => Box::new(i),
            Err(e) => {
                tracing::warn!(target: "opacusr", "input backend unavailable: {e}; falling back to null injector");
                Box::new(NullInjector)
            }
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Box::new(NullInjector)
    }
}

/// Convenience: does this mods byte contain ctrl?
pub fn has_ctrl(m: u8) -> bool {
    m & mods::CTRL != 0
}
