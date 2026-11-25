//! Runtime system-UI hooks from the UI layer into the Android glue
//! (`opacusr-android`): landscape orientation, immersive fullscreen and
//! the soft keyboard — the three phone controls the viewer needs while
//! a session is live («маленькое окно, не разворачивает, нет
//! управления»).
//!
//! Direction of the dependency: `opacusr-android` depends on
//! `opacusr-app`, so the app cannot call the JNI helpers directly.
//! Instead the glue registers plain function pointers here once, before
//! the event loop starts (the same pattern as
//! [`crate::android_insets::set_insets_px`], reversed). On desktop the
//! registry stays empty and every call below is a no-op, so the same
//! UI code runs on every platform.

use std::sync::OnceLock;

/// `on == true` → request sensor-landscape while a session is live;
/// `on == false` → restore full-sensor (both orientations).
pub type FnSetLandscape = fn(bool);
/// `on == true` → sticky immersive (system bars hidden); false → back
/// to the edge-to-edge state the insets module compensates for.
pub type FnSetImmersive = fn(bool);
/// `on == true` → open the soft keyboard; `on == false` → hide it.
pub type FnSetKeyboard = fn(bool);

struct Hooks {
    landscape: FnSetLandscape,
    immersive: FnSetImmersive,
    keyboard: FnSetKeyboard,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Registered by the Android glue in `android_main`, before the egui
/// event loop starts. Only the first registration wins (best-effort).
pub fn set_hooks(landscape: FnSetLandscape, immersive: FnSetImmersive, keyboard: FnSetKeyboard) {
    let _ = HOOKS.set(Hooks {
        landscape,
        immersive,
        keyboard,
    });
}

/// Test-only introspection for the single sequential test below.
#[cfg(test)]
pub(crate) fn hooks_registered() -> bool {
    HOOKS.get().is_some()
}

/// A desktop screen letterboxed into a portrait phone was the
/// «маленькое окно» complaint: start sessions in landscape (TeamViewer
/// behavior) and restore rotation on disconnect. No-op without the
/// Android glue.
pub fn set_landscape(on: bool) {
    if let Some(h) = HOOKS.get() {
        (h.landscape)(on);
    }
}

/// Sticky immersive while the viewer hides its chrome (⛶ button). Also
/// toggles the inset-panel compensation in
/// [`crate::android_insets`] — the system bars are gone in immersive,
/// so nothing must be reserved for them. No-op without the glue.
pub fn set_immersive(on: bool) {
    crate::android_insets::set_immersive(on);
    if let Some(h) = HOOKS.get() {
        (h.immersive)(on);
    }
}

/// Show/hide the soft keyboard (phones have no hardware keyboard; the
/// remote terminal and chat inputs are unusable without one). No-op
/// without the glue.
pub fn set_keyboard(on: bool) {
    if let Some(h) = HOOKS.get() {
        (h.keyboard)(on);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    static LAND: AtomicBool = AtomicBool::new(false);
    static IMM: AtomicBool = AtomicBool::new(false);
    static KB: AtomicBool = AtomicBool::new(false);

    fn land(on: bool) {
        LAND.store(on, Ordering::SeqCst);
    }
    fn imm(on: bool) {
        IMM.store(on, Ordering::SeqCst);
    }
    fn kb(on: bool) {
        KB.store(on, Ordering::SeqCst);
    }

    /// ONE sequential test (the hook registry is a process-global
    /// OnceLock — parallel tests would race over it): pre-registration
    /// calls must be safe no-ops, post-registration calls must reach
    /// the glue, and immersive must flip the inset-panel compensation.
    #[test]
    fn sysui_hook_lifecycle() {
        // Desktop / pre-glue: nothing registered, calls must not panic.
        assert!(!super::hooks_registered());
        super::set_landscape(true);
        super::set_keyboard(true);
        // set_immersive flips the insets compensation even without a
        // registered glue hook (the flag itself is harmless on desktop,
        // where the measured insets are (0, 0)); verify and clean up.
        super::set_immersive(true);
        assert!(crate::android_insets::immersive());
        super::set_immersive(false);
        assert!(!crate::android_insets::immersive());
        assert!(!super::hooks_registered());

        // Register and verify the round trip.
        super::set_hooks(land, imm, kb);
        assert!(super::hooks_registered());

        super::set_landscape(true);
        assert!(LAND.load(Ordering::SeqCst));
        super::set_landscape(false);
        assert!(!LAND.load(Ordering::SeqCst));

        super::set_immersive(true);
        assert!(IMM.load(Ordering::SeqCst));
        assert!(crate::android_insets::immersive());
        super::set_immersive(false);
        assert!(!IMM.load(Ordering::SeqCst));
        assert!(!crate::android_insets::immersive());

        super::set_keyboard(true);
        assert!(KB.load(Ordering::SeqCst));
        super::set_keyboard(false);
        assert!(!KB.load(Ordering::SeqCst));
    }
}
