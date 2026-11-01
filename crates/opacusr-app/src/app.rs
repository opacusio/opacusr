//! Main application state and UI: home screen (host + connect), the live
//! session view, and the settings shared between them.

use egui::{Context, TextureHandle};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use opacusr_agent::config::AgentConfig;
use opacusr_agent::service::{AgentEvent, AgentService, SessionDecision};
use opacusr_agent::session::HostCommand;
use opacusr_client::addressbook::{
    fmt_duration, fmt_ts, now_secs, AddressBook, HistoryEntry, Outcome,
};
use opacusr_client::session::{
    ClientEvent, Command, SessionClient, SessionSettings, TransportChoice,
};
use opacusr_proto::message::CodecId;
use opacusr_proto::PeerId;

use crate::{host_panel, spawn_runtime, view};

/// Which tab is active on the home screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Connect,
    Host,
}

/// One live viewer session (UI-side handle).
pub struct SessionHandle {
    pub commands: UnboundedSender<Command>,
    pub events: UnboundedReceiver<ClientEvent>,
    pub screen: (u32, u32),
    pub texture: Option<TextureHandle>,
    pub rtt: u32,
    pub paused: bool,
    /// Host granted view-only access (no input is sent).
    pub view_only: bool,
    /// Host monitors (for the screen selector).
    pub screens: Vec<opacusr_proto::message::ScreenInfo>,
    pub selected_screen: u32,
    pub audio_available: bool,
    /// Live clipboard sync toggle (mirrors the client-side state).
    pub clipboard_sync: bool,
    /// Audio requested from the host (button state).
    pub audio_on: bool,
    pub panel_chat: bool,
    pub panel_files: bool,
    pub panel_term: bool,
    pub last_mouse: Option<(u32, u32)>,
    pub chat: Vec<String>,
    pub chat_input: String,
    pub term_out: String,
    pub term_open: bool,
    pub term_input: String,
    pub uploads: Vec<(u32, String, u64, u64)>, // id, name, done, total
    /// Remote display name (from the host handshake).
    pub host_name: String,
    /// Video codec negotiated in `SessionAccept`.
    pub codec: Option<CodecId>,
    /// Host's suggested fps (`SessionAccept.fps_hint`).
    pub fps_hint: u8,
    /// Actual received frame rate (2 s sliding window).
    pub recv_fps: f32,
    /// Compressed wire bitrate, bytes per second (2 s sliding window).
    pub bitrate_bps: u64,
    /// Resolution of the last received frame.
    pub stream_res: (u32, u32),
    pub panel_stats: bool,
    /// (arrival, wire bytes) samples for the fps/bitrate window.
    pub stats_win: std::collections::VecDeque<(std::time::Instant, u64)>,
    pub status: String,
}

impl SessionHandle {
    fn new(commands: UnboundedSender<Command>, events: UnboundedReceiver<ClientEvent>) -> Self {
        Self {
            commands,
            events,
            screen: (1280, 720),
            texture: None,
            rtt: 0,
            paused: false,
            view_only: false,
            screens: Vec::new(),
            selected_screen: 0,
            audio_available: false,
            clipboard_sync: true,
            audio_on: false,
            panel_chat: false,
            panel_files: true,
            panel_term: false,
            last_mouse: None,
            chat: Vec::new(),
            chat_input: String::new(),
            term_out: String::new(),
            term_open: false,
            term_input: String::new(),
            uploads: Vec::new(),
            host_name: String::new(),
            codec: None,
            fps_hint: 0,
            recv_fps: 0.0,
            bitrate_bps: 0,
            stream_res: (0, 0),
            panel_stats: false,
            stats_win: std::collections::VecDeque::new(),
            status: "connecting…".into(),
        }
    }
}

/// The running host service (UI-side handle).
pub struct HostHandle {
    pub events: UnboundedReceiver<AgentEvent>,
    pub my_id: Option<PeerId>,
    pub notices: Vec<String>,
    pub consent: Option<(PeerId, String, std::sync::mpsc::Sender<SessionDecision>)>,
    /// Command handle for the ACTIVE session (send files, ...).
    pub commands: Option<UnboundedSender<HostCommand>>,
    /// True between SessionStarted and SessionEnded.
    pub session_active: bool,
}

pub struct App {
    ctx: Context,
    rt: crate::Runtime,

    // shared settings
    pub server_url: String,
    pub peer_id_input: String,
    pub password_input: String,
    pub display_name: String,
    pub transport_choice: TransportChoice,
    pub fps: f32,
    pub quality: f32,
    pub scale: f32,

    tab: Tab,
    session: Option<SessionHandle>,
    pub(crate) host: Option<HostHandle>,
    host_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    host_consent_rx: Option<
        std::sync::mpsc::Receiver<(PeerId, String, std::sync::mpsc::Sender<SessionDecision>)>,
    >,

    pub config: AgentConfig,
    pub status_message: String,
    pub host_password_input: String,

    /// Saved contacts + connection history (`addressbook.toml`).
    pub book: AddressBook,
    // Address-book form state.
    new_label: String,
    new_target: String,
    new_endpoint: String,
    show_history: bool,
    /// Contact id currently being renamed (inline editor in the list).
    renaming_id: Option<String>,
    /// Scratch buffer for the inline rename editor.
    rename_label: String,
    /// Whether the inline rename editor already grabbed keyboard focus.
    rename_focused: bool,
    /// History ts of the current connection attempt.
    cur_hist_ts: Option<u64>,
    /// Unix ts when the current attempt actually connected (for duration).
    cur_connected_at: Option<u64>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let config = AgentConfig::load();
        let display_name = config.name.clone();
        Self {
            ctx,
            rt: spawn_runtime(),
            server_url: config.server_url.clone(),
            peer_id_input: String::new(),
            password_input: String::new(),
            display_name,
            transport_choice: TransportChoice::Auto,
            fps: config.fps as f32,
            quality: config.jpeg_quality as f32,
            scale: config.scale_percent as f32,
            tab: Tab::Connect,
            session: None,
            host: None,
            host_shutdown: None,
            host_consent_rx: None,
            config,
            status_message: "Ready.".into(),
            host_password_input: String::new(),
            book: AddressBook::load(),
            new_label: String::new(),
            new_target: String::new(),
            new_endpoint: String::new(),
            show_history: false,
            renaming_id: None,
            rename_label: String::new(),
            rename_focused: false,
            cur_hist_ts: None,
            cur_connected_at: None,
        }
    }

    // ------------------------------------------------------------------
    // Viewer
    // ------------------------------------------------------------------

    fn connect_clicked(&mut self, label: Option<String>) {
        let input = self.peer_id_input.replace([' ', '-'], "");
        let password = self.password_input.clone();
        if password.is_empty() {
            self.status_message = "Enter the host password".into();
            return;
        }

        // Base values shared by every connection attempt; settings are
        // built per attempt because each signaling endpoint mints its
        // own TURN credentials.
        let base_ice: Vec<opacusr_transport::webrtc::IceServerConfig> = self
            .config
            .ice_urls
            .iter()
            .map(|u| opacusr_transport::webrtc::IceServerConfig::stun(&[u.as_str()]))
            .collect();
        let display_name = self.display_name.clone();
        let download_dir = self.config.download_dir.clone();
        let clipboard_sync = self.config.clipboard_sync;
        let make_settings =
            move |turn: Option<opacusr_transport::turn::TurnCredentials>| -> SessionSettings {
                let mut ice = base_ice.clone();
                if let Some(t) = turn {
                    let uris: Vec<&str> = t.uris.iter().map(String::as_str).collect();
                    ice.push(opacusr_transport::webrtc::IceServerConfig::turn(
                        &uris,
                        &t.username,
                        &t.password,
                    ));
                }
                SessionSettings {
                    password: password.clone(),
                    display_name: display_name.clone(),
                    download_dir: download_dir.clone(),
                    auto_accept_files: true,
                    clipboard_sync,
                    webrtc: opacusr_transport::webrtc::WebRtcConfig { ice_servers: ice },
                }
            };

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ui_events_tx, ui_events_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = self.ctx.clone();

        if input.contains(':') {
            // LAN mode: ip:port.
            let addr = input.clone();
            self.status_message = format!("Connecting to {addr}…");
            self.rt.spawn(async move {
                match SessionClient::connect_direct(&addr, make_settings(None)).await {
                    Ok((client, mut events)) => {
                        ctx.request_repaint();
                        let run_task = tokio::spawn(client.run(cmd_rx));
                        forward_events(&mut events, &ui_events_tx).await;
                        let _ = run_task.await;
                    }
                    Err(e) => {
                        let _ =
                            ui_events_tx.send(ClientEvent::Closed(format!("connect failed: {e}")));
                    }
                }
            });
        } else {
            let host_id: PeerId = match input.parse() {
                Ok(v) if opacusr_proto::is_valid_peer_id(v) => v,
                _ => {
                    self.status_message = "Enter a valid 9-digit host ID or ip:port".into();
                    return;
                }
            };
            // Connection ladder over the configured signaling endpoints:
            // the agent may register on several (fan-out — see
            // docs/serverless.md), so the viewer tries each URL in order
            // and the first endpoint that completes the handshake wins.
            let mut urls = if self.config.signaling_urls.is_empty() {
                vec![self.server_url.clone()]
            } else {
                let mut u = self.config.signaling_urls.clone();
                if !u.iter().any(|x| x == &self.server_url) {
                    u.insert(0, self.server_url.clone());
                }
                u
            };
            urls.dedup();
            let choice = self.transport_choice;
            let turn_api = self.config.turn_api;
            self.status_message = format!(
                "Requesting session with host {host_id} over {} endpoint(s)…",
                urls.len()
            );
            self.rt.spawn(async move {
                let mut cmd_rx = Some(cmd_rx);
                let mut last_err = String::from("no endpoints configured");
                for (i, url) in urls.iter().enumerate() {
                    let _ = ui_events_tx.send(ClientEvent::Notice(format!(
                        "endpoint {}/{}: {url}",
                        i + 1,
                        urls.len()
                    )));
                    // Time-limited TURN credentials from THIS endpoint's
                    // `GET /turn` (coturn REST API), when it mints them;
                    // any failure just leaves the STUN-only configuration.
                    let mut turn = None;
                    if turn_api {
                        if let Some(endpoint) = opacusr_transport::turn::turn_endpoint_of(url) {
                            let fetched = tokio::task::spawn_blocking(move || {
                                opacusr_transport::turn::fetch_turn_credentials(&endpoint)
                            })
                            .await
                            .ok();
                            if let Some(Ok(creds)) = fetched {
                                turn = Some(creds);
                            }
                        }
                    }
                    let settings = make_settings(turn);
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(25),
                        SessionClient::connect_via_server(url, host_id, settings, choice),
                    )
                    .await
                    {
                        Ok(Ok((client, mut events))) => {
                            ctx.request_repaint();
                            if let Some(cmd_rx) = cmd_rx.take() {
                                let run_task = tokio::spawn(client.run(cmd_rx));
                                forward_events(&mut events, &ui_events_tx).await;
                                let _ = run_task.await;
                            }
                            return;
                        }
                        Ok(Err(e)) => {
                            last_err = format!("{url}: {e}");
                            let _ = ui_events_tx.send(ClientEvent::Notice(
                                "endpoint failed, trying the next one…".into(),
                            ));
                        }
                        Err(_) => {
                            last_err = format!("{url}: timed out");
                            let _ = ui_events_tx.send(ClientEvent::Notice(
                                "endpoint timed out, trying the next one…".into(),
                            ));
                        }
                    }
                }
                let _ = ui_events_tx.send(ClientEvent::Closed(format!(
                    "all signaling endpoints failed ({last_err})"
                )));
            });
        }

        // History: remember the attempt; the outcome is filled in by the
        // Connected/Closed event handlers below.
        let ts = now_secs();
        self.book.push_history(HistoryEntry {
            ts,
            label,
            target: input,
            outcome: Outcome::Unknown,
            duration_secs: None,
        });
        let _ = self.book.save();
        self.cur_hist_ts = Some(ts);
        self.cur_connected_at = None;

        self.session = Some(SessionHandle::new(cmd_tx, ui_events_rx));
    }

    fn disconnect_clicked(&mut self) {
        if let Some(s) = self.session.as_mut() {
            let _ = s.commands.send(Command::Disconnect);
        }
        self.finish_attempt();
        self.session = None;
        self.status_message = "Disconnected.".into();
    }

    /// Finalize the history entry of the current attempt, if any.
    fn finish_attempt(&mut self) {
        if let Some(ts) = self.cur_hist_ts.take() {
            let (outcome, duration) = match self.cur_connected_at.take() {
                Some(t0) => (Outcome::Connected, Some(now_secs().saturating_sub(t0))),
                None => (Outcome::Failed, None),
            };
            self.book.finish_history(ts, outcome, duration);
            let _ = self.book.save();
        }
    }

    // ------------------------------------------------------------------
    // Host
    // ------------------------------------------------------------------

    pub(crate) fn host_start_clicked(&mut self) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut cfg = self.config.clone();
        cfg.server_url = self.server_url.clone();
        cfg.name = self.display_name.clone();
        cfg.fps = self.fps as u8;
        cfg.jpeg_quality = self.quality as u8;
        cfg.scale_percent = self.scale as u16;
        let unattended = cfg.is_unattended();
        let (consent_tx, consent_rx) =
            std::sync::mpsc::channel::<(PeerId, String, std::sync::mpsc::Sender<SessionDecision>)>(
            );

        let service = AgentService {
            cfg: cfg.clone(),
            events: tx,
            demo: false,
            unattended,
        };
        self.rt.spawn(async move {
            let consent = move |id: PeerId, name: &str, _platform| -> SessionDecision {
                let (ans_tx, ans_rx) = std::sync::mpsc::channel();
                if consent_tx.send((id, name.to_string(), ans_tx)).is_err() {
                    return SessionDecision::Reject;
                }
                ans_rx
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap_or(SessionDecision::Reject)
            };
            let _ = service.run(consent, shutdown_rx).await;
        });
        self.host = Some(HostHandle {
            events: rx,
            my_id: None,
            notices: Vec::new(),
            consent: None,
            commands: None,
            session_active: false,
        });
        self.host_consent_rx = Some(consent_rx);
        self.host_shutdown = Some(shutdown_tx);
        self.status_message = if unattended {
            "Agent started (unattended mode).".into()
        } else {
            "Agent started (attended: confirm each session).".into()
        };
        self.tab = Tab::Host;
    }

    pub(crate) fn host_stop_clicked(&mut self) {
        if let Some(tx) = self.host_shutdown.take() {
            let _ = tx.send(true);
        }
        self.host = None;
        self.host_consent_rx = None;
        self.status_message = "Agent stopped.".into();
    }

    pub(crate) fn save_host_password(&mut self) {
        let p = self.host_password_input.trim().to_string();
        if p.len() < 4 {
            self.status_message = "Password too short (min 4 chars).".into();
            return;
        }
        // Prefer the OS keychain; fall back to the 0600 config file.
        match opacusr_agent::keychain::store_password(&p) {
            Ok(()) => {
                self.config.password = None;
                self.config.password_in_keychain = true;
            }
            Err(e) => {
                self.config.password = Some(p);
                self.config.password_in_keychain = false;
                self.status_message = format!("Keychain unavailable ({e}); using config file. ");
            }
        }
        self.host_password_input.clear();
        match self.config.save() {
            Ok(path) => {
                if !self.status_message.is_empty() {
                    self.status_message
                        .push_str(&format!("Saved flag to {}", path.display()));
                } else {
                    self.status_message =
                        format!("Saved to the OS keychain (flag in {})", path.display());
                }
            }
            Err(e) => self.status_message = format!("Save failed: {e}"),
        }
    }
}

/// Forward client events into the UI bridge until the session closes.
async fn forward_events(
    events: &mut UnboundedReceiver<ClientEvent>,
    ui_tx: &UnboundedSender<ClientEvent>,
) {
    while let Some(ev) = events.recv().await {
        let closed = matches!(ev, ClientEvent::Closed(_));
        if ui_tx.send(ev).is_err() || closed {
            break;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        crate::update_banner::ensure_started(ctx);
        crate::update_banner::paint(ctx);
        // Keep repainting while a session streams.
        if self.session.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        // ---- poll consent bridge ----
        if let Some(consent_rx) = &self.host_consent_rx {
            while let Ok((id, name, ans)) = consent_rx.try_recv() {
                if let Some(h) = &mut self.host {
                    h.consent = Some((id, name, ans));
                    ctx.request_repaint();
                }
            }
        }

        // ---- poll host events (into a buffer, then apply) ----
        let mut host_evs = Vec::new();
        if let Some(host) = &mut self.host {
            crate::TryPoll::poll_all(&mut host.events, |ev| host_evs.push(ev));
        }
        for ev in host_evs {
            if let Some(h) = &mut self.host {
                match ev {
                    AgentEvent::Registered { id, .. } => h.my_id = Some(id),
                    AgentEvent::SessionEnded { reason } => {
                        h.commands = None;
                        h.session_active = false;
                        h.notices.push(format!("session ended: {reason}"))
                    }
                    AgentEvent::Notice(text) => {
                        h.notices.push(text);
                        if h.notices.len() > 200 {
                            h.notices.drain(..100);
                        }
                    }
                    AgentEvent::IncomingRequest { .. } => {}
                    AgentEvent::SessionStarted { commands, .. } => {
                        h.commands = Some(commands);
                        h.session_active = true;
                    }
                }
            }
        }

        // ---- poll session events (into a buffer, then apply) ----
        let mut sess_evs = Vec::new();
        if let Some(sess) = &mut self.session {
            crate::TryPoll::poll_all(&mut sess.events, |ev| sess_evs.push(ev));
        }
        for ev in sess_evs {
            let Some(sess) = &mut self.session else {
                return;
            };
            match ev {
                ClientEvent::Connected {
                    host_name,
                    screen,
                    view_only,
                    screens,
                    audio_available,
                    codec,
                    fps_hint,
                    ..
                } => {
                    sess.screen = screen;
                    sess.view_only = view_only;
                    sess.screens = screens;
                    sess.audio_available = audio_available;
                    sess.host_name = host_name;
                    sess.codec = Some(codec);
                    sess.fps_hint = fps_hint;
                    sess.selected_screen = 0;
                    sess.clipboard_sync = true;
                    sess.status = format!(
                        "connected to {host_name}{}",
                        if view_only { " (view only)" } else { "" }
                    );
                    self.cur_connected_at = Some(now_secs());
                    if let Some(ts) = self.cur_hist_ts {
                        self.book.mark_outcome(ts, Outcome::Connected);
                    }
                    let typed = self.peer_id_input.replace([' ', '-'], "");
                    self.book.touch_by_target(&typed);
                    let _ = self.book.save();
                }
                ClientEvent::Frame { frame, wire_bytes } => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    );
                    match &mut sess.texture {
                        Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                        None => {
                            sess.texture =
                                Some(ctx.load_texture("remote", img, egui::TextureOptions::LINEAR));
                        }
                    }
                    // Link statistics: sliding 2 s window of (arrival, wire bytes).
                    let now = std::time::Instant::now();
                    sess.stream_res = (frame.width, frame.height);
                    sess.stats_win.push_back((now, wire_bytes as u64));
                    while let Some(&(t, _)) = sess.stats_win.front() {
                        if now.duration_since(t).as_secs_f32() <= 2.0 {
                            break;
                        }
                        sess.stats_win.pop_front();
                    }
                    if sess.stats_win.len() >= 2 {
                        let span = now
                            .duration_since(sess.stats_win.front().unwrap().0)
                            .as_secs_f32();
                        if span > 0.2 {
                            let n = sess.stats_win.len() as f32;
                            let bytes: u64 = sess.stats_win.iter().map(|(_, b)| b).sum();
                            sess.recv_fps = n / span;
                            sess.bitrate_bps = (bytes as f64 / span as f64) as u64;
                        }
                    }
                }
                ClientEvent::Chat { text, .. } => sess.chat.push(format!("[host] {text}")),
                ClientEvent::Rtt(rtt) => sess.rtt = rtt,
                ClientEvent::FtStarted { id, name, total } => {
                    sess.uploads.push((id, name, 0, total));
                }
                ClientEvent::FtProgress { id, done, total } => {
                    if let Some(row) = sess.uploads.iter_mut().find(|(i, ..)| *i == id) {
                        row.2 = done;
                        row.3 = total;
                    }
                }
                ClientEvent::FtSentDone { id, ok } => {
                    sess.chat.push(format!(
                        "upload #{id} {}",
                        if ok { "done ✓" } else { "failed ✗" }
                    ));
                    sess.uploads.retain(|(i, ..)| *i != id);
                }
                ClientEvent::FtReceivedDone { path, verified } => {
                    sess.chat.push(format!(
                        "host sent {} ({})",
                        path.display(),
                        if verified {
                            "verified ✓"
                        } else {
                            "hash mismatch ✗"
                        }
                    ));
                }
                ClientEvent::FtFailed { reason, .. } => sess.status = format!("transfer: {reason}"),
                ClientEvent::TermOpened { ok, error, .. } => {
                    sess.term_open = ok;
                    if !ok {
                        sess.status = format!("terminal: {}", error.unwrap_or_default());
                    }
                }
                ClientEvent::TermData { data, .. } => {
                    sess.term_out.push_str(&String::from_utf8_lossy(&data));
                    let len = sess.term_out.len();
                    if len > 256 * 1024 {
                        sess.term_out.drain(..len - 128 * 1024);
                    }
                }
                ClientEvent::Notice(t) => sess.status = t,
                ClientEvent::Closed(reason) => {
                    sess.status = format!("closed: {reason}");
                    if let Some(ts) = self.cur_hist_ts.take() {
                        let (outcome, duration) = match self.cur_connected_at.take() {
                            Some(t0) => (Outcome::Connected, Some(now_secs().saturating_sub(t0))),
                            None => (Outcome::Failed, None),
                        };
                        self.book.finish_history(ts, outcome, duration);
                        let _ = self.book.save();
                    }
                }
            }
        }

        // ---- session hotkeys (F11 fullscreen, Ctrl+Shift+…) ----
        if self.session.is_some() {
            // F11 works even while a text field is focused.
            if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
                let full = ctx.input(|i| i.viewport().fullscreen == Some(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            // Letter shortcuts are ignored while the user is typing.
            if ctx.memory(|m| m.focused().is_none()) {
                let (d, p, c, f, t, s) = ctx.input(|i| {
                    let m = &i.modifiers;
                    let cs = m.ctrl && m.shift;
                    (
                        cs && i.key_pressed(egui::Key::D),
                        cs && i.key_pressed(egui::Key::P),
                        cs && i.key_pressed(egui::Key::C),
                        cs && i.key_pressed(egui::Key::F),
                        cs && i.key_pressed(egui::Key::T),
                        cs && i.key_pressed(egui::Key::S),
                    )
                });
                if let Some(sess) = &mut self.session {
                    if p {
                        sess.paused = !sess.paused;
                        let _ = sess.commands.send(Command::Pause(sess.paused));
                    }
                    if c {
                        sess.panel_chat = !sess.panel_chat;
                    }
                    if f {
                        sess.panel_files = !sess.panel_files;
                    }
                    if t {
                        sess.panel_term = !sess.panel_term;
                    }
                    if s {
                        sess.panel_stats = !sess.panel_stats;
                    }
                }
                if d {
                    self.disconnect_clicked();
                }
            }
        }

        // Drag & drop → push files to the host.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            if let Some(path) = f.path {
                if let Some(sess) = &mut self.session {
                    let _ = sess.commands.send(Command::PushFile(path));
                }
            }
        }

        // ---- top bar ----
        egui::TopBottomPanel::top("opacusr_top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("🟣 OpacusR");
                ui.separator();
                if self.session.is_none() {
                    ui.selectable_value(&mut self.tab, Tab::Connect, "Connect");
                    ui.selectable_value(&mut self.tab, Tab::Host, "Host");
                }
                if self.session.is_some() && ui.button("⏹ Disconnect").clicked() {
                    self.disconnect_clicked();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&self.status_message).weak().small());
                });
            });
            ui.add_space(4.0);
        });

        if let Some(session) = self.session.as_mut() {
            view::session_ui(ctx, session, &mut self.config);
            return;
        }

        match self.tab {
            Tab::Connect => self.connect_tab(ctx),
            Tab::Host => host_panel::host_ui(ctx, self),
        }
    }
}

impl App {
    fn connect_tab(&mut self, ctx: &Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(80.0);
                egui::Grid::new("connect_grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Signaling server").strong());
                        ui.add_sized([430.0, 26.0], egui::TextEdit::singleline(&mut self.server_url));
                        ui.end_row();

                        ui.label(egui::RichText::new("Host ID").strong());
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [200.0, 26.0],
                                egui::TextEdit::singleline(&mut self.peer_id_input)
                                    .hint_text("123 456 789 or ip:port"),
                            );
                            crate::hint(ui, "9-digit ID, or ip:port for LAN");
                        });
                        ui.end_row();

                        ui.label(egui::RichText::new("Password").strong());
                        ui.add_sized(
                            [430.0, 26.0],
                            egui::TextEdit::singleline(&mut self.password_input).password(true),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("Your name").strong());
                        ui.add_sized([430.0, 26.0], egui::TextEdit::singleline(&mut self.display_name));
                        ui.end_row();

                        ui.label(egui::RichText::new("Transport").strong());
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.transport_choice,
                                TransportChoice::Auto,
                                "Auto (WebRTC → relay)",
                            );
                            ui.selectable_value(&mut self.transport_choice, TransportChoice::WebRtcOnly, "WebRTC");
                            ui.selectable_value(&mut self.transport_choice, TransportChoice::RelayOnly, "TCP relay");
                        });
                        ui.end_row();

                        ui.label(egui::RichText::new("Quality").strong());
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut self.fps, 1.0..=60.0).text("fps"));
                            ui.add(egui::Slider::new(&mut self.quality, 20.0..=95.0).text("jpeg"));
                            ui.add(egui::Slider::new(&mut self.scale, 25.0..=100.0).text("scale %"));
                        });
                        ui.end_row();
                    });
            });

            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add_space(80.0);
                let connect = ui.add(
                    egui::Button::new(egui::RichText::new("▶  Connect").size(18.0))
                        .min_size([160.0, 40.0].into()),
                );
                if connect.clicked() {
                    self.connect_clicked(None);
                }
            });

            ui.add_space(24.0);
            ui.separator();
            self.book_ui(ui);

            ui.add_space(16.0);
            ui.separator();
            ui.label(egui::RichText::new("How it works").strong());
            crate::hint(
                ui,
                "Run `opacusr-agent run` (or the Host tab) on the remote machine — it shows a 9-digit ID.\n\
                 Type that ID + the host password here. Traffic is E2E-encrypted (Argon2id + ChaCha20-Poly1305)\n\
                 over WebRTC/DTLS, with a TCP relay fallback when direct P2P is blocked.",
            );
        });
    }

    // ------------------------------------------------------------------
    // Address book + connection history
    // ------------------------------------------------------------------

    fn book_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Address book").strong());
        egui::Grid::new("book_add_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Label");
                ui.add_sized(
                    [430.0, 24.0],
                    egui::TextEdit::singleline(&mut self.new_label).hint_text("My laptop"),
                );
                ui.end_row();

                ui.label("Target");
                ui.add_sized(
                    [430.0, 24.0],
                    egui::TextEdit::singleline(&mut self.new_target)
                        .hint_text("9-digit ID or ip:port"),
                );
                ui.end_row();

                ui.label("Endpoint");
                ui.add_sized(
                    [430.0, 24.0],
                    egui::TextEdit::singleline(&mut self.new_endpoint)
                        .hint_text("optional — signaling URL tried first (wss://…/signal)"),
                );
                ui.end_row();
            });
        if ui.button("💾 Save contact").clicked() {
            let target = self.new_target.replace([' ', '-'], "");
            if target.is_empty() {
                self.status_message = "Contact needs a target: 9-digit ID or ip:port".into();
            } else {
                let label = {
                    let l = self.new_label.trim();
                    if l.is_empty() {
                        target.clone()
                    } else {
                        l.to_string()
                    }
                };
                let endpoint = {
                    let e = self.new_endpoint.trim();
                    if e.is_empty() {
                        None
                    } else {
                        Some(e.to_string())
                    }
                };
                self.book.upsert(label, target, endpoint);
                self.new_label.clear();
                self.new_target.clear();
                self.new_endpoint.clear();
                self.status_message = "Contact saved.".into();
                let _ = self.book.save();
            }
        }

        ui.add_space(8.0);
        if self.book.contacts().is_empty() {
            crate::hint(ui, "No saved contacts yet.");
        } else {
            let ids: Vec<String> = self
                .book
                .sorted_contacts()
                .into_iter()
                .map(|c| c.id)
                .collect();
            egui::ScrollArea::vertical()
                .max_height(170.0)
                .show(ui, |ui| {
                    for id in ids {
                        let Some(c) = self.book.contacts().iter().find(|c| c.id == id).cloned()
                        else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .button(if c.favorite { "★" } else { "☆" })
                                .on_hover_text("Favorite")
                                .clicked()
                            {
                                self.book.set_favorite(&c.id, !c.favorite);
                                let _ = self.book.save();
                            }
                            let renaming = self.renaming_id.as_deref() == Some(c.id.as_str());
                            if renaming {
                                // Inline rename editor: ✔/Enter applies, ✖ cancels.
                                let resp = ui.add_sized(
                                    [180.0, 22.0],
                                    egui::TextEdit::singleline(&mut self.rename_label),
                                );
                                // Grab keyboard focus once, right after the
                                // editor opens — then leave it alone so that
                                // `lost_focus` can fire (Enter-to-apply).
                                if !self.rename_focused {
                                    resp.request_focus();
                                    self.rename_focused = true;
                                }
                                let confirmed = (resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                    || ui.button("✔").on_hover_text("Save name").clicked();
                                if ui.button("✖").on_hover_text("Cancel").clicked() {
                                    self.renaming_id = None;
                                }
                                if confirmed {
                                    let name = self.rename_label.trim().to_string();
                                    if self.book.rename(&c.id, &name) {
                                        self.status_message = "Contact renamed.".into();
                                        let _ = self.book.save();
                                    } else {
                                        self.status_message = "Name cannot be empty.".into();
                                    }
                                    self.renaming_id = None;
                                }
                            } else {
                                ui.label(egui::RichText::new(&c.label).strong());
                                ui.label(egui::RichText::new(&c.target).weak());
                                if let Some(ep) = &c.endpoint {
                                    ui.label(egui::RichText::new(ep).small().weak());
                                }
                                if ui.button("✎").on_hover_text("Rename").clicked() {
                                    self.renaming_id = Some(c.id.clone());
                                    self.rename_label = c.label.clone();
                                    self.rename_focused = false;
                                }
                            }
                            if ui.button("▶").on_hover_text("Connect").clicked() {
                                if let Some(ep) = c.endpoint.clone() {
                                    self.server_url = ep;
                                }
                                self.peer_id_input = c.target.clone();
                                let label = c.label.clone();
                                self.connect_clicked(Some(label));
                            }
                            if !renaming && ui.button("✕").on_hover_text("Remove").clicked() {
                                self.book.remove(&c.id);
                                let _ = self.book.save();
                            }
                        });
                    }
                });
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("History").strong());
            if ui
                .button(if self.show_history { "hide" } else { "show" })
                .clicked()
            {
                self.show_history = !self.show_history;
            }
            if self.show_history && ui.button("clear").clicked() {
                self.book.clear_history();
                let _ = self.book.save();
            }
        });
        if self.show_history {
            if self.book.history().is_empty() {
                crate::hint(ui, "No connection history yet.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for h in self.book.history() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(fmt_ts(h.ts)).weak().small().monospace(),
                                );
                                if let Some(label) = &h.label {
                                    ui.label(egui::RichText::new(label).small());
                                }
                                ui.label(egui::RichText::new(&h.target).small());
                                let dot = match h.outcome {
                                    Outcome::Connected => egui::RichText::new("● connected")
                                        .color(egui::Color32::from_rgb(80, 200, 120)),
                                    Outcome::Failed => egui::RichText::new("● failed")
                                        .color(egui::Color32::from_rgb(230, 90, 90)),
                                    Outcome::Unknown => egui::RichText::new("● unknown").weak(),
                                };
                                ui.label(dot.small());
                                if let Some(d) = h.duration_secs {
                                    ui.label(egui::RichText::new(fmt_duration(d)).weak().small());
                                }
                            });
                        }
                    });
            }
        }
    }
}
