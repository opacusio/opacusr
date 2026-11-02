//! Host-side display blanking — privacy mode v1 ("screen-off while
//! someone watches over your shoulder").
//!
//! While a session is active with [`DisplayBlank`] engaged, the host's
//! physical displays are put to sleep: a bystander sees a dark monitor
//! instead of the shared desktop. The capture pipeline keeps running —
//! the remote viewer continues to see the desktop — and any input lock
//! (`lock_input`) keeps working independently.
//!
//! Platform notes:
//! * **Windows** — `SendMessageW(HWND_BROADCAST, WM_SYSCOMMAND,
//!   SC_MONITORPOWER, 2)` powers the attached monitors down; the guard
//!   sends `-1` (on) on drop. Requires no privileges; user activity
//!   (key/mouse) also wakes the display — combine with `lock_input` so
//!   no one can wake it by touching the machine.
//! * **Linux (X11)** — `xset dpms force off`; the guard runs
//!   `xset dpms force on` on drop. Wayland compositors do not expose
//!   DPMS through a portable CLI — the call fails and the session
//!   continues unblanked (fail-open, documented v1 limitation).
//! * **macOS** — `pmset displaysleepnow`; the guard runs
//!   `caffeinate -u -t 2` (user-activity assertion) on drop to wake.
//!
//! Safety properties mirror [`crate::lock::HostInputLock`]:
//! * **fail-open** — if blanking cannot be engaged (no xset, headless
//!   session, unsupported backend) the caller receives an error and the
//!   session continues unblanked with a warning, never stranded;
//! * **crash-safe** — if the process dies the display wakes on the next
//!   user activity on every platform (DPMS is display-side state, the
//!   OS wakes it on input by default).

use crate::InputError;

/// Number of engaged [`DisplayBlank`] guards. Blanking is engaged when
/// the first guard appears, wake happens when the last one drops (two
/// concurrent sessions must not wake each other's display).
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII guard: displays are blanked while alive, woken on drop.
pub struct DisplayBlank {
    _priv: (),
}

impl DisplayBlank {
    /// Put the physical displays to sleep. Returns the guard; when the
    /// last guard is dropped the displays are woken again.
    pub fn enable() -> Result<DisplayBlank, InputError> {
        if ACTIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            if let Err(e) = blank() {
                ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return Err(e);
            }
        }
        Ok(DisplayBlank { _priv: () })
    }

    /// True while at least one guard is engaged.
    pub fn is_active() -> bool {
        ACTIVE.load(std::sync::atomic::Ordering::SeqCst) > 0
    }
}

impl Drop for DisplayBlank {
    fn drop(&mut self) {
        if ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            wake();
        }
    }
}

// ---------------------------------------------------------------------
// Platform implementations.
// ---------------------------------------------------------------------

#[cfg(unix)]
/// Command to engage blanking: `(program, args)`.
pub(crate) fn blank_command() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "macos") {
        ("pmset", vec!["displaysleepnow"])
    } else {
        ("xset", vec!["dpms", "force", "off"])
    }
}

#[cfg(unix)]
/// Command to wake the displays: `(program, args)`.
pub(crate) fn wake_command() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "macos") {
        ("caffeinate", vec!["-u", "-t", "2"])
    } else {
        ("xset", vec!["dpms", "force", "on"])
    }
}

#[cfg(unix)]
fn run_command(
    (program, args): (&'static str, Vec<&'static str>),
    what: &str,
) -> Result<(), InputError> {
    let out = std::process::Command::new(program)
        .args(&args)
        .output()
        .map_err(|e| InputError::Backend(format!("{what}: cannot run {program}: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let err = if err.is_empty() {
            "command failed"
        } else {
            err
        };
        return Err(InputError::Backend(format!("{what}: {err}")));
    }
    Ok(())
}

#[cfg(unix)]
fn blank() -> Result<(), InputError> {
    run_command(blank_command(), "display blank")
}

#[cfg(unix)]
fn wake() {
    // Best effort: a failed wake is not worth a panic in Drop.
    let _ = run_command(wake_command(), "display wake");
}

#[cfg(windows)]
mod imp {
    // Minimal user32 surface — opacusr-input deliberately carries no
    // windows-sys dependency; same approach as the low-level hooks in
    // `lock.rs`.
    const HWND_BROADCAST: usize = 0xFFFF;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_MONITORPOWER: usize = 0xF170;
    const MONITOR_OFF: isize = 2;
    const MONITOR_ON: isize = -1;

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageW(hwnd: usize, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    pub(super) fn blank() -> Result<(), crate::InputError> {
        // A broadcast send never fails meaningfully for monitor power
        // (no return value contract) — treat the call itself as success.
        unsafe {
            SendMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER, MONITOR_OFF);
        }
        Ok(())
    }

    pub(super) fn wake() {
        unsafe {
            SendMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER, MONITOR_ON);
        }
    }
}

#[cfg(windows)]
fn blank() -> Result<(), crate::InputError> {
    imp::blank()
}

#[cfg(windows)]
fn wake() {
    imp::wake()
}

// ---------------------------------------------------------------------
// Tests (unix-only command shaping; runs on the Linux/macOS CI jobs).
// ---------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn blank_and_wake_commands_per_os() {
        if cfg!(target_os = "macos") {
            assert_eq!(blank_command(), ("pmset", vec!["displaysleepnow"]));
            assert_eq!(wake_command(), ("caffeinate", vec!["-u", "-t", "2"]));
        } else {
            assert_eq!(blank_command(), ("xset", vec!["dpms", "force", "off"]));
            assert_eq!(wake_command(), ("xset", vec!["dpms", "force", "on"]));
        }
    }

    #[test]
    fn blank_is_inactive_before_use() {
        // The static starts false on every fresh process; guards are
        // never engaged in unit tests (that would sleep the CI display).
        assert!(!DisplayBlank::is_active());
    }
}
