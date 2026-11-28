import UIKit

/// Minimal UIKit entry for the OpacusR iOS viewer shell.
///
/// The Rust side (crates/opacusr-ios) exports `opacusr_ios_run` — an
/// `extern "C"` function that sets up tracing/config and calls
/// `eframe::run_native` (egui/winit). winit's iOS backend drives the
/// main CFRunLoop from there for the rest of the process lifetime, so
/// the hand-off must happen on the MAIN thread.
///
/// Hand-off timing: blocking `didFinishLaunchingWithOptions` would keep
/// UIKit from starting the run loop at all, so the call is queued with
/// `DispatchQueue.main.async` — it executes inside the first run-loop
/// iteration after launch returns, and never comes back (by design).
///
/// No storyboard / scene manifest on purpose: winit creates and owns
/// its own UIWindow; Swift only boots the process.
@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        DispatchQueue.main.async {
            opacusr_ios_run()
        }
        return true
    }
}
