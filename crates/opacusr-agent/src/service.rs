//! The agent service: registers on the signaling server, handles session
//! requests and consent, negotiates WebRTC/relay, and spawns
//! [`SessionHost`]s. Optionally also listens for direct LAN sessions.

use tokio::sync::mpsc;

use opacusr_capture::ScreenCapture;
use opacusr_input::InputInjector;
use opacusr_proto::signal::PeerPayload;
use opacusr_proto::signal::*;
use opacusr_proto::Platform;
use opacusr_transport::secure::SecureSession;
use opacusr_transport::signaling::SignalingClient;
use opacusr_transport::tcp::TcpTransport;
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
    pub async fn run(
        self,
        mut consent: impl FnMut(opacusr_proto::PeerId, &str, Platform) -> SessionDecision + Send,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        if let Some(warning) = opacusr_capture::wayland_startup_warning() {
            let _ = self.events.send(AgentEvent::Notice(warning));
        }
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            match self.serve_once(&mut consent, &mut shutdown).await {
                Ok(_) => return Ok(()), // graceful shutdown
                Err(e) => {
                    let _ = self.events.send(AgentEvent::Notice(format!(
                        "server connection lost: {e}; retrying in {}s",
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

    async fn serve_once(
        &self,
        consent: &mut impl FnMut(opacusr_proto::PeerId, &str, Platform) -> SessionDecision,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut sig = SignalingClient::connect(&self.cfg.server_url).await?;
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
                    server: self.cfg.server_url.clone(),
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
                                    self.start_webrtc_session(&mut sig, from, &sdp, view_only).await?;
                                }
                                PeerPayload::RelayUse { ticket, port } => {
                                    let view_only = decisions.get(&from)
                                        .copied()
                                        .unwrap_or(SessionDecision::Full)
                                        == SessionDecision::ViewOnly;
                                    self.start_relay_session(from, ticket, port, view_only).await?;
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

    fn webrtc_config(&self) -> WebRtcConfig {
        let mut ice = Vec::new();
        for url in &self.cfg.ice_urls {
            // Support `turn://user:pass@host:port` style entries minimally:
            // plain URLs only for now; TURN creds come from the app config
            // (`turn_username`/`turn_password` are roadmap items).
            if let Some(rest) = url.strip_prefix("stun:") {
                ice.push(IceServerConfig::stun(&[&format!("stun:{rest}")]));
            } else {
                ice.push(IceServerConfig::stun(&[url.as_str()]));
            }
        }
        WebRtcConfig { ice_servers: ice }
    }

    async fn start_webrtc_session(
        &self,
        sig: &mut SignalingClient,
        peer: opacusr_proto::PeerId,
        offer_sdp: &str,
        view_only: bool,
    ) -> anyhow::Result<()> {
        let pc = new_peer_connection(&self.webrtc_config()).await?;
        let mut pending = AnswerPending::new(pc);
        let answer = pending.accept_offer(offer_sdp).await?;
        sig.send(&SignalMessage::PeerSend(PeerSend {
            to: peer,
            payload: PeerPayload::SdpAnswer { sdp: answer },
        }))
        .await?;
        let transport = pending.finish().await?;
        let _ = self.events.send(AgentEvent::SessionStarted { peer });
        self.spawn_host(SecureSession::new(Box::new(transport), None), view_only);
        Ok(())
    }

    async fn start_relay_session(
        &self,
        peer: opacusr_proto::PeerId,
        ticket: [u8; 16],
        port: u16,
        view_only: bool,
    ) -> anyhow::Result<()> {
        // Connect to the relay endpoint on the same host as the signaling
        // server.
        let host = relay_host_of(&self.cfg.server_url)?;
        let relay = opacusr_transport::relay::RelayTransport::connect(
            &format!("{host}:{port}"),
            ticket,
            opacusr_transport::relay::RelayRole::Host,
        )
        .await?;
        let _ = self.events.send(AgentEvent::SessionStarted { peer });
        self.spawn_host(SecureSession::new(Box::new(relay), None), view_only);
        Ok(())
    }

    fn spawn_host(&self, session: SecureSession, view_only: bool) {
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
            password: self.cfg.effective_password().unwrap_or_default(),
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
        };
        let host = SessionHost { cfg, events: tx };
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
        tokio::spawn(host.run(session, capture, injector));
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
                    self.spawn_host(SecureSession::new(Box::new(transport), None), view_only);
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
