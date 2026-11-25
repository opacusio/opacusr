package io.opacusr.app;

/**
 * Foreground service holding the MediaProjection session (the consent
 * result data flows here from {@link CaptureActivity}). Creates the
 * ImageReader (RGBA_8888) + VirtualDisplay and hands both to native code
 * through static JNI callbacks implemented in libopacusr_android.so
 * (already loaded by NativeActivity in this process). The projection
 * intentionally STAYS UP between PC sessions — consent is granted once
 * per app lifetime and later connections start streaming instantly.
 */
public class CaptureService extends android.app.Service {
    private android.media.projection.MediaProjection mp;
    private android.hardware.display.VirtualDisplay vd;
    private android.media.ImageReader reader;

    @Override
    public android.os.IBinder onBind(android.content.Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(android.content.Intent intent, int flags, int startId) {
        if (intent == null) {
            stopSelf();
            return START_NOT_STICKY;
        }
        int code = intent.getIntExtra("code", -1);
        android.content.Intent data;
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            data = intent.getParcelableExtra("data", android.content.Intent.class);
        } else {
            data = (android.content.Intent) intent.getParcelableExtra("data");
        }
        startWithNotification();
        if (data == null) {
            nativeProjectionFailed();
            stopSelf();
            return START_NOT_STICKY;
        }
        try {
            Object o = getSystemService(MEDIA_PROJECTION_SERVICE);
            android.media.projection.MediaProjectionManager mpm =
                    (android.media.projection.MediaProjectionManager) o;
            mp = mpm.getMediaProjection(code, data);
            if (mp == null) {
                nativeProjectionFailed();
                stopSelf();
                return START_NOT_STICKY;
            }
            mp.registerCallback(new android.media.projection.MediaProjection.Callback() {
                @Override
                public void onStop() {
                    cleanup();
                    nativeProjectionStopped();
                }
            }, null);
            int dpi = getResources().getDisplayMetrics().densityDpi;
            android.graphics.Point size = new android.graphics.Point();
            ((android.view.WindowManager) getSystemService(WINDOW_SERVICE))
                    .getDefaultDisplay().getRealSize(size);
            int w = size.x;
            int h = size.y;
            reader = android.media.ImageReader.newInstance(
                    w, h, android.graphics.PixelFormat.RGBA_8888, 2);
            vd = mp.createVirtualDisplay("opacusr-screen", w, h, dpi,
                    android.hardware.display.DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    reader.getSurface(), null, null);
            nativeProjectionStarted(w, h, dpi, reader, mp);
        } catch (Exception e) {
            nativeProjectionFailed();
            stopSelf();
            return START_NOT_STICKY;
        }
        return START_STICKY;
    }

    private void startWithNotification() {
        String channel = "opacusr_capture";
        android.app.Notification.Builder b;
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            android.app.NotificationManager nm =
                    (android.app.NotificationManager) getSystemService(NOTIFICATION_SERVICE);
            nm.createNotificationChannel(new android.app.NotificationChannel(
                    channel, "Трансляция экрана", android.app.NotificationManager.IMPORTANCE_LOW));
            b = new android.app.Notification.Builder(this, channel);
        } else {
            b = new android.app.Notification.Builder(this);
        }
        int icon = getResources().getIdentifier("ic_launcher", "mipmap", getPackageName());
        if (icon == 0) {
            icon = android.R.drawable.ic_menu_share;
        }
        b.setSmallIcon(icon)
                .setContentTitle("OpacusR")
                .setContentText("Экран телефона транслируется");
        android.app.Notification n = b.build();
        if (android.os.Build.VERSION.SDK_INT >= 29) {
            startForeground(1, n,
                    android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
        } else {
            startForeground(1, n);
        }
    }

    private void cleanup() {
        if (vd != null) {
            try { vd.release(); } catch (Exception e) { }
            vd = null;
        }
        if (reader != null) {
            try { reader.close(); } catch (Exception e) { }
            reader = null;
        }
        if (mp != null) {
            try { mp.stop(); } catch (Exception e) { }
            mp = null;
        }
    }

    @Override
    public void onDestroy() {
        cleanup();
        super.onDestroy();
    }

    static native void nativeProjectionStarted(
            int w, int h, int dpi, android.media.ImageReader reader,
            android.media.projection.MediaProjection mp);

    static native void nativeProjectionStopped();
}
