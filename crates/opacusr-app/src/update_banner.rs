//! GUI "update available" banner.
//!
//! Once per app run a background thread asks GitHub for the latest release
//! (see `opacusr_transport::update`). When a newer version exists, a
//! dismissible banner is drawn above the top bar. Informational only:
//! nothing is ever downloaded or installed automatically.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Mutex, OnceLock};

use opacusr_transport::update::LatestRelease;

struct BannerState {
    rx: Receiver<LatestRelease>,
    latest: Option<LatestRelease>,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static DISMISSED: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<Mutex<Option<BannerState>>> = OnceLock::new();

/// Start the background check exactly once per process. Safe to call on
/// every frame; also nudges the UI to repaint so the banner appears as
/// soon as the answer arrives.
pub fn ensure_started(ctx: &egui::Context) {
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("OPACUSR_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let (tx, rx) = channel();
    let _ = STATE.set(Mutex::new(Some(BannerState { rx, latest: None })));
    let _ = std::thread::Builder::new()
        .name("opacusr-update-check".into())
        .spawn(move || {
            if let Some(rel) = opacusr_transport::update::check_for_update() {
                let _ = tx.send(rel);
            }
        });
    // The check can take up to 10 s; keep waking the UI meanwhile.
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
}

/// Poll the background check and draw the banner once a newer release is
/// known. Call at the start of `eframe::App::update`, before any panels,
/// so the banner sits above the top bar.
pub fn paint(ctx: &egui::Context) {
    if DISMISSED.load(Ordering::Relaxed) {
        return;
    }
    let cell = match STATE.get() {
        Some(c) => c,
        None => return,
    };
    let latest = {
        let mut guard = match cell.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(state) = guard.as_mut() else {
            return;
        };
        match state.rx.try_recv() {
            Ok(rel) => {
                state.latest = Some(rel);
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                // The check is still running; wake up again in a second.
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
            Err(TryRecvError::Disconnected) => {}
        }
        state.latest.clone()
    };
    let Some(rel) = latest else {
        return;
    };

    egui::TopBottomPanel::top("opacusr_update_banner").show(ctx, |ui| {
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new(format!(
                    "Update available: {} (you are running {})",
                    rel.tag,
                    env!("CARGO_PKG_VERSION")
                ))
                .strong(),
            );
            ui.hyperlink_to("release notes", rel.url.clone());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("x").clicked() {
                    DISMISSED.store(true, Ordering::Relaxed);
                }
            });
        });
        ui.add_space(5.0);
    });
}
