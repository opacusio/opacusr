//! OpacusR desktop GUI (egui): connect to a host, view and control the
//! remote screen, chat, transfer files, open a remote terminal — and run
//! the host agent with unattended password / consent dialogs.

pub mod app;
pub mod contacts;
pub mod discovery;
pub mod home;
pub mod host_panel;
pub mod print;
pub mod settings;
pub mod theme;
#[cfg(any(windows, target_os = "macos"))]
pub mod tray;
pub mod update_banner;
pub mod view;

pub use app::App;

use egui::Context;

/// Apply the Rev-3 design system (Carlito fonts + dark violet theme)
/// and CJK-capable fallback fonts so Russian/Chinese text renders even
/// on minimal systems (best effort — system fonts are usually fine).
pub fn install_fonts(cc: &eframe::CreationContext<'_>) {
    theme::install(&cc.egui_ctx);
}

/// Poll a tokio unbounded receiver without blocking the UI thread.
pub trait TryPoll<T> {
    fn poll_all(&mut self, f: impl FnMut(T));
}

impl<T> TryPoll<T> for tokio::sync::mpsc::UnboundedReceiver<T> {
    fn poll_all(&mut self, mut f: impl FnMut(T)) {
        while let Ok(item) = self.try_recv() {
            f(item);
        }
    }
}

/// Helper used by panels: mono-ish label.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak().small()));
}

/// Shared context handle for spawning tokio work from the UI thread.
pub type Runtime = tokio::runtime::Runtime;

pub fn spawn_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// Convenience: run an async future on the app runtime from the UI thread.
pub fn rt_send<R: Send + 'static>(
    rt: &Runtime,
    fut: impl std::future::Future<Output = R> + Send + 'static,
) -> tokio::sync::oneshot::Receiver<R> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.spawn(async move {
        let value = fut.await;
        let _ = tx.send(value);
    });
    rx
}

#[allow(dead_code)]
fn touch(ctx: &Context) {
    ctx.request_repaint();
}
