//! OpacusR signaling server.
//!
//! Responsibilities (deliberately minimal — the server is untrusted and
//! never sees plaintext session data):
//! * keep a registry of online peers behind short numeric ids;
//! * route opaque `PeerPayload`s (Accept/Reject/SDP) between peers;
//! * allocate tickets and pair connections for the TCP relay fallback.
//!
//! Two listeners: WebSocket `/signal` (axum) and a raw TCP relay port.
//!
//! Hardening: per-IP rate limits on new signaling connections and `/turn`
//! requests, a cap on concurrently registered peers, caps on outstanding
//! relay tickets, and time-limited TURN credential minting (coturn REST
//! API).

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message as WsMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;
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
/// Hard cap on outstanding (unpaired) relay tickets per peer.
pub const MAX_RELAY_TICKETS_PER_PEER: usize = 8;
/// Hard cap on outstanding relay tickets server-wide.
pub const MAX_RELAY_TICKETS: usize = 4096;

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
    owner: PeerId,
    viewer: Option<TcpStream>,
    host: Option<TcpStream>,
}

/// Ticket bookkeeping for the TCP relay fallback.
#[derive(Default)]
pub struct RelayHub {
    slots: std::sync::Mutex<HashMap<[u8; 16], RelaySlot>>,
}

impl RelayHub {
    /// Allocate a fresh ticket for `owner`.
    ///
    /// Returns `None` when the per-peer or server-wide cap on
    /// outstanding tickets is reached — callers relay that as an error
    /// to the peer instead of a ticket.
    pub fn alloc(&self, owner: PeerId) -> Option<[u8; 16]> {
        let mut slots = self.slots.lock().unwrap();
        if slots.len() >= MAX_RELAY_TICKETS {
            return None;
        }
        if slots.values().filter(|s| s.owner == owner).count() >= MAX_RELAY_TICKETS_PER_PEER {
            return None;
        }
        loop {
            let ticket: [u8; 16] = rand::random();
            if let std::collections::hash_map::Entry::Vacant(e) = slots.entry(ticket) {
                e.insert(RelaySlot {
                    created: Instant::now(),
                    owner,
                    viewer: None,
                    host: None,
                });
                return Some(ticket);
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

/// Sliding-window per-IP limiter for new signaling connections and
/// `/turn` requests. Hand-rolled on purpose — no extra dependency, and
/// a std mutex is plenty at the rates a signaling server sees.
pub struct ConnLimiter {
    max: usize,
    window: Duration,
    events: std::sync::Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl ConnLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            events: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Record one event for `ip`; `true` when it stays within budget.
    pub fn allow(&self, ip: IpAddr) -> bool {
        let mut events = self.events.lock().unwrap();
        let queue = events.entry(ip).or_default();
        let now = Instant::now();
        while let Some(oldest) = queue.front() {
            if now.duration_since(*oldest) >= self.window {
                queue.pop_front();
            } else {
                break;
            }
        }
        if queue.len() >= self.max {
            return false;
        }
        queue.push_back(now);
        true
    }

    /// Drop IPs with no events inside the window (janitor task).
    pub fn prune(&self) {
        let mut events = self.events.lock().unwrap();
        let now = Instant::now();
        events.retain(|_, queue| {
            queue.retain(|t| now.duration_since(*t) < self.window);
            !queue.is_empty()
        });
    }
}

/// Runtime limits. The binary fills these from the environment
/// ([`Limits::from_env`]); [`AppState::new`] uses the permissive preset
/// so embedded and test deployments keep the historical behavior.
pub struct Limits {
    /// Max new signaling connections / `/turn` requests per IP per window.
    pub rate_max: usize,
    /// Sliding-window length for [`Limits::rate_max`].
    pub rate_window: Duration,
    /// Max concurrently registered peers.
    pub max_peers: usize,
}

impl Default for Limits {
    /// Hardened defaults: 20 events per IP per 10 s, 1024 peers.
    fn default() -> Self {
        Self {
            rate_max: 20,
            rate_window: Duration::from_secs(10),
            max_peers: 1024,
        }
    }
}

impl Limits {
    /// No rate limiting and no peer cap (tests, embedders).
    pub fn permissive() -> Self {
        Self {
            rate_max: usize::MAX,
            rate_window: Duration::from_secs(10),
            max_peers: usize::MAX,
        }
    }

    /// `OPACUSR_RATE_MAX` (default 20), `OPACUSR_RATE_WINDOW_SECS`
    /// (default 10), `OPACUSR_MAX_PEERS` (default 1024); values clamp to
    /// a minimum of 1.
    pub fn from_env() -> Self {
        Self {
            rate_max: env_parse("OPACUSR_RATE_MAX", Self::default().rate_max).max(1),
            rate_window: Duration::from_secs((
                env_parse("OPACUSR_RATE_WINDOW_SECS", Self::default().rate_window.as_secs())
            )
            .max(1)),
            max_peers: env_parse("OPACUSR_MAX_PEERS", Self::default().max_peers).max(1),
        }
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// coturn "TURN REST API" style time-limited credential minting.
///
/// With coturn running `use-auth-secret` and the *same* secret as its
/// `static-auth-secret`, the minted pair is accepted as a TURN
/// username/password until the embedded expiry passes:
/// `username = <unix expiry seconds>`,
/// `password = base64(HMAC-SHA1(secret, username))`.
pub struct TurnConfig {
    pub secret: Vec<u8>,
    pub ttl_secs: u64,
    /// Optional comma-separated URI list override (else derived from the
    /// request's `Host` header).
    pub uri_override: Option<String>,
}

impl TurnConfig {
    /// Read from `OPACUSR_TURN_SECRET` (required to enable),
    /// `OPACUSR_TURN_TTL_SECS` (default 3600) and `OPACUSR_TURN_URI`.
    pub fn from_env() -> Option<Self> {
        let secret = std::env::var("OPACUSR_TURN_SECRET")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        let ttl_secs = env_parse("OPACUSR_TURN_TTL_SECS", 3600_u64).max(1);
        let uri_override = std::env::var("OPACUSR_TURN_URI")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Some(Self {
            secret: secret.into_bytes(),
            ttl_secs,
            uri_override,
        })
    }
}

/// Shared server state.
pub struct AppState {
    pub registry: Registry,
    pub relay: RelayHub,
    pub relay_port: u16,
    pub version: String,
    pub limits: Limits,
    pub limiter: ConnLimiter,
    pub turn: Option<TurnConfig>,
}

impl AppState {
    /// Historical constructor: no rate limiting, no caps, no TURN minting
    /// (existing tests and embedders rely on this).
    pub fn new(relay_port: u16) -> Self {
        Self::with_options(relay_port, Limits::permissive(), None)
    }

    /// Fully configured constructor (used by the binary).
    pub fn with_options(
        relay_port: u16,
        limits: Limits,
        turn: Option<TurnConfig>,
    ) -> Self {
        let limiter = ConnLimiter::new(limits.rate_max, limits.rate_window);
        Self {
            registry: Registry::default(),
            relay: RelayHub::default(),
            relay_port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            limits,
            limiter,
            turn,
        }
    }

    /// Periodic cleanup task (peers + relay slots + limiter buckets).
    pub fn spawn_janitor(state: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(HEARTBEAT_SECS as u64)).await;
                let stale = state.registry.prune();
                state.relay.prune();
                state.limiter.prune();
                if stale > 0 {
                    tracing::info!(target: "opacusr", "pruned {stale} stale peers");
                }
            }
        });
    }
}

/// HTTP router: `/signal` WebSocket, `/turn` credentials, `/health`.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/signal", get(signal_route))
        .route("/turn", get(turn_route))
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

/// Client IP, when the listener supplies connection info.
///
/// axum 0.8 has no `Option<ConnectInfo<...>>` extractor, so this is a tiny
/// infallible stand-in: `Some(ip)` under [`serve`] (which always attaches
/// `ConnectInfo<SocketAddr>`), `None` when the router is driven without
/// connection info (e.g. test setups) — such callers simply skip the
/// per-IP limits.
struct ClientAddr(Option<IpAddr>);

impl<S> FromRequestParts<S> for ClientAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientAddr(
            parts.extensions.get::<SocketAddr>().map(|a| a.ip()),
        ))
    }
}

async fn signal_route(
    State(state): State<Arc<AppState>>,
    ClientAddr(ip): ClientAddr,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(ip) = ip {
        if !state.limiter.allow(ip) {
            tracing::warn!(target: "opacusr", "rate limit: signaling connect from {ip}");
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "too many connections from this address, retry later",
            )
                .into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Time-limited TURN credentials (coturn "TURN REST API" response shape).
#[derive(serde::Serialize)]
struct TurnCredentials {
    username: String,
    password: String,
    ttl: u64,
    uris: Vec<String>,
}

/// `password = base64(HMAC-SHA1(secret, username))` — exactly the pair
/// that coturn's `use-auth-secret` mode verifies.
fn mint_turn_password(secret: &[u8], username: &str) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("hmac accepts any key size");
    mac.update(username.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Host part of the request's `Host` header (bracketed IPv6 aware), with
/// a fallback to the peer address when the header is missing.
fn host_of(headers: &HeaderMap, fallback: IpAddr) -> String {
    if let Some(host) = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        if let Some(rest) = host.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                return rest[..end].to_string();
            }
        }
        if let Some((host, _port)) = host.rsplit_once(':') {
            return host.to_string();
        }
        return host.to_string();
    }
    fallback.to_string()
}

/// Mint time-limited TURN credentials for coturn deployments.
///
/// Enabled by `OPACUSR_TURN_SECRET` (must equal coturn's
/// `static-auth-secret`); 503 otherwise. Rate limited per IP like
/// `/signal`. The [`ClientAddr`] extractor keeps the router usable in
/// test setups that serve it without connection info.
async fn turn_route(
    State(state): State<Arc<AppState>>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
) -> Response {
    if let Some(ip) = ip {
        if !state.limiter.allow(ip) {
            tracing::warn!(target: "opacusr", "rate limit: /turn from {ip}");
            return (StatusCode::TOO_MANY_REQUESTS, "too many requests").into_response();
        }
    }
    let Some(turn) = state.turn.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "TURN credential minting disabled: set OPACUSR_TURN_SECRET on the server",
        )
            .into_response();
    };
    let ttl = turn.ttl_secs;
    let expiry = now() + ttl;
    let username = expiry.to_string();
    let password = mint_turn_password(&turn.secret, &username);
    let uris = match &turn.uri_override {
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        None => {
            let host = host_of(
                &headers,
                ip.unwrap_or_else(|| std::net::Ipv4Addr::LOCALHOST.into()),
            );
            vec![
                format!("turn:{host}:3478?transport=udp"),
                format!("turn:{host}:3478?transport=tcp"),
            ]
        }
    };
    Json(TurnCredentials {
        username,
        password,
        ttl,
        uris,
    })
    .into_response()
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
    axum::serve(
        http,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
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
    if state.registry.len() >= state.limits.max_peers {
        tracing::warn!(target: "opacusr", "refusing registration: server is full");
        let _ = send_signal(
            &mut socket,
            SignalMessage::Error {
                message: "server is full, retry later".into(),
            },
        )
        .await;
        return;
    }
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
        SignalMessage::RelayAlloc(_) => match state.relay.alloc(from) {
            Some(ticket) => {
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::RelayTicket(RelayTicket {
                        ticket,
                        port: state.relay_port,
                    }),
                );
            }
            None => {
                tracing::warn!(target: "opacusr", "relay ticket budget hit (peer {from})");
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::Error {
                        message: "relay ticket budget exceeded, retry later".into(),
                    },
                );
            }
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_within_budget_and_blocks_beyond() {
        let limiter = ConnLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(!limiter.allow(ip));
        let other: IpAddr = "192.0.2.11".parse().unwrap();
        assert!(limiter.allow(other));
    }

    #[test]
    fn limiter_window_slides() {
        let limiter = ConnLimiter::new(1, Duration::from_millis(50));
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        assert!(limiter.allow(ip));
        assert!(!limiter.allow(ip));
        std::thread::sleep(Duration::from_millis(80));
        assert!(limiter.allow(ip));
    }

    #[test]
    fn limiter_prune_drops_idle_ips() {
        let limiter = ConnLimiter::new(5, Duration::from_millis(50));
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        assert!(limiter.allow(ip));
        std::thread::sleep(Duration::from_millis(80));
        limiter.prune();
        assert!(limiter.allow(ip));
    }

    #[test]
    fn turn_password_matches_reference_vector() {
        // Independent implementation (Python):
        // base64(hmac_sha1(key=b"opacusr-test-secret", msg=b"3600"))
        assert_eq!(
            mint_turn_password(b"opacusr-test-secret", "3600"),
            "tYuxT83z6ZW/pbiE1aO3RipEDAA="
        );
        assert_eq!(
            mint_turn_password(b"opacusr-test-secret", "4102444800"),
            "2DUr4yV5HBNTAygX4jzyayw26kk="
        );
    }

    #[test]
    fn host_of_strips_port_and_handles_ipv6() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            axum::http::HeaderValue::from_str("turn.example.com:8080").unwrap(),
        );
        assert_eq!(
            host_of(&headers, "192.0.2.9".parse().unwrap()),
            "turn.example.com"
        );
        let mut v6 = HeaderMap::new();
        v6.insert(
            header::HOST,
            axum::http::HeaderValue::from_str("[2001:db8::1]:8080").unwrap(),
        );
        assert_eq!(host_of(&v6, "192.0.2.9".parse().unwrap()), "2001:db8::1");
        assert_eq!(
            host_of(&HeaderMap::new(), "192.0.2.9".parse().unwrap()),
            "192.0.2.9"
        );
    }
}
