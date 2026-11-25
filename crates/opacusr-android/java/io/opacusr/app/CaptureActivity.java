package io.opacusr.app;

/**
 * MediaProjection consent shim (the only Java in the project — cargo-apk
 * 0.10 cannot declare extra activities/services, so CI compiles this file
 * into classes.dex and repackages the APK; see scripts/package_android.sh).
 *
 * NativeActivity cannot override onActivityResult, so the flow goes
 * through this translucent activity: Rust calls
 * opacusr_capture::android_projection::request_start() → this activity
 * pops the system consent dialog → on approval starts CaptureService
 * (foreground, type mediaProjection) which creates the VirtualDisplay
 * and hands the ImageReader to native code via JNI.
 */
public class CaptureActivity extends android.app.Activity {
    private static final int REQ = 1001;

    @Override
    protected void onCreate(android.os.Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Object o = getSystemService(MEDIA_PROJECTION_SERVICE);
        if (!(o instanceof android.media.projection.MediaProjectionManager)) {
            nativeProjectionFailed();
            finish();
            return;
        }
        android.media.projection.MediaProjectionManager mpm =
                (android.media.projection.MediaProjectionManager) o;
        try {
            startActivityForResult(mpm.createScreenCaptureIntent(), REQ);
        } catch (Exception e) {
            nativeProjectionFailed();
            finish();
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, android.content.Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ) {
            return;
        }
        if (resultCode == RESULT_OK && data != null) {
            android.content.Intent svc = new android.content.Intent(this, CaptureService.class);
            svc.putExtra("code", resultCode);
            svc.putExtra("data", data);
            if (android.os.Build.VERSION.SDK_INT >= 26) {
                startForegroundService(svc);
            } else {
                startService(svc);
            }
        } else {
            nativeProjectionFailed();
        }
        finish();
    }

    static native void nativeProjectionFailed();
}
