//! OpacusR desktop/mobile GUI entry point (eframe/egui).

// TeamViewer-style product build: no console window next to the GUI.
// Debug builds keep the console so `RUST_LOG=debug cargo run` still
// shows tracing output.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use opacusr_app::App;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,opacusr=info".into()),
        )
        .init();

    // Windows: one GUI instance at a time. A second opacusr.exe launch
    // signals the running one to show and focus its window, then exits
    // silently — no duplicate tray icons, no duplicate agents.
    #[cfg(windows)]
    if !opacusr_app::single_instance::acquire() {
        opacusr_app::single_instance::signal_show();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpacusR")
            // TeamViewer keeps its main window compact; sessions still
            // run fullscreen when needed (F11).
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([760.0, 560.0])
            // Created hidden: the first egui frame paints the dark
            // Rev-3 theme BEFORE the window is shown, so Windows can
            // never flash its stock white background (the «белый фон»
            // startup flash). update() reveals the window once the
            // dark frame + DWM chrome are in place.
            .with_visible(false)
            .with_icon(icon()),
        ..Default::default()
    };

    eframe::run_native(
        "OpacusR",
        options,
        Box::new(|cc| {
            opacusr_app::install_fonts(cc);
            Ok(Box::new(App::new(cc)))
        }),
    )
}

/// The app icon: the OpacusR ghost (flat tile) decoded from the
/// embedded brand PNG — the same artwork as the launcher/tray icons.
fn icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../assets/branding/logo.png");
    let img = image::load_from_memory(PNG)
        .expect("embedded app icon is a valid PNG")
        .to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
