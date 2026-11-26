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

pub mod metrics;

pub use metrics::{CountingReader, Metrics};

pub const HEARTBEAT_SECS: u16 = 15;
/// Peers silent for 3× the heartbeat interval are pruned.
pub const PEER_TTL: Duration = Duration::from_secs(3 * HEARTBEAT_SECS as u64);
/// Unpaired relay tickets expire after one minute.
pub const RELAY_SLOT_TTL: Duration = Duration::from_secs(60);
/// Preamble size: 16-byte ticket + 1 role byte.
pub const RELAY_PREAMBLE: usize = 17;

/// How long a relay connection may take to deliver its 17-byte preamble.
const RELAY_PREAMBLE_TIMEOUT: Duration = Duration::from_secs(10);
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
    tx: mpsc::Sender<SignalMessage>,
    last_seen: Arc<std::sync::atomic::AtomicU64>,
    /// Connection generation: a STALE connection's unregister must not
    /// delete a takeover's fresh registration of the same id.
    gen: u64,
    /// Fingerprint of the machine key this connection registered with
    /// (`None` for old clients). Takeover of a busy id is allowed only
    /// for the same fingerprint.
    fp: Option<String>,
}

/// Per-peer outbound queue depth. A peer that does not drain its queue
/// (dead websocket, stalled reader) fills this in seconds; after that its
/// senders get an error instead of the server growing unbounded memory
/// (a registered peer used to be able to soak gigabytes — remote OOM).
const PEER_QUEUE_DEPTH: usize = 512;

/// Sanity caps on peer-supplied strings/payloads (server-side defense;
/// the frame limit is the only other barrier on the wire).
const MAX_DISPLAY_NAME: usize = 256;
/// SDP offers are a few KiB at most; anything larger is abuse.
const MAX_PEER_PAYLOAD: usize = 512 * 1024;

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
    /// Machine-key fingerprint → the id this machine always gets
    /// (stable ids; the «peer … is offline» fix: the id used to be
    /// minted anew on EVERY registration, so saved contacts broke
    /// after each agent restart/reconnect).
    fps: std::sync::Mutex<HashMap<String, PeerId>>,
    /// Connection generation counter for the unregister guard.
    gen_counter: std::sync::atomic::AtomicU64,
}

/// Mint a random free id (the pre-0.14.0 behavior).
fn mint_random(peers: &HashMap<PeerId, PeerHandle>) -> PeerId {
    let mut rng = rand::thread_rng();
    loop {
        let candidate: PeerId =
            rng.gen_range(opacusr_proto::MIN_PEER_ID..=opacusr_proto::MAX_PEER_ID);
        if !peers.contains_key(&candidate) {
            return candidate;
        }
    }
}

impl Registry {
    /// Register a peer and return its id + inbound queue.
    ///
    /// Identity rules (stable ids):
    /// - a fingerprint seen before re-issues the SAME id — and when a
    ///   dead socket of the SAME machine still holds it, the takeover
    ///   replaces the handle (the old task's unregister is a no-op via
    ///   the generation guard); a busy id is NEVER given to a different
    ///   fingerprint;
    /// - otherwise `requested_id` is honored when valid and free;
    /// - otherwise a random id is minted (old behavior).
    ///
    /// `None` when the registry is at `max_peers` and no takeover
    /// applies (atomic check-and-insert — the old pre-check raced).
    pub fn register(
        &self,
        info: PeerInfo,
        requested_id: Option<PeerId>,
        key_fp: Option<&str>,
        max_peers: usize,
    ) -> Option<(PeerId, mpsc::Receiver<SignalMessage>, u64)> {
        let mut peers = self.peers.lock().unwrap();
        let mut fps = self.fps.lock().unwrap();
        let fp_known = key_fp.and_then(|fp| fps.get(fp).copied());
        // A takeover replaces an existing entry instead of growing the
        // registry, so it must not trip the capacity check.
        if fp_known.is_none() && peers.len() >= max_peers {
            return None;
        }
        let gen = self
            .gen_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let id = if let Some(id0) = fp_known {
            match peers.get(&id0) {
                // Occupied by the same machine (a dead socket not yet
                // pruned, or a duplicate agent process): take it back.
                Some(h) if h.fp.as_deref() == key_fp => id0,
                // Occupied by someone else (id collision): mint fresh.
                Some(_) => mint_random(&peers),
                None => id0,
            }
        } else {
            match requested_id {
                Some(r) if opacusr_proto::is_valid_peer_id(r) && !peers.contains_key(&r) => r,
                _ => mint_random(&peers),
            }
        };
        let (tx, rx) = mpsc::channel(PEER_QUEUE_DEPTH);
        let last_seen = Arc::new(std::sync::atomic::AtomicU64::new(now()));
        peers.insert(
            id,
            PeerHandle {
                info,
                tx,
                last_seen,
                gen,
                fp: key_fp.map(String::from),
            },
        );
        if let Some(fp) = key_fp {
            fps.insert(fp.to_string(), id);
        }
        Some((id, rx, gen))
    }

    /// Remove the peer registered by connection generation `gen`.
    /// Returns `true` when THIS connection still owned the id — only
    /// then the caller adjusts the peers gauge. After a takeover the
    /// stale connection's cleanup must be a no-op.
    pub fn unregister(&self, id: PeerId, gen: u64) -> bool {
        let mut peers = self.peers.lock().unwrap();
        match peers.get(&id) {
            Some(p) if p.gen == gen => {
                peers.remove(&id);
                true
            }
            _ => false,
        }
    }

    pub fn touch(&self, id: PeerId) {
        if let Some(p) = self.peers.lock().unwrap().get(&id) {
            p.last_seen
                .store(now(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Queue `msg` for `to`. Errors when the peer is gone or its queue
    /// is full (a stalled reader must never backpressure the whole
    /// server — the sender gets an explicit error instead).
    pub fn send_to(&self, to: PeerId, msg: SignalMessage) -> Result<(), String> {
        let peers = self.peers.lock().unwrap();
        match peers.get(&to) {
            Some(p) => p.tx.try_send(msg).map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => "peer queue full".to_string(),
                mpsc::error::TrySendError::Closed(_) => "peer queue closed".to_string(),
            }),
            None => Err(format!("peer {to} is offline")),
        }
    }

    /// Remove peers that have not heartbeat for [`PEER_TTL`]. Returns
    /// the removed ids so the janitor can adjust the peers gauge.
    pub fn prune(&self) -> Vec<PeerId> {
        // Saturating: a host clock that jumped backwards must not panic
        // the janitor task (which would then never run again).
        let cutoff = now().saturating_sub(PEER_TTL.as_secs());
        let mut peers = self.peers.lock().unwrap();
        let stale: Vec<PeerId> = peers
            .iter()
            .filter(|(_, p)| p.last_seen.load(std::sync::atomic::Ordering::Relaxed) < cutoff)
            .map(|(id, _)| *id)
            .collect();
        for id in &stale {
            peers.remove(id);
        }
        stale
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
pub struct RelayHub {
    slots: std::sync::Mutex<HashMap<[u8; 16], RelaySlot>>,
    metrics: Arc<Metrics>,
}

impl RelayHub {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self {
            slots: std::sync::Mutex::new(HashMap::new()),
            metrics,
        }
    }

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
                self.metrics
                    .tickets
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Some(ticket);
            }
        }
    }

    /// Accept a relay connection (17-byte preamble already validated).
    /// Pairs and pipes when both roles arrived.
    pub async fn accept(&self, stream: TcpStream) -> anyhow::Result<()> {
        let mut stream = stream;
        let mut preamble = [0u8; RELAY_PREAMBLE];
        // A connection that never sends its 17-byte preamble used to pin
        // a task + socket forever (slowloris on the relay port).
        tokio::time::timeout(RELAY_PREAMBLE_TIMEOUT, async {
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut preamble).await
        })
        .await
        .map_err(|_| anyhow::anyhow!("relay preamble timeout"))??;
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
                // Destructure instead of expect(): a poisoned mutex would
                // take every future relay pipe down with it.
                let (a, b) = match (slot.viewer, slot.host) {
                    (Some(a), Some(b)) => (a, b),
                    _ => anyhow::bail!("relay slot lost a half"),
                };
                drop(slots);
                // Raw bidirectional pipe with two *independent* directions:
                // one peer closing must not discard in-flight data for the
                // other (copy_bidirectional shuts both sides down early).
                // Payloads are app-encrypted E2E — the server stays
                // ignorant of the content. Each direction counts its bytes
                // into the shared relay counter.
                let metrics = self.metrics.clone();
                metrics
                    .paired
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                metrics
                    .pipes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let bytes_v = metrics.relay_bytes.clone();
                let bytes_h = metrics.relay_bytes.clone();
                let (mut a_rd, mut a_wr) = tokio::io::split(a);
                let (mut b_rd, mut b_wr) = tokio::io::split(b);
                let v2h = tokio::spawn(async move {
                    let mut reader = CountingReader::new(&mut a_rd, bytes_v);
                    let _ = tokio::io::copy(&mut reader, &mut b_wr).await;
                    // Viewer gone: politely half-close toward the host.
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut b_wr).await;
                });
                let h2v = tokio::spawn(async move {
                    let mut reader = CountingReader::new(&mut b_rd, bytes_h);
                    let _ = tokio::io::copy(&mut reader, &mut a_wr).await;
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut a_wr).await;
                });
                // The pipes gauge counts *pairs*, not directions: one
                // task joins both copies and closes the gauge only when
                // both directions are done.
                tokio::spawn(async move {
                    let _ = v2h.await;
                    let _ = h2v.await;
                    metrics
                        .pipes
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
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
            rate_window: Duration::from_secs(
                (env_parse(
                    "OPACUSR_RATE_WINDOW_SECS",
                    Self::default().rate_window.as_secs(),
                ))
                .max(1),
            ),
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
    /// Prometheus counters/gauges, served on `GET /metrics`.
    pub metrics: Arc<Metrics>,
}

impl AppState {
    /// Historical constructor: no rate limiting, no caps, no TURN minting
    /// (existing tests and embedders rely on this).
    pub fn new(relay_port: u16) -> Self {
        Self::with_options(relay_port, Limits::permissive(), None)
    }

    /// Fully configured constructor (used by the binary).
    pub fn with_options(relay_port: u16, limits: Limits, turn: Option<TurnConfig>) -> Self {
        let limiter = ConnLimiter::new(limits.rate_max, limits.rate_window);
        let metrics = Arc::new(Metrics::new());
        Self {
            registry: Registry::default(),
            relay: RelayHub::new(metrics.clone()),
            relay_port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            limits,
            limiter,
            turn,
            metrics,
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
                if !stale.is_empty() {
                    tracing::info!(target: "opacusr", "pruned {} stale peers", stale.len());
                    // The janitor removed registry entries whose sockets
                    // may still be draining — keep the peers gauge honest
                    // (it only decremented on socket close before).
                    state
                        .metrics
                        .peers
                        .fetch_sub(stale.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
    }
}

/// HTTP router: `/signal` WebSocket, `/turn` credentials, `/health`,
/// `/metrics` (Prometheus text format).
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/signal", get(signal_route))
        .route("/turn", get(turn_route))
        .route("/health", get(health))
        .route("/metrics", get(metrics_route))
        .fallback(unreachable)
        .with_state(state)
}

/// Prometheus text exposition of the server counters.
async fn metrics_route(State(state): State<Arc<AppState>>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render(&state.version),
    )
        .into_response()
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
            state
                .metrics
                .rate_limited
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(target: "opacusr", "rate limit: signaling connect from {ip}");
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "too many connections from this address, retry later",
            )
                .into_response();
        }
    }
    // 1 MiB per frame: SDP offers are KiB-sized; anything larger is abuse
    // (the tungstenite default is 64 MiB — a registered peer could soak
    // server memory with oversized frames).
    ws.max_message_size(1 << 20)
        .max_frame_size(1 << 20)
        .on_upgrade(move |socket| handle_socket(socket, state))
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
            state
                .metrics
                .rate_limited
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    state
        .metrics
        .turn_mints
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    state
        .metrics
        .upgrades
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut socket = socket;
    // First message must be a Register. Time-bounded: a websocket that
    // never sends one used to pin a task + socket forever (slowloris).
    let reg = match tokio::time::timeout(Duration::from_secs(10), recv_signal(&mut socket)).await {
        Ok(Ok(SignalMessage::Register(reg))) => reg,
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
    // Server-side sanity caps (defense in depth; the frame limit is the
    // only other barrier). Refuse absurd identities instead of storing
    // attacker-controlled unbounded strings in the registry.
    if reg.display_name.chars().count() > MAX_DISPLAY_NAME {
        let _ = send_signal(
            &mut socket,
            SignalMessage::Error {
                message: "display name too long".into(),
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
    let (my_id, mut inbound, my_gen) = {
        // Atomic check-and-insert inside the registry (the old
        // len()-precheck raced under concurrent registrations).
        let Some((id, inbound, gen)) = state.registry.register(
            my_info.clone(),
            reg.requested_id,
            reg.key_fp.as_deref(),
            state.limits.max_peers,
        ) else {
            tracing::warn!(target: "opacusr", "refusing registration: server is full");
            let _ = send_signal(
                &mut socket,
                SignalMessage::Error {
                    message: "server is full, retry later".into(),
                },
            )
            .await;
            return;
        };
        state
            .metrics
            .registrations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        state
            .metrics
            .peers
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        (id, inbound, gen)
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

    // Only a connection that STILL owns its id removes it and lowers
    // the gauge: after a same-machine takeover the stale socket's
    // cleanup must not delete the fresh registration.
    if state.registry.unregister(my_id, my_gen) {
        state
            .metrics
            .peers
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
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
            if display_name.chars().count() > MAX_DISPLAY_NAME {
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::Error {
                        message: "display name too long".into(),
                    },
                );
                return;
            }
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
            if payload.max_len() > MAX_PEER_PAYLOAD {
                let _ = state.registry.send_to(
                    from,
                    SignalMessage::Error {
                        message: "peer payload too large".into(),
                    },
                );
                return;
            }
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

    fn test_info() -> PeerInfo {
        PeerInfo {
            platform: Platform::Linux,
            display_name: "tester".into(),
            is_agent: true,
            unattended: false,
        }
    }

    /// The stable-ID rules: same fingerprint re-issues the same id even
    /// while a dead socket holds it (takeover), a different fingerprint
    /// never gets a busy id, stale unregisters are no-ops.
    #[test]
    fn registry_stable_ids_takeover_and_guard() {
        let reg = Registry::default();
        // First registration with a fingerprint.
        let (id1, _rx1, g1) = reg.register(test_info(), None, Some("fp-a"), 10).unwrap();
        // Same machine re-registering (its old socket is a zombie):
        // the id is re-issued, a takeover replaces the handle.
        let (id2, _rx2, g2) = reg.register(test_info(), None, Some("fp-a"), 10).unwrap();
        assert_eq!(id1, id2, "same fingerprint must keep its id");
        // The stale connection's cleanup must not delete the takeover.
        assert!(!reg.unregister(id1, g1), "stale unregister must be a no-op");
        // A different fingerprint is refused the busy id, and the fp
        // binding sticks: fp-b gets its own minted id, requested ids
        // are honored only for fingerprints seen for the first time.
        let (id3, _rx3, g3) = reg
            .register(test_info(), Some(id1), Some("fp-b"), 10)
            .unwrap();
        assert_ne!(id3, id1, "a busy id must not go to another fingerprint");
        // …and fp-b keeps that id on its next registration too.
        let (id4, _rx4, _g4) = reg
            .register(test_info(), Some(id1), Some("fp-b"), 10)
            .unwrap();
        assert_eq!(id4, id3, "a known fingerprint keeps its assigned id");
        assert!(reg.unregister(id1, g2), "the live owner can unregister");
        // id4 is the same peer id as id3 (a takeover re-registered it
        // under a new generation), so g3 is stale now and the live
        // _g4 owns the slot.
        assert!(!reg.unregister(id3, g3));
        assert!(reg.unregister(id4, _g4));
    }

    #[test]
    fn registry_requested_id_validated_and_cap_respected() {
        let reg = Registry::default();
        // An invalid requested id falls back to a random mint.
        let (id, _rx, g) = reg
            .register(test_info(), Some(12_345_678), None, 10)
            .unwrap();
        assert!(opacusr_proto::is_valid_peer_id(id));
        reg.unregister(id, g);
        // Capacity: no fingerprint → the cap applies.
        let mut gens = Vec::new();
        for _ in 0..10 {
            let (id, _rx, g) = reg.register(test_info(), None, None, 10).unwrap();
            gens.push((id, g));
        }
        assert!(
            reg.register(test_info(), None, None, 10).is_none(),
            "full registry must refuse a plain registration"
        );
        // A fingerprint seen BEFORE the cap still re-issues its id at the
        // cap (takeover replaces an entry instead of growing the map).
        let cap_reg = Registry::default();
        let (seed_id, _seed_rx, seed_g) = cap_reg
            .register(test_info(), None, Some("fp-c"), 10)
            .unwrap();
        let mut gens2 = vec![(seed_id, seed_g)];
        for _ in 0..9 {
            let (id, _rx, g) = cap_reg.register(test_info(), None, None, 10).unwrap();
            gens2.push((id, g));
        }
        assert!(cap_reg.register(test_info(), None, None, 10).is_none());
        let retaken = cap_reg
            .register(test_info(), None, Some("fp-c"), 10)
            .unwrap();
        assert_eq!(retaken.0, seed_id, "takeover at the cap keeps the id");
        for (id, g) in gens {
            reg.unregister(id, g);
        }
        for (id, g) in gens2 {
            cap_reg.unregister(id, g);
        }
        cap_reg.unregister(retaken.0, retaken.2);
    }

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
