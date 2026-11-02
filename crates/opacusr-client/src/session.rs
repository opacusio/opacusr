//! The viewer session: establishes the connection (signaling → WebRTC with
//! TCP-relay fallback, or direct LAN TCP), performs the password handshake
//! and runs the message pump. Control happens through [`Command`]s, output
//! through [`ClientEvent`]s — a shape designed for GUI event loops.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use opacusr_capture::Frame;
use opacusr_codec::{make_pair, VideoDecoder};
use opacusr_input::clipboard::{
    hash_clip, ClipContent, ClipboardHandle, CLIPBOARD_IMAGE_MAX_PNG_BYTES,
};
use opacusr_proto::message::{self, ByeReason, CodecId};
use opacusr_proto::signal::*;
use opacusr_proto::{Message, PeerId, Platform};
use opacusr_transport::auth::viewer_handshake;
use opacusr_transport::ft::{FtEngine, FtEvent};
use opacusr_transport::relay::{RelayRole, RelayTransport};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::signaling::SignalingClient;
use opacusr_transport::tcp::TcpTransport;
use opacusr_transport::webrtc::{
    accept_answer, create_channels, make_offer, new_peer_connection, WebRtcConfig, WebRtcTransport,
};
use opacusr_transport::TransportError;

/// Which wire to try for a server-mediated session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportChoice {
    /// Try WebRTC first, fall back to the TCP relay.
    Auto,
    WebRtcOnly,
    RelayOnly,
}

/// Commands the UI sends into the running session.
#[derive(Debug)]
pub enum Command {
    MouseMove {
        x: u32,
        y: u32,
    },
    MouseButton {
        x: u32,
        y: u32,
        button: message::MouseButtonId,
        down: bool,
    },
    MouseWheel {
        x: u32,
        y: u32,
        dx: i32,
        dy: i32,
    },
    KeyEvent {
        key: message::OpKey,
        mods: u8,
        down: bool,
    },
    TextInput {
        text: String,
    },
    Chat(String),
    SendClipboard(String),
    /// Switch the streamed host monitor (id from `Connected.screens`).
    SelectScreen(u32),
    /// Enable/disable live clipboard synchronization (both directions).
    ClipboardSync(bool),
    /// Request the host to start/stop audio streaming.
    Audio(bool),
    Quality {
        fps: u8,
        jpeg_quality: u8,
        scale_percent: u16,
    },
    Pause(bool),
    Ping,
    PushFile(PathBuf),
    CancelTransfer(u32),
    TermOpen {
        rows: u16,
        cols: u16,
    },
    TermData {
        id: u32,
        data: Vec<u8>,
    },
    TermResize {
        id: u32,
        rows: u16,
        cols: u16,
    },
    TermClose {
        id: u32,
    },
    /// Start forwarding a local TCP port through the session: connections
    /// to `127.0.0.1:listen_port` are relayed to `target_host:target_port`
    /// as seen from the host (SSH-style local forwarding). The host's
    /// tunnel policy gates every target; `listen_port = 0` binds an
    /// ephemeral port (the real one arrives via `ClientEvent::TunnelListening`).
    TunnelListen {
        listen_port: u16,
        target_host: String,
        target_port: u16,
    },
    /// Close a tunnel listener and all its live connections.
    TunnelStop {
        listen_port: u16,
    },
    Disconnect,
}

/// Events the session emits to the UI.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Connected {
        host_name: String,
        platform: Platform,
        screen: (u32, u32),
        /// `true` = host granted view-only access; the UI should not send
        /// input (the host ignores it anyway).
        view_only: bool,
        /// Host monitors available for [`Command::SelectScreen`].
        screens: Vec<message::ScreenInfo>,
        /// Host can stream audio on request.
        audio_available: bool,
        /// Video codec negotiated in `SessionAccept`.
        codec: CodecId,
        /// Host's suggested frame rate (`SessionAccept.fps_hint`).
        fps_hint: u8,
        /// `true` = the host records this session (opt-in policy).
        recording: bool,
        /// `true` = the host forwards TCP tunnels for this session
        /// (tunnel policy enabled, full-control session).
        tunnel: bool,
    },
    /// One decoded video frame. `wire_bytes` is the compressed size on
    /// the wire (for the bitrate meter).
    Frame {
        frame: Frame,
        wire_bytes: u32,
    },
    Chat {
        text: String,
        ts_ms: u64,
    },
    Rtt(u32),
    FtProgress {
        id: u32,
        done: u64,
        total: u64,
    },
    FtStarted {
        id: u32,
        name: String,
        total: u64,
    },
    FtReceivedDone {
        path: PathBuf,
        verified: bool,
    },
    FtSentDone {
        id: u32,
        ok: bool,
    },
    FtFailed {
        id: u32,
        reason: String,
    },
    TermOpened {
        id: u32,
        ok: bool,
        error: Option<String>,
        shell: String,
    },
    TermData {
        id: u32,
        data: Vec<u8>,
    },
    /// A local tunnel listener is bound; `listen_port` is the real port
    /// when the requested one was 0.
    TunnelListening {
        listen_port: u16,
        target_host: String,
        target_port: u16,
    },
    /// Host answered a tunnel open request.
    TunnelOpened {
        id: u32,
        ok: bool,
        error: Option<String>,
    },
    /// Host closed a tunnel connection (socket EOF on the host side).
    TunnelClosed {
        id: u32,
    },
    /// Host → viewer: the remote pointer changed appearance. Cache the
    /// bitmap by `id` until the next shape with the same id arrives.
    CursorShape {
        id: u64,
        width: u32,
        height: u32,
        hotspot_x: u32,
        hotspot_y: u32,
        rgba: Vec<u8>,
    },
    /// Host → viewer: the remote pointer moved. `(x, y)` is the hotspot in
    /// native streamed-screen pixels; the overlay is hidden when the
    /// position is outside the frame or `visible` is false.
    CursorPos {
        x: i32,
        y: i32,
        visible: bool,
        shape_id: u64,
    },
    Notice(String),
    Closed(String),
}

/// Client-side settings for one session.
#[derive(Debug, Clone)]
pub struct SessionSettings {
    pub password: String,
    pub display_name: String,
    pub download_dir: PathBuf,
    pub auto_accept_files: bool,
    /// Two-way clipboard synchronization.
    pub clipboard_sync: bool,
    /// RFC 6238 code typed by the user (hosts with 2FA verify it in
    /// addition to the password; ignored when the host has no secret).
    pub totp: Option<String>,
    pub webrtc: WebRtcConfig,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            password: String::new(),
            display_name: "OpacusR viewer".into(),
            download_dir: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("OpacusR-Received"),
            auto_accept_files: true,
            clipboard_sync: true,
            totp: None,
            webrtc: WebRtcConfig {
                ice_servers: vec![opacusr_transport::webrtc::IceServerConfig::stun(&[
                    "stun:stun.l.google.com:19302",
                ])],
            },
        }
    }
}

/// Internal message from a local tunnel socket (or its accept loop) into
/// the session pump.
#[derive(Debug)]
enum TunLocal {
    /// Local side connected; request the host to open the target.
    Accepted {
        id: u32,
        target_host: String,
        target_port: u16,
        /// Bytes from the host are written into the local socket via this.
        writer: mpsc::UnboundedSender<Vec<u8>>,
        listen_port: u16,
    },
    /// Bytes from the local app heading to the host.
    Data { id: u32, data: Vec<u8> },
    /// Local socket closed — tell the host.
    Closed { id: u32 },
}

/// A live tunnel listener (per `Command::TunnelListen`).
#[derive(Debug)]
struct TunListener {
    /// Any message stops the accept loop.
    stop: mpsc::UnboundedSender<()>,
}

/// One live forwarded connection.
#[derive(Debug)]
struct TunnelEntry {
    writer: mpsc::UnboundedSender<Vec<u8>>,
    listen_port: u16,
}

/// Spawn the local-socket half of a tunnel: reads from the app, forwards
/// to the session pump; writes whatever the host sends. An empty `Vec` in
/// the writer channel is the close marker (host side or session abort).
fn spawn_local_bridge(
    id: u32,
    sock: tokio::net::TcpStream,
    local_tx: mpsc::UnboundedSender<TunLocal>,
) -> mpsc::UnboundedSender<Vec<u8>> {
    let (w_tx, mut w_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut r, mut w) = sock.into_split();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            tokio::select! {
                // App → session. Stays active after the host half-closes so
                // the local app sees a clean EOF handshake.
                read = r.read(&mut buf) => {
                    match read {
                        Ok(0) | Err(_) => {
                            let _ = local_tx.send(TunLocal::Closed { id });
                            break;
                        }
                        Ok(n) => {
                            let _ = local_tx.send(TunLocal::Data {
                                id,
                                data: buf[..n].to_vec(),
                            });
                        }
                    }
                }
                chunk = w_rx.recv() => {
                    match chunk {
                        // Session dropped us (stop/disconnect).
                        None => break,
                        // Host closed: half-close the local socket, keep
                        // draining until the app closes too.
                        Some(m) if m.is_empty() => {
                            let _ = w.shutdown().await;
                        }
                        Some(m) => {
                            if w.write_all(&m).await.is_err() {
                                let _ = local_tx.send(TunLocal::Closed { id });
                                break;
                            }
                        }
                    }
                }
            }
        }
        let _ = w.shutdown().await;
    });
    w_tx
}

/// Accept loop for one tunnel listener: assign ids, bridge each local
/// connection, request opens through the pump.
async fn tunnel_accept_loop(
    listener: tokio::net::TcpListener,
    target_host: String,
    target_port: u16,
    listen_port: u16,
    next_tun_id: Arc<AtomicU32>,
    local_tx: mpsc::UnboundedSender<TunLocal>,
    mut stop_rx: mpsc::UnboundedReceiver<()>,
) {
    loop {
        tokio::select! {
            _ = stop_rx.recv() => break,
            acc = listener.accept() => {
                let Ok((sock, _)) = acc else { break };
                let id = next_tun_id.fetch_add(1, Ordering::Relaxed);
                let writer = spawn_local_bridge(id, sock, local_tx.clone());
                let _ = local_tx.send(TunLocal::Accepted {
                    id,
                    target_host: target_host.clone(),
                    target_port,
                    writer,
                    listen_port,
                });
            }
        }
    }
}

/// A connected viewer session.
pub struct SessionClient {
    session: SecureSession,
    decoder: Box<dyn VideoDecoder>,
    settings: SessionSettings,
    events: mpsc::UnboundedSender<ClientEvent>,
    ft: FtEngine,
    next_chat_id: u64,
    next_ping: u64,
    ping_times: HashMap<u64, Instant>,
    next_term_id: u32,
    /// Monotonic tunnel connection ids (shared with accept-loop tasks).
    next_tun_id: Arc<AtomicU32>,
    /// Live forwarded connections by tunnel id.
    tunnels: HashMap<u32, TunnelEntry>,
    /// Active tunnel listeners by real (bound) port.
    tun_listeners: HashMap<u16, TunListener>,
    /// From local tunnel sockets into the pump.
    tun_local_tx: mpsc::UnboundedSender<TunLocal>,
    tun_local_rx: mpsc::UnboundedReceiver<TunLocal>,
    screen: (u32, u32),
    /// Host-side permission (from `SessionAccept`).
    view_only: bool,
    /// Host forwards tunnels (from `SessionAccept`).
    tunnel_available: bool,
    /// Host monitors (from `SessionAccept`).
    screens: Vec<message::ScreenInfo>,
    audio_available: bool,
    /// Clipboard sync worker + echo-suppression state. Markers are
    /// content hashes so text and images share one mechanism.
    clipboard: Option<ClipboardHandle>,
    clip_sent: Option<u64>,
    clip_applied: Option<u64>,
    /// Audio playback (created lazily on the first `AudioFrame`).
    #[cfg(feature = "audio")]
    player: Option<crate::audio::AudioPlayer>,
    /// Frames received since the last `LinkStats` report (for recv_fps).
    frames_since_stats: u32,
    stats_started: Instant,
}

impl SessionClient {
    // ------------------------------------------------------------------
    // Establishment
    // ------------------------------------------------------------------

    /// Full server-mediated connection: request the session from `host_id`,
    /// negotiate WebRTC (with relay fallback) and authenticate.
    /// Returns the client plus the event receiver for the UI.
    pub async fn connect_via_server(
        server_url: &str,
        host_id: PeerId,
        settings: SessionSettings,
        choice: TransportChoice,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ClientEvent>), TransportError> {
        let mut sig = SignalingClient::connect(server_url).await?;
        sig.send(&SignalMessage::Register(Register {
            platform: Platform::current(),
            display_name: settings.display_name.clone(),
            is_agent: false,
            unattended: false,
        }))
        .await?;
        let _my_id = match sig.recv().await? {
            SignalMessage::Welcome(w) => w.id,
            SignalMessage::Error { message } => return Err(TransportError::Signaling(message)),
            other => return Err(TransportError::Signaling(format!("unexpected: {other:?}"))),
        };

        sig.send(&SignalMessage::RequestSession(RequestSession {
            peer: host_id,
            display_name: settings.display_name.clone(),
        }))
        .await?;

        // Await the host decision.
        let payload = loop {
            match sig.recv().await? {
                SignalMessage::PeerRecv(p) if p.from == host_id => break p.payload,
                SignalMessage::SessionFailed(f) => {
                    return Err(TransportError::Signaling(format!(
                        "host unavailable: {}",
                        f.reason
                    )));
                }
                SignalMessage::Error { message } => return Err(TransportError::Signaling(message)),
                SignalMessage::PeerGone(_) => {
                    return Err(TransportError::Signaling("host went offline".into()));
                }
                _ => {}
            }
        };
        match payload {
            PeerPayload::Reject { reason } => {
                return Err(TransportError::Signaling(format!(
                    "host declined: {reason}"
                )));
            }
            PeerPayload::Accept => {}
            _ => return Err(TransportError::Signaling("unexpected peer payload".into())),
        }

        // Negotiate the wire.
        let transport: Box<dyn opacusr_transport::Transport> = match choice {
            TransportChoice::WebRtcOnly | TransportChoice::Auto => {
                match Self::webrtc_via(&mut sig, host_id, &settings).await {
                    Ok(t) => Box::new(t),
                    Err(e) if choice == TransportChoice::Auto => {
                        tracing::warn!(target: "opacusr", "webrtc failed ({e}); falling back to the TCP relay");
                        Box::new(Self::relay_via(&mut sig, host_id, server_url).await?)
                    }
                    Err(e) => return Err(e),
                }
            }
            TransportChoice::RelayOnly => {
                Box::new(Self::relay_via(&mut sig, host_id, server_url).await?)
            }
        };

        let _ = sig.close().await;
        Self::finish_handshake(SecureSession::new(transport, None), settings).await
    }

    async fn webrtc_via(
        sig: &mut SignalingClient,
        host_id: PeerId,
        settings: &SessionSettings,
    ) -> Result<WebRtcTransport, TransportError> {
        let pc = new_peer_connection(&settings.webrtc).await?;
        let (control, media) = create_channels(&pc).await?;
        let offer = make_offer(&pc).await?;
        sig.send(&SignalMessage::PeerSend(PeerSend {
            to: host_id,
            payload: PeerPayload::SdpOffer { sdp: offer },
        }))
        .await?;
        // Wait for the answer.
        let answer = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                match sig.recv().await? {
                    SignalMessage::PeerRecv(p) if p.from == host_id => match p.payload {
                        PeerPayload::SdpAnswer { sdp } => return Ok(sdp),
                        PeerPayload::Reject { reason } => {
                            return Err(TransportError::Signaling(format!(
                                "host declined: {reason}"
                            )));
                        }
                        other => {
                            return Err(TransportError::Signaling(format!(
                                "unexpected payload {other:?}"
                            )));
                        }
                    },
                    SignalMessage::Error { message } => {
                        return Err(TransportError::Signaling(message))
                    }
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| TransportError::Timeout)??;
        accept_answer(&pc, &answer).await?;
        WebRtcTransport::from_offerer(pc, control, media).await
    }

    async fn relay_via(
        sig: &mut SignalingClient,
        host_id: PeerId,
        server_url: &str,
    ) -> Result<RelayTransport, TransportError> {
        // Ask the host to allocate a relay ticket.
        sig.send(&SignalMessage::PeerSend(PeerSend {
            to: host_id,
            payload: PeerPayload::RelayUse {
                ticket: [0u8; 16],
                port: 0,
            },
        }))
        .await?;
        let (ticket, port) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                match sig.recv().await? {
                    SignalMessage::PeerRecv(p) if p.from == host_id => match p.payload {
                        PeerPayload::RelayUse { ticket, port } => return Ok((ticket, port)),
                        other => {
                            return Err(TransportError::Signaling(format!(
                                "unexpected payload {other:?}"
                            )));
                        }
                    },
                    SignalMessage::Error { message } => {
                        return Err(TransportError::Signaling(message))
                    }
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| TransportError::Timeout)??;

        let host = relay_host(server_url)?;
        RelayTransport::connect(&format!("{host}:{port}"), ticket, RelayRole::Viewer).await
    }

    /// Direct TCP connection to a LAN agent (`ip:port`).
    pub async fn connect_direct(
        addr: &str,
        settings: SessionSettings,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ClientEvent>), TransportError> {
        let transport = TcpTransport::connect(addr).await?;
        Self::finish_handshake(SecureSession::new(Box::new(transport), None), settings).await
    }

    async fn finish_handshake(
        mut session: SecureSession,
        settings: SessionSettings,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ClientEvent>), TransportError> {
        let handshake = viewer_handshake(
            &mut session,
            &settings.password,
            &settings.display_name,
            settings.totp.as_deref(),
        )
        .await?;
        let accept = match session.recv_msg().await? {
            Message::SessionAccept(a) => a,
            other => {
                return Err(TransportError::Other(format!(
                    "expected SessionAccept, got {other:?}"
                )))
            }
        };
        let screen = (accept.screen_w, accept.screen_h);
        let screens = accept.screens.clone();
        let view_only = accept.view_only;
        let audio_available = accept.audio_available;
        let codec = accept.codec;
        let fps_hint = accept.fps_hint;
        let recording = accept.recording;
        let tunnel = accept.tunnel;
        let (events, rx) = mpsc::unbounded_channel::<ClientEvent>();
        let _ = events.send(ClientEvent::Connected {
            host_name: handshake.host_hello.display_name.clone(),
            platform: handshake.host_hello.platform,
            screen,
            view_only,
            screens: screens.clone(),
            audio_available,
            codec,
            fps_hint,
            recording,
            tunnel,
        });
        let (_encoder, decoder) = make_pair(accept.codec);
        let clipboard = if settings.clipboard_sync {
            ClipboardHandle::spawn()
        } else {
            None
        };
        let (tun_local_tx, tun_local_rx) = mpsc::unbounded_channel::<TunLocal>();
        let client = Self {
            session,
            decoder,
            settings,
            events,
            ft: FtEngine::new(),
            next_chat_id: 1,
            next_ping: 1,
            ping_times: HashMap::new(),
            next_term_id: 1,
            next_tun_id: Arc::new(AtomicU32::new(1)),
            tunnels: HashMap::new(),
            tun_listeners: HashMap::new(),
            tun_local_tx,
            tun_local_rx,
            screen,
            view_only,
            tunnel_available: tunnel,
            screens,
            audio_available,
            clipboard,
            clip_sent: None,
            clip_applied: None,
            #[cfg(feature = "audio")]
            player: None,
            frames_since_stats: 0,
            stats_started: Instant::now(),
        };
        Ok((client, rx))
    }

    // ------------------------------------------------------------------
    // Pump
    // ------------------------------------------------------------------

    /// Run the session loop: consume commands + wire messages, emit events.
    /// Returns the closing event.
    pub async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) -> ClientEvent {
        let mut ping_ticker = tokio::time::interval(std::time::Duration::from_secs(5));
        ping_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut clip_ticker = tokio::time::interval(std::time::Duration::from_millis(1200));
        clip_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let closed: ClientEvent = loop {
            tokio::select! {
                _ = ping_ticker.tick() => {
                    let n = self.next_ping;
                    self.next_ping += 1;
                    self.ping_times.insert(n, Instant::now());
                    if self.session.send_msg(&Message::Ping(n)).await.is_err() {
                        break ClientEvent::Closed("connection lost".into());
                    }
                }
                _ = clip_ticker.tick() => {
                    // Local → host clipboard push with echo suppression.
                    if let Some(clip) = &mut self.clipboard {
                        clip.request_get();
                        while let Some(content) = clip.poll() {
                            let hash = hash_clip(&content);
                            if Some(hash) == self.clip_sent
                                || Some(hash) == self.clip_applied
                            {
                                continue;
                            }
                            let msg = match content {
                                ClipContent::Text(text) => {
                                    if text.len() > opacusr_input::clipboard::CLIPBOARD_MAX_BYTES {
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
                            self.clip_sent = Some(hash);
                            if self.session.send_msg(&msg).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { break ClientEvent::Closed("ui closed".into()) };
                    match self.handle_command(cmd).await {
                        Ok(true) => {}
                        Ok(false) => break ClientEvent::Closed("disconnected by user".into()),
                        Err(e) => break ClientEvent::Closed(format!("{e}")),
                    }
                }
                local = self.tun_local_rx.recv() => {
                    match local {
                        Some(TunLocal::Accepted { id, target_host, target_port, writer, listen_port }) => {
                            let entry = TunnelEntry { writer, listen_port };
                            let msg = Message::TunnelOpen(message::TunnelOpen {
                                id,
                                host: target_host,
                                port: target_port,
                            });
                            if self.session.send_msg(&msg).await.is_err() {
                                break ClientEvent::Closed("connection lost".into());
                            }
                            self.tunnels.insert(id, entry);
                        }
                        Some(TunLocal::Data { id, data }) => {
                            let msg = Message::TunnelData(message::TunnelData { id, data });
                            if self.session.send_msg(&msg).await.is_err() {
                                break ClientEvent::Closed("connection lost".into());
                            }
                        }
                        Some(TunLocal::Closed { id }) => {
                            self.tunnels.remove(&id);
                            let msg = Message::TunnelClose(message::TunnelClose { id });
                            if self.session.send_msg(&msg).await.is_err() {
                                break ClientEvent::Closed("connection lost".into());
                            }
                        }
                        None => {
                            // The internal channel cannot outlive the client;
                            // ignore (it is a self-send arc).
                        }
                    }
                }
                msg = self.session.recv_msg() => {
                    match msg {
                        Ok(msg) => {
                            if let Some(end) = self.handle_message(msg).await {
                                break end;
                            }
                        }
                        Err(e) => break ClientEvent::Closed(format!("{e}")),
                    }
                }
            }
        };
        let _ = self.events.send(closed.clone());
        let _ = self
            .session
            .send_msg(&Message::Bye(ByeReason::Normal))
            .await;
        self.session.close().await;
        closed
    }

    async fn handle_command(&mut self, cmd: Command) -> Result<bool, TransportError> {
        let msg = match cmd {
            Command::MouseMove { x, y } => Message::MouseMove(message::MouseMove { x, y }),
            Command::MouseButton { x, y, button, down } => {
                Message::MouseButton(message::MouseButton { x, y, button, down })
            }
            Command::MouseWheel { x, y, dx, dy } => {
                Message::MouseWheel(message::MouseWheel { x, y, dx, dy })
            }
            Command::KeyEvent { key, mods, down } => {
                Message::KeyEvent(message::KeyEvent { key, mods, down })
            }
            Command::TextInput { text } => Message::TextInput(message::TextInput { text }),
            Command::Chat(text) => {
                let id = self.next_chat_id;
                self.next_chat_id += 1;
                Message::ChatMessage(message::ChatMessage {
                    id,
                    text,
                    ts_ms: now_ms(),
                })
            }
            Command::SendClipboard(text) => {
                // Manual push (viewer sidebar button): record the marker so
                // the next poll does not echo it straight back.
                let hash = hash_clip(&ClipContent::Text(text.clone()));
                self.clip_sent = Some(hash);
                Message::ClipboardText(message::ClipboardText { text })
            }
            Command::SelectScreen(id) => Message::SelectScreen(message::SelectScreen { id }),
            Command::ClipboardSync(on) => {
                self.clipboard = if on { ClipboardHandle::spawn() } else { None };
                return Ok(true); // no wire message — pure client-side state
            }
            Command::Audio(on) => Message::AudioControl(message::AudioControl { enable: on }),
            Command::Quality {
                fps,
                jpeg_quality,
                scale_percent,
            } => Message::QualitySet(message::QualitySet {
                fps,
                jpeg_quality,
                scale_percent,
            }),
            Command::Pause(p) => Message::Pause(p),
            Command::Ping => {
                let n = self.next_ping;
                self.next_ping += 1;
                self.ping_times.insert(n, Instant::now());
                Message::Ping(n)
            }
            Command::PushFile(path) => match self.ft.start_push(&path) {
                Ok(Message::FtOffer(offer)) => {
                    let _ = self.events.send(ClientEvent::FtStarted {
                        id: offer.id,
                        name: offer.name.clone(),
                        total: offer.size,
                    });
                    Message::FtOffer(offer)
                }
                Ok(other) => other,
                Err(e) => {
                    let _ = self.events.send(ClientEvent::FtFailed {
                        id: u32::MAX,
                        reason: e,
                    });
                    return Ok(true);
                }
            },
            Command::CancelTransfer(id) => Message::FtCancel(message::FtCancel {
                id,
                reason: "cancelled by user".into(),
            }),
            Command::TermOpen { rows, cols } => {
                let id = self.next_term_id;
                self.next_term_id += 1;
                Message::TermOpen(message::TermOpen { id, rows, cols })
            }
            Command::TermData { id, data } => Message::TermData(message::TermData { id, data }),
            Command::TermResize { id, rows, cols } => {
                Message::TermResize(message::TermResize { id, rows, cols })
            }
            Command::TermClose { id } => Message::TermClose(message::TermClose { id }),
            Command::TunnelListen {
                listen_port,
                target_host,
                target_port,
            } => {
                if !self.tunnel_available {
                    let _ = self.events.send(ClientEvent::Notice(
                        "host does not allow TCP tunneling".into(),
                    ));
                    return Ok(true);
                }
                if target_host.trim().is_empty() || target_port == 0 {
                    let _ = self
                        .events
                        .send(ClientEvent::Notice("invalid tunnel target".into()));
                    return Ok(true);
                }
                return match tokio::net::TcpListener::bind(("127.0.0.1", listen_port)).await {
                    Ok(l) => {
                        let real = l.local_addr().map(|a| a.port()).unwrap_or(listen_port);
                        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
                        self.tun_listeners
                            .insert(real, TunListener { stop: stop_tx });
                        tokio::spawn(tunnel_accept_loop(
                            l,
                            target_host.clone(),
                            target_port,
                            real,
                            self.next_tun_id.clone(),
                            self.tun_local_tx.clone(),
                            stop_rx,
                        ));
                        let _ = self.events.send(ClientEvent::TunnelListening {
                            listen_port: real,
                            target_host,
                            target_port,
                        });
                        Ok(true)
                    }
                    Err(e) => {
                        let _ = self
                            .events
                            .send(ClientEvent::Notice(format!("tunnel bind: {e}")));
                        Ok(true)
                    }
                };
            }
            Command::TunnelStop { listen_port } => {
                if let Some(l) = self.tun_listeners.remove(&listen_port) {
                    let _ = l.stop.send(());
                }
                // Close the connections opened through this listener.
                let dead: Vec<u32> = self
                    .tunnels
                    .iter()
                    .filter(|(_, e)| e.listen_port == listen_port)
                    .map(|(id, _)| *id)
                    .collect();
                for id in dead {
                    if let Some(e) = self.tunnels.remove(&id) {
                        let _ = e.writer.send(Vec::new());
                        let _ = self
                            .session
                            .send_msg(&Message::TunnelClose(message::TunnelClose { id }))
                            .await;
                    }
                }
                return Ok(true);
            }
            Command::Disconnect => return Ok(false),
        };
        self.session.send_msg(&msg).await?;
        self.pump_outgoing().await?;
        Ok(true)
    }

    async fn pump_outgoing(&mut self) -> Result<(), TransportError> {
        for m in self.ft.pump() {
            self.session.send_msg(&m).await?;
        }
        Ok(())
    }

    /// Returns `Some(closed)` when the session must end.
    async fn handle_message(&mut self, msg: Message) -> Option<ClientEvent> {
        match msg {
            Message::VideoFrame(frame) => {
                self.screen = (frame.width, frame.height);
                self.frames_since_stats = self.frames_since_stats.saturating_add(1);
                let wire_bytes = frame.data.len() as u32;
                match self.decoder.decode(&frame.data, frame.width, frame.height) {
                    Ok(f) => {
                        let _ = self.events.send(ClientEvent::Frame {
                            frame: f,
                            wire_bytes,
                        });
                    }
                    Err(e) => {
                        let _ = self
                            .events
                            .send(ClientEvent::Notice(format!("decode: {e}")));
                    }
                }
            }
            Message::ChatMessage(c) => {
                let _ = self.events.send(ClientEvent::Chat {
                    text: c.text,
                    ts_ms: c.ts_ms,
                });
            }
            Message::ClipboardText(c) => {
                if self.settings.clipboard_sync {
                    if c.text.len() > opacusr_input::clipboard::CLIPBOARD_MAX_BYTES {
                        tracing::debug!(target: "opacusr", "remote clipboard too large, dropped");
                    } else {
                        let content = ClipContent::Text(c.text.clone());
                        set_local_clipboard(&c.text);
                        let hash = hash_clip(&content);
                        self.clip_applied = Some(hash);
                        self.clip_sent = Some(hash);
                    }
                }
            }
            Message::ClipboardImage(c) => {
                if self.settings.clipboard_sync {
                    if c.png.len() > CLIPBOARD_IMAGE_MAX_PNG_BYTES {
                        tracing::debug!(target: "opacusr", "remote clipboard image too large, dropped");
                    } else {
                        match opacusr_input::clipboard::decode_png(&c.png) {
                            Some((width, height, rgba)) => {
                                let content = ClipContent::Image {
                                    width,
                                    height,
                                    rgba: rgba.clone(),
                                };
                                set_local_clipboard_image(width, height, &rgba);
                                let hash = hash_clip(&content);
                                self.clip_applied = Some(hash);
                                self.clip_sent = Some(hash);
                            }
                            None => {
                                tracing::debug!(target: "opacusr",
                                    "malformed remote clipboard image, dropped");
                            }
                        }
                    }
                }
            }
            Message::Pong(n) => {
                if let Some(start) = self.ping_times.remove(&n) {
                    let rtt = start.elapsed().as_millis() as u32;
                    let _ = self.events.send(ClientEvent::Rtt(rtt));
                    // Feed the host's adaptive-quality controller.
                    let elapsed = self.stats_started.elapsed().as_secs_f32();
                    let recv_fps = if elapsed >= 1.0 {
                        (self.frames_since_stats as f32 / elapsed).round() as u16
                    } else {
                        0
                    };
                    self.frames_since_stats = 0;
                    self.stats_started = Instant::now();
                    let _ = self
                        .session
                        .send_msg(&Message::LinkStats(message::LinkStats {
                            rtt_ms: rtt,
                            recv_fps,
                        }))
                        .await;
                }
            }
            Message::AudioFrame(frame) => {
                #[cfg(feature = "audio")]
                {
                    if self.player.is_none() {
                        match crate::audio::AudioPlayer::start() {
                            Ok(p) => self.player = Some(p),
                            Err(e) => {
                                let _ =
                                    self.events.send(ClientEvent::Notice(format!("audio: {e}")));
                            }
                        }
                    }
                    if let Some(p) = &self.player {
                        p.push(frame.data);
                    }
                }
                #[cfg(not(feature = "audio"))]
                {
                    let _ = frame;
                }
            }
            Message::Bye(reason) => {
                return Some(ClientEvent::Closed(format!("host closed: {reason:?}")));
            }
            Message::CursorShape(c) => {
                let _ = self.events.send(ClientEvent::CursorShape {
                    id: c.id,
                    width: c.width,
                    height: c.height,
                    hotspot_x: c.hotspot_x,
                    hotspot_y: c.hotspot_y,
                    rgba: c.rgba,
                });
            }
            Message::CursorPos(p) => {
                let _ = self.events.send(ClientEvent::CursorPos {
                    x: p.x,
                    y: p.y,
                    visible: p.visible,
                    shape_id: p.shape_id,
                });
            }
            other => {
                // File-transfer machinery (replies go straight to the wire).
                let (replies, events) = self.ft.on_message(
                    &other,
                    &self.settings.download_dir,
                    self.settings.auto_accept_files,
                );
                for r in replies {
                    let _ = self.session.send_msg(&r).await;
                }
                for ev in events {
                    let _ = self.events.send(map_ft_event(ev));
                }
                match other {
                    Message::TermOpened(t) => {
                        let _ = self.events.send(ClientEvent::TermOpened {
                            id: t.id,
                            ok: t.ok,
                            error: t.error,
                            shell: t.shell,
                        });
                    }
                    Message::TermData(t) => {
                        let _ = self.events.send(ClientEvent::TermData {
                            id: t.id,
                            data: t.data,
                        });
                    }
                    Message::TunnelOpened(t) => {
                        if !t.ok {
                            // Drop the local half: the app sees an immediate
                            // connection reset.
                            if let Some(e) = self.tunnels.remove(&t.id) {
                                let _ = e.writer.send(Vec::new());
                            }
                        }
                        let _ = self.events.send(ClientEvent::TunnelOpened {
                            id: t.id,
                            ok: t.ok,
                            error: t.error,
                        });
                    }
                    Message::TunnelData(t) => {
                        if let Some(e) = self.tunnels.get(&t.id) {
                            let _ = e.writer.send(t.data);
                        }
                    }
                    Message::TunnelClose(t) => {
                        if let Some(e) = self.tunnels.remove(&t.id) {
                            let _ = e.writer.send(Vec::new());
                            let _ = self.events.send(ClientEvent::TunnelClosed { id: t.id });
                        }
                    }
                    _ => {}
                }
                // Incoming acks advance our outgoing transfers.
                let _ = self.pump_outgoing().await;
            }
        }
        None
    }

    pub fn screen_size(&self) -> (u32, u32) {
        self.screen
    }

    /// Whether the host granted view-only access.
    pub fn view_only(&self) -> bool {
        self.view_only
    }

    /// Host monitors (ids for [`Command::SelectScreen`]).
    pub fn screens(&self) -> &[message::ScreenInfo] {
        &self.screens
    }

    /// Whether the host offered audio streaming.
    pub fn audio_available(&self) -> bool {
        self.audio_available
    }

    /// Whether the host forwards TCP tunnels for this session.
    pub fn tunnel_available(&self) -> bool {
        self.tunnel_available
    }
}

fn map_ft_event(ev: FtEvent) -> ClientEvent {
    match ev {
        FtEvent::ReceivedDone(_, path, verified) => ClientEvent::FtReceivedDone { path, verified },
        FtEvent::SentDone(id, ok) => ClientEvent::FtSentDone { id, ok },
        FtEvent::Progress(id, done, total) => ClientEvent::FtProgress { id, done, total },
        FtEvent::Failed(id, reason) => ClientEvent::FtFailed { id, reason },
    }
}

fn set_local_clipboard(text: &str) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text.to_string());
        }
    }
    #[allow(unused_variables)]
    let _ = text;
}

/// Place a decoded RGBA image on the viewer clipboard (best effort).
fn set_local_clipboard_image(width: u32, height: u32, rgba: &[u8]) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: std::borrow::Cow::Borrowed(rgba),
            });
        }
    }
    #[allow(unused_variables)]
    let _ = (width, height, rgba);
}

/// `ws://host[:port]/path` → `host`.
fn relay_host(server_url: &str) -> Result<String, TransportError> {
    let rest = server_url
        .strip_prefix("ws://")
        .or_else(|| server_url.strip_prefix("wss://"))
        .ok_or_else(|| TransportError::Signaling(format!("bad url {server_url}")))?;
    let host_port = rest.split('/').next().unwrap_or(rest);
    Ok(host_port.split(':').next().unwrap_or(host_port).to_string())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
