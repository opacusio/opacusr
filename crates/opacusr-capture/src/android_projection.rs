//! Android MediaProjection screen capture — the phone-as-host backend.
//!
//! Flow (mirrors AnyDesk/RustDesk minus the Java side we cannot ship via
//! cargo-apk 0.10 — the shim is compiled into `classes.dex` by CI):
//!
//! 1. `set_context` (called from `android_main` in opacusr-android)
//!    caches the JavaVM handle, the activity GLOBAL ref and the
//!    `CaptureActivity` class ref (loaded through the activity's own
//!    class loader — `FindClass` cannot see APK classes from an
//!    attached native thread).
//! 2. `request_start` launches `io.opacusr.app.CaptureActivity`, which
//!    shows the system MediaProjection consent dialog and, on approval,
//!    starts the foreground `CaptureService` (`mediaProjection` type).
//! 3. The service creates an `ImageReader` (RGBA_8888) + `VirtualDisplay`
//!    and calls back into `Java_..._nativeProjectionStarted` (implemented
//!    in opacusr-android), which hands us the reader + projection global
//!    refs via [`set_active`].
//! 4. [`AndroidCapture`] implements [`ScreenCapture`] by polling
//!    `ImageReader.acquireLatestImage()` through JNI and copying rows
//!    into a tightly-packed RGBA [`Frame`] — the same format every other
//!    backend produces, so the H.264 pipeline and the adaptive-quality
//!    controller consume it unchanged.
//!
//! The projection stays alive between sessions (consent is granted once
//! per service lifetime); `stop` tears it down and the service self-stops.
//! Every JNI call is exception-checked; every failure degrades to an
//! error/`None`, never a panic. The module is compiled on ALL platforms
//! (no `cfg` gating) so the desktop build type-checks the wiring — the
//! Android cross-check CI job additionally compiles it FOR Android.

use crate::source::{ScreenCapture, ScreenDesc};
use crate::{CaptureError, Frame};

/// (JavaVM handle address, activity GLOBAL-ref address,
/// CaptureActivity class GLOBAL-ref address) stashed by
/// `opacusr-android` at startup. All live for the whole process.
static CONTEXT: std::sync::OnceLock<(usize, usize, usize)> = std::sync::OnceLock::new();

/// The live projection: (vm handle, ImageReader global ref,
/// MediaProjection global ref, width, height, dpi).
static PROJECTION: std::sync::Mutex<Option<Projection>> = std::sync::Mutex::new(None);

/// The user denied the consent dialog (or the shim could not start the
/// capture) — remembered so the host spawn path can fail fast instead of
/// waiting out the full timeout again.
static START_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Projection {
    vm: usize,
    reader: usize,
    projection: usize,
    width: u32,
    height: u32,
    #[allow(dead_code)]
    dpi: u32,
}

// ---------------------------------------------------------------------------
// State plumbing (called from opacusr-android JNI callbacks / android_main)
// ---------------------------------------------------------------------------

/// Cache the process-wide JNI context. `vm` is the NDK JavaVM handle
/// address, `activity` a GLOBAL ref of the NativeActivity, `activity_cls`
/// a GLOBAL ref of `io.opacusr.app.CaptureActivity` (resolved through the
/// activity's class loader — see the module docs).
pub fn set_context(vm: usize, activity: usize, capture_activity_cls: usize) {
    let _ = CONTEXT.set((vm, activity, capture_activity_cls));
}

/// Called by `Java_io_opacusr_app_CaptureService_nativeProjectionStarted`
/// once the shim has the VirtualDisplay running. `reader` and
/// `projection` must already be GLOBAL refs (created by the caller while
/// the JNI env was live).
pub fn set_active(vm: usize, reader: usize, projection: usize, width: i32, height: i32, dpi: i32) {
    START_FAILED.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut guard = match PROJECTION.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    *guard = Some(Projection {
        vm,
        reader,
        projection,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
        dpi: dpi.max(0) as u32,
    });
    tracing::info!(target: "opacusr", "android projection started: {width}x{height}@{dpi}dpi");
}

/// Called by `Java_..._nativeProjectionStopped` (the MediaProjection
/// callback fired onStop — user revoked or the service died).
pub fn deactivate() {
    if let Ok(mut guard) = PROJECTION.lock() {
        *guard = None;
    }
    tracing::info!(target: "opacusr", "android projection stopped");
}

/// Called by `Java_io_opacusr_app_CaptureActivity_nativeProjectionFailed`
/// (consent denied / service could not start).
pub fn mark_failed() {
    START_FAILED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// `true` while the VirtualDisplay is producing frames.
pub fn is_active() -> bool {
    PROJECTION.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// `true` when the last consent attempt was denied (UI hint).
pub fn start_failed() -> bool {
    START_FAILED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Clear the «user denied» latch (before a new attempt).
fn reset_failed() {
    START_FAILED.store(false, std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// JNI plumbing
// ---------------------------------------------------------------------------

/// An attached JNIEnv + vtable pointer (the opacusr-android discipline:
/// deref the NDK `_JavaVM` handle ONCE for the vtable, pass the HANDLE
/// back as the first argument).
struct Jni {
    env: *mut jni_sys::JNIEnv,
    iface: *const jni_sys::JNINativeInterface_,
}

impl Jni {
    fn attach(vm_addr: usize) -> Option<Self> {
        unsafe {
            let vm: jni_sys::JavaVM = *(vm_addr as *mut jni_sys::JavaVM);
            if vm.is_null() {
                tracing::warn!(target: "opacusr", "projection: JavaVM vtable null");
                return None;
            }
            let mut env: *mut jni_sys::JNIEnv = std::ptr::null_mut();
            let res = ((*vm).v1_1.AttachCurrentThread)(
                vm_addr as *mut jni_sys::JavaVM,
                (&mut env) as *mut *mut jni_sys::JNIEnv as *mut *mut core::ffi::c_void,
                std::ptr::null_mut(),
            );
            if res != jni_sys::JNI_OK || env.is_null() {
                tracing::warn!(target: "opacusr", "projection: AttachCurrentThread failed ({res})");
                return None;
            }
            let iface: *const jni_sys::JNINativeInterface_ = *env;
            Some(Self { env, iface })
        }
    }

    fn check(&self, what: &str) -> bool {
        unsafe {
            if ((*self.iface).v1_6.ExceptionCheck)(self.env) {
                ((*self.iface).v1_6.ExceptionClear)(self.env);
                tracing::warn!(target: "opacusr", "projection: Java exception during {what}");
                false
            } else {
                true
            }
        }
    }

    /// Framework classes only — APK classes must come from the cached
    /// CaptureActivity class ref (see `set_context`).
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

    fn method(
        &self,
        cls: jni_sys::jclass,
        name: &core::ffi::CStr,
        sig: &core::ffi::CStr,
    ) -> Option<jni_sys::jmethodID> {
        unsafe {
            let mid = ((*self.iface).v1_6.GetMethodID)(self.env, cls, name.as_ptr(), sig.as_ptr());
            if self.check("GetMethodID") && !mid.is_null() {
                Some(mid)
            } else {
                None
            }
        }
    }

    fn call_object(&self, obj: jni_sys::jobject, mid: jni_sys::jmethodID) -> jni_sys::jobject {
        unsafe { ((*self.iface).v1_6.CallObjectMethod)(self.env, obj, mid) }
    }

    fn call_int(&self, obj: jni_sys::jobject, mid: jni_sys::jmethodID) -> i32 {
        unsafe { ((*self.iface).v1_6.CallIntMethod)(self.env, obj, mid) }
    }

    fn call_void(&self, obj: jni_sys::jobject, mid: jni_sys::jmethodID) {
        unsafe { ((*self.iface).v1_6.CallVoidMethod)(self.env, obj, mid) }
    }

    fn delete_local(&self, obj: jni_sys::jobject) {
        unsafe { ((*self.iface).v1_6.DeleteLocalRef)(self.env, obj) }
    }
}

/// Launch `io.opacusr.app.CaptureActivity` (the consent dialog). Safe to
/// call from any thread; a no-op when [`set_context`] was not called
/// (desktop) or the shim context is gone.
pub fn request_start() {
    reset_failed();
    let Some((vm, activity, cls)) = CONTEXT.get().copied() else {
        tracing::warn!(target: "opacusr", "projection: no JNI context (desktop build?)");
        return;
    };
    let Some(j) = Jni::attach(vm) else {
        return;
    };
    unsafe {
        let Some(intent_cls) = j.find_class(c"android/content/Intent") else {
            return;
        };
        let ctor = j.method(
            intent_cls,
            c"<init>",
            c"(Landroid/content/Context;Ljava/lang/Class;)V",
        );
        let Some(ctor) = ctor else {
            j.delete_local(intent_cls);
            return;
        };
        let intent = ((*j.iface).v1_6.NewObject)(
            j.env,
            intent_cls,
            ctor,
            activity as jni_sys::jobject,
            cls as jni_sys::jobject,
        );
        if !j.check("new Intent") || intent.is_null() {
            j.delete_local(intent_cls);
            return;
        }
        j.delete_local(intent_cls);
        // Activity.startActivity — the shim dialog comes up on top of the UI.
        let act_cls = ((*j.iface).v1_6.GetObjectClass)(j.env, activity as jni_sys::jobject);
        if j.check("GetObjectClass(activity)") && !act_cls.is_null() {
            if let Some(start) = j.method(act_cls, c"startActivity", c"(Landroid/content/Intent;)V")
            {
                j.call_void(activity as jni_sys::jobject, start);
                j.check("startActivity");
            }
            j.delete_local(act_cls);
        }
        j.delete_local(intent);
    }
}

/// Stop the live projection (`MediaProjection.stop()`); the shim's
/// `onStop` callback then self-stops the foreground service.
pub fn stop() {
    let Some(p) = PROJECTION.lock().ok().and_then(|mut g| g.take()) else {
        return;
    };
    let Some(j) = Jni::attach(p.vm) else {
        return;
    };
    unsafe {
        let mp_cls = ((*j.iface).v1_6.GetObjectClass)(j.env, p.projection as jni_sys::jobject);
        if j.check("GetObjectClass(projection)") && !mp_cls.is_null() {
            if let Some(mid) = j.method(mp_cls, c"stop", c"()V") {
                j.call_void(p.projection as jni_sys::jobject, mid);
                j.check("MediaProjection.stop");
            }
            j.delete_local(mp_cls);
        }
    }
}

// ---------------------------------------------------------------------------
// ScreenCapture backend
// ---------------------------------------------------------------------------

/// Captures the mirrored display by polling the shim's `ImageReader`.
/// Blocking per frame (like the DXGI/X11 backends): one JNI round trip
/// plus a row-wise copy of the RGBA buffer.
pub struct AndroidCapture {
    width: u32,
    height: u32,
    /// Last successfully read frame — served when the system has not
    /// produced a new image yet (idle screen) or during the brief gap
    /// right after the VirtualDisplay comes up.
    last: Option<Frame>,
}

impl AndroidCapture {
    /// Cheap constructor: the real state lives in the global [`PROJECTION`].
    pub fn new() -> Self {
        let (w, h) = dims();
        Self {
            width: w,
            height: h,
            last: None,
        }
    }
}

impl Default for AndroidCapture {
    fn default() -> Self {
        Self::new()
    }
}

fn dims() -> (u32, u32) {
    PROJECTION
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| (p.width, p.height)))
        .unwrap_or((1080, 2400))
}

impl ScreenCapture for AndroidCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        let (vm, reader, width, height) = {
            let guard = PROJECTION
                .lock()
                .map_err(|_| CaptureError::Other("projection lock poisoned".into()))?;
            let Some(p) = guard.as_ref() else {
                // The user revoked the projection mid-session.
                return Err(CaptureError::Unsupported);
            };
            (p.vm, p.reader, p.width, p.height)
        };
        let w = width;
        let h = height;
        let Some(j) = Jni::attach(vm) else {
            return Err(CaptureError::Other("JNI attach failed".into()));
        };
        unsafe {
            let reader_obj = reader as jni_sys::jobject;
            let reader_cls = ((*j.iface).v1_6.GetObjectClass)(j.env, reader_obj);
            if !j.check("GetObjectClass(reader)") || reader_cls.is_null() {
                return Err(CaptureError::Other("reader class".into()));
            }
            let acquire = j.method(
                reader_cls,
                c"acquireLatestImage",
                c"()Landroid/media/Image;",
            );
            j.delete_local(reader_cls);
            let Some(acquire) = acquire else {
                return Err(CaptureError::Other("acquireLatestImage lookup".into()));
            };
            let image = j.call_object(reader_obj, acquire);
            if !j.check("acquireLatestImage") {
                return Err(CaptureError::Other("acquireLatestImage exception".into()));
            }
            if image.is_null() {
                // No fresh image: re-serve the cached frame (idle screen)
                // or a black placeholder until the first frame arrives.
                if let Some(f) = &self.last {
                    return Ok(f.clone());
                }
                return Ok(Frame::new(w, h, vec![0u8; (w as usize) * (h as usize) * 4]));
            }

            let planes_mid =
                match j.method(image, c"getPlanes", c"()[Ljava/android/media/Image$Plane;") {
                    Some(m) => m,
                    None => {
                        j.delete_local(image);
                        return Err(CaptureError::Other("getPlanes lookup".into()));
                    }
                };
            let planes = j.call_object(image, planes_mid);
            if !j.check("getPlanes") {
                j.delete_local(image);
                return Err(CaptureError::Other("getPlanes exception".into()));
            }
            let plane = if planes.is_null() {
                std::ptr::null_mut()
            } else {
                ((*j.iface).v1_6.GetObjectArrayElement)(j.env, planes, 0)
            };
            j.delete_local(planes);
            if plane.is_null() {
                j.delete_local(image);
                if let Some(f) = &self.last {
                    return Ok(f.clone());
                }
                return Err(CaptureError::Other("no image plane".into()));
            }

            let plane_cls = ((*j.iface).v1_6.GetObjectClass)(j.env, plane);
            let row_stride = if !plane_cls.is_null() {
                let mid = j.method(plane_cls, c"getRowStride", c"()I");
                let v = mid.map(|m| j.call_int(plane, m)).unwrap_or((w as i32) * 4);
                j.delete_local(plane_cls);
                v.max((w as i32) * 4) as usize
            } else {
                ((w as i32) * 4).max(1) as usize
            };
            let buffer = match j.method(plane, c"getBuffer", c"()Ljava/nio/ByteBuffer;") {
                Some(mid) if !j.call_object(plane, mid).is_null() => j.call_object(plane, mid),
                _ => {
                    j.delete_local(plane);
                    j.delete_local(image);
                    return Err(CaptureError::Other("plane buffer".into()));
                }
            };
            let addr = ((*j.iface).v1_6.GetDirectBufferAddress)(j.env, buffer);
            j.delete_local(buffer);
            j.delete_local(plane);
            if addr.is_null() {
                j.delete_local(image);
                return Err(CaptureError::Other("plane buffer not direct".into()));
            }

            // Row-wise copy: the ImageReader stride can exceed width*4
            // (hardware padding). RGBA_8888 comes out byte-order RGBA.
            let row_bytes = (w as usize) * 4;
            let mut rgba = vec![0u8; row_bytes * (h as usize)];
            for row in 0..h as usize {
                let src = addr.add(row * row_stride) as *const u8;
                let dst = row * row_bytes;
                rgba[dst..dst + row_bytes]
                    .copy_from_slice(std::slice::from_raw_parts(src, row_bytes));
            }

            // Close the image so the reader can hand out the next one.
            if let Some(close) = j.method(image, c"close", c"()V") {
                j.call_void(image, close);
                j.check("image.close");
            }
            j.delete_local(image);

            let frame = Frame::new(w, h, rgba);
            self.last = Some(frame.clone());
            self.width = w;
            self.height = h;
            Ok(frame)
        }
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        Ok((self.width, self.height))
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        vec![ScreenDesc {
            id: 0,
            name: "screen".into(),
            w: self.width,
            h: self.height,
        }]
    }

    fn select_screen(&mut self, _id: u32) -> Result<bool, CaptureError> {
        Ok(false) // single mirrored display
    }

    fn backend(&self) -> &'static str {
        "android-mediaprojection"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_are_inactive_and_safe() {
        // No JNI context on the desktop test host: everything must
        // answer safely instead of panicking.
        assert!(!is_active());
        assert!(!start_failed());
        deactivate();
        stop();
        request_start(); // no-op without set_context
        let mut cap = AndroidCapture::new();
        let err = cap.capture().unwrap_err();
        assert!(matches!(err, CaptureError::Unsupported));
    }
}
