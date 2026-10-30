//! OpacusR desktop/mobile GUI entry point (eframe/egui).

use opacusr_app::App;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,opacusr=info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpacusR")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([720.0, 520.0])
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

/// Tiny inline app icon (a violet "signal dot" on a dark rounded square).
fn icon() -> egui::IconData {
    let size = 64usize;
    let mut rgba = vec![0u8; size * size * 4];
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let (i, o) = (c * 0.95, c * 0.55);
            let [r8, g8, b8, a8] = if r < o {
                [255, 255, 255, 255] // white core dot
            } else if r < i {
                [124u8, 92, 255, 255] // violet ring (#7C5CFF)
            } else {
                [24, 24, 32, 0]
            };
            let idx = (y * size + x) * 4;
            rgba[idx..idx + 4].copy_from_slice(&[r8, g8, b8, a8]);
        }
    }
    egui::IconData {
        width: size as u32,
        height: size as u32,
        rgba,
    }
}
