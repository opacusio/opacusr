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
/// usage too; on desktop it is simply never called.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_acquire_multicast_lock(
    vm_raw: *mut core::ffi::c_void,
    activity_raw: *mut core::ffi::c_void,
) {
    use std::sync::OnceLock;

    /// The acquired MulticastLock global ref, stored as an address and
    /// leaked on purpose: the lock has to stay valid for the whole app
    /// lifetime (raw pointers are not Send/Sync; we never delete it).
    static MULTICAST_LOCK: OnceLock<usize> = OnceLock::new();

    let fail = |what: &str| tracing::warn!(target: "opacusr", "multicast lock: {what}");
    if MULTICAST_LOCK.get().is_some() {
        return; // already acquired
    }
    if vm_raw.is_null() || activity_raw.is_null() {
        fail("no JavaVM or activity handle");
        return;
    }
    unsafe {
        let vm: jni_sys::JavaVM = vm_raw.cast();
        // The android_main thread is attached by android-activity;
        // AttachCurrentThread on an already-attached thread just
        // returns the existing JNIEnv.
        let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
        let res = ((*vm).v1_1.AttachCurrentThread)(
            &vm as *const jni_sys::JavaVM as *mut jni_sys::JavaVM,
            (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
            std::ptr::null_mut(),
        );
        if res != jni_sys::JNI_OK || env.is_null() {
            fail(&format!("AttachCurrentThread: {res}"));
            return;
        }
        let iface: *const jni_sys::JNINativeInterface_ = *env;
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
}
