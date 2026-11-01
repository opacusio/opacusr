//! Host-side physical input lock ("TeamViewer-style").
//!
//! While a viewer has **full control**, the host's own keyboard and mouse
//! are swallowed so a person at the machine cannot fight the remote user.
//! Input **injected through the OpacusR session still passes**.
//!
//! Platform notes:
//! * **Windows** — low-level (`WH_KEYBOARD_LL` / `WH_MOUSE_LL`) hooks on a
//!   dedicated message-pump thread. Hook callbacks drop every event whose
//!   `LLKHF_INJECTED` / `LLMHF_INJECTED` flag is clear (i.e. physical
//!   input) and let injected events (our own `SendInput`) through.
//!   **Ctrl+Alt+Del stays available**: the secure attention sequence is
//!   handled by winlogon and cannot be intercepted from user mode.
//! * **Linux** — `EVIOCGRAB` on every readable `/dev/input/event*` node.
//!   Grabs live at the kernel level, so physical devices stop delivering
//!   events to X11/Wayland entirely, while XTest-injected input (enigo's
//!   path) is unaffected. Requires read access to the input devices
//!   (membership in the `input` group, or root).
//! * **macOS** — not implemented yet (a CGEventTap-based design needs
//!   source-process filtering so injected events pass; future work).
//!
//! Safety properties:
//! * **view-only sessions never lock** (enforced by the caller);
//! * **crash-safe** — the guard's `Drop` releases the grabs, and the OS
//!   releases all hooks/grabs automatically if the process dies;
//! * **fail-open** — if the lock cannot be taken (permissions, headless
//!   session, unsupported backend) the caller receives an error and the
//!   session continues unlocked with a warning, never stranded.

use crate::InputError;

/// True while at least one [`HostInputLock`] is engaged.
static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    const WH_KEYBOARD_LL: i32 = 13;
    const WH_MOUSE_LL: i32 = 14;
    const LLKHF_INJECTED: u32 = 0x10;
    const LLMHF_INJECTED: u32 = 0x01;
    const WM_QUIT: u32 = 0x0012;

    #[repr(C)]
    struct KbdLlHook {
        _vk: u32,
        _scan: u32,
        flags: u32,
        _time: u32,
        _extra: usize,
    }

    #[repr(C)]
    struct MouseLlHook {
        _pt: [i32; 2],
        _data: u32,
        flags: u32,
        _time: u32,
        _extra: usize,
    }

    #[repr(C)]
    struct Msg {
        _hwnd: isize,
        _message: u32,
        _w: usize,
        _l: isize,
        _time: u32,
        _pt: [i32; 2],
    }

    unsafe extern "system" {
        fn SetWindowsHookExW(
            id: i32,
            proc_: unsafe extern "system" fn(i32, usize, isize) -> isize,
            mod_: isize,
            tid: u32,
        ) -> isize;
        fn UnhookWindowsHookEx(handle: isize) -> i32;
        fn CallNextHookEx(handle: isize, code: i32, w: usize, l: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        fn PostThreadMessageW(tid: u32, msg: u32, w: usize, l: isize) -> i32;
        fn GetCurrentThreadId() -> u32;
    }

    /// Thread id of the hook-pump thread; `Some` while hooks are installed.
    static PUMP_TID: Mutex<Option<u32>> = Mutex::new(None);

    unsafe extern "system" fn keyboard_hook(code: i32, w: usize, l: isize) -> isize {
        if super::ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
            && code >= 0
            && l != 0
            && (*(l as *const KbdLlHook)).flags & LLKHF_INJECTED == 0
        {
            return 1; // swallow a physical key
        }
        CallNextHookEx(0, code, w, l)
    }

    unsafe extern "system" fn mouse_hook(code: i32, w: usize, l: isize) -> isize {
        if super::ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
            && code >= 0
            && l != 0
            && (*(l as *const MouseLlHook)).flags & LLMHF_INJECTED == 0
        {
            return 1; // swallow a physical click/move/wheel
        }
        CallNextHookEx(0, code, w, l)
    }

    pub struct Guard;

    /// Install both LL hooks and run the pump on a dedicated thread.
    /// Low-level hooks may be installed with a NULL module handle.
    pub fn acquire() -> Result<Guard, String> {
        let mut slot = PUMP_TID.lock().unwrap();
        if slot.is_some() {
            return Err("input lock is already engaged".into());
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("opacusr-input-lock".into())
            .spawn(move || unsafe {
                let hkb = SetWindowsHookExW(WH_KEYBOARD_LL, keyboard_hook, 0, 0);
                let hms = SetWindowsHookExW(WH_MOUSE_LL, mouse_hook, 0, 0);
                if hkb == 0 || hms == 0 {
                    if hkb != 0 {
                        UnhookWindowsHookEx(hkb);
                    }
                    if hms != 0 {
                        UnhookWindowsHookEx(hms);
                    }
                    let _ = tx.send(Err(
                        "SetWindowsHookExW failed (an interactive desktop is required)".into(),
                    ));
                    return;
                }
                let _ = tx.send(Ok(GetCurrentThreadId()));
                // Pump messages until WM_QUIT; hooks only fire while this
                // thread pumps. After the pump exits the hooks are gone.
                let mut msg = std::mem::zeroed::<Msg>();
                while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
                super::ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
                UnhookWindowsHookEx(hkb);
                UnhookWindowsHookEx(hms);
            })
            .map_err(|e| format!("hook thread: {e}"))?;

        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(tid)) => {
                *slot = Some(tid);
                Ok(Guard)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err("hook thread did not report back in time".into()),
        }
    }

    pub fn release() {
        super::ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
        let tid = PUMP_TID.lock().unwrap().take();
        if let Some(tid) = tid {
            unsafe {
                PostThreadMessageW(tid, WM_QUIT, 0, 0);
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;

    /// `EVIOCGRAB` = `_IOW('E', 0x90, int)`.
    const EVIOCGRAB: libc::c_ulong = 0x4004_4590;

    pub struct Guard {
        /// Grabbed evdev nodes; dropped fds release their grabs.
        _grabbed: Vec<File>,
    }

    pub fn acquire() -> Result<Guard, String> {
        let mut grabbed = Vec::new();
        let dir = std::fs::read_dir("/dev/input")
            .map_err(|e| format!("/dev/input: {e} (Wayland/X11 hosts need the `input` group)"))?;
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("event") {
                continue;
            }
            if let Ok(file) = OpenOptions::new().read(true).open(entry.path()) {
                let fd = file.as_raw_fd();
                let ok = unsafe { libc::ioctl(fd, EVIOCGRAB, 1_i32) } == 0;
                if ok {
                    grabbed.push(file); // keep open; grab lives with the fd
                }
            }
        }
        if grabbed.is_empty() {
            return Err(
                "no /dev/input/event* could be grabbed (add the user to the `input` group)".into(),
            );
        }
        Ok(Guard { _grabbed: grabbed })
    }

    pub fn release() {
        // No global state: guards drop per-session.
    }

    // `File` fds are closed (and thus ungrabbed) by Drop — closing an
    // evdev fd releases its grab automatically.
}

#[cfg(target_os = "macos")]
mod imp {
    pub struct Guard;

    pub fn acquire() -> Result<Guard, String> {
        Err("input lock is not implemented on macOS yet (see docs/platforms.md)".into())
    }

    pub fn release() {}
}

/// Locks the host's physical keyboard/mouse for the lifetime of the guard.
/// Obtain via [`HostInputLock::enable`]; dropping the guard (or the
/// process) releases the lock.
pub struct HostInputLock {
    _guard: imp::Guard,
}

impl HostInputLock {
    /// Engage the lock. Fails (fail-open) when the platform cannot take
    /// it — the caller should warn and continue with the session.
    pub fn enable() -> Result<Self, InputError> {
        let guard = imp::acquire().map_err(InputError::Backend)?;
        ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(target: "opacusr", "physical input lock engaged");
        Ok(Self { _guard: guard })
    }

    /// Whether the lock is currently engaged.
    pub fn is_active() -> bool {
        ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for HostInputLock {
    fn drop(&mut self) {
        imp::release();
        ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(target: "opacusr", "physical input lock released");
    }
}
