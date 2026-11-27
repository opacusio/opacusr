//! Android remote input via the AccessibilityService shim
//! (`io.opacusr.app.InputService`, classes.dex — see
//! `crates/opacusr-android/java/`). The user enables the service once
//! in system settings; the native agent then drives it through the
//! static Java bridge: pointer drags as continued gesture strokes,
//! right click as long press, wheel as short swipes, Escape as Back,
//! text as clipboard + paste.
//!
//! Raw JNI vtable discipline — same as `opacusr-app::android_sysui`
//! and `opacusr_capture::android_projection`: no jni crate, every call
//! exception-checked, every failure degrades to an error/log line.
//! The bridge compiles on ALL targets so the desktop build type-checks
//! the wiring; `set_context` is only ever called from android_main.
//!
//! Touch reality vs the protocol (documented in platforms.md):
//! - hover `mouse_move` without a held button is a no-op (no hover on
//!   a touch screen);
//! - `mouse_button(Left, down)` starts a drag stroke (Android 8+; on
//!   older devices a plain tap fallback), `mouse_move` continues it,
//!   `mouse_button(Left, up)` ends it;
//! - `mouse_button(Right, …)` is a long press at the point;
//! - `wheel` becomes a swipe (protocol positive dy = wheel UP, so the
//!   finger swipes DOWN to reveal content above — mirrors uinput.rs);
//! - `key(Escape, …)` is GLOBAL_ACTION_BACK, every other key is
//!   `InputError::UnsupportedKey`;
//! - `text` clobbers the clipboard and pastes at the caret (IME
//!   service is the v2 fix).

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use opacusr_proto::message::{MouseButtonId, OpKey};

use crate::{InputError, InputInjector};

// ---------------------------------------------------------------------
// JNI context: built once from android_main — the InputService class is
// loaded through the ACTIVITY's class loader there (FindClass cannot
// see APK classes from a native thread) and pinned with a global ref.
// Method IDs are resolved in the same visit; the finished record is
// published atomically, so a failed resolution leaves NO context and
// the injector reports Unsupported honestly.
// ---------------------------------------------------------------------

#[derive(Debug)]
struct BridgeIds {
    vm: usize,
    class: usize, // GLOBAL ref to io.opacusr.app.InputService
    ready: jni_sys::jmethodID,
    drag_start: jni_sys::jmethodID,
    drag_move: jni_sys::jmethodID,
    drag_end: jni_sys::jmethodID,
    tap: jni_sys::jmethodID,
    long_press: jni_sys::jmethodID,
    scroll: jni_sys::jmethodID,
    go_back: jni_sys::jmethodID,
    paste: jni_sys::jmethodID,
}

// SAFETY: jmethodID / global-ref handles are process-global JNI values
// valid from any attached thread; the record is written once (OnceLock)
// and never mutated afterwards.
unsafe impl Send for BridgeIds {}
unsafe impl Sync for BridgeIds {}

static CONTEXT: OnceLock<BridgeIds> = OnceLock::new();

/// Wire the bridge (addresses, not refs: they live for the whole
/// process, exactly like the multicast lock / projection bridge).
pub fn set_context(vm: usize, _activity: usize, bridge_class_global: usize) {
    if CONTEXT.get().is_some() {
        return;
    }
    match resolve_ids(vm, bridge_class_global) {
        Ok(ids) => {
            let _ = CONTEXT.set(ids);
            tracing::info!(target: "opacusr", "input bridge ready (accessibility remote input available)");
        }
        Err(e) => tracing::warn!(
            target: "opacusr",
            "input bridge: method resolution failed ({e}); remote input stays disabled"
        ),
    }
}

fn resolve_ids(vm: usize, class_addr: usize) -> Result<BridgeIds, InputError> {
    let thread = unsafe { ThreadEnv::acquire(vm)? };
    let env = thread.env;
    let iface = unsafe { *env };
    let class = class_addr as jni_sys::jclass;

    let get = |name: &core::ffi::CStr,
               sig: &core::ffi::CStr|
     -> Result<jni_sys::jmethodID, InputError> {
        let mid =
            unsafe { ((*iface).v1_6.GetStaticMethodID)(env, class, name.as_ptr(), sig.as_ptr()) };
        if mid.is_null() {
            unsafe { clear_exception(env) };
            return Err(InputError::Backend(format!(
                "input bridge: missing method {name:?}"
            )));
        }
        Ok(mid)
    };

    Ok(BridgeIds {
        vm,
        class: class_addr,
        ready: get(c"ready", c"()Z")?,
        drag_start: get(c"dragStart", c"(FF)Z")?,
        drag_move: get(c"dragMove", c"(FF)Z")?,
        drag_end: get(c"dragEnd", c"()Z")?,
        tap: get(c"tap", c"(FF)Z")?,
        long_press: get(c"longPress", c"(FF)Z")?,
        scroll: get(c"scroll", c"(FFFF)Z")?,
        go_back: get(c"goBack", c"()Z")?,
        paste: get(c"paste", c"(Ljava/lang/String;)Z")?,
    })
}

// ---------------------------------------------------------------------
// Thread attach helper: GetEnv first (works for already-attached
// threads, INCLUDING android_main's — those are never detached),
// attach + detach otherwise. Detach uses the stored VM handle, not the
// global context, so it is correct during set_context itself.
// ---------------------------------------------------------------------

struct ThreadEnv {
    env: *mut jni_sys::JNIEnv,
    vm_handle: usize,
    detach: bool,
}

impl ThreadEnv {
    unsafe fn acquire(vm_addr: usize) -> Result<Self, InputError> {
        let vm_handle = vm_addr as *mut jni_sys::JavaVM;
        let vm: jni_sys::JavaVM = *(vm_handle as *mut jni_sys::JavaVM);
        if vm.is_null() {
            return Err(InputError::Unsupported);
        }
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_2.GetEnv)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            jni_sys::JNI_VERSION_1_6,
        );
        if res == jni_sys::JNI_OK && !env.is_null() {
            return Ok(ThreadEnv {
                env,
                vm_handle: vm_addr,
                detach: false,
            });
        }
        let res = ((*vm).v1_1.AttachCurrentThread)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            return Err(InputError::Backend("input bridge: attach failed".into()));
        }
        Ok(ThreadEnv {
            env,
            vm_handle: vm_addr,
            detach: true,
        })
    }
}

impl Drop for ThreadEnv {
    fn drop(&mut self) {
        if self.detach {
            unsafe {
                let vm_handle = self.vm_handle as *mut jni_sys::JavaVM;
                let vm: jni_sys::JavaVM = *(vm_handle as *mut jni_sys::JavaVM);
                if !vm.is_null() {
                    ((*vm).v1_1.DetachCurrentThread)(vm_handle);
                }
            }
        }
    }
}

unsafe fn clear_exception(env: *mut jni_sys::JNIEnv) {
    let iface = *env;
    if ((*iface).v1_6.ExceptionCheck)(env) {
        ((*iface).v1_6.ExceptionClear)(env);
    }
}

// ---------------------------------------------------------------------
// Static bridge calls (boolean-returning: primitives + one string).
// Every helper re-attaches for the duration of the call; UI-thread
// calls ride GetEnv (no detach).
// ---------------------------------------------------------------------

fn call_bool(
    name: &str,
    mid: jni_sys::jmethodID,
    args: *mut jni_sys::jvalue,
) -> Result<bool, InputError> {
    let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
    if mid.is_null() {
        return Err(InputError::Unsupported);
    }
    let thread = unsafe { ThreadEnv::acquire(ctx.vm)? };
    let env = thread.env;
    let iface = unsafe { *env };
    let r = unsafe {
        ((*iface).v1_6.CallStaticBooleanMethodA)(env, ctx.class as jni_sys::jclass, mid, args)
    };
    if unsafe { ((*iface).v1_6.ExceptionCheck)(env) } {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend(format!("input bridge: {name} threw")));
    }
    Ok(r)
}

fn call_bool_f(name: &str, mid: jni_sys::jmethodID, a: f32, b: f32) -> Result<bool, InputError> {
    let mut args = [jni_sys::jvalue { f: a }, jni_sys::jvalue { f: b }];
    call_bool(name, mid, args.as_mut_ptr())
}

fn call_bool_f4(
    name: &str,
    mid: jni_sys::jmethodID,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
) -> Result<bool, InputError> {
    let mut args = [
        jni_sys::jvalue { f: a },
        jni_sys::jvalue { f: b },
        jni_sys::jvalue { f: c },
        jni_sys::jvalue { f: d },
    ];
    call_bool(name, mid, args.as_mut_ptr())
}

/// `paste(String)` — builds a real UTF-16 jstring (NewStringUTF is
/// MODIFIED UTF-8 and would mangle emoji and NUL).
fn call_paste(mid: jni_sys::jmethodID, text: &str) -> Result<bool, InputError> {
    let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
    if mid.is_null() {
        return Err(InputError::Unsupported);
    }
    let thread = unsafe { ThreadEnv::acquire(ctx.vm)? };
    let env = thread.env;
    let iface = unsafe { *env };
    let units: Vec<u16> = text.encode_utf16().collect();
    let jstr =
        unsafe { ((*iface).v1_6.NewString)(env, units.as_ptr(), units.len() as jni_sys::jsize) };
    if jstr.is_null() {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend("input bridge: NewString failed".into()));
    }
    let mut args = [jni_sys::jvalue {
        l: jstr as jni_sys::jobject,
    }];
    let r = unsafe {
        ((*iface).v1_6.CallStaticBooleanMethodA)(
            env,
            ctx.class as jni_sys::jclass,
            mid,
            args.as_mut_ptr(),
        )
    };
    unsafe { ((*iface).v1_6.DeleteLocalRef)(env, jstr as jni_sys::jobject) };
    if unsafe { ((*iface).v1_6.ExceptionCheck)(env) } {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend("input bridge: paste threw".into()));
    }
    Ok(r)
}

// ---------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------

/// Is the remote-input accessibility service enabled right now? Polled
/// by the app UI (consent dialog hints) with a short cache TTL — the
/// JNI round-trip must not run per frame.
static READY_CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
const READY_TTL: Duration = Duration::from_secs(2);

pub fn input_service_ready() -> bool {
    let ctx = match CONTEXT.get() {
        Some(c) => c,
        None => return false,
    };
    if let Ok(mut guard) = READY_CACHE.lock() {
        if let Some((at, v)) = *guard {
            if at.elapsed() < READY_TTL {
                return v;
            }
        }
        let v = call_bool("ready", ctx.ready, std::ptr::null_mut()).unwrap_or(false);
        *guard = Some((Instant::now(), v));
        return v;
    }
    false
}

// ---------------------------------------------------------------------
// The injector
// ---------------------------------------------------------------------

/// Which bridge call a protocol key maps to (pure, unit-tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Esc → GLOBAL_ACTION_BACK.
    Back,
}

fn key_action(key: OpKey) -> Option<KeyAction> {
    match key {
        OpKey::Escape => Some(KeyAction::Back),
        _ => None,
    }
}

/// Accessibility-based injector for the Android host. Coordinates
/// arrive already scaled to the phone's real screen px (session's
/// InputExecutor), which is exactly the space dispatchGesture wants.
#[derive(Debug)]
pub struct AndroidInjector {
    /// Real capture viewport (device px) — the wheel anchor until the
    /// first pointer event carries coordinates.
    viewport: (u32, u32),
    last_pos: (f32, f32),
    /// Left button held → moves continue the drag stroke.
    left_down: bool,
    /// Android < 8 fallback: the down event dispatched as a plain tap.
    tap_fallback: bool,
    /// The drag chain broke (stroke cancelled / dispatch too slow);
    /// stop continuing until the button is released.
    chain_broken: bool,
}

impl AndroidInjector {
    pub fn new() -> Result<Self, InputError> {
        let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
        if !call_bool("ready", ctx.ready, std::ptr::null_mut())? {
            return Err(InputError::Backend(
                "accessibility service not enabled (Настройки → Специальные возможности → OpacusR)"
                    .into(),
            ));
        }
        Ok(Self {
            viewport: (1920, 1080),
            last_pos: (960.0, 540.0),
            left_down: false,
            tap_fallback: false,
            chain_broken: false,
        })
    }
}

impl InputInjector for AndroidInjector {
    fn set_viewport(&mut self, w: u32, h: u32) {
        self.viewport = (w, h);
        self.last_pos = (w as f32 / 2.0, h as f32 / 2.0);
    }

    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError> {
        self.last_pos = (x as f32, y as f32);
        // Touch screens have no hover: only continue a running drag.
        if !self.left_down || self.chain_broken {
            return Ok(());
        }
        let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
        match call_bool_f("dragMove", ctx.drag_move, x as f32, y as f32) {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.chain_broken = true;
                tracing::warn!(target: "opacusr", "input bridge: drag chain broken (stroke cancelled?); moves dropped until release");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        button: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError> {
        self.last_pos = (x as f32, y as f32);
        match button {
            MouseButtonId::Left => {
                if down {
                    let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
                    match call_bool_f("dragStart", ctx.drag_start, x as f32, y as f32) {
                        Ok(true) => {
                            self.left_down = true;
                            self.chain_broken = false;
                            Ok(())
                        }
                        Ok(false) => {
                            // Android < 8 (no stroke continuation): a
                            // plain tap still lands single clicks.
                            if call_bool_f("tap", ctx.tap, x as f32, y as f32).unwrap_or(false) {
                                self.tap_fallback = true;
                                Ok(())
                            } else {
                                Err(InputError::Backend("input bridge: gesture rejected".into()))
                            }
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
                    if self.left_down {
                        self.left_down = false;
                        if self.chain_broken {
                            self.chain_broken = false;
                            return Ok(()); // chain dead: nothing to end
                        }
                        let r = call_bool("dragEnd", ctx.drag_end, std::ptr::null_mut())?;
                        if !r {
                            tracing::warn!(target: "opacusr", "input bridge: dragEnd rejected");
                        }
                    }
                    self.tap_fallback = false;
                    Ok(())
                }
            }
            MouseButtonId::Right => {
                if !down {
                    return Ok(()); // the long press gesture already ran
                }
                let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
                match call_bool_f("longPress", ctx.long_press, x as f32, y as f32) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(InputError::Backend(
                        "input bridge: long press rejected (drag running?)".into(),
                    )),
                    Err(e) => Err(e),
                }
            }
            // No middle-button equivalent on a touch screen.
            MouseButtonId::Middle => Ok(()),
        }
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        if dx == 0 && dy == 0 {
            return Ok(());
        }
        let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
        let (x, y) = self.last_pos;
        match call_bool_f4("scroll", ctx.scroll, x, y, dx as f32, dy as f32) {
            Ok(true) => Ok(()),
            Ok(false) => Err(InputError::Backend(
                "input bridge: scroll rejected (drag running?)".into(),
            )),
            Err(e) => Err(e),
        }
    }

    fn key(&mut self, key: OpKey, _m: u8, down: bool) -> Result<(), InputError> {
        match key_action(key) {
            None => Err(InputError::UnsupportedKey(key)),
            Some(KeyAction::Back) => {
                if !down {
                    return Ok(());
                }
                let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
                match call_bool("goBack", ctx.go_back, std::ptr::null_mut()) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(InputError::Backend("input bridge: Back rejected".into())),
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn text(&mut self, text: &str) -> Result<(), InputError> {
        if text.is_empty() {
            return Ok(());
        }
        let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
        match call_paste(ctx.paste, text) {
            Ok(true) => Ok(()),
            Ok(false) => Err(InputError::Backend(
                "input bridge: paste failed (no editable focus?)".into(),
            )),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct() -> AndroidInjector {
        AndroidInjector {
            viewport: (1080, 2400),
            last_pos: (540.0, 1200.0),
            left_down: false,
            tap_fallback: false,
            chain_broken: false,
        }
    }

    #[test]
    fn key_mapping_escape_is_back_everything_else_unsupported() {
        assert_eq!(key_action(OpKey::Escape), Some(KeyAction::Back));
        for k in [
            OpKey::Enter,
            OpKey::Tab,
            OpKey::Backspace,
            OpKey::ArrowUp,
            OpKey::Space,
            OpKey::F5,
            OpKey::CapsLock,
            OpKey::Char('x'),
            OpKey::ContextMenu,
        ] {
            assert_eq!(key_action(k), None, "{k:?} must stay unsupported");
        }
    }

    #[test]
    fn without_bridge_everything_reports_unsupported() {
        // No set_context() in tests: JNI is never touched, the injector
        // must fail closed with honest errors.
        assert!(matches!(
            AndroidInjector::new(),
            Err(InputError::Unsupported)
        ));
        let mut inj = direct();
        assert!(matches!(
            inj.mouse_button(10, 20, MouseButtonId::Left, true),
            Err(InputError::Unsupported)
        ));
        assert!(matches!(
            inj.mouse_button(10, 20, MouseButtonId::Right, true),
            Err(InputError::Unsupported)
        ));
        assert!(matches!(inj.wheel(0, 3), Err(InputError::Unsupported)));
        assert!(matches!(
            inj.key(OpKey::Escape, 0, true),
            Err(InputError::Unsupported)
        ));
        assert!(matches!(inj.text("привет"), Err(InputError::Unsupported)));
        assert!(!input_service_ready());
    }

    #[test]
    fn hover_moves_are_noops_even_without_bridge() {
        let mut inj = direct();
        // No button held → no hover on touch → Ok without any JNI call.
        assert!(inj.mouse_move(500, 700).is_ok());
        assert_eq!(inj.last_pos, (500.0, 700.0));
    }

    #[test]
    fn empty_text_is_a_noop() {
        let mut inj = direct();
        assert!(inj.text("").is_ok());
        assert!(inj.wheel(0, 0).is_ok());
    }
}
