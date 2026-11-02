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
use std::sync::Arc;

use tokio::sync::mpsc;

use opacusr_capture::{resize_nearest, CursorSource, ScreenCapture};
use opacusr_codec::make_pair;
use opacusr_input::clipboard::{hash_clip, ClipContent, ClipboardHandle};
use opacusr_input::InputInjector;
use opacusr_proto::message::{self, ByeReason, CodecId, SessionAccept};
use opacusr_proto::{Message, Platform};
use opacusr_transport::auth::host_handshake;
use opacusr_transport::ft::{FtEngine, FtEvent};
use opacusr_transport::lockout::AuthGate;
use opacusr_transport::secure::SecureSession;
use opacusr_transport::TransportError;

use crate::adaptive::AdaptiveQuality;
use crate::recording::SessionRecorder;

/// Tunables for one host session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Session password; `None` rejects the handshake outright — never
    /// pass an empty string for "no password configured".
    pub password: Option<String>,
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
    /// Audio capture device filter (empty = default / Windows loopback).
    pub audio_device: String,
    /// Swallow the host's physical keyboard/mouse while the viewer has
    /// full control (TeamViewer-style; view-only sessions never lock,
    /// fail-open when the platform cannot take the lock).
    pub lock_input: bool,
    /// Optional RFC 6238 secret (base32). When set, viewers must present
    /// a valid TOTP code in addition to the password.
    pub totp_secret: Option<String>,
    /// Host-side TCP tunnel policy for this session (SSH-style local
    /// forwarding). View-only sessions never get tunnels.
    pub tunnel: crate::tunnel::TunnelPolicy,
    /// Opt-in session recording (MP4, H.264 codec only).
    pub record: bool,
    /// Directory for session recording files.
    pub record_dir: PathBuf,
    /// Shared auth-failure limiter (rate limiting + lockout). One instance
    /// per accept path (signaling / LAN), shared by all its sessions.
    pub auth_gate: Option<Arc<AuthGate>>,
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

/// Commands the host UI can push into a live session.
#[derive(Debug, Clone)]
pub enum HostCommand {
    /// Send a local file to the connected viewer.
    SendFile(std::path::PathBuf),
}

/// Everything needed to run one session.
pub struct SessionHost {
    pub cfg: SessionConfig,
    pub events: mpsc::UnboundedSender<HostEvent>,
    /// Commands from the host UI (send files, ...). The UI may drop the
    /// sender — the loop then simply never fires the command branch.
    pub commands: mpsc::UnboundedReceiver<HostCommand>,
}

impl SessionHost {
    /// Drive the session to completion. `capture`, `injector` and `cursor`
    /// are created by the caller so tests can plug mocks in.
    pub async fn run(
        self,
        mut session: SecureSession,
        mut capture: Box<dyn ScreenCapture>,
        mut injector: Box<dyn InputInjector>,
        mut cursor: Box<dyn CursorSource>,
    ) {
        let cfg = self.cfg;
        let events = self.events;
        let mut commands = self.commands;

        // TeamViewer-style physical input lock for full-control sessions.
        // The guard covers every exit path below; if the lock cannot be
        // taken the session continues unlocked with a notice (fail-open).
        let _input_lock = if cfg.lock_input && !cfg.view_only {
            match opacusr_input::lock::HostInputLock::enable() {
                Ok(lock) => {
                    let _ = events.send(HostEvent::Notice("physical input locked".into()));
                    Some(lock)
                }
                Err(e) => {
                    let _ = events.send(HostEvent::Notice(format!("input lock unavailable: {e}")));
                    None
                }
            }
        } else {
            None
        };

        // Opt-in session recording: started inside `run_session` (the peer
        // name for the file name comes from the handshake), finalized here
        // on every exit path.
        let mut recorder: Option<SessionRecorder> = None;

        let outcome = run_session(
            &cfg,
            &events,
            &mut commands,
            &mut session,
            capture.as_mut(),
            injector.as_mut(),
            cursor.as_mut(),
            &mut recorder,
        )
        .await;

        if let Some(mut rec) = recorder.take() {
            let path = rec.path();
            match rec.finish() {
                Ok(()) => {
                    let _ = events.send(HostEvent::Notice(format!(
                        "recording saved: {}",
                        path.display()
                    )));
                }
                Err(e) => {
                    let _ = events.send(HostEvent::Notice(format!("recording: {e}")));
                }
            }
        }

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

/// PNG cap for image clipboard transfers (transport frame limit is 8 MiB).
const CLIPBOARD_IMAGE_MAX_PNG_BYTES: usize =
    opacusr_input::clipboard::CLIPBOARD_IMAGE_MAX_PNG_BYTES;

#[allow(clippy::too_many_arguments)]
async fn run_session(
    cfg: &SessionConfig,
    events: &mpsc::UnboundedSender<HostEvent>,
    commands: &mut mpsc::UnboundedReceiver<HostCommand>,
    session: &mut SecureSession,
    capture: &mut dyn ScreenCapture,
    injector: &mut dyn InputInjector,
    cursor: &mut dyn CursorSource,
    recorder: &mut Option<SessionRecorder>,
) -> Result<String, TransportError> {
    // --- handshake ---------------------------------------------------------
    let auth = host_handshake(
        session,
        cfg.password.as_deref(),
        cfg.totp_secret.as_deref(),
        &cfg.display_name,
        cfg.auth_gate.as_deref(),
    )
    .await?;
    let _ = events.send(HostEvent::Started {
        viewer: auth.viewer_hello.display_name.clone(),
        platform: auth.viewer_hello.platform,
    });

    // --- capabilities ------------------------------------------------------
    let (mut cur_w, mut cur_h) = capture.size().unwrap_or((1280, 720));
    let screens = capture.screens();
    let codec = cfg.codec.resolve();

    // Opt-in recording: MP4 muxing needs Annex-B parameter sets, so it is
    // H.264-only in this release. Viewers are told via `SessionAccept`.
    if cfg.record {
        if codec == CodecId::H264 {
            match SessionRecorder::start(&cfg.record_dir, &auth.viewer_hello.display_name, cfg.fps)
            {
                Ok(rec) => {
                    let _ = events.send(HostEvent::Notice(format!(
                        "recording → {}",
                        rec.path().display()
                    )));
                    *recorder = Some(rec);
                }
                Err(e) => {
                    let _ = events.send(HostEvent::Notice(format!("recording unavailable: {e}")));
                }
            }
        } else {
            let _ = events.send(HostEvent::Notice(
                "recording needs the H.264 codec (set codec = \"h264\")".into(),
            ));
        }
    }

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
            audio_available: cfg.audio && cfg!(feature = "audio"),
            recording: recorder.is_some(),
            tunnel: cfg.tunnel != crate::tunnel::TunnelPolicy::Off && !cfg.view_only,
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

    // Host-side TCP tunnels (viewer-initiated, policy-checked on open).
    let mut tunnels: HashMap<u32, crate::tunnel::TunnelConn> = HashMap::new();

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

    // Cursor overlay state: the shape is pushed once per serial change,
    // the position only when it actually moves (keeps idle links quiet).
    let mut cursor_serial: Option<u64> = None;
    let mut last_cursor: Option<(i32, i32, bool)> = None;

    // Clipboard sync state (echo suppression). Markers are content hashes
    // so text and images share one mechanism.
    let mut clipboard = if cfg.clipboard_sync {
        ClipboardHandle::spawn()
    } else {
        None
    };
    let mut clip_sent: Option<u64> = None;
    let mut clip_applied: Option<u64> = None;

    // Audio stream (started/stopped via AudioControl from the viewer).
    #[cfg(feature = "audio")]
    let mut audio_stream: Option<crate::audio::HostAudioStream> = None;

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

                // Cursor overlay: independent of the capture result, so the
                // pointer keeps tracking even while frames error out.
                // Positions are native streamed-screen pixels (the viewer
                // scales the whole frame to fit, so quality scaling does
                // not affect them).
                if let Some(pos) = cursor.poll() {
                    let (ox, oy) = capture.origin();
                    let (cx, cy) = (pos.x - ox, pos.y - oy);
                    if cursor_serial != Some(pos.serial) {
                        if let Some(sh) = cursor.shape(pos.serial) {
                            session.send_msg(&Message::CursorShape(message::CursorShape {
                                id: pos.serial,
                                width: sh.width,
                                height: sh.height,
                                hotspot_x: sh.hotspot_x,
                                hotspot_y: sh.hotspot_y,
                                rgba: sh.rgba,
                            })).await?;
                        }
                        // Mark as seen even on failure: retrying every
                        // frame would spam the wire for missing shapes.
                        cursor_serial = Some(pos.serial);
                    }
                    let next = (cx, cy, pos.visible);
                    if last_cursor.as_ref() != Some(&next) {
                        last_cursor = Some(next);
                        session.send_msg(&Message::CursorPos(message::CursorPos {
                            x: cx,
                            y: cy,
                            visible: pos.visible,
                            shape_id: pos.serial,
                        })).await?;
                    }
                }

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
                                let still_on = recorder
                                    .as_mut()
                                    .map(|rec| {
                                        rec.write(std::time::Instant::now(), &data, frame.width, frame.height)
                                    })
                                    .unwrap_or(true);
                                if !still_on {
                                    if let Some(mut rec) = recorder.take() {
                                        let why = rec
                                            .last_error()
                                            .unwrap_or("unknown error")
                                            .to_string();
                                        let _ = rec.finish();
                                        let _ = events.send(HostEvent::Notice(format!(
                                            "recording stopped: {why}"
                                        )));
                                    }
                                }
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
                // content we sent ourselves earlier or applied from the viewer.
                if let Some(clip) = &mut clipboard {
                    clip.request_get();
                    while let Some(content) = clip.poll() {
                        let hash = hash_clip(&content);
                        if Some(hash) == clip_sent || Some(hash) == clip_applied {
                            continue;
                        }
                        let msg = match content {
                            ClipContent::Text(text) => {
                                if text.len() > CLIPBOARD_MAX_BYTES {
                                    continue;
                                }
                                Message::ClipboardText(message::ClipboardText { text })
                            }
                            ClipContent::Image { width, height, rgba } => {
                                // Encoding happens only for *new* content —
                                // suppressed copies cost one hash, not one PNG.
                                let Some(png) = opacusr_input::clipboard::encode_png(
                                    width, height, &rgba,
                                ) else {
                                    continue;
                                };
                                if png.len() > CLIPBOARD_IMAGE_MAX_PNG_BYTES {
                                    tracing::debug!(target: "opacusr",
                                        "clipboard image too large after encode, dropped");
                                    continue;
                                }
                                Message::ClipboardImage(message::ClipboardImage {
                                    width,
                                    height,
                                    png,
                                })
                            }
                        };
                        clip_sent = Some(hash);
                        session.send_msg(&msg).await?;
                    }
                }
            }
            Some(cmd) = commands.recv(), if !commands.is_closed() => {
                match cmd {
                    HostCommand::SendFile(path) => {
                        if cfg.view_only {
                            let _ = events.send(HostEvent::Notice(
                                "view-only session: sending files is disabled".into(),
                            ));
                            continue;
                        }
                        match ft.start_push(&path) {
                            Ok(Message::FtOffer(offer)) => {
                                let _ = events.send(HostEvent::Notice(format!(
                                    "sending {} to the viewer…",
                                    path.display()
                                )));
                                session.send_msg(&Message::FtOffer(offer)).await?;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                let _ =
                                    events.send(HostEvent::Notice(format!("send file: {e}")));
                            }
                        }
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
                        let content = ClipContent::Text(c.text.clone());
                        set_clipboard(&c.text);
                        let hash = hash_clip(&content);
                        clip_applied = Some(hash);
                        clip_sent = Some(hash);
                    }
                    Message::ClipboardImage(c) => {
                        if c.png.len() > CLIPBOARD_IMAGE_MAX_PNG_BYTES {
                            tracing::debug!(target: "opacusr", "clipboard image too large, dropped");
                            continue;
                        }
                        match opacusr_input::clipboard::decode_png(&c.png) {
                            Some((width, height, rgba)) => {
                                let content = ClipContent::Image {
                                    width,
                                    height,
                                    rgba: rgba.clone(),
                                };
                                set_clipboard_image(width, height, &rgba);
                                let hash = hash_clip(&content);
                                clip_applied = Some(hash);
                                clip_sent = Some(hash);
                            }
                            None => {
                                tracing::debug!(target: "opacusr",
                                    "malformed clipboard image, dropped");
                            }
                        }
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
                        if let Some(p) = adaptive.on_link_stats(s.rtt_ms, s.recv_fps, s.jitter_ms) {
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
                        #[cfg(feature = "audio")]
                        if cfg.audio {
                            match (ac.enable, audio_stream.is_some()) {
                                (true, false) => {
                                    // Packet pump: number AudioFrames and
                                    // push them onto the outbound queue.
                                    let (pkt_tx, mut pkt_rx) =
                                        mpsc::unbounded_channel::<Vec<u8>>();
                                    let out = out_tx.clone();
                                    tokio::spawn(async move {
                                        let mut seq: u32 = 0;
                                        while let Some(data) = pkt_rx.recv().await {
                                            seq = seq.wrapping_add(1);
                                            if out
                                                .send(Message::AudioFrame(
                                                    message::AudioFrame { seq, data },
                                                ))
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                    });
                                    match crate::audio::HostAudioStream::start(
                                        &cfg.audio_device,
                                        pkt_tx,
                                    ) {
                                        Ok(s) => {
                                            let _ = events.send(HostEvent::Notice(
                                                "audio streaming started".into(),
                                            ));
                                            audio_stream = Some(s);
                                        }
                                        Err(e) => {
                                            let _ = events
                                                .send(HostEvent::Notice(format!("audio: {e}")));
                                        }
                                    }
                                }
                                (false, true) => {
                                    if let Some(s) = audio_stream.as_mut() {
                                        s.stop();
                                    }
                                    audio_stream = None;
                                    let _ = events
                                        .send(HostEvent::Notice("audio streaming stopped".into()));
                                }
                                _ => {}
                            }
                        } else if ac.enable {
                            let _ = events.send(HostEvent::Notice(
                                "audio requested but disabled in agent.toml".into(),
                            ));
                        }
                        #[cfg(not(feature = "audio"))]
                        {
                            let _ = ac;
                            let _ = events.send(HostEvent::Notice(
                                "audio requested but this build has no audio feature".into(),
                            ));
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

                        // Tunnel forwarding (all platforms; tokio TCP is
                        // available everywhere). Runs after the ft engine,
                        // which ignores tunnel messages.
                        match &other {
                            Message::TunnelOpen(t) => {
                                let (reply, conn) = crate::tunnel::TunnelConn::open(
                                    t.id,
                                    &t.host,
                                    t.port,
                                    cfg.tunnel,
                                    out_tx.clone(),
                                )
                                .await;
                                if reply.ok {
                                    let _ = events.send(HostEvent::Notice(format!(
                                        "tunnel #{} opened → {}:{}",
                                        t.id, t.host, t.port
                                    )));
                                    tunnels.insert(t.id, conn.expect("ok reply carries a conn"));
                                } else {
                                    let _ = events.send(HostEvent::Notice(format!(
                                        "tunnel #{} refused: {}",
                                        t.id,
                                        reply.error.as_deref().unwrap_or("?")
                                    )));
                                }
                                session.send_msg(&Message::TunnelOpened(reply)).await?;
                            }
                            Message::TunnelData(t) => {
                                if let Some(c) = tunnels.get(&t.id) {
                                    c.send(&t.data);
                                }
                            }
                            Message::TunnelClose(t) => {
                                if let Some(c) = tunnels.remove(&t.id) {
                                    c.abort();
                                }
                            }
                            _ => {}
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

    // Cleanup terminals and tunnels.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    for (_, t) in terms.drain() {
        t.kill();
    }
    for (_, c) in tunnels.drain() {
        c.abort();
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
        Message::TunnelOpen(t) => {
            session
                .send_msg(&Message::TunnelOpened(message::TunnelOpened {
                    id: t.id,
                    ok: false,
                    error: Some("view-only session: tunneling is disabled".into()),
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

/// Place a decoded RGBA image on the host clipboard (best effort).
fn set_clipboard_image(width: u32, height: u32, rgba: &[u8]) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        match arboard::Clipboard::new().and_then(|mut c| {
            c.set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: std::borrow::Cow::Borrowed(rgba),
            })
        }) {
            Ok(_) => {}
            Err(e) => tracing::debug!(target: "opacusr", "clipboard image set failed: {e}"),
        }
    }
    #[allow(unused_variables)]
    let _ = (width, height, rgba);
}
