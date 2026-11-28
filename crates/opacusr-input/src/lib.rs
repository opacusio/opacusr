//! OpacusR input injection.
//!
//! Desktop backends use [`enigo`] (X11 / Windows / macOS); native Wayland
//! sessions use the kernel [`uinput`] virtual devices instead (enigo's
//! XTest path needs an X server, which a pure Wayland session may not
//! offer). Mobile targets and tests use the stub / mock implementations.
//! The host scales incoming coordinates from the *viewer's* view of the
//! remote screen to its own real screen size before calling
//! [`InputInjector`] — see `opacusr-agent::session::InputExecutor`.

use opacusr_proto::message::{mods, MouseButtonId, OpKey, OutputMapping};
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
    /// Report the host's real capture viewport (device px). Absolute-pointer
    /// backends (Wayland uinput) normalize coordinates against it; the rest
    /// inject in the given space directly and ignore it.
    fn set_viewport(&mut self, _w: u32, _h: u32) {}
    /// Report where the streamed output sits inside the compositor's
    /// desktop (native Wayland multi-monitor: the compositor maps an
    /// absolute pointer over the WHOLE desktop bounding box, so frame
    /// coordinates need the per-output translation). `None` = no layout
    /// info — backends keep the single-monitor mapping; every non-uinput
    /// backend ignores it (default no-op).
    fn set_output_mapping(&mut self, _mapping: Option<OutputMapping>) {}
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

/// Linux-native Wayland input injection (virtual evdev devices).
#[cfg(target_os = "linux")]
mod uinput;
#[cfg(target_os = "linux")]
pub use uinput::{is_wayland_session, UinputInjector};

/// Android host remote input through the accessibility shim
/// (InputService + static JNI bridge). Compiles everywhere; set_context
/// is only called from android_main.
pub mod android_bridge;
pub use android_bridge::{input_service_ready, keyboard_service_ready, AndroidInjector};

pub mod clipboard;

/// TeamViewer-style physical input lock (Windows LL hooks / Linux evdev
/// grabs). Engaged by full-control sessions when `lock_input` is set.
/// Desktop-only: a phone IS the physical input device — nothing to lock.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod lock;

/// Privacy mode v1: put the host's physical displays to sleep while a
/// session is active (`privacy_mode`); capture keeps running.
pub mod privacy;

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
    fn set_viewport(&mut self, w: u32, h: u32) {
        self.events.push(format!("viewport {w} {h}"));
    }
    fn set_output_mapping(&mut self, m: Option<OutputMapping>) {
        self.events.push(format!("mapping {m:?}"));
    }
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
        // Native Wayland session: uinput first (enigo needs an X server).
        // Failure falls through to enigo/XWayland, then the null injector.
        #[cfg(target_os = "linux")]
        if uinput::is_wayland_session() {
            match uinput::UinputInjector::new() {
                Ok(i) => {
                    tracing::info!(target: "opacusr", "native Wayland session: remote input via uinput virtual devices");
                    return Box::new(i);
                }
                Err(e) => {
                    tracing::warn!(target: "opacusr", "uinput unavailable: {e}; falling back to X11/XWayland input")
                }
            }
        }
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
        // Android host: remote input through the accessibility shim.
        // Without the service enabled (or on iOS) the session honestly
        // stays view-only.
        #[cfg(target_os = "android")]
        match android_bridge::AndroidInjector::new() {
            Ok(i) => {
                tracing::info!(target: "opacusr", "Android host: remote input via accessibility service");
                return Box::new(i);
            }
            Err(e) => {
                tracing::warn!(target: "opacusr", "Android remote input unavailable: {e}; session runs view-only")
            }
        }
        Box::new(NullInjector)
    }
}

/// Convenience: does this mods byte contain ctrl?
pub fn has_ctrl(m: u8) -> bool {
    m & mods::CTRL != 0
}
