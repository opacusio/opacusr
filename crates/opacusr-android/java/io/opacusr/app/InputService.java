package io.opacusr.app;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.graphics.Path;
import android.os.Build;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;

/**
 * Remote input for the phone-as-host mode: an AccessibilityService the
 * user enables once in system settings (Settings -> Accessibility ->
 * OpacusR). The native agent drives it through the static bridge below
 * (raw JNI, see opacusr-input/src/android_bridge.rs).
 *
 * Touch model:
 *   - left button down/move/up  -> a continued stroke (drag): one
 *     GestureDescription per move segment, chained via
 *     StrokeDescription.continueStroke (API 26+, Android 8+);
 *   - right button              -> long press (the standard touch
 *     equivalent of right click);
 *   - wheel                     -> a short swipe (protocol positive dy
 *     = wheel UP, so finger goes DOWN to reveal content above);
 *   - Escape                    -> GLOBAL_ACTION_BACK (AnyDesk-style:
 *     Esc = Back on an Android host);
 *   - text                      -> clipboard + ACTION_PASTE at the
 *     caret of the focused editable node (clobbers the clipboard —
 *     documented limitation, an IME service is the v2 fix).
 *
 * Hover moves (no button held) are dropped in the native layer: touch
 * screens have no hover.
 */
public final class InputService extends AccessibilityService {

    /** Set while the user has the service enabled; polled by ready(). */
    static volatile InputService instance;
    /** Drag continuation: the stroke still running, willContinue=true. */
    static volatile Stroke pending;

    /** A dispatched stroke + its continuity + its current end point. */
    static final class Stroke {
        final AccessibilityService.StrokeDescription desc;
        final boolean continuing;
        final float x, y;
        Stroke(AccessibilityService.StrokeDescription d, boolean c, float x, float y) {
            desc = d;
            continuing = c;
            this.x = x;
            this.y = y;
        }
    }

    static final long SEG_MS = 60;        // drag segment length
    static final long TAP_MS = 30;
    static final long LONG_PRESS_MS = 550;
    static final long SCROLL_MS = 160;
    static final float SCROLL_MIN_PX = 40f;
    static final float SCROLL_MAX_PX = 400f;
    static final float SCROLL_PX_PER_NOTCH = 26f;

    @Override
    public void onServiceConnected() {
        instance = this;
        pending = null;
    }

    @Override
    public boolean onUnbind(Intent intent) {
        instance = null;
        pending = null;
        return super.onUnbind(intent);
    }

    @Override
    public void onDestroy() {
        instance = null;
        pending = null;
    }

    @Override
    public void onInterrupt() {
        // Nothing to interrupt: we dispatch gestures, we do not listen.
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        // Intentionally empty: the config requests typeWindowStateChanged
        // only to keep the service cheap; input is push-only.
    }

    // ------------------------------------------------------- bridge API
    // Static, called from any native thread via JNI. Every method is
    // best-effort: false = service off, gesture rejected or cancelled.

    public static boolean ready() {
        return instance != null;
    }

    /** Left button down at (x, y): starts a continued drag stroke. */
    public static boolean dragStart(float x, float y) {
        InputService s = instance;
        if (s == null || Build.VERSION.SDK_INT < 26) return false;
        Path p = new Path();
        p.moveTo(x, y);
        Stroke st = new Stroke(
                new AccessibilityService.StrokeDescription(p, 0, SEG_MS, true),
                true, x, y);
        return s.dispatch(st);
    }

    /** Left button move: continue the running drag stroke. */
    public static boolean dragMove(float x, float y) {
        InputService s = instance;
        if (s == null || Build.VERSION.SDK_INT < 26) return false;
        Stroke prev = pending;
        if (prev == null) return false;
        Path p = new Path();
        p.moveTo(prev.x, prev.y);
        p.lineTo(x, y);
        Stroke st = new Stroke(
                prev.desc.continueStroke(p, 0, SEG_MS, true), true, x, y);
        return s.dispatch(st);
    }

    /** Left button up: finish the drag stroke. */
    public static boolean dragEnd() {
        InputService s = instance;
        if (s == null || Build.VERSION.SDK_INT < 26) return false;
        Stroke prev = pending;
        if (prev == null) return false;
        Path p = new Path();
        p.moveTo(prev.x, prev.y);
        // Nudge by a hair so the final segment is not degenerate.
        p.lineTo(prev.x + 0.01f, prev.y + 0.01f);
        Stroke st = new Stroke(
                prev.desc.continueStroke(p, 0, 40, false), false, prev.x, prev.y);
        return s.dispatch(st);
    }

    /** Right button: long press = the touch equivalent of right click. */
    public static boolean longPress(float x, float y) {
        InputService s = instance;
        if (s == null) return false;
        if (pending != null) return false; // do not cancel a running drag
        Path p = new Path();
        p.moveTo(x, y);
        return s.dispatch(new Stroke(
                new AccessibilityService.StrokeDescription(p, 0, LONG_PRESS_MS, false),
                false, x, y));
    }

    /** Plain tap (kept for completeness; the viewer path uses drag). */
    public static boolean tap(float x, float y) {
        InputService s = instance;
        if (s == null) return false;
        if (pending != null) return false;
        Path p = new Path();
        p.moveTo(x, y);
        return s.dispatch(new Stroke(
                new AccessibilityService.StrokeDescription(p, 0, TAP_MS, false),
                false, x, y));
    }

    /**
     * Wheel emulation: a short swipe at (x, y). Protocol positive dy =
     * wheel UP (see uinput.rs) — to reveal content ABOVE, the finger
     * swipes DOWN. Positive dx = wheel right — finger swipes RIGHT to
     * reveal content on the right.
     */
    public static boolean scroll(float x, float y, float dx, float dy) {
        InputService s = instance;
        if (s == null) return false;
        if (pending != null) return false; // do not cancel a running drag
        if (dx == 0 && dy == 0) return true;
        float dist = (Math.abs(dx) + Math.abs(dy)) * SCROLL_PX_PER_NOTCH;
        if (dist < SCROLL_MIN_PX) dist = SCROLL_MIN_PX;
        if (dist > SCROLL_MAX_PX) dist = SCROLL_MAX_PX;
        float sx = x, sy = y, ex = x, ey = y;
        if (dy != 0) {
            float d = dy > 0 ? -dist : dist; // up-notch: finger starts BELOW
            sy = y + d;
            ey = y - d;
        } else {
            float d = dx > 0 ? -dist : dist; // right-notch: finger starts LEFT
            sx = x + d;
            ex = x - d;
        }
        Path p = new Path();
        p.moveTo(sx, sy);
        p.lineTo(ex, ey);
        return s.dispatch(new Stroke(
                new AccessibilityService.StrokeDescription(p, 0, SCROLL_MS, false),
                false, ex, ey));
    }

    /** Escape on the viewer = Back on the phone. */
    public static boolean goBack() {
        InputService s = instance;
        if (s == null) return false;
        return s.performGlobalAction(GLOBAL_ACTION_BACK);
    }

    /**
     * Type text: clipboard + paste at the caret. Overwrites the user's
     * clipboard (honest v1 limitation; needs an IME service to avoid).
     */
    public static boolean paste(String text) {
        InputService s = instance;
        if (s == null) return false;
        if (text == null || text.isEmpty()) return true;
        ClipboardManager cm =
                (ClipboardManager) s.getSystemService(CLIPBOARD_SERVICE);
        if (cm == null) return false;
        cm.setPrimaryClip(ClipData.newPlainText("opacusr", text));
        AccessibilityNodeInfo root = s.getRootInActiveWindow();
        if (root == null) return false;
        AccessibilityNodeInfo target = root.findFocus(FOCUS_INPUT);
        if (target == null || !target.isEditable()) {
            target = findEditable(root, 0);
        }
        if (target == null) return false;
        return target.performAction(AccessibilityNodeInfo.ACTION_PASTE);
    }

    // ------------------------------------------------------- internals

    boolean dispatch(Stroke st) {
        pending = st.continuing ? st : null;
        GestureDescription.Builder b = new GestureDescription.Builder();
        b.addStroke(st.desc);
        return dispatchGesture(b.build(), callback, null);
    }

    final GestureResultCallback callback = new GestureResultCallback() {
        @Override
        public void onCompleted(GestureDescription gestureDescription) {
            // The stroke finished normally; continuity is managed by the
            // next event (dragMove chains from `pending`).
        }

        @Override
        public void onCancelled(GestureDescription gestureDescription) {
            // A cancelled continuation kills the drag; clear so the next
            // dragMove does not chain from a dead stroke.
            pending = null;
        }
    };

    /** Depth-limited search for the first editable node (paste fallback). */
    static AccessibilityNodeInfo findEditable(AccessibilityNodeInfo node, int depth) {
        if (node == null || depth > 25) return null;
        if (node.isEditable()) return node;
        for (int i = 0; i < node.getChildCount(); i++) {
            AccessibilityNodeInfo found = findEditable(node.getChild(i), depth + 1);
            if (found != null) return found;
        }
        return null;
    }
}
