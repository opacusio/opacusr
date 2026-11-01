//! The agent service: registers on the signaling server, handles session
//! requests and consent, negotiates WebRTC/relay, and spawns
//! [`SessionHost`]s. Optionally also listens for direct LAN sessions.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use opacusr_capture::ScreenCapture;
use opacusr_input::InputInjector;
use opacusr_proto::signal::PeerPayload;
use opacusr_proto::signal::*;
use opacusr_proto::Platform;
use opacusr_transport::lockout::{AuthGate, AuthGateConfig};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::signaling::SignalingClient;
use opacusr_transport::tcp::TcpTransport;
use opacusr_transport::turn::{TurnCredentialCache, TurnCredentials};
use opacusr_transport::webrtc::{
    new_peer_connection, AnswerPending, IceServerConfig, WebRtcConfig,
};

use crate::config::AgentConfig;
use crate::session::{CodecChoice, HostEvent, SessionConfig, SessionHost};

/// Decision about an incoming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDecision {
    /// Full control (input, files, terminal).
    Full,
    /// Watch only — the host ignores control traffic.
    ViewOnly,
    /// Refuse the connection.
    Reject,
}

/// Consent callback, shareable across all signaling endpoint tasks
/// (fan-out registration calls it from whichever endpoint got the
/// request first).
pub type ConsentFn = dyn Fn(opacusr_proto::PeerId, &str, Platform) -> SessionDecision + Send + Sync;

impl From<crate::config::Permission> for SessionDecision {
    fn from(p: crate::config::Permission) -> Self {
        match p {
            crate::config::Permission::Full => SessionDecision::Full,
            crate::config::Permission::ViewOnly => SessionDecision::ViewOnly,
            crate::config::Permission::Deny => SessionDecision::Reject,
        }
    }
}

/// Events surfaced to the UI / CLI.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Registered {
        id: opacusr_proto::PeerId,
        server: String,
    },
    IncomingRequest {
        from: opacusr_proto::PeerId,
        name: String,
        platform: Platform,
    },
    SessionStarted {
        peer: opacusr_proto::PeerId,
        /// Push host-side commands (file sends) into the live session.
        commands: mpsc::UnboundedSender<crate::session::HostCommand>,
    },
    SessionEnded {
        reason: String,
    },
    Notice(String),
}

pub struct AgentService {
    pub cfg: AgentConfig,
    pub events: mpsc::UnboundedSender<AgentEvent>,
    /// Use the deterministic test source instead of a real screen
    /// (`--demo`, CI).
    pub demo: bool,
    /// Auto-accept incoming sessions (unattended mode). When `false`, a
    /// consent callback is used.
    pub unattended: bool,
}

impl AgentService {
    /// Consent callback for attended mode (GUI dialog / CLI prompt).
    /// Returning [`SessionDecision::Reject`] refuses the session.
    ///
    /// Registers on **every** configured signaling endpoint
    /// (`signaling_endpoints()` + the embedded listener when
    /// `signal_listen` is on) simultaneously — one supervised task per
    /// endpoint, reconnecting independently with backoff. Each endpoint
    /// assigns its own ID; the loss of one endpoint never takes the
    /// others down.
    pub async fn run(
        self,
        consent: impl Fn(opacusr_proto::PeerId, &str, Platform) -> SessionDecision
            + Send
            + Sync
            + 'static,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        if let Some(warning) = opacusr_capture::wayland_startup_warning() {
            let _ = self.events.send(AgentEvent::Notice(warning));
        }
        // One auth gate for the whole signaling path: every endpoint and
        // every session it accepts shares the failure budget (see
        // crate::service docs).
        let gate = self.auth_gate();

        // Zero-infra mode: serve the registry + relay in-process and
        // register on the local endpoint like on any other one.
        let mut endpoints = self.cfg.signaling_endpoints();
        if self.cfg.signal_listen {
            match self.spawn_embedded_signaling().await {
                Ok(()) => {
                    let url = self.cfg.embedded_signal_url();
                    if !endpoints.iter().any(|u| u == &url) {
                        endpoints.push(url);
                    }
                }
                Err(e) => {
                    let _ = self.events.send(AgentEvent::Notice(format!(
                        "embedded signaling unavailable: {e}"
                    )));
                }
            }
        }

        let consent: Arc<ConsentFn> = Arc::new(consent);
        let mut set = tokio::task::JoinSet::new();
        for url in endpoints {
            let svc = AgentService {
                cfg: self.cfg.clone(),
                events: self.events.clone(),
                demo: self.demo,
                unattended: self.unattended,
            };
            let consent = consent.clone();
            let gate = gate.clone();
            let shutdown = shutdown.clone();
            set.spawn(async move { svc.endpoint_loop(url, consent, gate, shutdown).await });
        }

        // Tasks end only on shutdown (they retry errors internally), so
        // an `Ok` here means the agent is shutting down. Everything else
        // is a dead task; fail only when *all* endpoints are dead.
        let mut last_err = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => last_err = Some(e),
                Err(e) => last_err = Some(anyhow::anyhow!("endpoint task failed: {e}")),
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Supervised loop for one signaling endpoint: connect → register →
    /// serve, reconnecting with capped backoff until shutdown. A flapping
    /// endpoint never takes the other endpoints down.
    async fn endpoint_loop(
        &self,
        url: String,
        consent: Arc<ConsentFn>,
        gate: Arc<AuthGate>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // Time-limited TURN credentials per endpoint, cached across
        // sessions for that endpoint's TTL (no-op when turn_api is off).
        let turn = if self.cfg.turn_api {
            TurnCredentialCache::from_server_url(&url)
        } else {
            TurnCredentialCache::disabled()
        };
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            match self
                .serve_once(&url, consent.as_ref(), &mut shutdown, &gate, &turn)
                .await
            {
                Ok(()) => return Ok(()), // graceful shutdown
                Err(e) => {
                    let _ = self.events.send(AgentEvent::Notice(format!(
                        "{url}: connection lost: {e}; retrying in {}s",
                        backoff.as_secs()
                    )));
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.changed() => return Ok(()),
                    }
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
        }
    }

    /// Serve the signaling registry + TCP relay inside this process
    /// (zero-infra mode, `signal_listen`): the same machinery as the
    /// standalone `opacusr-server`, sharing its env knobs
    /// (`OPACUSR_RATE_MAX`, `OPACUSR_MAX_PEERS`, `OPACUSR_TURN_SECRET`, …).
    /// Expose it behind a free tunnel or a mesh VPN — docs/serverless.md.
    async fn spawn_embedded_signaling(&self) -> anyhow::Result<()> {
        use opacusr_server::{AppState, Limits, TurnConfig};
        let http_addr: std::net::SocketAddr =
            format!("0.0.0.0:{}", self.cfg.signal_listen_port).parse()?;
        let relay_port = self
            .cfg
            .signal_listen_port
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("relay port overflow"))?;
        let relay_addr: std::net::SocketAddr = format!("0.0.0.0:{relay_port}").parse()?;
        let http = tokio::net::TcpListener::bind(http_addr).await?;
        let relay = tokio::net::TcpListener::bind(relay_addr).await?;
        let state = Arc::new(AppState::with_options(
            relay_port,
            Limits::from_env(),
            TurnConfig::from_env(),
        ));
        tokio::spawn(async move {
            if let Err(e) = opacusr_server::serve(http, relay, state).await {
                tracing::warn!(target: "opacusr", "embedded signaling stopped: {e}");
            }
        });
        let _ = self.events.send(AgentEvent::Notice(format!(
            "embedded signaling on 0.0.0.0:{} (relay on {}); expose it via a tunnel and list the tunnel URL in `signaling_urls`",
            self.cfg.signal_listen_port, relay_port
        )));
        Ok(())
    }

    async fn serve_once(
        &self,
        url: &str,
        consent: &ConsentFn,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        gate: &Arc<AuthGate>,
        turn: &TurnCredentialCache,
    ) -> anyhow::Result<()> {
        let mut sig = SignalingClient::connect(url).await?;
        sig.send(&SignalMessage::Register(Register {
            platform: opacusr_proto::Platform::current(),
            display_name: self.cfg.name.clone(),
            is_agent: true,
            unattended: self.unattended,
        }))
        .await?;

        let my_id = match sig.recv().await? {
            SignalMessage::Welcome(w) => {
                let _ = self.events.send(AgentEvent::Registered {
                    id: w.id,
                    server: url.to_string(),
                });
                w.id
            }
            SignalMessage::Error { message } => anyhow::bail!("server error: {message}"),
            other => anyhow::bail!("unexpected server reply: {other:?}"),
        };
        let _ = my_id;

        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Per-peer permission decisions, keyed by PeerId (filled on consent).
        let mut decisions: std::collections::HashMap<opacusr_proto::PeerId, SessionDecision> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    let _ = sig.close().await;
                    return Ok(());
                }
                _ = heartbeat.tick() => {
                    let _ = sig.send(&SignalMessage::Heartbeat).await;
                }
                msg = sig.recv() => {
                    let msg = msg?;
                    match msg {
                        SignalMessage::SessionIncoming(incoming) => {
                            // Permission policy: ACL (+ default) applies in
                            // unattended mode; a human decides in attended
                            // mode. "Deny" always refuses.
                            let decision = if self.unattended {
                                SessionDecision::from(self.cfg.permission_for(&incoming.display_name))
                            } else if !self.cfg.is_unattended() {
                                let _ = self.events.send(AgentEvent::IncomingRequest {
                                    from: incoming.from,
                                    name: incoming.display_name.clone(),
                                    platform: incoming.platform,
                                });
                                consent(incoming.from, &incoming.display_name, incoming.platform)
                            } else {
                                // Password configured but running attended
                                // (LAN fallback) — still ask, default allow.
                                let _ = self.events.send(AgentEvent::IncomingRequest {
                                    from: incoming.from,
                                    name: incoming.display_name.clone(),
                                    platform: incoming.platform,
                                });
                                consent(incoming.from, &incoming.display_name, incoming.platform)
                            };
                            if decision == SessionDecision::Reject {
                                let _ = self.events.send(AgentEvent::Notice(format!(
                                    "session from {} denied by policy",
                                    incoming.display_name
                                )));
                            }
                            let payload = match decision {
                                SessionDecision::Reject => PeerPayload::Reject {
                                    reason: "declined by the host".into(),
                                },
                                _ => PeerPayload::Accept,
                            };
                            if decision != SessionDecision::Reject {
                                decisions.insert(incoming.from, decision);
                            }
                            sig.send(&SignalMessage::PeerSend(PeerSend { to: incoming.from, payload }))
                                .await?;
                        }
                        SignalMessage::PeerRecv(PeerRecv { from, payload }) => {
                            match payload {
                                PeerPayload::SdpOffer { sdp } => {
                                    let view_only = decisions.get(&from)
                                        .copied()
                                        .unwrap_or(SessionDecision::Full)
                                        == SessionDecision::ViewOnly;
                                    self.start_webrtc_session(
                                        &mut sig,
                                        from,
                                        &sdp,
                                        view_only,
                                        gate,
                                        turn,
                                    )
                                    .await?;
                                }
                                PeerPayload::RelayUse { ticket, port } => {
                                    let view_only = decisions.get(&from)
                                        .copied()
                                        .unwrap_or(SessionDecision::Full)
                                        == SessionDecision::ViewOnly;
                                    self.start_relay_session(url, from, ticket, port, view_only, gate)
                                        .await?;
                                }
                                _ => {}
                            }
                        }
                        SignalMessage::PeerGone(g) => {
                            let _ = self.events.send(AgentEvent::Notice(format!("peer {} went offline", g.peer)));
                        }
                        SignalMessage::Error { message } => {
                            let _ = self.events.send(AgentEvent::Notice(format!("server: {message}")));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// One auth gate per accept path: failed proofs are shared across all
    /// sessions of that path, so rotating peer IDs / display names do not
    /// reset the brute-force budget.
    fn auth_gate(&self) -> Arc<AuthGate> {
        Arc::new(AuthGate::new(AuthGateConfig {
            max_attempts: self.cfg.auth_max_attempts,
            window: Duration::from_secs(self.cfg.auth_window_secs),
            lockout: Duration::from_secs(self.cfg.auth_lockout_secs),
            max_lockout: Duration::from_secs(15 * 60),
        }))
    }

    /// ICE configuration: the configured STUN entries plus, when minted
    /// credentials are available, the server's coturn URIs as a TURN
    /// entry (media can relay when direct paths fail).
    fn webrtc_config(&self, turn: Option<&TurnCredentials>) -> WebRtcConfig {
        let mut ice = Vec::new();
        for url in &self.cfg.ice_urls {
            // Plain STUN URLs only — the TURN entry (if any) comes from
            // the server's `GET /turn` below, with matching credentials.
            if let Some(rest) = url.strip_prefix("stun:") {
                ice.push(IceServerConfig::stun(&[&format!("stun:{rest}")]));
            } else {
                ice.push(IceServerConfig::stun(&[url.as_str()]));
            }
        }
        if let Some(t) = turn {
            let uris: Vec<&str> = t.uris.iter().map(String::as_str).collect();
            ice.push(IceServerConfig::turn(&uris, &t.username, &t.password));
        }
        WebRtcConfig { ice_servers: ice }
    }

    /// Minted TURN credentials for the current signaling path, or `None`
    /// (degrade to STUN-only) when disabled, cooling down or unreachable.
    async fn turn_credentials(&self, turn: &TurnCredentialCache) -> Option<TurnCredentials> {
        if !turn.enabled() {
            return None;
        }
        let fetch = tokio::task::spawn_blocking({
            let turn = turn.clone();
            move || turn.get()
        });
        match tokio::time::timeout(Duration::from_secs(8), fetch).await {
            Ok(Ok(creds)) => creds,
            Ok(Err(e)) => {
                tracing::warn!(target: "opacusr", "TURN fetch task failed: {e}");
                None
            }
            Err(_) => {
                tracing::warn!(target: "opacusr", "TURN credential fetch timed out; STUN-only");
                None
            }
        }
    }

    async fn start_webrtc_session(
        &self,
        sig: &mut SignalingClient,
        peer: opacusr_proto::PeerId,
        offer_sdp: &str,
        view_only: bool,
        gate: &Arc<AuthGate>,
        turn: &TurnCredentialCache,
    ) -> anyhow::Result<()> {
        let turn_creds = self.turn_credentials(turn).await;
        let pc = new_peer_connection(&self.webrtc_config(turn_creds.as_ref())).await?;
        let mut pending = AnswerPending::new(pc);
        let answer = pending.accept_offer(offer_sdp).await?;
        sig.send(&SignalMessage::PeerSend(PeerSend {
            to: peer,
            payload: PeerPayload::SdpAnswer { sdp: answer },
        }))
        .await?;
        let transport = pending.finish().await?;
        self.spawn_host(
            SecureSession::new(Box::new(transport), None),
            peer,
            view_only,
            gate,
        );
        Ok(())
    }

    async fn start_relay_session(
        &self,
        url: &str,
        peer: opacusr_proto::PeerId,
        ticket: [u8; 16],
        port: u16,
        view_only: bool,
        gate: &Arc<AuthGate>,
    ) -> anyhow::Result<()> {
        // Connect to the relay endpoint on the same host as THIS
        // signaling endpoint (fan-out: every endpoint carries its own
        // relay).
        let host = relay_host_of(url)?;
        let relay = opacusr_transport::relay::RelayTransport::connect(
            &format!("{host}:{port}"),
            ticket,
            opacusr_transport::relay::RelayRole::Host,
        )
        .await?;
        self.spawn_host(
            SecureSession::new(Box::new(relay), None),
            peer,
            view_only,
            gate,
        );
        Ok(())
    }

    fn spawn_host(
        &self,
        session: SecureSession,
        peer: opacusr_proto::PeerId,
        view_only: bool,
        gate: &Arc<AuthGate>,
    ) {
        // Event pump: HostEvent → AgentEvent for the UI.
        let (tx, rx) = mpsc::unbounded_channel();
        let events = self.events.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(ev) = rx.recv().await {
                let _ = events.send(match ev {
                    HostEvent::Started { viewer, .. } => {
                        AgentEvent::Notice(format!("session started with {viewer}"))
                    }
                    HostEvent::Ended { reason } => AgentEvent::SessionEnded { reason },
                    HostEvent::Chat(text) => AgentEvent::Notice(format!("chat: {text}")),
                    HostEvent::Ft(f) => AgentEvent::Notice(format!("file transfer: {f:?}")),
                    HostEvent::Notice(s) => AgentEvent::Notice(s),
                });
            }
        });

        // Per-session resources.
        let cfg = SessionConfig {
            // `None` (nothing configured) rejects handshakes outright —
            // an empty password must never become the session secret.
            password: self.cfg.effective_password(),
            display_name: self.cfg.name.clone(),
            fps: self.cfg.fps,
            jpeg_quality: self.cfg.jpeg_quality,
            scale_percent: self.cfg.scale_percent,
            download_dir: self.cfg.download_dir.clone(),
            auto_accept_files: self.cfg.auto_accept_files,
            codec: CodecChoice::parse(&self.cfg.codec),
            view_only,
            clipboard_sync: self.cfg.clipboard_sync,
            adaptive_quality: self.cfg.adaptive_quality,
            audio: self.cfg.audio,
            audio_device: self.cfg.audio_device.clone(),
            lock_input: self.cfg.lock_input,
            totp_secret: self.cfg.totp_secret.clone(),
            record: self.cfg.record,
            record_dir: self.cfg.record_dir.clone(),
            tunnel: crate::tunnel::TunnelPolicy::parse(&self.cfg.tunnel),
            auth_gate: Some(gate.clone()),
        };
        // Per-session command channel: the UI pushes HostCommands (file
        // sends) into the live session through it.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let _ = self.events.send(AgentEvent::SessionStarted {
            peer,
            commands: cmd_tx,
        });
        let host = SessionHost {
            cfg,
            events: tx,
            commands: cmd_rx,
        };
        let capture: Box<dyn ScreenCapture> = if self.demo {
            Box::new(opacusr_capture::test::TestSource::new(1280, 720))
        } else {
            match opacusr_capture::platform_capture(
                false,
                opacusr_capture::WinBackend::parse(&self.cfg.capture),
            ) {
                Ok(c) => c,
                Err(e) => {
                    let _ = self
                        .events
                        .send(AgentEvent::Notice(format!("no capture backend: {e}")));
                    return;
                }
            }
        };
        let injector: Box<dyn InputInjector> = opacusr_input::platform_injector();
        // Cursor overlay source: synthetic in demo mode, platform poller
        // otherwise (backends without support simply never send updates).
        let cursor: Box<dyn opacusr_capture::CursorSource> = if self.demo {
            Box::new(opacusr_capture::TestCursor::new((1280, 720)))
        } else {
            opacusr_capture::platform_cursor()
        };
        tokio::spawn(host.run(session, capture, injector, cursor));
    }

    /// Direct LAN listener: no signaling, password still required.
    pub async fn serve_lan(
        &self,
        port: u16,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        if let Some(warning) = opacusr_capture::wayland_startup_warning() {
            let _ = self.events.send(AgentEvent::Notice(warning));
        }
        // LAN path has its own gate (separate budget from signaling).
        let gate = self.auth_gate();
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
        let _ = self.events.send(AgentEvent::Notice(format!(
            "LAN listener on 0.0.0.0:{port}"
        )));
        loop {
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                accepted = listener.accept() => {
                    let (sock, addr) = accepted?;
                    let _ = self.events.send(AgentEvent::Notice(format!("LAN connection from {addr}")));
                    let transport = TcpTransport::from_stream(sock);
                    // LAN sessions are password-gated; the ACL still applies
                    // (the viewer's Hello carries its display name — checked
                    // here by name after the handshake would be too late, so
                    // the configured default decides).
                    let view_only = SessionDecision::from(
                        self.cfg.permission_for(""),
                    ) == SessionDecision::ViewOnly;
                    self.spawn_host(SecureSession::new(Box::new(transport), None), 0, view_only, &gate);
                }
            }
        }
    }
}

/// Extract `host[:port]` from a `ws://host[:port]/path` URL (the relay
/// endpoint shares the signaling server host).
fn relay_host_of(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("ws://")
        .or_else(|| url.strip_prefix("wss://"))
        .ok_or_else(|| anyhow::anyhow!("unsupported signaling url: {url}"))?;
    let host_port = rest.split('/').next().unwrap_or(rest);
    Ok(host_port.to_string())
}
