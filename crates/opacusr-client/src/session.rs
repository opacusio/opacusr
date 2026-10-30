//! The viewer session: establishes the connection (signaling → WebRTC with
//! TCP-relay fallback, or direct LAN TCP), performs the password handshake
//! and runs the message pump. Control happens through [`Command`]s, output
//! through [`ClientEvent`]s — a shape designed for GUI event loops.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::mpsc;

use opacusr_capture::Frame;
use opacusr_codec::{make_pair, VideoDecoder};
use opacusr_input::clipboard::ClipboardHandle;
use opacusr_proto::message::{self, ByeReason};
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
    },
    Frame(Frame),
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
            webrtc: WebRtcConfig {
                ice_servers: vec![opacusr_transport::webrtc::IceServerConfig::stun(&[
                    "stun:stun.l.google.com:19302",
                ])],
            },
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
    screen: (u32, u32),
    /// Host-side permission (from `SessionAccept`).
    view_only: bool,
    /// Host monitors (from `SessionAccept`).
    screens: Vec<message::ScreenInfo>,
    audio_available: bool,
    /// Clipboard sync worker + echo-suppression state.
    clipboard: Option<ClipboardHandle>,
    clip_sent: Option<String>,
    clip_applied: Option<String>,
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
        let handshake =
            viewer_handshake(&mut session, &settings.password, &settings.display_name).await?;
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
        let (events, rx) = mpsc::unbounded_channel::<ClientEvent>();
        let _ = events.send(ClientEvent::Connected {
            host_name: handshake.host_hello.display_name.clone(),
            platform: handshake.host_hello.platform,
            screen,
            view_only,
            screens: screens.clone(),
            audio_available,
        });
        let (_encoder, decoder) = make_pair(accept.codec);
        let clipboard = if settings.clipboard_sync {
            ClipboardHandle::spawn()
        } else {
            None
        };
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
            screen,
            view_only,
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
                        while let Some(text) = clip.poll() {
                            if text.len() > opacusr_input::clipboard::CLIPBOARD_MAX_BYTES {
                                continue;
                            }
                            if Some(&text) == self.clip_sent.as_ref()
                                || Some(&text) == self.clip_applied.as_ref()
                            {
                                continue;
                            }
                            self.clip_sent = Some(text.clone());
                            if self.session.send_msg(&Message::ClipboardText(
                                message::ClipboardText { text },
                            )).await.is_err() {
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
            Command::SendClipboard(text) => Message::ClipboardText(message::ClipboardText { text }),
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
                match self.decoder.decode(&frame.data, frame.width, frame.height) {
                    Ok(f) => {
                        let _ = self.events.send(ClientEvent::Frame(f));
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
                        set_local_clipboard(&c.text);
                        self.clip_applied = Some(c.text.clone());
                        self.clip_sent = Some(c.text);
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
