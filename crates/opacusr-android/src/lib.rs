//! OpacusR Android app: NativeActivity glue around the egui UI.
//!
//! The phone acts as a VIEWER: it connects to an OpacusR host (by
//! partner ID or ip:port, the same two input fields as on the desktop)
//! and shows the shared screen with touch controls. Capturing the
//! Android screen itself (host mode) needs MediaProjection and comes
//! later. The cargo-apk manifest lives in this crate's Cargo.toml;
//! CI builds `OpacusR.apk` for arm64-v8a on release tags.
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
