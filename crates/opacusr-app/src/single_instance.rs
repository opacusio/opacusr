//! Single-instance guard for the Windows GUI (TeamViewer behavior: a
//! second launch brings up the first instance's window instead of
//! spawning a duplicate tray icon / second agent).
//!
//! Implementation: a per-session named mutex («Local\…» — one guard per
//! interactive session, so separate terminal/RDP users each get their
//! own instance) plus an auto-reset event. The first instance keeps both
//! kernel handles open for its lifetime and parks a thread on the
//! event; the second instance sees the existing mutex, signals the
//! event and exits before creating any window. The first instance
//! translates the signal into a show/focus request that `update()`
//! consumes through [`take_show_request`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const MUTEX_NAME: &str = "Local\\OpacusR.App.SingleInstance";
const EVENT_NAME: &str = "Local\\OpacusR.App.ShowWindow";

/// Set by `App::new` so the waiter thread can wake the egui UI without
/// owning the context itself.
static SHOW_CALLBACK: OnceLock<Arc<dyn Fn() + Send + Sync + 'static>> = OnceLock::new();
/// Latched signal consumed by the first instance's `update()`.
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Try to become the only OpacusR GUI instance. Returns `true` when
/// this process owns the guard (proceed with startup) and `false` when
/// another instance already runs (the caller should signal
/// [`signal_show`] and exit without opening a window).
pub fn acquire() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW};

    let mutex_name: Vec<u16> = MUTEX_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
    if handle == 0 {
        // Cannot even create the guard (never observed in practice):
        // fail open — a working app beats a strict guard.
        tracing::warn!(target: "opacusr", "single-instance mutex unavailable: {}", unsafe {
            GetLastError()
        });
        return true;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Keep the object balance clean in this short-lived process.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return false;
    }

    // First instance: hold the mutex (never closed — the kernel object
    // lives until process exit, which is exactly the guard we want) and
    // park a waiter on the auto-reset show event.
    let event_name: Vec<u16> = EVENT_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    if event == 0 {
        // The guard still works (duplicates exit); only the «activate the
        // running window» nicety is missing.
        tracing::warn!(target: "opacusr", "single-instance show event unavailable");
        return true;
    }
    std::thread::Builder::new()
        .name("opacusr-show-watcher".into())
        .spawn(move || {
            loop {
                let r = unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(event, 0xFFFF_FFFF)
                };
                if r != windows_sys::Win32::Foundation::WAIT_OBJECT_0 {
                    // WAIT_FAILED (handle closed at process teardown) —
                    // stop busy-looping on it.
                    std::thread::sleep(Duration::from_secs(1));
                }
                SHOW_REQUESTED.store(true, Ordering::SeqCst);
                if let Some(f) = SHOW_CALLBACK.get() {
                    f();
                }
            }
        })
        .ok();
    true
}

/// Second instance: ask the first one to show and focus its window.
/// Retries briefly — the first instance may still be starting up and
/// has not created the event yet.
pub fn signal_show() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};

    let name: Vec<u16> = EVENT_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    for _ in 0..10 {
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if event != 0 {
            unsafe {
                SetEvent(event);
                CloseHandle(event);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Install the UI wakeup hook (called once from `App::new`).
pub fn set_show_callback(f: Arc<dyn Fn() + Send + Sync + 'static>) {
    let _ = SHOW_CALLBACK.set(f);
}

/// Consume a pending «show me» request from a second launch. Returns
/// `true` exactly once per request.
pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}
