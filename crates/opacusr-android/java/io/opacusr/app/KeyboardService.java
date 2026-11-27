package io.opacusr.app;

import android.inputmethodservice.InputMethodService;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Remote keyboard for the phone-as-host mode (v2 of text input): a
 * SILENT InputMethodService — no visible keyboard UI — the user selects
 * once as the current input method (Settings -> System -> Languages &
 * input -> On-screen keyboard). The native agent drives it through the
 * static bridge below (raw JNI, see
 * opacusr-input/src/android_bridge.rs).
 *
 * What this buys over the accessibility paste path (InputService.java):
 *   - text lands via InputConnection.commitText — ANY Unicode (emoji,
 *     CJK, NUL-safe UTF-16 strings) WITHOUT clobbering the clipboard;
 *   - real key events into the focused editor via
 *     InputConnection.sendKeyEvent (Enter, Tab, Backspace, Delete,
 *     arrows, Home/End, PgUp/PgDn, F1..F12) with SHIFT/CTRL/ALT/META
 *     state (Ctrl+C/V/X/A work in editors that handle them).
 *
 * Escape stays GLOBAL_ACTION_BACK (a system action, editors cannot
 * consume Esc) — that still rides the accessibility shim.
 *
 * When the editor focus is absent (or the IME is not the selected
 * keyboard) the native layer falls back to the clipboard+paste path for
 * text and reports keys as unsupported — every degradation is honest.
 *
 * Known tradeoff (same as AnyDesk): while OpacusR is the selected
 * keyboard, LOCAL typing on the phone does nothing — switch keyboards
 * from the quick settings to type locally. The service shows nothing
 * (onShowInputRequested = false), so selecting it changes no UI.
 */
public final class KeyboardService extends InputMethodService {

    /** Set while the system has this IME bound; polled by imeReady(). */
    static volatile KeyboardService instance;

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    /** Bounded wait for a main-thread hop; a busy UI thread degrades
     *  the call to false instead of hanging a native thread. */
    private static final long HOP_WAIT_MS = 700;

    /** One editor-bound job, run on the service (main) thread. */
    private interface Job {
        boolean run();
    }

    @Override
    public void onCreate() {
        super.onCreate();
        instance = this;
    }

    @Override
    public void onDestroy() {
        instance = null;
        super.onDestroy();
    }

    @Override
    public void onStartInput(EditorInfo attribute, boolean restarting) {
        super.onStartInput(attribute, restarting);
        // Nothing to keep: the bridge calls getCurrentInputConnection()
        // fresh on the main thread for every event — the connection can
        // be replaced by the framework at any input restart.
    }

    // ------------------------------------------------- invisible IME UI

    @Override
    public boolean onShowInputRequested(int flags, boolean configChange) {
        // Never show the keyboard window: the IME exists so remote
        // events have an InputConnection to ride, not for local typing.
        return false;
    }

    @Override
    public boolean onEvaluateFullscreenMode() {
        return false; // never the fullscreen extract editor
    }

    @Override
    public View onCreateInputView() {
        // Defensive: if some OEM path forces view creation, make it a
        // zero-interaction empty view instead of a standard keyboard.
        return new View(this);
    }

    // ------------------------------------------------------- bridge API
    // Static, called from any native thread via JNI. Every method is
    // best-effort: false = IME not selected, no editor focus, or the
    // main thread did not answer within HOP_WAIT_MS.

    /** Cheap check (no thread hop): is the OpacusR keyboard bound? */
    public static boolean imeReady() {
        return instance != null;
    }

    /**
     * Commit text at the caret of the focused editor. Any Unicode:
     * jstring arrives as UTF-16 (the native side uses NewString, NOT
     * the modified-UTF-8 NewStringUTF — emoji and NUL survive).
     */
    public static boolean imeType(final String text) {
        return onMain(new Job() {
            @Override
            public boolean run() {
                KeyboardService s = instance; // may unbind mid-flight
                if (s == null) return false;
                InputConnection ic = s.getCurrentInputConnection();
                if (ic == null || text == null || text.isEmpty()) return false;
                return ic.commitText(text, 1);
            }
        });
    }

    /**
     * Send a key event (down + up) into the focused editor with the
     * given meta state (META_SHIFT_ON / META_CTRL_ON / META_ALT_ON /
     * META_META_ON bits — see KeyEvent). FLAG_SOFT_KEYBOARD marks the
     * event as coming from an IME so editors treat it like typing.
     */
    public static boolean imeKey(final int keyCode, final int metaState) {
        return onMain(new Job() {
            @Override
            public boolean run() {
                KeyboardService s = instance; // may unbind mid-flight
                if (s == null) return false;
                InputConnection ic = s.getCurrentInputConnection();
                if (ic == null) return false;
                long now = SystemClock.uptimeMillis();
                KeyEvent down = new KeyEvent(now, now, KeyEvent.ACTION_DOWN,
                        keyCode, 0, metaState, 0, 0,
                        KeyEvent.FLAG_SOFT_KEYBOARD | KeyEvent.FLAG_KEEP_TOUCH_MODE);
                boolean ok = ic.sendKeyEvent(down);
                KeyEvent up = new KeyEvent(now, now + 1, KeyEvent.ACTION_UP,
                        keyCode, 0, metaState, 0, 0,
                        KeyEvent.FLAG_SOFT_KEYBOARD | KeyEvent.FLAG_KEEP_TOUCH_MODE);
                ok = ic.sendKeyEvent(up) && ok;
                return ok;
            }
        });
    }

    // ------------------------------------------------------- internals

    /**
     * Run a job on the main (service) thread and wait for it. The
     * InputConnection is documented as usable from the IME's thread —
     * hopping keeps us correct on every Android version instead of
     * racing the framework. Direct call when already on main.
     */
    static boolean onMain(Job job) {
        KeyboardService s = instance;
        if (s == null) return false;
        if (Looper.myLooper() == Looper.getMainLooper()) return job.run();
        final AtomicBoolean ok = new AtomicBoolean(false);
        final CountDownLatch latch = new CountDownLatch(1);
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                try {
                    ok.set(job.run());
                } catch (RuntimeException e) {
                    ok.set(false);
                } finally {
                    latch.countDown();
                }
            }
        });
        try {
            if (!latch.await(HOP_WAIT_MS, TimeUnit.MILLISECONDS)) return false;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return false;
        }
        return ok.get();
    }
}
