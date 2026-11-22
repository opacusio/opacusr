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
