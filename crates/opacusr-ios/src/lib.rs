//! OpacusR iOS viewer: static-lib shell around the shared egui UI.
//!
//! The iPhone works as a VIEWER: it connects to an OpacusR host by
//! partner ID (TCP relay) or ip:port (LAN direct) and shows the shared
//! screen with touch controls. Hosting FROM iOS is a separate roadmap
//! item (ReplayKit broadcast capture); the embedded agent answers
//! incoming WebRTC offers with an explicit `Reject` there.
//!
//! Transport note: the WebRTC data-channel stack is not compiled for
//! iOS — webrtc 0.14 → webrtc-util 0.12 → nix 0.26 references
//! `libc::sockaddr_ctl`, which does not exist for iOS (see
//! opacusr-transport/Cargo.toml). `SessionClient::connect_via_server`
//! degrades `TransportChoice::Auto` to the TCP relay automatically.
//!
//! Building: `cargo build -p opacusr-ios --release
//! --target aarch64-apple-ios` (macOS host with Xcode) produces
//! `libopacusr_ios.a`. An Xcode project that links it, wires UIKit
//! lifecycle and signs the bundle is the follow-up step; CI cross-checks
//! the whole graph today (proto → crypto → transport → codec → client →
//! agent → app → ios, all cfg-gated correctly for aarch64-apple-ios).
//!
//! Desktop note: on non-iOS targets this crate is an empty staticlib
//! (everything below is `cfg(target_os = "ios")`), so plain
//! `cargo test --workspace` stays green everywhere.

#[cfg(target_os = "ios")]
mod ios {
    /// App-sandbox directory for settings, history and downloads.
    ///
    /// On iOS `std::env::temp_dir()` resolves to the app container's
    /// `NSTemporaryDirectory()`, which is writable but swept by the OS;
    /// `Library/` next to it survives. The Xcode shell may export
    /// `OPACUSR_CONFIG_DIR` before calling [`super::opacusr_ios_run`] to
    /// point at `Documents/` instead — everything (agent config,
    /// client downloads, address book) routes through that variable.
    pub fn state_dir() -> std::path::PathBuf {
        if let Some(base) = std::env::var_os("OPACUSR_CONFIG_DIR") {
            if !base.is_empty() {
                return std::path::PathBuf::from(base);
            }
        }
        std::env::temp_dir().join("opacusr")
    }

    /// UIKit entry point for the (future) Xcode shell: call from the
    /// app delegate's `didFinishLaunchingWithOptions` ON the main
    /// thread — winit's iOS event loop takes over from there and this
    /// call blocks for the lifetime of the app.
    #[no_mangle]
    pub extern "C" fn opacusr_ios_run() {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info,opacusr=info".into()),
            )
            .init();

        // `dirs::config_dir()` is `None` on iOS and the fallback (`.`)
        // is a read-only bundle root — settings, the access password
        // and the address book would silently fail to persist. Export
        // the sandbox dir BEFORE any config load; opacusr-agent and
        // opacusr-client route their paths through $OPACUSR_CONFIG_DIR.
        let base = state_dir();
        let _ = std::fs::create_dir_all(&base);
        std::env::set_var("OPACUSR_CONFIG_DIR", &base);
        tracing::info!(target: "opacusr", "state dir: {}", base.display());

        // No `event_loop_builder` hook is needed on iOS: winit's UIKit
        // backend attaches to the running UIApplication on its own.
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_title("OpacusR"),
            ..Default::default()
        };

        let app_creator = |cc: &eframe::CreationContext<'_>| {
            opacusr_app::install_fonts(cc);
            Ok(Box::new(opacusr_app::App::new(cc)) as Box<dyn eframe::App>)
        };
        eframe::run_native("OpacusR", options, Box::new(app_creator)).expect("OpacusR UI failed");
    }
}

#[cfg(target_os = "ios")]
pub use ios::{opacusr_ios_run, state_dir};

#[cfg(all(test, target_os = "ios"))]
mod tests {
    use super::ios::state_dir;

    #[test]
    fn state_dir_prefers_the_env_override() {
        // SAFETY: tests run single-threaded per suite (cargo test is
        // invoked with --test-threads=1 on this crate in CI).
        std::env::set_var("OPACUSR_CONFIG_DIR", "/tmp/opacusr-test-state");
        assert_eq!(
            state_dir(),
            std::path::PathBuf::from("/tmp/opacusr-test-state")
        );
        std::env::remove_var("OPACUSR_CONFIG_DIR");
    }

    #[test]
    fn state_dir_falls_back_to_the_sandbox_tmp() {
        let dir = state_dir();
        assert!(dir.ends_with("opacusr"), "unexpected fallback dir: {dir:?}");
    }
}
