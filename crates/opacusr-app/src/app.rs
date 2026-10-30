//! Main application state and UI: home screen (host + connect), the live
//! session view, and the settings shared between them.

use egui::{Context, TextureHandle};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use opacusr_agent::config::AgentConfig;
use opacusr_agent::service::{AgentEvent, AgentService};
use opacusr_client::session::{
    ClientEvent, Command, SessionClient, SessionSettings, TransportChoice,
};
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
            status: "connecting…".into(),
        }
    }
}

/// The running host service (UI-side handle).
pub struct HostHandle {
    pub events: UnboundedReceiver<AgentEvent>,
    pub my_id: Option<PeerId>,
    pub notices: Vec<String>,
    pub consent: Option<(PeerId, String, std::sync::mpsc::Sender<bool>)>,
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
    host_consent_rx:
        Option<std::sync::mpsc::Receiver<(PeerId, String, std::sync::mpsc::Sender<bool>)>>,

    pub config: AgentConfig,
    pub status_message: String,
    pub host_password_input: String,
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
        }
    }

    // ------------------------------------------------------------------
    // Viewer
    // ------------------------------------------------------------------

    fn connect_clicked(&mut self) {
        let input = self.peer_id_input.replace([' ', '-'], "");
        let password = self.password_input.clone();
        if password.is_empty() {
            self.status_message = "Enter the host password".into();
            return;
        }

        let settings = SessionSettings {
            password,
            display_name: self.display_name.clone(),
            download_dir: self.config.download_dir.clone(),
            auto_accept_files: true,
            webrtc: opacusr_transport::webrtc::WebRtcConfig {
                ice_servers: self
                    .config
                    .ice_urls
                    .iter()
                    .map(|u| opacusr_transport::webrtc::IceServerConfig::stun(&[u.as_str()]))
                    .collect(),
            },
        };

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ui_events_tx, ui_events_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = self.ctx.clone();

        if input.contains(':') {
            // LAN mode: ip:port.
            let addr = input.clone();
            self.status_message = format!("Connecting to {addr}…");
            self.rt.spawn(async move {
                match SessionClient::connect_direct(&addr, settings).await {
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
            let server = self.server_url.clone();
            let choice = self.transport_choice;
            self.status_message = format!("Requesting session with host {host_id}…");
            self.rt.spawn(async move {
                match SessionClient::connect_via_server(&server, host_id, settings, choice).await {
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
        }

        self.session = Some(SessionHandle::new(cmd_tx, ui_events_rx));
    }

    fn disconnect_clicked(&mut self) {
        if let Some(s) = self.session.as_mut() {
            let _ = s.commands.send(Command::Disconnect);
        }
        self.session = None;
        self.status_message = "Disconnected.".into();
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
        let unattended = cfg.password.is_some();
        let (consent_tx, consent_rx) =
            std::sync::mpsc::channel::<(PeerId, String, std::sync::mpsc::Sender<bool>)>();

        let service = AgentService {
            cfg: cfg.clone(),
            events: tx,
            demo: false,
            unattended,
        };
        self.rt.spawn(async move {
            let consent = move |id: PeerId, name: &str, _platform| -> bool {
                let (ans_tx, ans_rx) = std::sync::mpsc::channel();
                if consent_tx.send((id, name.to_string(), ans_tx)).is_err() {
                    return false;
                }
                ans_rx
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap_or(false)
            };
            let _ = service.run(consent, shutdown_rx).await;
        });
        self.host = Some(HostHandle {
            events: rx,
            my_id: None,
            notices: Vec::new(),
            consent: None,
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
        self.config.password = Some(p);
        self.host_password_input.clear();
        match self.config.save() {
            Ok(path) => self.status_message = format!("Saved to {}", path.display()),
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
                        h.notices.push(format!("session ended: {reason}"))
                    }
                    AgentEvent::Notice(text) => {
                        h.notices.push(text);
                        if h.notices.len() > 200 {
                            h.notices.drain(..100);
                        }
                    }
                    AgentEvent::IncomingRequest { .. } | AgentEvent::SessionStarted { .. } => {}
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
                    host_name, screen, ..
                } => {
                    sess.screen = screen;
                    sess.status = format!("connected to {host_name}");
                }
                ClientEvent::Frame(frame) => {
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
                ClientEvent::Closed(reason) => sess.status = format!("closed: {reason}"),
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
                    self.connect_clicked();
                }
            });

            ui.add_space(24.0);
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
}
