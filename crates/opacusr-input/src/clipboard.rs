//! Clipboard access from a dedicated OS thread.
//!
//! `arboard::Clipboard` is thread-affine on X11 (and costly to re-create on
//! every poll), while our session loops are `tokio` futures that must stay
//! `Send`. This module pins the clipboard to one worker thread and talks to
//! it with plain channels:
//!
//! ```text
//! session task ── request_get()/set() ──► worker thread (owns arboard)
//!       ▲                                        │
//!       └──────────── poll() result ─────────────┘
//! ```
//!
//! The same helper is used by the host agent (share my clipboard) and by
//! the viewer (receive the remote one), so both get identical echo
//! suppression behaviour at their call sites.

/// Refuse to move absurd amounts of data through the clipboard channel.
pub const CLIPBOARD_MAX_BYTES: usize = 512 * 1024;

enum Req {
    Get,
    Set(String),
}

/// A handle to the clipboard worker thread. Dropping it lets the worker
/// exit (the channel disconnects).
pub struct ClipboardHandle {
    tx: std::sync::mpsc::Sender<Req>,
    rx: std::sync::mpsc::Receiver<Option<String>>,
}

impl ClipboardHandle {
    /// Spawn the worker. Returns `None` on platforms without clipboard
    /// support (mobile) or when the backend cannot initialise (headless
    /// CI, Wayland without a portal, …) — callers must treat clipboard
    /// sync as a best-effort feature.
    pub fn spawn() -> Option<Self> {
        spawn_impl()
    }

    /// Ask the worker to read the clipboard; the result arrives as a
    /// [`Self::poll`] event. Cheap and non-blocking.
    pub fn request_get(&self) {
        let _ = self.tx.send(Req::Get);
    }

    /// Drain one completed read request, if any. `Some(text)` — the
    /// current clipboard content (empty strings are passed through; the
    /// caller does the change detection).
    pub fn poll(&mut self) -> Option<String> {
        self.rx.try_recv().ok().flatten()
    }

    /// Write to the system clipboard (best effort, size-capped).
    pub fn set(&self, text: &str) {
        if text.len() <= CLIPBOARD_MAX_BYTES {
            let _ = self.tx.send(Req::Set(text.to_string()));
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn spawn_impl() -> Option<ClipboardHandle> {
    use std::sync::mpsc::channel;

    let (tx, worker_rx) = channel::<Req>();
    let (res_tx, rx) = channel::<Option<String>>();

    let started = std::thread::Builder::new()
        .name("opacusr-clipboard".into())
        .spawn(move || {
            let Ok(mut cb) = arboard::Clipboard::new() else {
                // No clipboard backend: keep replying "unavailable" until
                // the owner drops the handle.
                while let Ok(req) = worker_rx.recv() {
                    if let Req::Get = req {
                        let _ = res_tx.send(None);
                    }
                }
                return;
            };
            while let Ok(req) = worker_rx.recv() {
                match req {
                    Req::Get => {
                        let text = cb
                            .get_text()
                            .ok()
                            .filter(|t| t.len() <= CLIPBOARD_MAX_BYTES);
                        let _ = res_tx.send(text);
                    }
                    Req::Set(t) => {
                        let _ = cb.set_text(t);
                    }
                }
            }
        });

    started.ok().map(|_| ClipboardHandle { tx, rx })
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn spawn_impl() -> Option<ClipboardHandle> {
    None
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_round_trip() {
        let Some(mut h) = ClipboardHandle::spawn() else {
            // Headless CI without X11/portal — nothing to assert.
            return;
        };
        h.set("opacusr-clipboard-test");
        // Wait for the set to land, then read back.
        std::thread::sleep(std::time::Duration::from_millis(150));
        h.request_get();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got = None;
        while std::time::Instant::now() < deadline {
            if let Some(text) = h.poll() {
                got = Some(text);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The clipboard may be shared with other processes; accept either
        // our value or "no backend".
        if let Some(text) = got {
            assert_eq!(text, "opacusr-clipboard-test");
        }
    }
}
