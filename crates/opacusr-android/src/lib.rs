//! OpacusR Android app: NativeActivity glue around the egui UI.
//!
//! The phone works as a VIEWER (connects to an OpacusR host by partner
//! ID or ip:port and shows the shared screen with touch controls) AND
//! as a HOST: MediaProjection capture via the CaptureActivity/
//! CaptureService shim (classes.dex, packaged by CI — see
//! crates/opacusr-android/java/ and AndroidManifest.xml). The
//! cargo-apk manifest lives in this crate's Cargo.toml; CI builds
//! `OpacusR.apk` for arm64-v8a on release tags.
//!
//! Desktop note: on non-Android targets this crate is an empty cdylib
//! (everything below is `cfg(target_os = "android")`).

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

/// NativeActivity entry point, called by the android-activity glue.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(android_app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,opacusr=info".into()),
        )
        .init();

    // `dirs::config_dir()` is `None` on Android and the fallback (`.`)
    // is the unwritable process root — settings, the access password
    // and the address book silently failed to persist («невозможно
    // задать пароль»). Export the app's internal files dir as the base
    // BEFORE any config load; opacusr-agent and opacusr-client route
    // their paths through $OPACUSR_CONFIG_DIR.
    if let Some(data) = android_app.internal_data_path() {
        std::env::set_var("OPACUSR_CONFIG_DIR", &data);
        tracing::info!(target: "opacusr", "state dir: {}", data.display());
    }

    // mDNS multicast receive needs a WiFiManager.MulticastLock on
    // Android: without it the Wi-Fi driver drops multicast, «Контакты»
    // stays empty and the connect ladder never learns LAN signaling
    // endpoints. Best-effort — failures are logged, never fatal.
    android_acquire_multicast_lock(android_app.vm_as_ptr(), android_app.activity_as_ptr());

    // Measure the system status/navigation bar heights ONCE and hand
    // them to the UI: the NativeActivity window is edge-to-edge and the
    // ROM draws the status bar straight over our header — the
    // «⏻ Закрыть» button and the version marker sat underneath the
    // clock/battery icons (0.11.19 report: «не вижу кнопку закрытия»).
    // cargo-apk 0.10 drops the manifest `theme` key (the built APK's
    // AndroidManifest.xml has no theme attribute), so the platform
    // cannot reserve the safe area for us — the UI reserves it itself.
    // Best-effort: on failure the heights stay (0, 0) and rendering
    // matches the old behavior.
    let (status_px, nav_px) =
        android_system_bar_heights(android_app.vm_as_ptr(), android_app.activity_as_ptr());
    opacusr_app::android_insets::set_insets_px(status_px, nav_px);

    // Runtime system-UI hooks for the session UI: landscape rotation
    // while a viewer session is live, sticky-immersive bars for the
    // chrome-less mode, soft keyboard for typing. The raw activity
    // handle from android-activity is a LOCAL ref valid only inside
    // android_main — the hooks fire later, so keep a GLOBAL ref (leaked
    // on purpose, like the multicast lock) alongside the VM handle.
    if let Some(activity_global) =
        android_new_global_ref(android_app.vm_as_ptr(), android_app.activity_as_ptr())
    {
        let _ = SYSUI_CONTEXT.set((android_app.vm_as_ptr() as usize, activity_global as usize));
        opacusr_app::android_sysui::set_hooks(
            hook_set_landscape,
            hook_set_immersive,
            hook_set_keyboard,
        );
        tracing::info!(target: "opacusr", "system-UI hooks registered");

        // Phone-as-host: cache the JNI context + the CaptureActivity class
        // ref for the MediaProjection backend (the class resolves through
        // the ACTIVITY's class loader — FindClass cannot see APK classes
        // from a native thread). Best-effort: without it the phone simply
        // stays viewer-only and the honest hints explain that.
        match android_load_capture_class(android_app.vm_as_ptr(), android_app.activity_as_ptr()) {
            Some(cls_global) => {
                opacusr_capture::android_projection::set_context(
                    android_app.vm_as_ptr() as usize,
                    activity_global,
                    cls_global,
                );
                tracing::info!(target: "opacusr", "projection bridge ready");
            }
            None => {
                tracing::warn!(target: "opacusr", "projection bridge unavailable (no capture shim)")
            }
        }

        // Remote input: same class-loader dance for the accessibility
        // shim (io.opacusr.app.InputService, remote pointer/keyboard —
        // see opacusr_input::android_bridge). Best-effort: without the
        // class (or without the user enabling the service) the phone
        // stays view-only.
        wire_input_bridge(android_app.vm_as_ptr(), android_app.activity_as_ptr());
    } else {
        tracing::warn!(target: "opacusr", "system-UI hooks unavailable (no global ref)");
    }

    // The one android-specific bit: hand the NativeActivity handle to
    // winit (eframe exposes the event-loop builder hook for this).
    let options = eframe::NativeOptions {
        event_loop_builder: Some(Box::new(move |builder| {
            builder.with_android_app(android_app.clone());
        })),
        viewport: egui::ViewportBuilder::default().with_title("OpacusR"),
        ..Default::default()
    };

    let app_creator = |cc: &eframe::CreationContext<'_>| {
        opacusr_app::install_fonts(cc);
        Ok(Box::new(opacusr_app::App::new(cc)) as Box<dyn eframe::App>)
    };
    eframe::run_native("OpacusR", options, Box::new(app_creator)).expect("OpacusR UI failed");
}

// ---------------------------------------------------------------------
// Runtime system-UI hooks (registered into opacusr_app::android_sysui
// above): landscape rotation / sticky-immersive bars / soft keyboard.
// Same raw jni-sys vtable discipline as the multicast lock — every
// call exception-checked, every failure degrades to a log line.
// NOT gated to Android so the host build type-checks the wiring too.
// ---------------------------------------------------------------------

/// (NDK JavaVM handle, activity GLOBAL ref) stashed by android_main.
/// Both stored as plain addresses; they live for the whole process.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
static SYSUI_CONTEXT: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();

/// Attached thread context for a best-effort UI hook call.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
struct UiJni {
    env: *mut jni_sys::JNIEnv,
    iface: *const jni_sys::JNINativeInterface_,
    activity: jni_sys::jobject,
}

impl UiJni {
    /// Attach the calling thread and fetch the vtable. The egui/winit
    /// UI thread IS the android_main thread (already attached by the
    /// android-activity glue), so AttachCurrentThread just returns the
    /// existing JNIEnv. `None` = hooks unavailable (desktop, or JNI
    /// broken — logged, never fatal).
    fn attach() -> Option<Self> {
        let (vm_handle, activity): (usize, usize) = *SYSUI_CONTEXT.get()?;
        unsafe {
            let vm: jni_sys::JavaVM = *(vm_handle as *mut jni_sys::JavaVM);
            if vm.is_null() {
                tracing::warn!(target: "opacusr", "sysui: JavaVM vtable pointer is null");
                return None;
            }
            let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
            let res = ((*vm).v1_1.AttachCurrentThread)(
                vm_handle as *mut jni_sys::JavaVM,
                (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
                std::ptr::null_mut(),
            );
            if res != jni_sys::JNI_OK || env.is_null() {
                tracing::warn!(target: "opacusr", "sysui: AttachCurrentThread failed ({res})");
                return None;
            }
            let iface: *const jni_sys::JNINativeInterface_ = *env;
            let version = ((*iface).v1_6.GetVersion)(env);
            if version < jni_sys::JNI_VERSION_1_6 {
                tracing::warn!(target: "opacusr", "sysui: JNI sanity check {version:#x}");
                return None;
            }
            Some(Self {
                env,
                iface,
                activity: activity as jni_sys::jobject,
            })
        }
    }

    /// Exception check + clear; `false` = a Java exception occurred.
    fn check(&self, what: &str) -> bool {
        unsafe {
            if ((*self.iface).v1_6.ExceptionCheck)(self.env) {
                ((*self.iface).v1_6.ExceptionClear)(self.env);
                tracing::warn!(target: "opacusr", "sysui: Java exception during {what}");
                return false;
            }
        }
        true
    }

    /// Framework-class lookup (FindClass cannot resolve APK classes from
    /// an attached native thread — those go through cached class refs).
    fn find_class(&self, name: &core::ffi::CStr) -> Option<jni_sys::jclass> {
        unsafe {
            let cls = ((*self.iface).v1_6.FindClass)(self.env, name.as_ptr());
            if self.check("FindClass") && !cls.is_null() {
                Some(cls)
            } else {
                None
            }
        }
    }
}

/// Creates a global ref of the activity for the runtime hooks (the
/// android_main-scope handle is a local ref). Returns the address as a
/// usize, or None on failure. The ref is intentionally never deleted.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_new_global_ref(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
) -> Option<usize> {
    if vm_raw.is_null() || activity_raw.is_null() {
        return None;
    }
    unsafe {
        let vm_handle: *mut jni_sys::JavaVM = vm_raw.cast();
        let vm: jni_sys::JavaVM = *vm_handle;
        if vm.is_null() {
            return None;
        }
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_1.AttachCurrentThread)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            return None;
        }
        let iface: *const jni_sys::JNINativeInterface_ = *env;
        let global = ((*iface).v1_6.NewGlobalRef)(env, activity_raw.cast());
        if global.is_null() {
            ((*iface).v1_6.ExceptionClear)(env);
            return None;
        }
        Some(global as usize)
    }
}

/// UI-thread hook: sensor-landscape while a session is live, back to the
/// manifest default afterwards (Activity.setRequestedOrientation:
/// SENSOR_LANDSCAPE = 6, FULL_USER = 13 — the manifest's `fullUser`
/// default; the old values 13/10 actually meant FULL_USER/FULL_SENSOR,
/// so sessions never really held landscape — 0.12.1 report).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn hook_set_landscape(on: bool) {
    let Some(j) = UiJni::attach() else { return };
    unsafe {
        let cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, j.activity);
        if !j.check("landscape: GetObjectClass") {
            return;
        }
        let mid = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            cls,
            c"setRequestedOrientation".as_ptr(),
            c"(I)V".as_ptr(),
        );
        if j.check("landscape: GetMethodID") {
            let orientation: core::ffi::c_int = if on { 6 } else { 13 };
            ((*j.iface).v1_6.CallVoidMethod)(j.env, j.activity, mid, orientation);
            j.check("landscape: call");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, cls);
    }
}

/// UI-thread hook: sticky-immersive system bars (chrome-less fullscreen)
/// or back to the edge-to-edge state the insets module compensates for.
///
/// API 30+ first: `Window.getInsetsController().hide(systemBars())` —
/// the legacy `View.setSystemUiVisibility(0x1706)` path is deprecated,
/// and on several Android 14/15 ROMs it no longer hides the bars at all
/// (0.12.1 report: «не разворачивает на весь экран» with the bars still
/// visible). The legacy path stays as the pre-API-30 fallback.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn hook_set_immersive(on: bool) {
    let Some(j) = UiJni::attach() else { return };
    unsafe {
        // Activity.getWindow() → Window.
        let act_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, j.activity);
        if !j.check("immersive: activity class") {
            return;
        }
        let get_window = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            act_cls,
            c"getWindow".as_ptr(),
            c"()Landroid/view/Window;".as_ptr(),
        );
        let mut window: jni_sys::jobject = std::ptr::null_mut();
        if j.check("immersive: getWindow") {
            window = ((*j.iface).v1_6.CallObjectMethod)(j.env, j.activity, get_window);
            j.check("immersive: window");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, act_cls);
        if window.is_null() {
            return;
        }

        let sdk = android_sdk_int(&j);
        if sdk >= 30 {
            let win_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, window);
            let get_ctrl = ((*j.iface).v1_6.GetMethodID)(
                j.env,
                win_cls,
                c"getInsetsController".as_ptr(),
                c"()Landroid/view/WindowInsetsController;".as_ptr(),
            );
            let mut ctrl: jni_sys::jobject = std::ptr::null_mut();
            if j.check("immersive: getInsetsController") {
                ctrl = ((*j.iface).v1_6.CallObjectMethod)(j.env, window, get_ctrl);
                j.check("immersive: controller");
            }
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, win_cls);
            if !ctrl.is_null() {
                if let Some(mask) = system_bars_mask(&j) {
                    let ctrl_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, ctrl);
                    let hide_show = ((*j.iface).v1_6.GetMethodID)(
                        j.env,
                        ctrl_cls,
                        if on { c"hide" } else { c"show" }.as_ptr(),
                        c"(I)V".as_ptr(),
                    );
                    if j.check("immersive: hide/show lookup") {
                        ((*j.iface).v1_6.CallVoidMethod)(j.env, ctrl, hide_show, mask);
                        j.check("immersive: hide/show call");
                    }
                    let behavior = ((*j.iface).v1_6.GetMethodID)(
                        j.env,
                        ctrl_cls,
                        c"setSystemBarsBehavior".as_ptr(),
                        c"(I)V".as_ptr(),
                    );
                    if j.check("immersive: behavior lookup") {
                        // BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE = 2.
                        ((*j.iface).v1_6.CallVoidMethod)(j.env, ctrl, behavior, 2);
                        j.check("immersive: behavior call");
                    }
                    ((*j.iface).v1_6.DeleteLocalRef)(j.env, ctrl_cls);
                    ((*j.iface).v1_6.DeleteLocalRef)(j.env, ctrl);
                    ((*j.iface).v1_6.DeleteLocalRef)(j.env, window);
                    return;
                }
            }
            // Controller unavailable (unexpected) → fall through to legacy.
        }

        // Legacy path (pre-API 30, and the API 30+ fallback).
        let win_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, window);
        let get_decor = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            win_cls,
            c"getDecorView".as_ptr(),
            c"()Landroid/view/View;".as_ptr(),
        );
        let mut decor: jni_sys::jobject = std::ptr::null_mut();
        if j.check("immersive: getDecorView") {
            decor = ((*j.iface).v1_6.CallObjectMethod)(j.env, window, get_decor);
            j.check("immersive: decor");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, window);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, win_cls);
        if decor.is_null() {
            return;
        }
        let view_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, decor);
        let set_vis = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            view_cls,
            c"setSystemUiVisibility".as_ptr(),
            c"(I)V".as_ptr(),
        );
        if j.check("immersive: setSystemUiVisibility") {
            // SYSTEM_UI_FLAG_LAYOUT_STABLE | LAYOUT_HIDE_NAVIGATION |
            // LAYOUT_FULLSCREEN | HIDE_NAVIGATION | FULLSCREEN |
            // IMMERSIVE_STICKY = 0x1706.
            let flags: core::ffi::c_int = if on { 0x1706 } else { 0 };
            ((*j.iface).v1_6.CallVoidMethod)(j.env, decor, set_vis, flags);
            j.check("immersive: call");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, view_cls);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, decor);
    }
}

/// `Build.VERSION.SDK_INT` (framework class — safe from any thread).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_sdk_int(j: &UiJni) -> i32 {
    unsafe {
        let Some(cls) = j.find_class(c"android/os/Build$VERSION") else {
            return 0;
        };
        let fid =
            ((*j.iface).v1_6.GetStaticFieldID)(j.env, cls, c"SDK_INT".as_ptr(), c"I".as_ptr());
        if !j.check("SDK_INT lookup") {
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, cls);
            return 0;
        }
        let v = ((*j.iface).v1_6.GetStaticIntField)(j.env, cls, fid);
        j.check("SDK_INT read");
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, cls);
        v
    }
}

/// `WindowInsets.Type.systemBars()` mask (API 30+ static helper).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn system_bars_mask(j: &UiJni) -> Option<jni_sys::jint> {
    unsafe {
        let cls = j.find_class(c"android/view/WindowInsets$Type")?;
        let mid = ((*j.iface).v1_6.GetStaticMethodID)(
            j.env,
            cls,
            c"systemBars".as_ptr(),
            c"()I".as_ptr(),
        );
        if !j.check("systemBars lookup") {
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, cls);
            return None;
        }
        let mask = ((*j.iface).v1_6.CallStaticIntMethod)(j.env, cls, mid);
        j.check("systemBars call");
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, cls);
        Some(mask)
    }
}

/// UI-thread hook: soft keyboard. Show via InputMethodManager
/// toggleSoftInput(SHOW_FORCED, 0); hide via
/// hideSoftInputFromWindow(decorView token) — more reliable than a
/// second toggle if something else opened the keyboard meanwhile.
/// While the keyboard is up the window switches to
/// SOFT_INPUT_ADJUST_NOTHING: the remote video must NOT shrink when
/// the IME opens (the 0.12.1 report — «с нее невозможно работать», the
/// keyboard covered half the screen); the default adjust mode is
/// restored on hide.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn hook_set_keyboard(on: bool) {
    let Some(j) = UiJni::attach() else { return };
    unsafe {
        // The IME must not resize/shrink the video window.
        set_soft_input_mode(&j, if on { 0x30 } else { 0x00 });
        let act_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, j.activity);
        if !j.check("keyboard: activity class") {
            return;
        }
        let get_service = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            act_cls,
            c"getSystemService".as_ptr(),
            c"(Ljava/lang/String;)Ljava/lang/Object;".as_ptr(),
        );
        let name: jni_sys::jstring =
            ((*j.iface).v1_6.NewStringUTF)(j.env, c"input_method".as_ptr());
        let mut imm: jni_sys::jobject = std::ptr::null_mut();
        if j.check("keyboard: getSystemService") {
            imm = ((*j.iface).v1_6.CallObjectMethod)(j.env, j.activity, get_service, name);
            j.check("keyboard: imm");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, name);
        if imm.is_null() {
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, act_cls);
            return;
        }
        let imm_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, imm);
        // The decor view is needed by BOTH paths: show needs its focus,
        // hide needs its window token.
        let act_get_window = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            act_cls,
            c"getWindow".as_ptr(),
            c"()Landroid/view/Window;".as_ptr(),
        );
        let mut window: jni_sys::jobject = std::ptr::null_mut();
        if j.check("keyboard: getWindow") {
            window = ((*j.iface).v1_6.CallObjectMethod)(j.env, j.activity, act_get_window);
            j.check("keyboard: window");
        }
        let mut decor: jni_sys::jobject = std::ptr::null_mut();
        if !window.is_null() {
            let win_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, window);
            let get_decor = ((*j.iface).v1_6.GetMethodID)(
                j.env,
                win_cls,
                c"getDecorView".as_ptr(),
                c"()Landroid/view/View;".as_ptr(),
            );
            if j.check("keyboard: getDecorView") {
                decor = ((*j.iface).v1_6.CallObjectMethod)(j.env, window, get_decor);
                j.check("keyboard: decor");
            }
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, win_cls);
        }
        if on {
            // showSoftInput REQUIRES a focused view: with a NativeActivity
            // nothing is focused by default, so the old bare
            // toggleSoftInput silently did nothing on several ROMs
            // («нет вызова клавиатуры»). Focus the decor view
            // explicitly, then show the IME over it; the toggle stays
            // as the last-resort fallback.
            if !decor.is_null() {
                let decor_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, decor);
                let focus = ((*j.iface).v1_6.GetMethodID)(
                    j.env,
                    decor_cls,
                    c"requestFocus".as_ptr(),
                    c"()Z".as_ptr(),
                );
                if j.check("keyboard: requestFocus") {
                    ((*j.iface).v1_6.CallBooleanMethod)(j.env, decor, focus);
                }
                ((*j.iface).v1_6.DeleteLocalRef)(j.env, decor_cls);
            }
            let show = ((*j.iface).v1_6.GetMethodID)(
                j.env,
                imm_cls,
                c"showSoftInput".as_ptr(),
                c"(Landroid/view/View;I)Z".as_ptr(),
            );
            if j.check("keyboard: showSoftInput") && !decor.is_null() {
                // SHOW_IMPLICIT = 0: SHOW_FORCED is deprecated and its
                // forced state blocks the user's manual dismissal.
                let ok = ((*j.iface).v1_6.CallBooleanMethod)(
                    j.env,
                    imm,
                    show,
                    decor,
                    0 as core::ffi::c_int,
                );
                // jni-sys maps jboolean to bool: false → the IME refused
                // the implicit show (no focus yet on some ROMs) — fall
                // back to the old forced toggle.
                if !ok {
                    let toggle = ((*j.iface).v1_6.GetMethodID)(
                        j.env,
                        imm_cls,
                        c"toggleSoftInput".as_ptr(),
                        c"(II)V".as_ptr(),
                    );
                    if j.check("keyboard: toggleSoftInput") {
                        // SHOW_FORCED = 2, hideFlags = 0.
                        ((*j.iface).v1_6.CallVoidMethod)(j.env, imm, toggle, 2, 0);
                        j.check("keyboard: toggle show");
                    }
                }
            }
        } else if !decor.is_null() {
            // Window token for hideSoftInputFromWindow — more reliable
            // than a second toggle if something else opened the IME.
            let decor_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, decor);
            let get_token = ((*j.iface).v1_6.GetMethodID)(
                j.env,
                decor_cls,
                c"getWindowToken".as_ptr(),
                c"()Landroid/os/IBinder;".as_ptr(),
            );
            let mut token: jni_sys::jobject = std::ptr::null_mut();
            if j.check("keyboard: getWindowToken") {
                token = ((*j.iface).v1_6.CallObjectMethod)(j.env, decor, get_token);
                j.check("keyboard: token");
            }
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, decor_cls);
            if !token.is_null() {
                let hide = ((*j.iface).v1_6.GetMethodID)(
                    j.env,
                    imm_cls,
                    c"hideSoftInputFromWindow".as_ptr(),
                    c"(Landroid/os/IBinder;I)Z".as_ptr(),
                );
                if j.check("keyboard: hideSoftInputFromWindow") {
                    ((*j.iface).v1_6.CallBooleanMethod)(
                        j.env,
                        imm,
                        hide,
                        token,
                        0 as core::ffi::c_int,
                    );
                    j.check("keyboard: hide");
                }
            }
            ((*j.iface).v1_6.DeleteLocalRef)(j.env, token);
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, decor);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, window);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, imm_cls);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, imm);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, act_cls);
    }
}

/// `Window.setSoftInputMode(mode)` — ADJUST_NOTHING while the soft
/// keyboard is open, stock mode (UNSPECIFIED = 0x00) when hidden.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn set_soft_input_mode(j: &UiJni, mode: core::ffi::c_int) {
    unsafe {
        let act_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, j.activity);
        if !j.check("softinput: activity class") {
            return;
        }
        let get_window = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            act_cls,
            c"getWindow".as_ptr(),
            c"()Landroid/view/Window;".as_ptr(),
        );
        let mut window: jni_sys::jobject = std::ptr::null_mut();
        if j.check("softinput: getWindow") {
            window = ((*j.iface).v1_6.CallObjectMethod)(j.env, j.activity, get_window);
            j.check("softinput: window");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, act_cls);
        if window.is_null() {
            return;
        }
        let win_cls: jni_sys::jclass = ((*j.iface).v1_6.GetObjectClass)(j.env, window);
        let mid = ((*j.iface).v1_6.GetMethodID)(
            j.env,
            win_cls,
            c"setSoftInputMode".as_ptr(),
            c"(I)V".as_ptr(),
        );
        if j.check("softinput: lookup") {
            ((*j.iface).v1_6.CallVoidMethod)(j.env, window, mid, mode);
            j.check("softinput: call");
        }
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, win_cls);
        ((*j.iface).v1_6.DeleteLocalRef)(j.env, window);
    }
}

// -------------------------------------------------------------------------
// MediaProjection host capture: JNI bridge to the CaptureActivity /
// CaptureService shim (classes.dex; see crates/opacusr-android/java/).
// The class ref must be resolved through the activity's own class
// loader — FindClass cannot see APK classes from a native thread.
// -------------------------------------------------------------------------

/// Load the input shim classes and hand them to the input bridge (see
/// opacusr_input::android_bridge): the accessibility shim (gestures +
/// paste) and the silent keyboard shim (IME text/keys). Either may be
/// missing (old APK without the class) — each degrades on its own.
/// Called from android_main; defined ungated so the desktop build keeps
/// the opacusr-input dependency type-checked (same story as the capture
/// wiring above).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn wire_input_bridge(vm_raw: *mut core::ffi::c_void, activity_raw: *mut core::ffi::c_void) {
    let input = android_load_class(vm_raw, activity_raw, c"io.opacusr.app.InputService");
    let keyboard = android_load_class(vm_raw, activity_raw, c"io.opacusr.app.KeyboardService");
    if input.is_none() && keyboard.is_none() {
        tracing::warn!(target: "opacusr", "input bridge unavailable (no input shims)")
    }
    opacusr_input::android_bridge::set_context(
        vm_raw as usize,
        activity_raw as usize,
        input,
        keyboard,
    );
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_load_capture_class(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
) -> Option<usize> {
    android_load_class(vm_raw, activity_raw, c"io.opacusr.app.CaptureActivity")
}

/// Load an APK class through the activity's class loader and pin it
/// with a global ref (FindClass cannot see APK classes from a native
/// thread). Returns the global ref as a plain address.
fn android_load_class(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
    name: &core::ffi::CStr,
) -> Option<usize> {
    if vm_raw.is_null() || activity_raw.is_null() {
        return None;
    }
    unsafe {
        let vm_handle: *mut jni_sys::JavaVM = vm_raw.cast();
        let vm: jni_sys::JavaVM = *vm_handle;
        if vm.is_null() {
            return None;
        }
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_1.AttachCurrentThread)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            return None;
        }
        let iface: *const jni_sys::JNINativeInterface_ = *env;
        let fail = |what: &str| {
            ((*iface).v1_6.ExceptionCheck)(env);
            ((*iface).v1_6.ExceptionClear)(env);
            tracing::warn!(target: "opacusr", "capture class: {what}");
        };

        let act_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, activity_raw.cast());
        if act_cls.is_null() {
            fail("GetObjectClass");
            return None;
        }
        let get_class = ((*iface).v1_6.GetMethodID)(
            env,
            act_cls,
            c"getClass".as_ptr(),
            c"()Ljava/lang/Class;".as_ptr(),
        );
        ((*iface).v1_6.DeleteLocalRef)(env, act_cls);
        if get_class.is_null() {
            fail("getClass");
            return None;
        }
        let cls_obj = ((*iface).v1_6.CallObjectMethod)(env, activity_raw.cast(), get_class);
        if cls_obj.is_null() {
            fail("getClass call");
            return None;
        }
        let cls_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, cls_obj);
        let get_loader = ((*iface).v1_6.GetMethodID)(
            env,
            cls_cls,
            c"getClassLoader".as_ptr(),
            c"()Ljava/lang/ClassLoader;".as_ptr(),
        );
        ((*iface).v1_6.DeleteLocalRef)(env, cls_cls);
        if get_loader.is_null() {
            fail("getClassLoader");
            ((*iface).v1_6.DeleteLocalRef)(env, cls_obj);
            return None;
        }
        let loader = ((*iface).v1_6.CallObjectMethod)(env, cls_obj, get_loader);
        ((*iface).v1_6.DeleteLocalRef)(env, cls_obj);
        if loader.is_null() {
            fail("getClassLoader call");
            return None;
        }
        let loader_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, loader);
        let load_class = ((*iface).v1_6.GetMethodID)(
            env,
            loader_cls,
            c"loadClass".as_ptr(),
            c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr(),
        );
        ((*iface).v1_6.DeleteLocalRef)(env, loader_cls);
        if load_class.is_null() {
            fail("loadClass");
            ((*iface).v1_6.DeleteLocalRef)(env, loader);
            return None;
        }
        let name: jni_sys::jstring = ((*iface).v1_6.NewStringUTF)(env, name.as_ptr());
        let capt_cls = ((*iface).v1_6.CallObjectMethod)(env, loader, load_class, name);
        ((*iface).v1_6.DeleteLocalRef)(env, name);
        ((*iface).v1_6.DeleteLocalRef)(env, loader);
        if ((*iface).v1_6.ExceptionCheck)(env) {
            ((*iface).v1_6.ExceptionClear)(env);
            tracing::warn!(target: "opacusr", "capture class: loadClass failed (shim missing?)");
            return None;
        }
        if capt_cls.is_null() {
            tracing::warn!(target: "opacusr", "capture class: null");
            return None;
        }
        let global = ((*iface).v1_6.NewGlobalRef)(env, capt_cls);
        ((*iface).v1_6.DeleteLocalRef)(env, capt_cls);
        if global.is_null() {
            ((*iface).v1_6.ExceptionClear)(env);
            tracing::warn!(target: "opacusr", "capture class: global ref failed");
            return None;
        }
        Some(global as usize)
    }
}

// --- JNI callbacks the Java shim calls back into (static native methods;
// symbol names must match the Java declarations byte for byte). These
// are NOT cfg-gated so the desktop build type-checks them; the Android
// cross-check CI job compiles them FOR Android.

/// `CaptureService` reports the VirtualDisplay is live: cache the
/// ImageReader + MediaProjection as global refs and mark the state
/// active. The frame poller (AndroidCapture) reads from the reader.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // exported JNI symbol: the ABI mandates raw-pointer params
pub extern "system" fn Java_io_opacusr_app_CaptureService_nativeProjectionStarted(
    env: *mut jni_sys::JNIEnv,
    _cls: jni_sys::jclass,
    width: core::ffi::c_int,
    height: core::ffi::c_int,
    dpi: core::ffi::c_int,
    reader: jni_sys::jobject,
    projection: jni_sys::jobject,
) {
    if env.is_null() || reader.is_null() || projection.is_null() {
        return;
    }
    unsafe {
        let iface: *const jni_sys::JNINativeInterface_ = *env;
        if iface.is_null() {
            return;
        }
        let mut vm_raw: *mut jni_sys::JavaVM = std::ptr::null_mut();
        if ((*iface).v1_6.GetJavaVM)(env, &mut vm_raw) != jni_sys::JNI_OK || vm_raw.is_null() {
            ((*iface).v1_6.ExceptionClear)(env);
            return;
        }
        let rg = ((*iface).v1_6.NewGlobalRef)(env, reader);
        let pg = ((*iface).v1_6.NewGlobalRef)(env, projection);
        ((*iface).v1_6.ExceptionClear)(env);
        if rg.is_null() || pg.is_null() {
            tracing::warn!(target: "opacusr", "projection start: global refs failed");
            return;
        }
        opacusr_capture::android_projection::set_active(
            vm_raw as usize,
            rg as usize,
            pg as usize,
            width,
            height,
            dpi,
        );
    }
}

/// `CaptureService` observed `MediaProjection.Callback.onStop` — the
/// user (or the system) revoked the capture.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // exported JNI symbol: the ABI mandates raw-pointer params
pub extern "system" fn Java_io_opacusr_app_CaptureService_nativeProjectionStopped(
    _env: *mut jni_sys::JNIEnv,
    _cls: jni_sys::jclass,
) {
    opacusr_capture::android_projection::deactivate();
}

/// `CaptureActivity`: the consent dialog was denied (or the service
/// could not start) — remember so the host spawn path fails fast.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // exported JNI symbol: the ABI mandates raw-pointer params
pub extern "system" fn Java_io_opacusr_app_CaptureActivity_nativeProjectionFailed(
    _env: *mut jni_sys::JNIEnv,
    _cls: jni_sys::jclass,
) {
    opacusr_capture::android_projection::mark_failed();
    tracing::info!(target: "opacusr", "projection consent denied");
}

/// The acquired MulticastLock global ref, stored as an address and
/// leaked on purpose: the lock has to stay valid for the whole app
/// lifetime (raw pointers are not Send/Sync; we never delete it).
/// Module-level so the fake-JNI wiring test below can assert that the
/// success path really ran to completion.
static MULTICAST_LOCK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Acquires a WiFiManager.MulticastLock for the process lifetime via
/// raw JNI (jni-sys vtable calls — deliberately NOT the `jni` crate,
/// whose 0.22 macro API churn we do not need for five calls).
///
/// jni-sys 0.4 models the JNIEnv/JavaVM vtables as VERSIONED UNIONS:
/// every call goes through the `v1_6` variant (Android 7+ exposes JNI
/// 1.6; our minSdk is 24). Sequence: `Activity.getSystemService("wifi")`
/// → `WifiManager`, `createMulticastLock("opacusr-mdns")` →
/// `setReferenceCounted(false)` → `acquire()`. The lock is kept as a
/// leaked global ref — it must outlive everything; every call is
/// exception-checked and the whole thing degrades to a log line on
/// failure. NOT gated to Android so the host build type-checks the JNI
/// usage too; on desktop it is exercised by the fake-JNI wiring test.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_acquire_multicast_lock(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
) {
    let fail = |what: &str| tracing::warn!(target: "opacusr", "multicast lock: {what}");
    if MULTICAST_LOCK.get().is_some() {
        return; // already acquired
    }
    if vm_raw.is_null() || activity_raw.is_null() {
        fail("no JavaVM or activity handle");
        return;
    }
    unsafe {
        // THE v0.11.15/16 WHITE-SCREEN ROOT CAUSE, fixed here. jni-sys
        // 0.4 types `JavaVM` as the raw JNIInvokeInterface VTABLE VALUE,
        // but `AndroidApp::vm_as_ptr()` hands us the NDK JavaVM handle —
        // a `struct _JavaVM*` whose FIRST FIELD is that vtable pointer.
        // The JNIInvokeInterface fns take the HANDLE back as their first
        // argument (typed `*mut JavaVM` in jni-sys terms) and recover
        // their ART-side state from it. So: dereference the handle ONCE
        // to load the vtable, then pass the HANDLE itself as the first
        // argument. The old code did the exact opposite on both ends:
        // it dereferenced the handle as if it were the vtable (reading a
        // function pointer from memory past the 8-byte `_JavaVM` struct
        // — i.e. calling garbage) and passed the address of the local
        // `vm` copy as the handle. SIGSEGV before the first UI frame on
        // every real device — the desktop test only exercised the
        // null-handle early return and could never see it.
        let vm_handle: *mut jni_sys::JavaVM = vm_raw.cast();
        let vm: jni_sys::JavaVM = *vm_handle;
        if vm.is_null() {
            fail("JavaVM vtable pointer is null");
            return;
        }
        // The android_main thread is attached by android-activity;
        // AttachCurrentThread on an already-attached thread just
        // returns the existing JNIEnv.
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_1.AttachCurrentThread)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            fail(&format!("AttachCurrentThread: {res}"));
            return;
        }
        let iface: *const jni_sys::JNINativeInterface_ = *env;
        // Wiring self-check: GetVersion on a correctly resolved vtable
        // must report at least JNI 1.6 (Android 7+, our minSdk). If our
        // handle/vtable model were wrong AGAIN, this bails out with a
        // log line instead of jumping through a garbage function pointer.
        let version = ((*iface).v1_6.GetVersion)(env);
        if version < jni_sys::JNI_VERSION_1_6 {
            fail(&format!("GetVersion sanity check: {version:#x}"));
            return;
        }
        // Every JNI call below is followed by an exception check; any
        // problem aborts the whole (optional) feature.
        macro_rules! clean {
            () => {
                if ((*iface).v1_6.ExceptionCheck)(env) {
                    ((*iface).v1_6.ExceptionClear)(env);
                    fail("Java exception during lock acquisition");
                    return;
                }
            };
        }
        let ctx: jni_sys::jobject = activity_raw.cast();
        let ctx_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, ctx);
        clean!();
        let get_service = ((*iface).v1_6.GetMethodID)(
            env,
            ctx_cls,
            c"getSystemService".as_ptr(),
            c"(Ljava/lang/String;)Ljava/lang/Object;".as_ptr(),
        );
        clean!();
        let wifi: jni_sys::jstring = ((*iface).v1_6.NewStringUTF)(env, c"wifi".as_ptr());
        clean!();
        let wifi_mgr: jni_sys::jobject =
            ((*iface).v1_6.CallObjectMethod)(env, ctx, get_service, wifi);
        clean!();
        if wifi_mgr.is_null() {
            fail("WifiManager unavailable");
            return;
        }
        let wm_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, wifi_mgr);
        clean!();
        let create_lock = ((*iface).v1_6.GetMethodID)(
            env,
            wm_cls,
            c"createMulticastLock".as_ptr(),
            c"(Ljava/lang/String;)Landroid/net/wifi/MulticastLock;".as_ptr(),
        );
        clean!();
        let tag: jni_sys::jstring = ((*iface).v1_6.NewStringUTF)(env, c"opacusr-mdns".as_ptr());
        clean!();
        let lock: jni_sys::jobject =
            ((*iface).v1_6.CallObjectMethod)(env, wifi_mgr, create_lock, tag);
        clean!();
        if lock.is_null() {
            fail("createMulticastLock returned null");
            return;
        }
        let lock_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, lock);
        clean!();
        let set_ref = ((*iface).v1_6.GetMethodID)(
            env,
            lock_cls,
            c"setReferenceCounted".as_ptr(),
            c"(Z)V".as_ptr(),
        );
        clean!();
        ((*iface).v1_6.CallVoidMethod)(env, lock, set_ref, 0 as core::ffi::c_int);
        clean!();
        let acquire =
            ((*iface).v1_6.GetMethodID)(env, lock_cls, c"acquire".as_ptr(), c"()V".as_ptr());
        clean!();
        ((*iface).v1_6.CallVoidMethod)(env, lock, acquire);
        clean!();
        let global: jni_sys::jobject = ((*iface).v1_6.NewGlobalRef)(env, lock);
        clean!();
        if global.is_null() {
            fail("NewGlobalRef failed");
            return;
        }
        let _ = MULTICAST_LOCK.set(global as usize);
        // Drop the temporary local refs politely; the global ref stays.
        for local in [wifi, wifi_mgr, lock, ctx_cls, wm_cls, lock_cls] {
            ((*iface).v1_6.DeleteLocalRef)(env, local);
        }
        tracing::info!(target: "opacusr", "multicast lock acquired — mDNS discoverable");
    }
}

/// Measures the system STATUS bar and NAVIGATION bar heights (device
/// pixels) via JNI, for the edge-to-edge inset panels the UI reserves
/// each frame (see `opacusr_app::android_insets`). Sequence:
/// `Activity.getResources()` → `Resources`,
/// `Resources.getIdentifier(name, "dimen", "android")` for
/// "status_bar_height" / "navigation_bar_height",
/// `Resources.getDimensionPixelSize(id)`. Every call is
/// exception-checked; any failure returns (0, 0) and the UI renders
/// exactly like before (no reserved strips). Same raw jni-sys vtable
/// discipline as the multicast lock above; NOT gated to Android so
/// the host build type-checks the JNI usage too.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_system_bar_heights(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
) -> (core::ffi::c_int, core::ffi::c_int) {
    let fail = |what: &str| {
        tracing::warn!(target: "opacusr", "system bar heights: {what}");
        (0, 0)
    };
    if vm_raw.is_null() || activity_raw.is_null() {
        return fail("no JavaVM or activity handle");
    }
    unsafe {
        // Same handle-vs-vtable wiring as the multicast lock: deref the
        // NDK JavaVM handle ONCE to load the vtable, pass the HANDLE as
        // the first argument (see the long comment there — this exact
        // mix-up was the v0.11.15/16 white screen).
        let vm_handle: *mut jni_sys::JavaVM = vm_raw.cast();
        let vm: jni_sys::JavaVM = *vm_handle;
        if vm.is_null() {
            return fail("JavaVM vtable pointer is null");
        }
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_1.AttachCurrentThread)(
            vm_handle,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            return fail(&format!("AttachCurrentThread: {res}"));
        }
        let iface: *const jni_sys::JNINativeInterface_ = *env;
        let version = ((*iface).v1_6.GetVersion)(env);
        if version < jni_sys::JNI_VERSION_1_6 {
            return fail(&format!("GetVersion sanity check: {version:#x}"));
        }
        macro_rules! clean {
            () => {
                if ((*iface).v1_6.ExceptionCheck)(env) {
                    ((*iface).v1_6.ExceptionClear)(env);
                    return fail("Java exception during bar height query");
                }
            };
        }
        let ctx: jni_sys::jobject = activity_raw.cast();
        let ctx_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, ctx);
        clean!();
        let get_res = ((*iface).v1_6.GetMethodID)(
            env,
            ctx_cls,
            c"getResources".as_ptr(),
            c"()Landroid/content/res/Resources;".as_ptr(),
        );
        clean!();
        let res_obj: jni_sys::jobject = ((*iface).v1_6.CallObjectMethod)(env, ctx, get_res);
        clean!();
        if res_obj.is_null() {
            return fail("Resources unavailable");
        }
        let res_cls: jni_sys::jclass = ((*iface).v1_6.GetObjectClass)(env, res_obj);
        clean!();
        let get_id = ((*iface).v1_6.GetMethodID)(
            env,
            res_cls,
            c"getIdentifier".as_ptr(),
            c"(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I".as_ptr(),
        );
        clean!();
        let get_px = ((*iface).v1_6.GetMethodID)(
            env,
            res_cls,
            c"getDimensionPixelSize".as_ptr(),
            c"(I)I".as_ptr(),
        );
        clean!();
        let dimen: jni_sys::jstring = ((*iface).v1_6.NewStringUTF)(env, c"dimen".as_ptr());
        clean!();
        let android: jni_sys::jstring = ((*iface).v1_6.NewStringUTF)(env, c"android".as_ptr());
        clean!();
        let measure = |name: &'static core::ffi::CStr| -> core::ffi::c_int {
            let name_str: jni_sys::jstring =
                ((*iface).v1_6.NewStringUTF)(env, name.as_ptr() as *const _);
            if name_str.is_null() {
                return 0;
            }
            let id = ((*iface).v1_6.CallIntMethod)(env, res_obj, get_id, name_str, dimen, android);
            ((*iface).v1_6.DeleteLocalRef)(env, name_str);
            if ((*iface).v1_6.ExceptionCheck)(env) {
                ((*iface).v1_6.ExceptionClear)(env);
                return 0;
            }
            if id <= 0 {
                return 0;
            }
            let px = ((*iface).v1_6.CallIntMethod)(env, res_obj, get_px, id);
            if ((*iface).v1_6.ExceptionCheck)(env) {
                ((*iface).v1_6.ExceptionClear)(env);
                return 0;
            }
            px
        };
        let status = measure(c"status_bar_height");
        let nav = measure(c"navigation_bar_height");
        for local in [res_obj, ctx_cls, res_cls, dimen, android] {
            ((*iface).v1_6.DeleteLocalRef)(env, local);
        }
        (status, nav)
    }
}

#[cfg(test)]
mod tests {
    /// Host-side compile+run check of the JNI helper: null handles hit
    /// the early-return path (no JNI calls), so this runs everywhere —
    /// it exists to make the jni-sys vtable usage type-checked by the
    /// desktop build too, not only inside the Android CI build.
    #[test]
    fn multicast_lock_helper_degrades_gracefully_on_null_handles() {
        super::android_acquire_multicast_lock(std::ptr::null_mut(), std::ptr::null_mut());
        // No panic, no lock — best-effort semantics preserved.
    }

    /// Same compile+degrade check for the system-bar height query: null
    /// handles must return (0, 0) (→ no inset panels, old rendering)
    /// without touching any JNI function pointer.
    #[test]
    fn system_bar_heights_degrade_gracefully_on_null_handles() {
        let (status, nav) =
            super::android_system_bar_heights(std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!((status, nav), (0, 0));
    }

    /// The runtime system-UI hooks must be safe no-ops when android_main
    /// never stashed the (VM, activity) context — i.e. on desktop. They
    /// are compiled here so the host build type-checks the JNI wiring.
    #[test]
    fn sysui_hooks_degrade_gracefully_without_context() {
        super::hook_set_landscape(true);
        super::hook_set_landscape(false);
        super::hook_set_immersive(true);
        super::hook_set_immersive(false);
        super::hook_set_keyboard(true);
        super::hook_set_keyboard(false);
    }

    /// The global-ref helper must reject null handles (same contract as
    /// the other JNI helpers) instead of crashing.
    #[test]
    fn global_ref_helper_rejects_null_handles() {
        assert!(
            super::android_new_global_ref(std::ptr::null_mut(), std::ptr::null_mut()).is_none()
        );
    }

    // ------------------------------------------------------------------
    // Fake-JNI wiring test. The v0.11.15/16 white screen came from a
    // handle-vs-vtable mix-up in the AttachCurrentThread call — a bug
    // class that COMPILES cleanly (all types are raw pointers) and that
    // the null-handle test above cannot see. These fakes build REAL
    // memory layouts (NDK-style handles whose first field points at a
    // vtable) and verify the helper calls the right slots with the right
    // arguments, end to end, on the desktop.
    // ------------------------------------------------------------------
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// What fake_attach_current_thread received as its first argument.
    static RECORDED_VM_ARG: AtomicUsize = AtomicUsize::new(0);
    /// Fake JNIEnv handle written into the p_env out-param.
    static FAKE_ENV_HANDLE: AtomicUsize = AtomicUsize::new(0);
    /// CallObjectMethod invocation counter (systemService → createLock).
    static CALL_OBJECT_SEQ: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn fake_attach_current_thread(
        vm: *mut jni_sys::JavaVM,
        penv: *mut *mut core::ffi::c_void,
        _args: *mut core::ffi::c_void,
    ) -> jni_sys::jint {
        RECORDED_VM_ARG.store(vm as usize, Ordering::SeqCst);
        *penv = FAKE_ENV_HANDLE.load(Ordering::SeqCst) as *mut core::ffi::c_void;
        jni_sys::JNI_OK
    }

    unsafe extern "system" fn fake_get_version(_env: *mut jni_sys::JNIEnv) -> jni_sys::jint {
        jni_sys::JNI_VERSION_1_6
    }

    unsafe extern "system" fn fake_exception_check(
        _env: *mut jni_sys::JNIEnv,
    ) -> jni_sys::jboolean {
        false
    }

    unsafe extern "system" fn fake_get_object_class(
        _env: *mut jni_sys::JNIEnv,
        _obj: jni_sys::jobject,
    ) -> jni_sys::jclass {
        0x1010usize as jni_sys::jclass
    }

    unsafe extern "system" fn fake_get_method_id(
        _env: *mut jni_sys::JNIEnv,
        _cls: jni_sys::jclass,
        _name: *const core::ffi::c_char,
        _sig: *const core::ffi::c_char,
    ) -> jni_sys::jmethodID {
        0x2020usize as jni_sys::jmethodID
    }

    unsafe extern "system" fn fake_new_string_utf(
        _env: *mut jni_sys::JNIEnv,
        _utf: *const core::ffi::c_char,
    ) -> jni_sys::jstring {
        0x3030usize as jni_sys::jstring
    }

    unsafe extern "C" fn fake_call_object_method(
        _env: *mut jni_sys::JNIEnv,
        _obj: jni_sys::jobject,
        _mid: jni_sys::jmethodID,
        _: ...
    ) -> jni_sys::jobject {
        // 1st call: getSystemService("wifi") → WifiManager; 2nd:
        // createMulticastLock → MulticastLock. Both non-null sentinels.
        let n = CALL_OBJECT_SEQ.fetch_add(1, Ordering::SeqCst);
        (0x4040 + n) as jni_sys::jobject
    }

    unsafe extern "C" fn fake_call_void_method(
        _env: *mut jni_sys::JNIEnv,
        _obj: jni_sys::jobject,
        _mid: jni_sys::jmethodID,
        _: ...
    ) {
    }

    unsafe extern "system" fn fake_new_global_ref(
        _env: *mut jni_sys::JNIEnv,
        _obj: jni_sys::jobject,
    ) -> jni_sys::jobject {
        0x5050usize as jni_sys::jobject
    }

    unsafe extern "system" fn fake_delete_local_ref(
        _env: *mut jni_sys::JNIEnv,
        _obj: jni_sys::jobject,
    ) {
    }

    #[test]
    fn multicast_lock_wiring_survives_a_fake_vm_handle() {
        use jni_sys::JavaVM;

        // Field offsets inside the real jni-sys vtable structs are taken
        // from uninit temporaries below, so the fakes cannot drift from
        // the crate's actual layout.

        // Fake JavaVM vtable (16 words is plenty) + the VM handle: NDK
        // `struct _JavaVM` — one word holding the vtable address. Heap
        // allocation (Box::leak): the helper reads these through raw
        // pointers, and stack slots the compiler cannot see being read
        // would be dead-code-eliminated — the fake calls would then
        // jump through zero and kill the test process.
        let vm_table: &mut [usize; 16] = Box::leak(Box::new([0usize; 16]));
        let vm_table_addr = vm_table.as_mut_ptr() as usize;
        let vm_handle_mem: &mut [usize; 2] = Box::leak(Box::new([vm_table_addr, 0]));
        let vm_handle = vm_handle_mem.as_mut_ptr() as *mut JavaVM as usize;

        let off = |raw: usize, base: usize| (raw - base) / 8;

        let invoke_base: *const jni_sys::JNIInvokeInterface_ = {
            let tmp = core::mem::MaybeUninit::<jni_sys::JNIInvokeInterface_>::uninit();
            tmp.as_ptr()
        };
        let ib = invoke_base as usize;
        vm_table[off(
            unsafe { core::ptr::addr_of!((*invoke_base).v1_1.AttachCurrentThread) as usize },
            ib,
        )] = fake_attach_current_thread as *const () as usize;

        // Fake JNIEnv vtable (the real struct has ~230 slots) + handle.
        let env_table: &mut [usize; 256] = Box::leak(Box::new([0usize; 256]));
        let env_table_addr = env_table.as_mut_ptr() as usize;
        let env_handle_mem: &mut [usize; 2] = Box::leak(Box::new([env_table_addr, 0]));
        let env_handle = env_handle_mem.as_mut_ptr() as *mut jni_sys::JNIEnv as usize;

        let env_base: *const jni_sys::JNINativeInterface_ = {
            let tmp = core::mem::MaybeUninit::<jni_sys::JNINativeInterface_>::uninit();
            tmp.as_ptr()
        };
        let eb = env_base as usize;
        let mut put = |slot: usize, f: usize| env_table[slot] = f;
        unsafe {
            put(
                (core::ptr::addr_of!((*env_base).v1_6.GetVersion) as usize - eb) / 8,
                fake_get_version as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.ExceptionCheck) as usize - eb) / 8,
                fake_exception_check as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.GetObjectClass) as usize - eb) / 8,
                fake_get_object_class as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.GetMethodID) as usize - eb) / 8,
                fake_get_method_id as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.NewStringUTF) as usize - eb) / 8,
                fake_new_string_utf as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.CallObjectMethod) as usize - eb) / 8,
                fake_call_object_method as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.CallVoidMethod) as usize - eb) / 8,
                fake_call_void_method as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.NewGlobalRef) as usize - eb) / 8,
                fake_new_global_ref as *const () as usize,
            );
            put(
                (core::ptr::addr_of!((*env_base).v1_6.DeleteLocalRef) as usize - eb) / 8,
                fake_delete_local_ref as *const () as usize,
            );
        }

        RECORDED_VM_ARG.store(0, Ordering::SeqCst);
        FAKE_ENV_HANDLE.store(env_handle, Ordering::SeqCst);
        CALL_OBJECT_SEQ.store(0, Ordering::SeqCst);

        // Run the helper against the fakes. If the handle/vtable wiring
        // is wrong in ANY way (the 0.11.15 bug), this jumps through a
        // zero slot and the test process dies — the crash we ship-fixed.
        super::android_acquire_multicast_lock(
            vm_handle as *mut core::ffi::c_void,
            0x6060usize as *mut core::ffi::c_void,
        );

        // AttachCurrentThread must have received THE HANDLE (the address
        // whose first field is the vtable), not the vtable itself and
        // not the address of some local copy.
        assert_eq!(RECORDED_VM_ARG.load(Ordering::SeqCst), vm_handle);
        // And the full success path must have completed.
        assert_eq!(CALL_OBJECT_SEQ.load(Ordering::SeqCst), 2);
        assert_eq!(super::MULTICAST_LOCK.get(), Some(&0x5050usize));
    }
}
