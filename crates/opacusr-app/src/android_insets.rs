//! Android system-bar insets for the edge-to-edge NativeActivity.
//!
//! The APK runs a NativeActivity whose window spans the whole display;
//! the ROM (even with cargo-apk's default fullscreen theme) draws the
//! STATUS bar (clock, signal, battery) and the NAVIGATION bar straight
//! over our pixels. Anything living in the app header top row —
//! «⏻ Закрыть», the version marker, half of the ghost logo — sits
//! underneath the system icons and is both invisible and untouchable
//! (user report, 0.11.19: «не вижу кнопку закрытия»). The bottom
//! gesture/3-button bar covers the history footer the same way.
//!
//! cargo-apk 0.10 drops the manifest `theme` key (verified: the built
//! AndroidManifest.xml contains no theme attribute at all), so the
//! platform cannot reserve the bar areas for us. Fix: measure the real
//! bar heights once at startup (JNI, system dimen resources) and
//! reserve matching egui panels FIRST every frame, so every other
//! panel — home top bar, session toolbar, settings drawer, central
//! content — lays out inside the safe area.
//!
//! Desktop/other targets: the statics stay empty and the helper is a
//! no-op, so the same call runs everywhere.

use std::sync::OnceLock;

/// Device-pixel heights of (status bar, navigation bar), set once by
/// the Android glue before the egui event loop starts. (0, 0) means
/// «unknown» — no panels are added and rendering matches the old
/// edge-to-edge behavior.
static BAR_PX: OnceLock<(i32, i32)> = OnceLock::new();

/// Called by the Android glue with the measured system bar heights in
/// DEVICE PIXELS (Resources#getDimensionPixelSize). Best-effort: the
/// glue only calls this when both heights were resolved successfully.
pub fn set_insets_px(status_px: i32, nav_px: i32) {
    let _ = BAR_PX.set(clamp_pair(status_px, nav_px));
}

/// Negative heights are meaningless; clamp to zero.
fn clamp_pair(status_px: i32, nav_px: i32) -> (i32, i32) {
    (status_px.max(0), nav_px.max(0))
}

/// Measured (status, navigation) heights in device pixels; (0, 0)
/// before the Android glue ran (desktop) or when the JNI query failed.
pub fn insets_px() -> (i32, i32) {
    BAR_PX.get().copied().unwrap_or((0, 0))
}

/// Reserve the system-bar areas as top/bottom spacer panels. Must be
/// called at the very start of the frame, BEFORE any other panel, so
/// the home top bar, session toolbar, settings drawer and central
/// content all stay clear of the system bars.
pub fn show_system_bar_insets(ctx: &egui::Context) {
    let (status_px, nav_px) = insets_px();
    if status_px <= 0 && nav_px <= 0 {
        return;
    }
    let ppp = ctx.pixels_per_point();
    if status_px > 0 {
        egui::TopBottomPanel::top("inset_status_bar")
            .exact_height(status_px as f32 / ppp)
            .show(ctx, |_| {});
    }
    if nav_px > 0 {
        egui::TopBottomPanel::bottom("inset_nav_bar")
            .exact_height(nav_px as f32 / ppp)
            .show(ctx, |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_pair;

    #[test]
    fn clamp_rejects_negative_heights() {
        assert_eq!(clamp_pair(-5, 48), (0, 48));
        assert_eq!(clamp_pair(24, -1), (24, 0));
        assert_eq!(clamp_pair(0, 0), (0, 0));
    }

    #[test]
    fn defaults_to_no_insets_and_helper_is_a_pure_noop() {
        // Desktop never runs the Android glue, so (unless a test called
        // set_insets_px first — none does, clamping is tested on the
        // pure function) the statics are unset: (0, 0) and the frame
        // helper must early-return WITHOUT touching any panel code.
        // That matters because adding a panel outside a full egui pass
        // panics — the early return is what makes this call safe in a
        // bare-Context test.
        assert_eq!(super::insets_px(), (0, 0));
        let ctx = egui::Context::default();
        super::show_system_bar_insets(&ctx);
    }
}
