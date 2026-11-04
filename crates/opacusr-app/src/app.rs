//! Main application state and UI: home screen (host + connect), the live
//! session view, and the settings shared between them.

use egui::{Context, TextureHandle};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use opacusr_agent::config::AgentConfig;
use opacusr_agent::service::{AgentEvent, AgentService, SessionDecision};
use opacusr_agent::session::HostCommand;
use opacusr_client::addressbook::{now_secs, AddressBook, HistoryEntry, Outcome};
use opacusr_client::session::{
    ClientEvent, Command, SessionClient, SessionSettings, TransportChoice,
};
use opacusr_proto::message::CodecId;
use opacusr_proto::PeerId;

use crate::{contacts, home, host_panel, settings, spawn_runtime, theme, view};

/// One cached remote-cursor shape (bitmap + geometry for the overlay).
pub struct RemoteCursor {
    pub texture: TextureHandle,
    /// Hotspot inside the bitmap, in bitmap pixels.
    pub hotspot: (f32, f32),
    pub size: (f32, f32),
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
    /// TCP tunnel side panel (SSH-style local forwarding).
    pub panel_tunnel: bool,
    /// Active tunnel listeners: (bound port, target host, target port).
    pub tunnels: Vec<(u16, String, u16)>,
    pub last_mouse: Option<(u32, u32)>,
    /// Last decoded frame (w, h, rgba) — refreshed at most twice a
    /// second — for the "print current frame" action.
    pub last_frame: Option<(u32, u32, Vec<u8>)>,
    /// Instant of the last `last_frame` refresh (throttle).
    pub last_frame_at: Option<std::time::Instant>,
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
    /// `true` = the host records this session (opt-in policy).
    pub recording: bool,
    /// `true` = the host forwards TCP tunnels for this session.
    pub tunnel_available: bool,
    /// New-tunnel form fields (local port, target host, target port).
    pub tun_local_port: String,
    pub tun_target_host: String,
    pub tun_target_port: String,
    /// Actual received frame rate (2 s sliding window).
    pub recv_fps: f32,
    /// Compressed wire bitrate, bytes per second (2 s sliding window).
    pub bitrate_bps: u64,
    /// Resolution of the last received frame.
    pub stream_res: (u32, u32),
    pub panel_stats: bool,
    /// (arrival, wire bytes) samples for the fps/bitrate window.
    pub stats_win: std::collections::VecDeque<(std::time::Instant, u64)>,
    /// Adaptive-quality tier (0..=3) the host currently streams at;
    /// `None` = not reported yet (or adaptation off).
    pub quality_tier: Option<u8>,
    /// Parameters the host streams with (fps, jpeg quality, scale %).
    pub quality_params: Option<(u8, u8, u16)>,
    /// `false` = a manual quality setting disabled the controller.
    pub quality_adaptive: bool,
    /// Remote cursor shapes seen this session (bounded cache).
    pub cursor_shapes: std::collections::HashMap<u64, RemoteCursor>,
    /// Latest cursor position: (x, y, visible, shape id) in native
    /// streamed-screen pixels.
    pub cursor_pos: Option<(i32, i32, bool, u64)>,
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
            panel_tunnel: false,
            tunnels: Vec::new(),
            last_mouse: None,
            last_frame: None,
            last_frame_at: None,
            chat: Vec::new(),
            chat_input: String::new(),
            term_out: String::new(),
            term_open: false,
            term_input: String::new(),
            uploads: Vec::new(),
            host_name: String::new(),
            codec: None,
            fps_hint: 0,
            recording: false,
            tunnel_available: false,
            tun_local_port: String::new(),
            tun_target_host: "127.0.0.1".into(),
            tun_target_port: String::new(),
            recv_fps: 0.0,
            bitrate_bps: 0,
            stream_res: (0, 0),
            panel_stats: false,
            stats_win: std::collections::VecDeque::new(),
            quality_tier: None,
            quality_params: None,
            quality_adaptive: false,
            cursor_shapes: std::collections::HashMap::new(),
            cursor_pos: None,
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

    session: Option<SessionHandle>,
    pub(crate) host: Option<HostHandle>,
    host_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    host_consent_rx: Option<
        std::sync::mpsc::Receiver<(PeerId, String, std::sync::mpsc::Sender<SessionDecision>)>,
    >,

    /// Optional TOTP code typed for hosts that require a second factor.
    pub totp_input: String,

    pub config: AgentConfig,
    pub status_message: String,
    pub host_password_input: String,

    /// Saved contacts + connection history (`addressbook.toml`).
    pub book: AddressBook,
    /// mDNS browser for LAN discovery (Phase 3); refreshed on a
    /// background thread for the whole app lifetime.
    pub discovery: crate::discovery::Discovery,
    // Address-book form state (shared with the contacts window).
    pub(crate) new_label: String,
    pub(crate) new_target: String,
    pub(crate) new_endpoint: String,
    /// «История подключений» window open (AnyDesk-style log).
    pub(crate) show_history: bool,
    /// «Контакты» window open (AnyDesk-style device panel).
    pub(crate) show_contacts: bool,
    /// Settings window open (TeamViewer-style «Options»). All settings
    /// live here — the home screen shows none.
    pub(crate) show_settings: bool,
    /// Selected section of the settings window.
    pub(crate) settings_nav: settings::SettingsTab,
    /// Contact-search scratch field (contacts window).
    pub(crate) contacts_search: String,
    /// Whether the «add contact» form starts expanded.
    pub(crate) add_contact_open: bool,
    /// Contact id currently being renamed (inline editor in the list).
    pub(crate) renaming_id: Option<String>,
    /// Scratch buffer for the inline rename editor.
    pub(crate) rename_label: String,
    /// Whether the inline rename editor already grabbed keyboard focus.
    pub(crate) rename_focused: bool,
    /// Unattended password shown in the host card (TeamViewer-style);
    /// cached so the keychain is not queried every frame.
    pub(crate) unattended_display: Option<String>,
    /// Host-card password reveal toggle.
    pub(crate) reveal_password: bool,
    /// Host-card inline password-change editor open.
    pub(crate) password_editing: bool,
    /// History ts of the current connection attempt.
    cur_hist_ts: Option<u64>,
    /// Unix ts when the current attempt actually connected (for duration).
    cur_connected_at: Option<u64>,
    /// System tray (Windows/macOS): icon + menu, TeamViewer-style
    /// background operation. `None` where the tray is unavailable.
    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) tray: Option<crate::tray::Tray>,
    /// Actions picked in the tray menu (queued by the event handlers).
    #[cfg(any(windows, target_os = "macos"))]
    tray_action_rx: Option<std::sync::mpsc::Receiver<crate::tray::TrayAction>>,
    /// The main window is currently hidden (app lives in the tray).
    #[cfg(any(windows, target_os = "macos"))]
    window_hidden: bool,
    /// Tray «Выход» pressed — the next close request must really exit.
    #[cfg(any(windows, target_os = "macos"))]
    quit_requested: bool,
    /// `--tray` CLI flag (MSI autostart): hide the window on the first
    /// frame; the app keeps running in the tray.
    #[cfg(any(windows, target_os = "macos"))]
    start_hidden: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let config = AgentConfig::load();
        // «Имя в сети»: the machine name by default (Rev-3 mockup),
        // editable; a user-customized config name wins.
        let env_user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_default();
        let display_name = if config.name.trim().is_empty() || config.name == env_user {
            theme::machine_name()
        } else {
            config.name.clone()
        };
        let mut app = Self {
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
            session: None,
            host: None,
            host_shutdown: None,
            host_consent_rx: None,
            config,
            status_message: "Готово.".into(),
            host_password_input: String::new(),
            totp_input: String::new(),
            book: AddressBook::load(),
            discovery: crate::discovery::Discovery::start(),
            new_label: String::new(),
            new_target: String::new(),
            new_endpoint: String::new(),
            show_history: false,
            show_contacts: false,
            show_settings: false,
            settings_nav: settings::SettingsTab::General,
            contacts_search: String::new(),
            add_contact_open: false,
            renaming_id: None,
            rename_label: String::new(),
            rename_focused: false,
            unattended_display: None,
            reveal_password: false,
            password_editing: false,
            cur_hist_ts: None,
            cur_connected_at: None,
            #[cfg(any(windows, target_os = "macos"))]
            tray: None,
            #[cfg(any(windows, target_os = "macos"))]
            tray_action_rx: None,
            #[cfg(any(windows, target_os = "macos"))]
            window_hidden: false,
            #[cfg(any(windows, target_os = "macos"))]
            quit_requested: false,
            #[cfg(any(windows, target_os = "macos"))]
            start_hidden: std::env::args().any(|a| a == "--tray"),
        };
        app.refresh_unattended_display();

        // TeamViewer-style tray: the icon outlives the window; closing
        // the window keeps the app (and its agent) in the tray.
        #[cfg(any(windows, target_os = "macos"))]
        {
            let (tx, rx) = std::sync::mpsc::channel::<crate::tray::TrayAction>();
            let wake_ctx = cc.egui_ctx.clone();
            let wake: std::sync::Arc<dyn Fn() + Send + Sync + 'static> =
                std::sync::Arc::new(move || wake_ctx.request_repaint());
            app.tray = crate::tray::Tray::create(tx, wake);
            app.tray_action_rx = Some(rx);
        }

        // TeamViewer model: the agent starts with the app unless the
        // user turned the autostart off (Настройки → Хост).
        if app.config.start_agent_automatically {
            app.host_start_clicked();
        }

        app
    }

    /// Tray actions + the TeamViewer close behavior: the window's X
    /// hides to tray (the agent keeps serving), «Выход» in the tray
    /// menu exits for real.
    #[cfg(any(windows, target_os = "macos"))]
    fn process_tray(&mut self, ctx: &Context) {
        use crate::tray::TrayAction;
        use egui::ViewportCommand;

        if !self.quit_requested && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.window_hidden = true;
            self.status_message = "Свернуто в трей — OpacusR продолжает работать.".into();
        }

        let mut picked: Vec<TrayAction> = Vec::new();
        if let Some(rx) = &self.tray_action_rx {
            while let Ok(a) = rx.try_recv() {
                picked.push(a);
            }
        }
        for a in picked {
            match a {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.window_hidden = false;
                }
                TrayAction::Hide => {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                    self.window_hidden = true;
                }
                TrayAction::StartAgent => {
                    if self.host.is_none() {
                        self.host_start_clicked();
                    }
                }
                TrayAction::StopAgent => {
                    if self.host.is_some() {
                        self.host_stop_clicked();
                    }
                }
                TrayAction::Quit => {
                    self.quit_requested = true;
                    self.host_stop_clicked();
                    // Not canceled — eframe will exit after this frame.
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        // Keep serving while hidden: slow heartbeat so agent events and
        // consent requests keep being processed without a visible
        // window.
        if self.window_hidden {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    /// Pop the window back up when a host-side consent request arrives
    /// (the user must see and answer it even if the app lives in the
    /// tray).
    #[cfg(any(windows, target_os = "macos"))]
    fn auto_show_for_consent(&mut self, ctx: &Context) {
        if self.window_hidden && self.host.as_ref().is_some_and(|h| h.consent.is_some()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.window_hidden = false;
        }
    }

    /// Reload the cached unattended password for the host card display.
    /// Keychain first (its entry wins), config file as the fallback.
    pub(crate) fn refresh_unattended_display(&mut self) {
        self.unattended_display = if self.config.password_in_keychain {
            opacusr_agent::keychain::load_password()
                .unwrap_or_else(|_| self.config.password.clone())
        } else {
            self.config.password.clone()
        };
    }

    // ------------------------------------------------------------------
    // Viewer
    // ------------------------------------------------------------------

    pub(crate) fn connect_clicked(&mut self, label: Option<String>) {
        let input = self.peer_id_input.replace([' ', '-'], "");
        let password = self.password_input.clone();
        if password.is_empty() {
            self.status_message = "Введите пароль компьютера".into();
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
        // Outbound proxy for signaling + relay (validated early so the
        // user sees the typo, not a cryptic connect failure).
        let proxy_raw = self.config.proxy.trim().to_string();
        if !proxy_raw.is_empty() {
            if let Err(e) = opacusr_transport::Proxy::parse(&proxy_raw) {
                self.status_message = format!("Bad proxy URL: {e}");
                return;
            }
        }
        let totp = {
            let t = self.totp_input.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
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
                    totp: totp.clone(),
                    proxy: if proxy_raw.is_empty() {
                        None
                    } else {
                        Some(proxy_raw.clone())
                    },
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
                    self.status_message = "Введите корректный 9-значный ID или ip:port".into();
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
        self.status_message = "Отключено.".into();
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
            "Агент запущен (режим без присмотра).".into()
        } else {
            "Агент запущен (каждое подключение подтверждается вручную).".into()
        };
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
        // Re-assert the dark Rev-3 visuals every frame: egui's system
        // theme sync would otherwise repaint the UI light on Windows
        // with a light OS theme.
        crate::theme::install_visuals_each_frame(ctx);
        // `--tray` (MSI autostart): hide the window on the first frame;
        // the tray keeps the app (and the agent) running.
        #[cfg(any(windows, target_os = "macos"))]
        if self.start_hidden {
            self.start_hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_hidden = true;
        }
        crate::update_banner::ensure_started(ctx);
        crate::update_banner::paint(ctx);

        // Tray + close-to-tray (TeamViewer model). Runs first so a
        // queued «Выход» short-circuits the frame.
        #[cfg(any(windows, target_os = "macos"))]
        self.process_tray(ctx);
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
                    AgentEvent::Registered { id, .. } => {
                        h.my_id = Some(id);
                        // Persist the server-assigned ID so the home
                        // card can keep showing it while offline.
                        if self.config.last_peer_id != Some(id) {
                            self.config.last_peer_id = Some(id);
                            let _ = self.config.save();
                        }
                    }
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
                    recording,
                    tunnel,
                    ..
                } => {
                    sess.screen = screen;
                    sess.view_only = view_only;
                    sess.screens = screens;
                    sess.audio_available = audio_available;
                    sess.codec = Some(codec);
                    sess.fps_hint = fps_hint;
                    sess.recording = recording;
                    sess.tunnel_available = tunnel;
                    sess.selected_screen = 0;
                    sess.clipboard_sync = true;
                    sess.status = format!(
                        "connected to {host_name}{}",
                        if view_only { " (view only)" } else { "" }
                    );
                    sess.host_name = host_name;
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
                    // Keep the last frame for "print current frame":
                    // refreshed at most twice a second (a full RGBA copy
                    // per frame would be wasteful at 60 fps).
                    let now = std::time::Instant::now();
                    if sess
                        .last_frame_at
                        .map(|t| now.duration_since(t).as_millis() >= 500)
                        .unwrap_or(true)
                    {
                        sess.last_frame = Some((frame.width, frame.height, frame.rgba.clone()));
                        sess.last_frame_at = Some(now);
                    }
                    // Link statistics: sliding 2 s window of (arrival, wire bytes).
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
                ClientEvent::CursorShape {
                    id,
                    width,
                    height,
                    hotspot_x,
                    hotspot_y,
                    rgba,
                } => {
                    if width == 0 || height == 0 {
                        continue;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let tex =
                        ctx.load_texture(format!("cursor-{id}"), img, egui::TextureOptions::LINEAR);
                    sess.cursor_shapes.insert(
                        id,
                        RemoteCursor {
                            texture: tex,
                            hotspot: (hotspot_x as f32, hotspot_y as f32),
                            size: (width as f32, height as f32),
                        },
                    );
                    // Cursors change shape rarely; a small cache covers
                    // arrow/caret/hand/resign… plus headroom. Bounded so a
                    // pathological host cannot grow it forever.
                    if sess.cursor_shapes.len() > 32 {
                        sess.cursor_shapes.clear();
                    }
                }
                ClientEvent::CursorPos {
                    x,
                    y,
                    visible,
                    shape_id,
                } => {
                    sess.cursor_pos = Some((x, y, visible, shape_id));
                }
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
                ClientEvent::TunnelListening {
                    listen_port,
                    target_host,
                    target_port,
                } => {
                    sess.status = format!(
                        "tunnel: 127.0.0.1:{listen_port} → {target_host}:{target_port} (via host)"
                    );
                    sess.tunnels.push((listen_port, target_host, target_port));
                }
                ClientEvent::TunnelOpened { ok, error, .. } => {
                    if !ok {
                        sess.status = format!(
                            "tunnel refused: {}",
                            error.unwrap_or_else(|| "unknown".into())
                        );
                    }
                }
                ClientEvent::TunnelClosed { .. } => {}
                ClientEvent::QualityTier {
                    tier,
                    fps,
                    jpeg_quality,
                    scale_percent,
                    enabled,
                } => {
                    sess.quality_adaptive = enabled;
                    sess.quality_tier = if enabled { Some(tier) } else { None };
                    sess.quality_params = Some((fps, jpeg_quality, scale_percent));
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

        // ---- top bar (TeamViewer-style toolbar: logo left, window
        // opener icons right; no settings on the home screen itself) ----
        egui::TopBottomPanel::top("opacusr_top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("●")
                        .strong()
                        .size(18.0)
                        .color(theme::ACCENT),
                );
                ui.label(theme::bold("OpacusR").strong().size(18.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.session.is_some() && ui.button("⏹ Отключиться").clicked() {
                        self.disconnect_clicked();
                    }
                    if self.session.is_none() {
                        if ui
                            .button("👥 Контакты")
                            .on_hover_text("Контакты и устройства в сети (AnyDesk-стиль)")
                            .clicked()
                        {
                            self.show_contacts = !self.show_contacts;
                        }
                        if ui
                            .button("🕘 История")
                            .on_hover_text("История подключений")
                            .clicked()
                        {
                            self.show_history = !self.show_history;
                        }
                        if ui
                            .button("⚙ Настройки")
                            .on_hover_text("Все настройки — как в TeamViewer, в отдельном окне")
                            .clicked()
                        {
                            self.show_settings = !self.show_settings;
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Floating windows: settings (TeamViewer «Options»), contacts and
        // history (AnyDesk panels). Home-screen only — a viewer session
        // replaces the whole central area and hides the tool windows.
        if self.session.is_none() {
            settings::settings_window(ctx, self);
            contacts::contacts_window(ctx, self);
            contacts::history_window(ctx, self);
        }

        // Incoming-session consent popup: visible on the home screen and
        // while watching a remote desktop (a host-side request must not
        // get lost behind a viewer session).
        host_panel::consent_window(ctx, self);

        // A hidden (tray-mode) window must pop up for the user to
        // answer an incoming-session request.
        #[cfg(any(windows, target_os = "macos"))]
        self.auto_show_for_consent(ctx);

        if let Some(session) = self.session.as_mut() {
            view::session_ui(ctx, session, &mut self.config);
            return;
        }

        self.home_ui(ctx);
    }
}

impl App {
    /// The TeamViewer-like home screen: just the two cards (allow
    /// control / control a remote computer). Everything else — settings,
    /// contacts, history — lives in its own window opened from the top
    /// toolbar.
    fn home_ui(&mut self, ctx: &Context) {
        home::home_ui(ctx, self);
    }
}
