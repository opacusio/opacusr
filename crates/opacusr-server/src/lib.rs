//! OpacusR signaling server.
//!
//! Responsibilities (deliberately minimal — the server is untrusted and
//! never sees plaintext session data):
//! * keep a registry of online peers behind short numeric ids;
//! * route opaque `PeerPayload`s (Accept/Reject/SDP) between peers;
//! * allocate tickets and pair connections for the TCP relay fallback.
//!
//! Two listeners: WebSocket `/signal` (axum) and a raw TCP relay port.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message as WsMsg, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use rand::Rng;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use opacusr_proto::signal::*;
use opacusr_proto::{PeerId, Platform};

pub const HEARTBEAT_SECS: u16 = 15;
/// Peers silent for 3× the heartbeat interval are pruned.
pub const PEER_TTL: Duration = Duration::from_secs(3 * HEARTBEAT_SECS as u64);
/// Unpaired relay tickets expire after one minute.
pub const RELAY_SLOT_TTL: Duration = Duration::from_secs(60);
/// Preamble size: 16-byte ticket + 1 role byte.
pub const RELAY_PREAMBLE: usize = 17;

/// Everything the registry knows about an online peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub platform: Platform,
    pub display_name: String,
    pub is_agent: bool,
    pub unattended: bool,
}

struct PeerHandle {
    #[allow(dead_code)] // surfaced in an admin listing endpoint later
    info: PeerInfo,
    tx: mpsc::UnboundedSender<SignalMessage>,
    last_seen: Arc<std::sync::atomic::AtomicU64>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Peer id → outbound queue.
#[derive(Default)]
pub struct Registry {
    peers: std::sync::Mutex<HashMap<PeerId, PeerHandle>>,
}

impl Registry {
    /// Register a peer and return its fresh public id + inbound queue.
    pub fn register(&self, info: PeerInfo) -> (PeerId, mpsc::UnboundedReceiver<SignalMessage>) {
        let mut peers = self.peers.lock().unwrap();
        let mut rng = rand::thread_rng();
        let id = loop {
            let candidate: PeerId =
                rng.gen_range(opacusr_proto::MIN_PEER_ID..=opacusr_proto::MAX_PEER_ID);
            if !peers.contains_key(&candidate) {
                break candidate;
            }
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let last_seen = Arc::new(std::sync::atomic::AtomicU64::new(now()));
        peers.insert(
            id,
            PeerHandle {
                info,
                tx,
                last_seen,
            },
        );
        (id, rx)
    }

    pub fn unregister(&self, id: PeerId) -> bool {
        self.peers.lock().unwrap().remove(&id).is_some()
    }

    pub fn touch(&self, id: PeerId) {
        if let Some(p) = self.peers.lock().unwrap().get(&id) {
            p.last_seen
                .store(now(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Queue `msg` for `to`. Errors when the peer is gone.
    pub fn send_to(&self, to: PeerId, msg: SignalMessage) -> Result<(), String> {
        let peers = self.peers.lock().unwrap();
        match peers.get(&to) {
            Some(p) => p.tx.send(msg).map_err(|_| "peer queue closed".to_string()),
            None => Err(format!("peer {to} is offline")),
        }
    }

    /// Remove peers that have not heartbeat for [`PEER_TTL`].
    pub fn prune(&self) -> usize {
        let cutoff = now() - PEER_TTL.as_secs();
        let mut peers = self.peers.lock().unwrap();
        let stale: Vec<PeerId> = peers
            .iter()
            .filter(|(_, p)| p.last_seen.load(std::sync::atomic::Ordering::Relaxed) < cutoff)
            .map(|(id, _)| *id)
            .collect();
        for id in &stale {
            peers.remove(id);
        }
        stale.len()
    }

    pub fn len(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

struct RelaySlot {
    created: Instant,
    viewer: Option<TcpStream>,
    host: Option<TcpStream>,
}

/// Ticket bookkeeping for the TCP relay fallback.
#[derive(Default)]
pub struct RelayHub {
    slots: std::sync::Mutex<HashMap<[u8; 16], RelaySlot>>,
}

impl RelayHub {
    /// Allocate a fresh ticket.
    pub fn alloc(&self) -> [u8; 16] {
        let mut slots = self.slots.lock().unwrap();
        loop {
            let ticket: [u8; 16] = rand::random();
            if let std::collections::hash_map::Entry::Vacant(e) = slots.entry(ticket) {
                e.insert(RelaySlot {
                    created: Instant::now(),
                    viewer: None,
                    host: None,
                });
                return ticket;
            }
        }
    }

    /// Accept a relay connection (17-byte preamble already validated).
    /// Pairs and pipes when both roles arrived.
    pub async fn accept(&self, stream: TcpStream) -> anyhow::Result<()> {
        let mut stream = stream;
        let mut preamble = [0u8; RELAY_PREAMBLE];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut preamble).await?;
        let ticket: [u8; 16] = preamble[..16].try_into().unwrap();
        let role = preamble[16];

        let piped = {
            let mut slots = self.slots.lock().unwrap();
            match slots.get_mut(&ticket) {
                None => {
                    anyhow::bail!("unknown relay ticket");
                }
                Some(slot) => {
                    let target = match role {
                        0 => &mut slot.viewer,
                        _ => &mut slot.host,
                    };
                    if target.is_some() {
                        anyhow::bail!("duplicate role for ticket");
                    }
                    *target = Some(stream);
                    slot.viewer.is_some() && slot.host.is_some()
                }
            }
        };

        if piped {
            let mut slots = self.slots.lock().unwrap();
            if let Some(slot) = slots.remove(&ticket) {
                let (a, b) = (
                    slot.viewer.expect("both present"),
                    slot.host.expect("both present"),
                );
                drop(slots);
                // Raw bidirectional pipe with two *independent* directions:
                // one peer closing must not discard in-flight data for the
                // other (copy_bidirectional shuts both sides down early).
                // Payloads are app-encrypted E2E — the server stays
                // ignorant of the content.
                let (mut a_rd, mut a_wr) = tokio::io::split(a);
                let (mut b_rd, mut b_wr) = tokio::io::split(b);
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut a_rd, &mut b_wr).await;
                    // Viewer gone: politely half-close toward the host.
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut b_wr).await;
                });
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut b_rd, &mut a_wr).await;
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut a_wr).await;
                });
            }
        }
        Ok(())
    }

    /// Drop unpaired slots older than [`RELAY_SLOT_TTL`].
    pub fn prune(&self) {
        let mut slots = self.slots.lock().unwrap();
        slots.retain(|_, s| s.created.elapsed() < RELAY_SLOT_TTL);
    }
}

/// Shared server state.
pub struct AppState {
    pub registry: Registry,
    pub relay: RelayHub,
    pub relay_port: u16,
    pub version: String,
}

impl AppState {
    pub fn new(relay_port: u16) -> Self {
        Self {
            registry: Registry::default(),
            relay: RelayHub::default(),
            relay_port,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Periodic cleanup task (peers + relay slots).
    pub fn spawn_janitor(state: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(HEARTBEAT_SECS as u64)).await;
                let stale = state.registry.prune();
                state.relay.prune();
                if stale > 0 {
                    tracing::info!(target: "opacusr", "pruned {stale} stale peers");
                }
            }
        });
    }
}

/// HTTP router: `/signal` WebSocket + `/health`.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/signal", get(signal_route))
        .route("/health", get(health))
        .fallback(unreachable)
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> String {
    format!(
        "opacusr-server {} — peers: {}",
        state.version,
        state.registry.len()
    )
}

async fn unreachable() -> &'static str {
    "not found"
}

async fn signal_route(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Serve the signaling + relay stack on the two listeners until shutdown.
pub async fn serve(
    http: tokio::net::TcpListener,
    relay: tokio::net::TcpListener,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            match relay.accept().await {
                Ok((sock, _)) => {
                    let st = st.clone();
                    tokio::spawn(async move {
                        if let Err(e) = st.relay.accept(sock).await {
                            tracing::debug!(target: "opacusr", "relay conn rejected: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(target: "opacusr", "relay accept: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    AppState::spawn_janitor(state.clone());
    axum::serve(http, router(state)).await
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let mut socket = socket;
    // First message must be a Register.
    let reg = match recv_signal(&mut socket).await {
        Ok(SignalMessage::Register(reg)) => reg,
        _ => {
            let _ = send_signal(
                &mut socket,
                SignalMessage::Error {
                    message: "expected Register".into(),
                },
            )
            .await;
            return;
        }
    };
    let my_info = PeerInfo {
        platform: reg.platform,
        display_name: reg.display_name.clone(),
        is_agent: reg.is_agent,
        unattended: reg.unattended,
    };
    let (my_id, mut inbound) = {
        let (id, inbound) = state.registry.register(my_info.clone());
        tracing::info!(target: "opacusr", "peer {id} registered ({}{}, agent={})",
            reg.platform.as_str(),
            if reg.unattended { ", unattended" } else { "" },
            reg.is_agent);
        let _ = send_signal(
            &mut socket,
            SignalMessage::Welcome(Welcome {
                id,
                server_version: state.version.clone(),
                heartbeat_secs: HEARTBEAT_SECS,
            }),
        )
        .await;
        (id, inbound)
    };

    // Split the socket: the write half is pumped by a dedicated task.
    let (mut tx_sink, mut rx_stream) = socket.split();
    let out_task = tokio::spawn(async move {
        while let Some(msg) = inbound.recv().await {
            let bytes = match opacusr_proto::framing::encode_signal(&msg) {
                Ok(b) => b,
                Err(_) => break,
            };
            if tx_sink.send(WsMsg::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    // Client → server dispatch loop (read half).
    while let Some(frame) = rx_stream.next().await {
        match frame {
            Ok(WsMsg::Binary(bytes)) => {
                state.registry.touch(my_id);
                match opacusr_proto::framing::decode_signal(&bytes) {
                    Ok(msg) => dispatch(&state, my_id, &my_info, msg).await,
                    Err(e) => {
                        let _ = state.registry.send_to(
                            my_id,
                            SignalMessage::Error {
                                message: e.to_string(),
                            },
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    state.registry.unregister(my_id);
    out_task.abort();
    tracing::info!(target: "opacusr", "peer {my_id} disconnected");
    // Peers discover the disappearance via SessionFailed / PeerSend errors.
}

/// Handle one client → server signaling message.
async fn dispatch(state: &Arc<AppState>, from: PeerId, from_info: &PeerInfo, msg: SignalMessage) {
    match msg {
        SignalMessage::Heartbeat => state.registry.touch(from),
        SignalMessage::RequestSession(RequestSession { peer, display_name }) => {
            // The viewer asks `peer` (a host) for a session.
            let incoming = SignalMessage::SessionIncoming(SessionIncoming {
                from,
                display_name,
                platform: from_info.platform,
            });
            if let Err(e) = state.registry.send_to(peer, incoming) {
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::SessionFailed(SessionFailed { peer, reason: e }),
                );
            }
        }
        SignalMessage::PeerSend(PeerSend { to, payload }) => {
            let fwd = SignalMessage::PeerRecv(PeerRecv { from, payload });
            if let Err(e) = state.registry.send_to(to, fwd) {
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::Error {
                        message: format!("peer send failed: {e}"),
                    },
                );
            }
        }
        SignalMessage::RelayAlloc(_) => {
            let ticket = state.relay.alloc();
            let _ = state.registry.send_to(
                from,
                SignalMessage::RelayTicket(RelayTicket {
                    ticket,
                    port: state.relay_port,
                }),
            );
        }
        SignalMessage::Register(_) | SignalMessage::Welcome(_) => {
            tracing::debug!(target: "opacusr", "peer {from} sent an unexpected message");
        }
        SignalMessage::Error { message } => {
            tracing::debug!(target: "opacusr", "peer {from} error: {message}");
        }
        // Server → client directions are never valid inbound.
        SignalMessage::SessionIncoming(_)
        | SignalMessage::SessionFailed(_)
        | SignalMessage::PeerRecv(_)
        | SignalMessage::PeerGone(_)
        | SignalMessage::RelayTicket(_) => {
            let _ = state.registry.send_to(
                from,
                SignalMessage::Error {
                    message: "that message is server→client only".into(),
                },
            );
        }
    }
}

async fn recv_signal(socket: &mut WebSocket) -> Result<SignalMessage, String> {
    while let Some(Ok(frame)) = socket.recv().await {
        match frame {
            WsMsg::Binary(bytes) => {
                return opacusr_proto::framing::decode_signal(&bytes).map_err(|e| e.to_string());
            }
            WsMsg::Close(_) => return Err("closed".into()),
            _ => {}
        }
    }
    Err("closed".into())
}

async fn send_signal(socket: &mut WebSocket, msg: SignalMessage) -> Result<(), String> {
    let bytes = opacusr_proto::framing::encode_signal(&msg).map_err(|e| e.to_string())?;
    socket
        .send(WsMsg::Binary(bytes.into()))
        .await
        .map_err(|e| e.to_string())
}
