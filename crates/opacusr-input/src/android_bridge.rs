//! Android remote input via the AccessibilityService shim
//! (`io.opacusr.app.InputService`, classes.dex — see
//! `crates/opacusr-android/java/`). The user enables the service once
//! in system settings; the native agent then drives it through the
//! static Java bridge: pointer drags as continued gesture strokes,
//! right click as long press, wheel as short swipes, Escape as Back.
//!
//! Keys and text ride the IME keyboard bridge
//! (`io.opacusr.app.KeyboardService`, also classes.dex): a SILENT
//! InputMethodService the user selects once as the current keyboard.
//! It gives the agent a live `InputConnection`, so `text` lands via
//! `commitText` — real Unicode (emoji, CJK) WITHOUT clobbering the
//! clipboard — and keys as `sendKeyEvent` with SHIFT/CTRL/ALT/META
//! state. When the keyboard is not selected (or no editor has focus),
//! `text` falls back to the v1 clipboard + paste path; keys degrade to
//! `InputError::UnsupportedKey` honestly. Escape stays
//! GLOBAL_ACTION_BACK (a system action, editors cannot consume Esc).
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
//! - `key(Escape, …)` is GLOBAL_ACTION_BACK, editing keys ride the IME
//!   bridge, everything else is `InputError::UnsupportedKey`;
//! - `text` prefers IME commitText, falls back to clipboard + paste.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use opacusr_proto::message::{mods, MouseButtonId, OpKey};

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

/// The IME keyboard bridge (`io.opacusr.app.KeyboardService`): a
/// silent InputMethodService selected once as the current keyboard.
/// Resolved independently of the gesture bridge — an old APK or an
/// unselected keyboard degrades to the v1 text path, never breaks the
/// accessibility gestures.
#[derive(Debug)]
struct ImeIds {
    vm: usize,
    class: usize, // GLOBAL ref to io.opacusr.app.KeyboardService
    ready: jni_sys::jmethodID,
    type_text: jni_sys::jmethodID,
    key: jni_sys::jmethodID,
}

// SAFETY: same as BridgeIds — process-global JNI values, written once.
unsafe impl Send for ImeIds {}
unsafe impl Sync for ImeIds {}

static IME: OnceLock<ImeIds> = OnceLock::new();

/// Wire the bridges (addresses, not refs: they live for the whole
/// process, exactly like the multicast lock / projection bridge).
/// `input_class` is the accessibility shim, `ime_class` the silent
/// keyboard shim — either may be absent (old APK): each degrades on
/// its own, gestures and typing are independent.
pub fn set_context(
    vm: usize,
    _activity: usize,
    input_class: Option<usize>,
    ime_class: Option<usize>,
) {
    match input_class {
        Some(addr) if CONTEXT.get().is_none() => match resolve_ids(vm, addr) {
            Ok(ids) => {
                let _ = CONTEXT.set(ids);
                tracing::info!(target: "opacusr", "input bridge ready (accessibility remote input available)");
            }
            Err(e) => tracing::warn!(
                target: "opacusr",
                "input bridge: method resolution failed ({e}); remote input stays disabled"
            ),
        },
        Some(_) => {}
        None => tracing::warn!(
            target: "opacusr",
            "input bridge: no InputService class (old APK?); remote input stays disabled"
        ),
    }
    match ime_class {
        Some(addr) if IME.get().is_none() => match resolve_ime_ids(vm, addr) {
            Ok(ids) => {
                let _ = IME.set(ids);
                tracing::info!(target: "opacusr", "keyboard bridge ready (IME text/keys available)");
            }
            Err(e) => tracing::warn!(
                target: "opacusr",
                "keyboard bridge: method resolution failed ({e}); text falls back to clipboard paste"
            ),
        },
        Some(_) => {}
        None => tracing::warn!(
            target: "opacusr",
            "keyboard bridge: no KeyboardService class (old APK?); text falls back to clipboard paste"
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

fn resolve_ime_ids(vm: usize, class_addr: usize) -> Result<ImeIds, InputError> {
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
                "keyboard bridge: missing method {name:?}"
            )));
        }
        Ok(mid)
    };

    Ok(ImeIds {
        vm,
        class: class_addr,
        ready: get(c"imeReady", c"()Z")?,
        type_text: get(c"imeType", c"(Ljava/lang/String;)Z")?,
        key: get(c"imeKey", c"(II)Z")?,
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

/// Shared static boolean call (both bridges are static-method only:
/// primitives + one string, no state to carry across calls).
fn call_static_bool(
    vm: usize,
    class: usize,
    mid: jni_sys::jmethodID,
    name: &str,
    args: *mut jni_sys::jvalue,
) -> Result<bool, InputError> {
    if mid.is_null() {
        return Err(InputError::Unsupported);
    }
    let thread = unsafe { ThreadEnv::acquire(vm)? };
    let env = thread.env;
    let iface = unsafe { *env };
    let r = unsafe {
        ((*iface).v1_6.CallStaticBooleanMethodA)(env, class as jni_sys::jclass, mid, args)
    };
    if unsafe { ((*iface).v1_6.ExceptionCheck)(env) } {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend(format!("input bridge: {name} threw")));
    }
    Ok(r)
}

fn call_bool(
    name: &str,
    mid: jni_sys::jmethodID,
    args: *mut jni_sys::jvalue,
) -> Result<bool, InputError> {
    let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
    call_static_bool(ctx.vm, ctx.class, mid, name, args)
}

fn call_ime_bool(
    name: &str,
    mid: jni_sys::jmethodID,
    args: *mut jni_sys::jvalue,
) -> Result<bool, InputError> {
    let ctx = IME.get().ok_or(InputError::Unsupported)?;
    call_static_bool(ctx.vm, ctx.class, mid, name, args)
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

/// `String`-taking static bridge call — builds a real UTF-16 jstring
/// (NewStringUTF is MODIFIED UTF-8 and would mangle emoji and NUL).
/// Used by both bridges: `paste` (accessibility) and `imeType` (IME).
fn call_string_bool(
    vm: usize,
    class: usize,
    mid: jni_sys::jmethodID,
    name: &str,
    text: &str,
) -> Result<bool, InputError> {
    if mid.is_null() {
        return Err(InputError::Unsupported);
    }
    let thread = unsafe { ThreadEnv::acquire(vm)? };
    let env = thread.env;
    let iface = unsafe { *env };
    let units: Vec<u16> = text.encode_utf16().collect();
    let jstr =
        unsafe { ((*iface).v1_6.NewString)(env, units.as_ptr(), units.len() as jni_sys::jsize) };
    if jstr.is_null() {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend(format!(
            "input bridge: {name}: NewString failed"
        )));
    }
    let mut args = [jni_sys::jvalue {
        l: jstr as jni_sys::jobject,
    }];
    let r = unsafe {
        ((*iface).v1_6.CallStaticBooleanMethodA)(
            env,
            class as jni_sys::jclass,
            mid,
            args.as_mut_ptr(),
        )
    };
    unsafe { ((*iface).v1_6.DeleteLocalRef)(env, jstr as jni_sys::jobject) };
    if unsafe { ((*iface).v1_6.ExceptionCheck)(env) } {
        unsafe { clear_exception(env) };
        return Err(InputError::Backend(format!("input bridge: {name} threw")));
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
/// Same discipline for the keyboard-bridge probe.
static KEYBOARD_CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
const READY_TTL: Duration = Duration::from_secs(2);

/// Shared TTL cache for the two `ready` probes (the UI polls these per
/// frame — the JNI round-trip must not run per frame).
fn ready_cached(
    cache: &Mutex<Option<(Instant, bool)>>,
    name: &str,
    vm: usize,
    class: usize,
    mid: jni_sys::jmethodID,
) -> bool {
    if let Ok(mut guard) = cache.lock() {
        if let Some((at, v)) = *guard {
            if at.elapsed() < READY_TTL {
                return v;
            }
        }
        let v = call_static_bool(vm, class, mid, name, std::ptr::null_mut()).unwrap_or(false);
        *guard = Some((Instant::now(), v));
        return v;
    }
    false
}

pub fn input_service_ready() -> bool {
    match CONTEXT.get() {
        Some(c) => ready_cached(&READY_CACHE, "ready", c.vm, c.class, c.ready),
        None => false,
    }
}

/// Is the OpacusR keyboard the selected input method right now? Same
/// TTL-cache discipline as `input_service_ready` (consent dialog poll).
pub fn keyboard_service_ready() -> bool {
    match IME.get() {
        Some(c) => ready_cached(&KEYBOARD_CACHE, "imeReady", c.vm, c.class, c.ready),
        None => false,
    }
}

// ---------------------------------------------------------------------
// The injector
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// Key mapping (pure, unit-tested): protocol OpKey + mods byte →
// what the Android reality can do with it.
// ---------------------------------------------------------------------

/// Android KeyEvent meta bits (mirror android.view.KeyEvent — values
/// duplicated here because the bridge speaks raw ints over JNI).
const META_SHIFT_ON: i32 = 0x0000_0001;
const META_ALT_ON: i32 = 0x0000_0010;
const META_CTRL_ON: i32 = 0x0000_1000;
const META_META_ON: i32 = 0x0001_0000;

/// Android keycodes used by the IME path (mirror android.view.KeyEvent).
const K_DPAD_UP: i32 = 19;
const K_DPAD_DOWN: i32 = 20;
const K_DPAD_LEFT: i32 = 21;
const K_DPAD_RIGHT: i32 = 22;
const K_KEYCODE_0: i32 = 7;
const K_KEYCODE_A: i32 = 29;
const K_TAB: i32 = 61;
const K_SPACE: i32 = 62;
const K_ENTER: i32 = 66;
const K_DEL: i32 = 67; // backspace
const K_PAGE_UP: i32 = 92;
const K_PAGE_DOWN: i32 = 93;
const K_FORWARD_DEL: i32 = 112;
const K_MOVE_HOME: i32 = 122;
const K_MOVE_END: i32 = 123;
const K_INSERT: i32 = 124;
const K_F1: i32 = 131; // F12 = 142

/// Protocol mods byte → Android meta-state int.
fn meta_state(m: u8) -> i32 {
    let mut out = 0;
    if m & mods::SHIFT != 0 {
        out |= META_SHIFT_ON;
    }
    if m & mods::CTRL != 0 {
        out |= META_CTRL_ON;
    }
    if m & mods::ALT != 0 {
        out |= META_ALT_ON;
    }
    if m & mods::META != 0 {
        out |= META_META_ON;
    }
    out
}

/// US-layout char → Android keycode for CHORDS (a modifier is held,
/// so the editor gets a real key event, not a text commit). Letters
/// map case-insensitively (the shift bit rides meta_state), digits and
/// space map directly. Punctuation chords are NOT covered here: typed
/// punctuation arrives as text (commitText), and its shifted forms
/// differ per layout — keep honest instead of guessing.
fn char_keycode(c: char) -> Option<i32> {
    match c.to_ascii_lowercase() {
        'a'..='z' => Some(K_KEYCODE_A + (c.to_ascii_lowercase() as u8 - b'a') as i32),
        '0'..='9' => Some(K_KEYCODE_0 + (c.to_ascii_lowercase() as u8 - b'0') as i32),
        ' ' => Some(K_SPACE),
        _ => None,
    }
}

/// Which bridge call a protocol key maps to (pure, unit-tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Esc → GLOBAL_ACTION_BACK (accessibility, always available).
    Back,
    /// Real key event: Android keycode + meta state, via the IME
    /// InputConnection (needs the OpacusR keyboard selected).
    Key(i32, i32),
    /// Character typed without modifiers → commitText (IME first,
    /// clipboard-paste fallback).
    Type(char),
}

fn key_action(key: OpKey, m: u8) -> Option<KeyAction> {
    use OpKey::*;
    let meta = meta_state(m);
    match key {
        Escape => Some(KeyAction::Back),
        Enter => Some(KeyAction::Key(K_ENTER, meta)),
        Tab => Some(KeyAction::Key(K_TAB, meta)),
        Backspace => Some(KeyAction::Key(K_DEL, meta)),
        Delete => Some(KeyAction::Key(K_FORWARD_DEL, meta)),
        Insert => Some(KeyAction::Key(K_INSERT, meta)),
        Home => Some(KeyAction::Key(K_MOVE_HOME, meta)),
        End => Some(KeyAction::Key(K_MOVE_END, meta)),
        PageUp => Some(KeyAction::Key(K_PAGE_UP, meta)),
        PageDown => Some(KeyAction::Key(K_PAGE_DOWN, meta)),
        ArrowUp => Some(KeyAction::Key(K_DPAD_UP, meta)),
        ArrowDown => Some(KeyAction::Key(K_DPAD_DOWN, meta)),
        ArrowLeft => Some(KeyAction::Key(K_DPAD_LEFT, meta)),
        ArrowRight => Some(KeyAction::Key(K_DPAD_RIGHT, meta)),
        Space => Some(KeyAction::Key(K_SPACE, meta)),
        F1 => Some(KeyAction::Key(K_F1, meta)),
        F2 => Some(KeyAction::Key(K_F1 + 1, meta)),
        F3 => Some(KeyAction::Key(K_F1 + 2, meta)),
        F4 => Some(KeyAction::Key(K_F1 + 3, meta)),
        F5 => Some(KeyAction::Key(K_F1 + 4, meta)),
        F6 => Some(KeyAction::Key(K_F1 + 5, meta)),
        F7 => Some(KeyAction::Key(K_F1 + 6, meta)),
        F8 => Some(KeyAction::Key(K_F1 + 7, meta)),
        F9 => Some(KeyAction::Key(K_F1 + 8, meta)),
        F10 => Some(KeyAction::Key(K_F1 + 9, meta)),
        F11 => Some(KeyAction::Key(K_F1 + 10, meta)),
        F12 => Some(KeyAction::Key(K_F1 + 11, meta)),
        Char(c) if m == 0 => Some(KeyAction::Type(c)),
        Char(c) => char_keycode(c).map(|code| KeyAction::Key(code, meta)),
        // Lock modifiers and screen keys have no touch reality.
        CapsLock | NumLock | PrintScreen | ContextMenu => None,
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

    fn key(&mut self, key: OpKey, m: u8, down: bool) -> Result<(), InputError> {
        match key_action(key, m) {
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
            Some(KeyAction::Key(code, meta)) => {
                if !down {
                    return Ok(()); // IC.sendKeyEvent is down+up in one hop
                }
                let ctx = IME.get().ok_or(InputError::Unsupported)?;
                let mut args = [jni_sys::jvalue { i: code }, jni_sys::jvalue { i: meta }];
                match call_ime_bool("imeKey", ctx.key, args.as_mut_ptr()) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(InputError::Backend(
                        "keyboard bridge: no editor focus (выбрана ли клавиатура OpacusR?)".into(),
                    )),
                    Err(e) => Err(e),
                }
            }
            Some(KeyAction::Type(c)) => {
                if !down {
                    return Ok(());
                }
                self.send_text(&c.to_string())
            }
        }
    }

    fn text(&mut self, text: &str) -> Result<(), InputError> {
        self.send_text(text)
    }
}

impl AndroidInjector {
    /// Text path (v2): IME commitText FIRST — real Unicode into the
    /// focused editor with the clipboard untouched. When the OpacusR
    /// keyboard is not selected (or no editor has focus → Ok(false)),
    /// the v1 accessibility path takes over: clipboard + ACTION_PASTE
    /// at the caret (clobbers the clipboard, needs an editable node).
    fn send_text(&mut self, text: &str) -> Result<(), InputError> {
        if text.is_empty() {
            return Ok(());
        }
        if let Ok(true) = ime_type_text(text) {
            return Ok(());
        }
        let ctx = CONTEXT.get().ok_or(InputError::Unsupported)?;
        match call_string_bool(ctx.vm, ctx.class, ctx.paste, "paste", text) {
            Ok(true) => Ok(()),
            Ok(false) => Err(InputError::Backend(
                "input bridge: paste failed (no editable focus?)".into(),
            )),
            Err(e) => Err(e),
        }
    }
}

/// `imeType(String)` — commitText at the caret of the focused editor.
/// Ok(false) = IME not selected / no editor focus (caller falls back
/// to the paste path); Err = JNI-level failure.
fn ime_type_text(text: &str) -> Result<bool, InputError> {
    let ctx = IME.get().ok_or(InputError::Unsupported)?;
    call_string_bool(ctx.vm, ctx.class, ctx.type_text, "imeType", text)
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
    fn escape_is_back_editing_keys_map_to_ime_keycodes() {
        assert_eq!(key_action(OpKey::Escape, 0), Some(KeyAction::Back));
        assert_eq!(key_action(OpKey::Enter, 0), Some(KeyAction::Key(66, 0)));
        assert_eq!(key_action(OpKey::Tab, 0), Some(KeyAction::Key(61, 0)));
        assert_eq!(key_action(OpKey::Backspace, 0), Some(KeyAction::Key(67, 0)));
        assert_eq!(key_action(OpKey::Delete, 0), Some(KeyAction::Key(112, 0)));
        assert_eq!(key_action(OpKey::ArrowUp, 0), Some(KeyAction::Key(19, 0)));
        assert_eq!(key_action(OpKey::ArrowLeft, 0), Some(KeyAction::Key(21, 0)));
        assert_eq!(key_action(OpKey::Home, 0), Some(KeyAction::Key(122, 0)));
        assert_eq!(key_action(OpKey::End, 0), Some(KeyAction::Key(123, 0)));
        assert_eq!(key_action(OpKey::PageUp, 0), Some(KeyAction::Key(92, 0)));
        assert_eq!(key_action(OpKey::PageDown, 0), Some(KeyAction::Key(93, 0)));
        assert_eq!(key_action(OpKey::Insert, 0), Some(KeyAction::Key(124, 0)));
        assert_eq!(key_action(OpKey::Space, 0), Some(KeyAction::Key(62, 0)));
        assert_eq!(key_action(OpKey::F1, 0), Some(KeyAction::Key(131, 0)));
        assert_eq!(key_action(OpKey::F12, 0), Some(KeyAction::Key(142, 0)));
        // Meta rides along: Ctrl+Left = word jump in editors.
        assert_eq!(
            key_action(OpKey::ArrowLeft, mods::CTRL),
            Some(KeyAction::Key(21, META_CTRL_ON))
        );
    }

    #[test]
    fn chars_type_or_chord_by_modifiers() {
        assert_eq!(key_action(OpKey::Char('x'), 0), Some(KeyAction::Type('x')));
        assert_eq!(key_action(OpKey::Char('Ж'), 0), Some(KeyAction::Type('Ж')));
        // Ctrl+C → a real key event with meta (copy in editors).
        assert_eq!(
            key_action(OpKey::Char('c'), mods::CTRL),
            Some(KeyAction::Key(31, META_CTRL_ON))
        );
        // Ctrl+Alt+5 → digit keycode with combined meta.
        assert_eq!(
            key_action(OpKey::Char('5'), mods::CTRL | mods::ALT),
            Some(KeyAction::Key(12, META_CTRL_ON | META_ALT_ON))
        );
        // Punctuation chords stay honest (typed punctuation rides text()).
        assert_eq!(key_action(OpKey::Char('!'), mods::SHIFT), None);
        // Lock modifiers and screen keys have no touch reality.
        for k in [
            OpKey::CapsLock,
            OpKey::NumLock,
            OpKey::PrintScreen,
            OpKey::ContextMenu,
        ] {
            assert_eq!(key_action(k, 0), None, "{k:?} must stay unsupported");
        }
    }

    #[test]
    fn meta_state_bits_mirror_android() {
        assert_eq!(meta_state(0), 0);
        assert_eq!(meta_state(mods::SHIFT), META_SHIFT_ON);
        assert_eq!(meta_state(mods::CTRL), META_CTRL_ON);
        assert_eq!(meta_state(mods::ALT), META_ALT_ON);
        assert_eq!(meta_state(mods::META), META_META_ON);
        assert_eq!(
            meta_state(mods::SHIFT | mods::CTRL | mods::ALT | mods::META),
            META_SHIFT_ON | META_CTRL_ON | META_ALT_ON | META_META_ON
        );
    }

    #[test]
    fn char_keycode_covers_letters_digits_space_only() {
        assert_eq!(char_keycode('a'), Some(29));
        assert_eq!(char_keycode('A'), Some(29)); // case rides meta
        assert_eq!(char_keycode('z'), Some(54));
        assert_eq!(char_keycode('0'), Some(7));
        assert_eq!(char_keycode('9'), Some(16));
        assert_eq!(char_keycode(' '), Some(62));
        assert_eq!(char_keycode('é'), None);
        assert_eq!(char_keycode('!'), None);
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
        // Back rides the gesture bridge — unsupported without it.
        assert!(matches!(
            inj.key(OpKey::Escape, 0, true),
            Err(InputError::Unsupported)
        ));
        // Editing keys ride the IME bridge — unsupported without it.
        assert!(matches!(
            inj.key(OpKey::Enter, 0, true),
            Err(InputError::Unsupported)
        ));
        // Chars fall IME → paste — both missing without bridges.
        assert!(matches!(
            inj.key(OpKey::Char('x'), 0, true),
            Err(InputError::Unsupported)
        ));
        assert!(matches!(inj.text("привет"), Err(InputError::Unsupported)));
        assert!(!input_service_ready());
        assert!(!keyboard_service_ready());
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
