//! One host session: handshake → capabilities → stream/input/files/terminal
//! pump until `Bye` or error.
//!
//! Session policies applied here:
//! * **view-only** — input, file-transfer and terminal requests are ignored
//!   (files/terminal get explicit refusals), regardless of what the viewer
//!   sends (set by the consent dialog / ACL, announced via `SessionAccept`);
//! * **clipboard sync** — both directions, with echo suppression;
//! * **adaptive quality** — `LinkStats` reports drive the tier controller
//!   from [`crate::adaptive`]; a manual `QualitySet` disables it.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;

use opacusr_capture::{resize_nearest, ScreenCapture};
use opacusr_codec::make_pair;
use opacusr_input::clipboard::ClipboardHandle;
use opacusr_input::InputInjector;
use opacusr_proto::message::{self, ByeReason, CodecId, SessionAccept};
use opacusr_proto::{Message, Platform};
use opacusr_transport::auth::host_handshake;
use opacusr_transport::ft::{FtEngine, FtEvent};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::TransportError;

use crate::adaptive::AdaptiveQuality;

/// Tunables for one host session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub password: String,
    pub display_name: String,
    pub fps: u8,
    pub jpeg_quality: u8,
    pub scale_percent: u16,
    pub download_dir: PathBuf,
    pub auto_accept_files: bool,
    pub codec: CodecChoice,
    /// Ignore input/files/terminal from the viewer.
    pub view_only: bool,
    /// Two-way clipboard synchronization.
    pub clipboard_sync: bool,
    /// Adapt fps/quality/scale to `LinkStats` reports.
    pub adaptive_quality: bool,
    /// Audio streaming offered to viewers.
    pub audio: bool,
}

/// Preferred video codec for outgoing streams (from `agent.toml` `codec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodecChoice {
    /// H.264 when the binary has the `h264` feature, JPEG otherwise.
    #[default]
    Auto,
    Jpeg,
    H264,
}

impl CodecChoice {
    /// Parse the config value (`auto` | `jpeg` | `h264`); anything else → auto.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "jpeg" => Self::Jpeg,
            "h264" | "h.264" => Self::H264,
            _ => Self::Auto,
        }
    }

    /// The codec actually streamed by this build.
    pub fn resolve(self) -> CodecId {
        match (self, opacusr_codec::H264_AVAILABLE) {
            (Self::Jpeg, _) | (_, false) => CodecId::Jpeg,
            (Self::Auto, true) | (Self::H264, true) => CodecId::H264,
        }
    }
}

/// Session lifecycle events for the UI / CLI.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Started { viewer: String, platform: Platform },
    Ended { reason: String },
    Chat(String),
    Ft(FtEvent),
    Notice(String),
}

/// Everything needed to run one session.
pub struct SessionHost {
    pub cfg: SessionConfig,
    pub events: mpsc::UnboundedSender<HostEvent>,
}

impl SessionHost {
    /// Drive the session to completion. `capture` and `injector` are
    /// created by the caller so tests can plug mocks in.
    pub async fn run(
        self,
        mut session: SecureSession,
        mut capture: Box<dyn ScreenCapture>,
        mut injector: Box<dyn InputInjector>,
    ) {
        let cfg = self.cfg;
        let events = self.events;

        let outcome = run_session(
            &cfg,
            &events,
            &mut session,
            capture.as_mut(),
            injector.as_mut(),
        )
        .await;

        // Politely close (best effort — the wire may already be gone).
        let _ = session.send_msg(&Message::Bye(ByeReason::Normal)).await;
        session.close().await;

        let reason = match outcome {
            Ok(r) => r,
            Err(e) => format!("{e}"),
        };
        let _ = events.send(HostEvent::Ended { reason });
    }
}

/// Clipboard size cap for viewer → host transfers.
const CLIPBOARD_MAX_BYTES: usize = opacusr_input::clipboard::CLIPBOARD_MAX_BYTES;

async fn run_session(
    cfg: &SessionConfig,
    events: &mpsc::UnboundedSender<HostEvent>,
    session: &mut SecureSession,
    capture: &mut dyn ScreenCapture,
    injector: &mut dyn InputInjector,
) -> Result<String, TransportError> {
    // --- handshake ---------------------------------------------------------
    let auth = host_handshake(session, Some(&cfg.password), &cfg.display_name).await?;
    let _ = events.send(HostEvent::Started {
        viewer: auth.viewer_hello.display_name.clone(),
        platform: auth.viewer_hello.platform,
    });

    // --- capabilities ------------------------------------------------------
    let (mut cur_w, mut cur_h) = capture.size().unwrap_or((1280, 720));
    let screens = capture.screens();
    let codec = cfg.codec.resolve();
    session
        .send_msg(&Message::SessionAccept(SessionAccept {
            screen_w: cur_w,
            screen_h: cur_h,
            codec,
            fps_hint: cfg.fps,
            view_only: cfg.view_only,
            screens: screens
                .iter()
                .map(|s| message::ScreenInfo {
                    id: s.id,
                    name: s.name.clone(),
                    w: s.w,
                    h: s.h,
                    active: s.id == 0,
                })
                .collect(),
            audio_available: cfg.audio,
        }))
        .await?;
    // Coordinate space the viewer currently maps its input against.
    let mut viewer_screen = (cur_w, cur_h);

    let (mut encoder, _decoder) = make_pair(codec);
    let mut ft = FtEngine::new();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let mut terms: HashMap<u32, crate::pty::TermSession> = HashMap::new();
    #[allow(unused_mut)]
    let mut next_term_id: u32 = 1;

    let mut adaptive = AdaptiveQuality::new(
        cfg.adaptive_quality,
        cfg.fps,
        cfg.jpeg_quality,
        cfg.scale_percent,
    );
    let mut fps = adaptive.params().fps.max(1);
    let mut quality = adaptive.params().jpeg_quality;
    let mut scale = adaptive.params().scale_percent;
    let mut paused = false;
    let mut seq: u64 = 0;

    // Clipboard sync state (echo suppression).
    let mut clipboard = if cfg.clipboard_sync {
        ClipboardHandle::spawn()
    } else {
        None
    };
    let mut clip_sent: Option<String> = None;
    let mut clip_applied: Option<String> = None;

    // Outbound queue (terminal output, engine replies, ...).
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / fps as u64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut clip_ticker = tokio::time::interval(std::time::Duration::from_millis(1000));
    clip_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    #[allow(unused_assignments)]
    let mut reason = String::from("session ended");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if paused { continue; }
                match capture.capture() {
                    Ok(frame) => {
                        cur_w = frame.width;
                        cur_h = frame.height;
                        let frame = if scale < 100 {
                            resize_nearest(&frame, frame.width * scale as u32 / 100, frame.height * scale as u32 / 100)
                        } else { frame };
                        match encoder.encode(&frame, quality) {
                            Ok(data) => {
                                seq += 1;
                                let msg = Message::VideoFrame(message::VideoFrame {
                                    seq,
                                    width: frame.width,
                                    height: frame.height,
                                    keyframe: encoder.codec() == CodecId::Jpeg,
                                    codec,
                                    data,
                                });
                                session.send_msg(&msg).await?;
                            }
                            Err(e) => { let _ = events.send(HostEvent::Notice(format!("encode: {e}"))); }
                        }
                    }
                    Err(e) => { let _ = events.send(HostEvent::Notice(format!("capture: {e}"))); }
                }
            }
            _ = clip_ticker.tick() => {
                // Host → viewer clipboard push with echo suppression: skip
                // text we sent ourselves earlier or applied from the viewer.
                if let Some(clip) = &mut clipboard {
                    clip.request_get();
                    while let Some(text) = clip.poll() {
                        if text.len() > CLIPBOARD_MAX_BYTES { continue; }
                        if Some(&text) == clip_sent.as_ref()
                            || Some(&text) == clip_applied.as_ref()
                        {
                            continue;
                        }
                        clip_sent = Some(text.clone());
                        session.send_msg(&Message::ClipboardText(
                            message::ClipboardText { text },
                        )).await?;
                    }
                }
            }
            msg = session.recv_msg() => {
                match msg? {
                    Message::MouseMove(m) => {
                        if cfg.view_only { continue; }
                        let (x, y) = rescale(m.x, m.y, viewer_screen.0, viewer_screen.1, cur_w, cur_h);
                        let _ = injector.mouse_move(x, y);
                    }
                    Message::MouseButton(m) => {
                        if cfg.view_only { continue; }
                        let (x, y) = rescale(m.x, m.y, viewer_screen.0, viewer_screen.1, cur_w, cur_h);
                        let _ = injector.mouse_button(x, y, m.button, m.down);
                    }
                    Message::MouseWheel(w) => {
                        if cfg.view_only { continue; }
                        let _ = injector.wheel(w.dx, w.dy);
                    }
                    Message::KeyEvent(k) => {
                        if cfg.view_only { continue; }
                        if let Err(e) = injector.key(k.key, k.mods, k.down) {
                            tracing::debug!(target: "opacusr", "key injection: {e}");
                        }
                    }
                    Message::TextInput(t) => {
                        if cfg.view_only { continue; }
                        let _ = injector.text(&t.text);
                    }
                    Message::ChatMessage(c) => {
                        let _ = events.send(HostEvent::Chat(c.text));
                    }
                    Message::ClipboardText(c) => {
                        if c.text.len() > CLIPBOARD_MAX_BYTES {
                            tracing::debug!(target: "opacusr", "clipboard text too large, dropped");
                            continue;
                        }
                        set_clipboard(&c.text);
                        clip_applied = Some(c.text.clone());
                        clip_sent = Some(c.text);
                    }
                    Message::QualitySet(q) => {
                        // A manual setting wins for the rest of the session.
                        adaptive.manual_override();
                        let old_fps = fps;
                        fps = q.fps.max(1);
                        quality = q.jpeg_quality.clamp(1, 100);
                        scale = q.scale_percent.clamp(10, 100);
                        if fps != old_fps {
                            ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / fps as u64));
                            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        }
                    }
                    Message::LinkStats(s) => {
                        if let Some(p) = adaptive.on_link_stats(s.rtt_ms, s.recv_fps) {
                            let old_fps = fps;
                            fps = p.fps.max(1);
                            quality = p.jpeg_quality;
                            scale = p.scale_percent;
                            if fps != old_fps {
                                ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / fps as u64));
                                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            }
                            tracing::debug!(target: "opacusr",
                                "adaptive tier {} → fps {fps} quality {quality} scale {scale}%",
                                adaptive.tier());
                        }
                    }
                    Message::SelectScreen(sel) => {
                        match capture.select_screen(sel.id) {
                            Ok(true) => {
                                let (w, h) = capture.size().unwrap_or((1280, 720));
                                cur_w = w;
                                cur_h = h;
                                viewer_screen = (w, h);
                                // The viewer rescales from frame dims, and the
                                // next frame carries the new resolution; the
                                // H.264 encoder re-initialises itself on size
                                // change, JPEG is per-frame.
                                tracing::info!(target: "opacusr", "switched to screen {}", sel.id);
                            }
                            Ok(false) => {}
                            Err(e) => {
                                let _ = events.send(HostEvent::Notice(format!("screen switch: {e}")));
                            }
                        }
                    }
                    Message::AudioControl(ac) => {
                        // Real audio plumbing lands with the audio backend;
                        // until then only the availability flag is advertised.
                        if ac.enable && !cfg.audio {
                            let _ = events.send(HostEvent::Notice(
                                "audio requested but disabled in agent.toml".into()));
                        }
                    }
                    Message::Pause(p) => paused = p,
                    Message::Ping(n) => session.send_msg(&Message::Pong(n)).await?,
                    Message::Bye(_) => { reason = "viewer disconnected".into(); break; }
                    other => {
                        // File transfer + terminal messages.
                        if cfg.view_only {
                            reject_view_only(&other, session).await?;
                            continue;
                        }
                        let (mut replies, ft_events) = ft.on_message(&other, &cfg.download_dir, cfg.auto_accept_files);
                        for ev in ft_events {
                            let _ = events.send(HostEvent::Ft(ev));
                        }
                        for r in replies.drain(..) {
                            session.send_msg(&r).await?;
                        }
                        // Our outgoing pushes advance after every message.
                        for r in ft.pump() {
                            session.send_msg(&r).await?;
                        }

                        #[cfg(not(any(target_os = "android", target_os = "ios")))]
                        match other {
                            Message::TermOpen(t) => {
                                let id = next_term_id;
                                next_term_id += 1;
                                let (res, opened, out_rx_term) =
                                    crate::pty::TermSession::spawn(id, t.rows, t.cols, None);
                                match res {
                                    Ok(term) => {
                                        let tx = out_tx.clone();
                                        tokio::spawn(async move {
                                            let mut out_rx_term = out_rx_term;
                                            while let Some(mut data) = out_rx_term.recv().await {
                                                data.id = id;
                                                if tx.send(Message::TermData(data)).is_err() { break; }
                                            }
                                        });
                                        terms.insert(id, term);
                                    }
                                    Err(_) => { /* reply already carries the error */ }
                                }
                                session.send_msg(&Message::TermOpened(opened)).await?;
                            }
                            Message::TermData(t) => {
                                if let Some(term) = terms.get(&t.id) {
                                    if let Err(e) = term.write(&t.data) {
                                        let _ = events.send(HostEvent::Notice(format!("term write: {e}")));
                                    }
                                }
                            }
                            Message::TermResize(t) => {
                                if let Some(term) = terms.get(&t.id) {
                                    let _ = term.resize(t.rows, t.cols);
                                }
                            }
                            Message::TermClose(t) => {
                                if let Some(term) = terms.remove(&t.id) {
                                    term.kill();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some(msg) = out_rx.recv() => {
                session.send_msg(&msg).await?;
            }
        }
    }

    // Cleanup terminals.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    for (_, t) in terms.drain() {
        t.kill();
    }
    Ok(reason)
}

/// Explicit refusals for control operations in a view-only session.
async fn reject_view_only(
    msg: &Message,
    session: &mut SecureSession,
) -> Result<(), TransportError> {
    match msg {
        Message::FtOffer(o) => {
            session
                .send_msg(&Message::FtReject(message::FtReject {
                    id: o.id,
                    reason: "view-only session: file transfer is disabled".into(),
                }))
                .await
        }
        Message::TermOpen(t) => {
            session
                .send_msg(&Message::TermOpened(message::TermOpened {
                    id: t.id,
                    ok: false,
                    error: Some("view-only session: terminal is disabled".into()),
                    shell: String::new(),
                }))
                .await
        }
        _ => Ok(()),
    }
}

/// Map viewer coordinates (reported-resolution space) onto the current
/// physical screen size.
fn rescale(x: u32, y: u32, aw: u32, ah: u32, cw: u32, ch: u32) -> (u32, u32) {
    if aw == 0 || ah == 0 || (aw == cw && ah == ch) {
        return (x, y);
    }
    (
        ((x as u64 * cw as u64) / aw as u64) as u32,
        ((y as u64 * ch as u64) / ah as u64) as u32,
    )
}

fn set_clipboard(text: &str) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
            Ok(_) => {}
            Err(e) => tracing::debug!(target: "opacusr", "clipboard set failed: {e}"),
        }
    }
}
