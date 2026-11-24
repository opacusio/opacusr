//! Phase-1 prototype of the decentralized OpacusR mode.
//!
//! Goal: connect two devices behind home routers **without any user-owned
//! rendezvous infrastructure**, using the proven rust-libp2p stack:
//!
//! * **Kademlia DHT** — publish/lookup of a device's reachable addresses
//!   under its `PeerId` (connect-by-ID, no fixed signaling server).
//! * **AutoNAT** — learns whether this node is publicly reachable and its
//!   external address.
//! * **Relay v2** — publicly reachable seed nodes carry traffic for peers
//!   that cannot accept inbound connections (the TURN role).
//! * **DCUtR** — coordinated hole punching: once two peers are in touch
//!   through the relay, they race a simultaneous connect and upgrade to a
//!   direct link when the NATs allow it.
//!
//! Transport: QUIC (plus TCP/noise/yamux) so media and the control channel
//! share one multiplexed, encrypted connection.
//!
//! Honest notes for this phase:
//! * "Serverless" still needs a few **bootstrap/relay nodes** reachable from
//!   the internet (few, interchangeable, self-hostable). They are the only
//!   thing that must exist — every other node stays behind its NAT.
//! * Records in the DHT make a device's online status visible to the swarm.
//!   Device auth stays at the OpacusR protocol layer (this prototype answers
//!   a trivial ping), and a private swarm is selectable by pointing the
//!   bootstrap list at your own nodes and keeping the custom protocol names.
//! * Symmetric NATs (carrier 5G) often cannot be punched directly — those
//!   peers stay on the relay path, which is exactly what Relay v2 is for.
//! * Known gap (phase 1): the DCUtR upgrade did not trigger on the loopback
//!   test harness (both sides keep talking over the relay, which still
//!   works). The hole punch itself is upstream-proven code; verify it under
//!   real NAT conditions in the field test before relying on direct links.
//!
//! Ladder mapping vs the server-based roadmap:
//!
//! | Role                   | Server plan      | This crate         |
//! |------------------------|------------------|--------------------|
//! | find device by ID      | signaling server | Kademlia put/get   |
//! | learn external address | STUN             | AutoNAT + identify |
//! | punch NAT directly     | ICE candidates   | DCUtR              |
//! | relay fallback         | TURN             | Relay v2           |

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::core::multiaddr::Protocol as AddrProtocol;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::{NetworkBehaviour, StreamProtocol, SwarmEvent};
use libp2p::{autonat, dcutr, identify, kad, noise, relay, request_response, tcp, yamux, Swarm};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Re-exported so embedders can build bootstrap addresses:
/// `seed_addr.with(Protocol::P2p(seed_peer_id))`.
pub use libp2p::core::multiaddr::Protocol;
pub use libp2p::identity::Keypair;
/// Re-exported: embedders and the CLI build/parse multiaddrs and PeerIds.
pub use libp2p::{Multiaddr, PeerId};

/// Stable node identity (see [`keypair`] for the file format).
pub mod keypair;

const IDENTIFY_PROTO: &str = "/opacusr/id/1.0.0";
const KAD_PROTO: &str = "/opacusr/kad/1.0.0";
const CONTROL_PROTO: &str = "/opacusr/control/1.0.0";
const RECORD_TTL: Duration = Duration::from_secs(30 * 60);
/// The swarm default idle timeout is 10 s — far too twitchy for phones that
/// park a connection between sessions.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Minimal control-channel message. Phase 1 only implements the `ping`
/// roundtrip; real OpacusR signaling moves here in phase 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub kind: String,
    pub payload: Vec<u8>,
}

/// Reply to [`ControlRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    pub payload: Vec<u8>,
}

/// Events the node loop surfaces to the embedder.
#[derive(Debug, Clone)]
pub enum Event {
    Listening {
        addr: Multiaddr,
    },
    RelayReserved {
        relay: PeerId,
    },
    ReachabilityChanged {
        public_addr: Option<Multiaddr>,
    },
    AddressesPublished {
        addrs: Vec<Multiaddr>,
    },
    PeerFound {
        peer: PeerId,
        addrs: Vec<Multiaddr>,
    },
    LookupFailed {
        peer: PeerId,
    },
    Connected {
        peer: PeerId,
    },
    Disconnected {
        peer: PeerId,
    },
    HolePunchSucceeded {
        peer: PeerId,
    },
    ControlRequest {
        peer: PeerId,
        request: ControlRequest,
    },
    ControlResponse {
        peer: PeerId,
        response: ControlResponse,
    },
}

enum Command {
    ConnectByPeerId(PeerId),
    DialAddrs(Vec<Multiaddr>),
    SendControl {
        peer: PeerId,
        request: ControlRequest,
    },
    PublishNow,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Seed nodes, e.g. `/ip4/host/udp/4001/quic-v1/p2p/<PeerId>`. Their
    /// multiaddrs double as relay addresses for the circuit listen.
    pub bootstrap: Vec<Multiaddr>,
    pub listen: Vec<Multiaddr>,
    pub keypair: Option<Keypair>,
    /// Kademlia server mode (seed nodes store DHT records).
    pub kad_server: bool,
    /// Offer relay circuits to other peers (seed/relay nodes only).
    pub serve_relay: bool,
    pub publish_interval: Duration,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            bootstrap: Vec::new(),
            listen: vec![
                "/ip4/0.0.0.0/udp/0/quic-v1".parse().expect("static addr"),
                "/ip4/0.0.0.0/tcp/0".parse().expect("static addr"),
            ],
            keypair: None,
            kad_server: false,
            serve_relay: false,
            publish_interval: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum P2pError {
    #[error("command channel closed: node loop dropped")]
    ChannelClosed,
    #[error("swarm build failed: {0}")]
    Build(String),
}

/// Cloneable sender side. Keep the [`mpsc::Receiver`] from [`spawn`] to read
/// [`Event`]s.
#[derive(Clone, Debug)]
pub struct Handle {
    peer_id: PeerId,
    cmd_tx: mpsc::Sender<Command>,
}

impl Handle {
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Full phase-1 ladder: DHT lookup by ID, then dial every address the
    /// record carries — direct ones win when punchable, relayed circuit
    /// addresses always connect and DCUtR upgrades them afterwards.
    pub async fn connect_by_peer_id(&self, peer: PeerId) -> Result<(), P2pError> {
        self.send(Command::ConnectByPeerId(peer)).await
    }

    pub async fn dial_addrs(&self, addrs: Vec<Multiaddr>) -> Result<(), P2pError> {
        self.send(Command::DialAddrs(addrs)).await
    }

    pub async fn send_control(
        &self,
        peer: PeerId,
        request: ControlRequest,
    ) -> Result<(), P2pError> {
        self.send(Command::SendControl { peer, request }).await
    }

    pub async fn publish_now(&self) -> Result<(), P2pError> {
        self.send(Command::PublishNow).await
    }

    /// Stop the node loop. The event receiver ends after this.
    pub async fn shutdown(&self) -> Result<(), P2pError> {
        self.send(Command::Shutdown).await
    }

    async fn send(&self, cmd: Command) -> Result<(), P2pError> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| P2pError::ChannelClosed)
    }
}

/// Crate-owned fallback runtime for embedders that call [`spawn`]
/// outside any tokio context (the egui UI thread of the GUI has none).
/// Created lazily on first use, lives for the rest of the process.
static NODE_RT: std::sync::OnceLock<Option<tokio::runtime::Runtime>> = std::sync::OnceLock::new();

/// The runtime the node loop must run on: the caller's when one is
/// active (the `p2p-node` CLI under `#[tokio::main]`, tests), else the
/// crate-owned fallback. `Err` only if neither exists.
fn node_runtime_handle() -> Result<tokio::runtime::Handle, P2pError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return Ok(handle);
    }
    match NODE_RT
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("opacusr-p2p")
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
    {
        Some(rt) => Ok(rt.handle().clone()),
        None => Err(P2pError::Build(
            "tokio runtime unavailable: spawn must be called from inside a runtime \
             and the crate-owned fallback runtime failed to start"
                .into(),
        )),
    }
}

/// Spawn a node. Returns its handle plus the event stream.
///
/// Callable from any thread: when the caller is NOT inside a tokio
/// runtime (the egui UI thread), the node runs on the crate-owned
/// fallback runtime instead of panicking — 0.11.23 shipped that panic
/// («must be called from the context of a Tokio 1.x runtime»): the
/// persisted `p2p_enabled` flag made every GUI start call this from
/// `App::new` on the UI thread, so enabling the decentralized mode in
/// the settings bricked the app on Windows until the flag was removed.
pub fn spawn(cfg: NodeConfig) -> Result<(Handle, mpsc::Receiver<Event>), P2pError> {
    // Everything below — swarm build, the eager socket binds inside
    // listen_on, the bootstrap dial and the tokio::spawn of the node
    // loop — must run inside a runtime context.
    let _rt_guard = node_runtime_handle()?.enter();

    let keypair = cfg
        .keypair
        .clone()
        .unwrap_or_else(libp2p::identity::Keypair::generate_ed25519);
    let peer_id = keypair.public().to_peer_id();

    let mut swarm = build_swarm(&keypair, &cfg)?;

    for addr in &cfg.listen {
        swarm
            .listen_on(addr.clone())
            .map_err(|e| P2pError::Build(format!("listen {addr}: {e}")))?;
    }
    for b in &cfg.bootstrap {
        swarm
            .dial(b.clone())
            .map_err(|e| P2pError::Build(format!("dial bootstrap {b}: {e}")))?;
    }

    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (evt_tx, evt_rx) = mpsc::channel(256);
    tokio::spawn(run(swarm, cfg, peer_id, cmd_rx, evt_tx));

    Ok((Handle { peer_id, cmd_tx }, evt_rx))
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    relay_server: Toggle<relay::Behaviour>,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    autonat: autonat::Behaviour,
    control: request_response::cbor::Behaviour<ControlRequest, ControlResponse>,
}

fn build_swarm(keypair: &Keypair, cfg: &NodeConfig) -> Result<Swarm<Behaviour>, P2pError> {
    let peer_id = keypair.public().to_peer_id();

    let store = kad::store::MemoryStore::new(peer_id);
    let mut kademlia = kad::Behaviour::with_config(
        peer_id,
        store,
        kad::Config::new(StreamProtocol::new(KAD_PROTO)),
    );
    kademlia.set_mode(Some(if cfg.kad_server {
        kad::Mode::Server
    } else {
        kad::Mode::Client
    }));

    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| P2pError::Build(e.to_string()))?
        .with_quic()
        .with_dns()
        .expect("with_dns is infallible")
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|e| P2pError::Build(e.to_string()))?
        .with_behaviour(|kp, relay_client| {
            Behaviour {
                relay_client,
                relay_server: Toggle::from(if cfg.serve_relay {
                    let mut relay_server =
                        relay::Behaviour::new(kp.public().to_peer_id(), relay::Config::default());
                    // Seeds are relays on purpose: skip the auto "am I
                    // externally reachable?" flapping, which races the first
                    // client RESERVE with the identify-based confirmation.
                    relay_server.set_status(Some(relay::Status::Enable));
                    Some(relay_server)
                } else {
                    None
                }),
                identify: identify::Behaviour::new(
                    identify::Config::new(IDENTIFY_PROTO.to_string(), kp.public())
                        .with_agent_version("opacusr-libp2p/0.1.0".to_string()),
                ),
                dcutr: dcutr::Behaviour::new(kp.public().to_peer_id()),
                kademlia,
                autonat: autonat::Behaviour::new(
                    kp.public().to_peer_id(),
                    autonat::Config::default(),
                ),
                control: request_response::Behaviour::new(
                    [(
                        StreamProtocol::new(CONTROL_PROTO),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default()
                        .with_request_timeout(Duration::from_secs(15)),
                ),
            }
        })
        .expect("with_behaviour phase cannot fail")
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_TIMEOUT))
        .build();

    Ok(swarm)
}

struct NodeState {
    /// bootstrap peer -> its multiaddr (to build the circuit listen addr)
    bootstrap_addrs: HashMap<PeerId, Multiaddr>,
    my_circuit: Option<Multiaddr>,
    my_observed: Option<Multiaddr>,
    pending_lookups: HashMap<kad::QueryId, PeerId>,
    /// Lookups whose DHT query came back without a record — retried on the
    /// next established connection (the record may appear at any moment).
    deferred_lookups: Vec<PeerId>,
    pending_publishes: HashSet<kad::QueryId>,
    pending_controls: HashMap<request_response::OutboundRequestId, PeerId>,
    kad_bootstrapped: bool,
    connected: HashSet<PeerId>,
    /// Bootstrap relays we already issued a circuit `listen_on` for.
    relay_listening: HashSet<PeerId>,
    /// Bounded retry counter for circuit listeners.
    circuit_retries: u32,
}

async fn run(
    mut swarm: Swarm<Behaviour>,
    cfg: NodeConfig,
    peer_id: PeerId,
    mut cmd_rx: mpsc::Receiver<Command>,
    evt_tx: mpsc::Sender<Event>,
) {
    let mut state = NodeState {
        bootstrap_addrs: HashMap::new(),
        my_circuit: None,
        my_observed: None,
        pending_lookups: HashMap::new(),
        deferred_lookups: Vec::new(),
        pending_publishes: HashSet::new(),
        pending_controls: HashMap::new(),
        kad_bootstrapped: false,
        connected: HashSet::new(),
        relay_listening: HashSet::new(),
        circuit_retries: 0,
    };

    for b in &cfg.bootstrap {
        if let Some(peer) = multiaddr_peer_id(b) {
            // Keep the relay address WITHOUT its `/p2p/<peer>` tail — the
            // circuit address is built as `<relay_addr>/p2p/<relay>/p2p-circuit`.
            state.bootstrap_addrs.insert(peer, strip_peer_id(b));
        }
    }

    let mut publish_tick = tokio::time::interval(cfg.publish_interval);
    publish_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(%peer_id, "opacusr-libp2p node started");

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::ConnectByPeerId(peer)) => {
                        lookup_peer(&mut swarm, &mut state, peer, &evt_tx);
                    }
                    Some(Command::DialAddrs(addrs)) => {
                        for a in &addrs {
                            if let Err(e) = swarm.dial(a.clone()) {
                                debug!(%a, %e, "dial failed");
                            }
                        }
                    }
                    Some(Command::SendControl { peer, request }) => {
                        let id = swarm.behaviour_mut().control.send_request(&peer, request);
                        state.pending_controls.insert(id, peer);
                    }
                    Some(Command::PublishNow) => {
                        publish_addresses(&mut swarm, &mut state, peer_id);
                    }
                }
            }
            _ = publish_tick.tick() => {
                publish_addresses(&mut swarm, &mut state, peer_id);
            }
            ev = swarm.select_next_some() => {
                handle_event(&mut swarm, &mut state, peer_id, &evt_tx, ev);
            }
        }
    }
    info!(%peer_id, "node loop stopped");
}

fn lookup_peer(
    swarm: &mut Swarm<Behaviour>,
    state: &mut NodeState,
    peer: PeerId,
    evt_tx: &mpsc::Sender<Event>,
) {
    let qid = swarm
        .behaviour_mut()
        .kademlia
        .get_record(kad::RecordKey::new(&peer.to_bytes()));
    state.pending_lookups.insert(qid, peer);
    let _ = evt_tx; // lookup results arrive as kad query events
}

fn publish_addresses(swarm: &mut Swarm<Behaviour>, state: &mut NodeState, self_peer: PeerId) {
    let addrs = current_addrs(swarm, state);
    if addrs.is_empty() {
        return;
    }
    let value = encode_addrs(&addrs);
    let mut record = kad::Record::new(kad::RecordKey::new(&self_peer.to_bytes()), value);
    record.expires = Some(Instant::now() + RECORD_TTL);
    match swarm
        .behaviour_mut()
        .kademlia
        .put_record(record, kad::Quorum::One)
    {
        Ok(qid) => {
            state.pending_publishes.insert(qid);
        }
        Err(e) => debug!(%e, "put_record failed (no kad peers yet)"),
    }
}

/// Dialable addresses of this node: concrete listeners, the relayed circuit
/// and any confirmed/observed external address. Wildcard listeners are
/// useless to other peers and are dropped.
fn current_addrs(swarm: &Swarm<Behaviour>, state: &NodeState) -> Vec<Multiaddr> {
    let mut out: Vec<Multiaddr> = Vec::new();
    let push = |a: &Multiaddr, out: &mut Vec<Multiaddr>| {
        if is_dialable(a) && !out.contains(a) {
            out.push(a.clone());
        }
    };
    for a in swarm.listeners() {
        push(a, &mut out);
    }
    if let Some(c) = &state.my_circuit {
        push(c, &mut out);
    }
    if let Some(o) = &state.my_observed {
        push(o, &mut out);
    }
    for rec in swarm.external_addresses() {
        push(rec, &mut out);
    }
    out
}

fn is_dialable(addr: &Multiaddr) -> bool {
    !addr.iter().any(|p| match p {
        AddrProtocol::Ip4(ip) => ip.is_unspecified(),
        AddrProtocol::Ip6(ip) => ip.is_unspecified(),
        _ => false,
    })
}

fn encode_addrs(addrs: &[Multiaddr]) -> Vec<u8> {
    let strs: Vec<String> = addrs.iter().map(ToString::to_string).collect();
    bincode::serde::encode_to_vec(&strs, bincode::config::standard()).expect("bincode encode")
}

fn decode_addrs(bytes: &[u8]) -> Vec<Multiaddr> {
    match bincode::serde::decode_from_slice::<Vec<String>, _>(bytes, bincode::config::standard()) {
        Ok((strs, _)) => strs.iter().filter_map(|s| s.parse().ok()).collect(),
        Err(e) => {
            debug!(%e, "record value decode failed");
            Vec::new()
        }
    }
}

fn multiaddr_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        AddrProtocol::P2p(id) => Some(id),
        _ => None,
    })
}

fn strip_peer_id(addr: &Multiaddr) -> Multiaddr {
    let mut out = addr.clone();
    if matches!(out.iter().last(), Some(AddrProtocol::P2p(_))) {
        out.pop();
    }
    out
}

fn handle_event(
    swarm: &mut Swarm<Behaviour>,
    state: &mut NodeState,
    self_peer: PeerId,
    evt_tx: &mpsc::Sender<Event>,
    ev: SwarmEvent<BehaviourEvent>,
) {
    match ev {
        SwarmEvent::NewListenAddr { address, .. } => {
            if address
                .iter()
                .any(|p| matches!(p, AddrProtocol::P2pCircuit))
            {
                state.my_circuit = Some(address.clone());
            }
            let _ = evt_tx.try_send(Event::Listening {
                addr: address.clone(),
            });
            debug!(%address, "listening");
        }
        SwarmEvent::ExternalAddrConfirmed { address } => {
            debug!(%address, "external address confirmed");
            state.my_observed = Some(address.clone());
            let _ = evt_tx.try_send(Event::ReachabilityChanged {
                public_addr: Some(address.clone()),
            });
            publish_addresses(swarm, state, self_peer);
        }
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            let newly = state.connected.insert(peer_id);
            if newly {
                let _ = evt_tx.try_send(Event::Connected { peer: peer_id });
            }
            let addr = endpoint.get_remote_address().clone();
            swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
            let deferred: Vec<PeerId> = std::mem::take(&mut state.deferred_lookups);
            for peer in deferred {
                lookup_peer(swarm, state, peer, evt_tx);
            }
            if state.bootstrap_addrs.contains_key(&peer_id) && !state.kad_bootstrapped {
                state.kad_bootstrapped = swarm.behaviour_mut().kademlia.bootstrap().is_ok();
                if state.kad_bootstrapped {
                    debug!("kad bootstrap started");
                }
            }
            // The relay client requests a reservation exactly when we listen
            // on a `/p2p-circuit` address — so listen as soon as the relay
            // connection is up (doing it only after the confirmation would
            // deadlock).
            if state.bootstrap_addrs.contains_key(&peer_id) && state.relay_listening.insert(peer_id)
            {
                if let Some(relay_addr) = state.bootstrap_addrs.get(&peer_id) {
                    // The circuit address must carry the relay's PeerId:
                    // `/relay-addr/p2p/<relay>/p2p-circuit`.
                    let circuit = relay_addr
                        .clone()
                        .with(AddrProtocol::P2p(peer_id))
                        .with(AddrProtocol::P2pCircuit);
                    if let Err(e) = swarm.listen_on(circuit) {
                        warn!(?e, "listen on relay circuit failed");
                    } else {
                        info!(relay=%peer_id, "circuit listen issued");
                    }
                }
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            if state.connected.remove(&peer_id) {
                let _ = evt_tx.try_send(Event::Disconnected { peer: peer_id });
            }
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            debug!(peer=?peer_id, %error, "outgoing connection failed");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info:
                identify::Info {
                    observed_addr,
                    listen_addrs,
                    ..
                },
            ..
        })) => {
            debug!(%peer_id, %observed_addr, ?listen_addrs, "identify received");
            // The remote saw our connection from `observed_addr` — record it
            // as a candidate external address (swarm dedups by score).
            swarm.add_external_address(observed_addr.clone());
            state.my_observed = Some(observed_addr.clone());
            for a in &listen_addrs {
                if is_dialable(a) {
                    swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, a.clone());
                }
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
            id,
            result,
            ..
        })) => match result {
            kad::QueryResult::Bootstrap(Ok(_)) => {
                debug!(%self_peer, "kad bootstrap done");
                // The routing table is live now — publish immediately
                // instead of waiting for the next periodic tick (the
                // first publish would otherwise lag up to
                // publish_interval and DHT discovery stalls).
                publish_addresses(swarm, state, self_peer);
            }
            kad::QueryResult::Bootstrap(Err(e)) => {
                debug!(?e, "kad bootstrap failed");
            }
            kad::QueryResult::PutRecord(Ok(kad::PutRecordOk { .. })) => {
                if state.pending_publishes.remove(&id) {
                    let addrs = current_addrs(swarm, state);
                    let _ = evt_tx.try_send(Event::AddressesPublished { addrs });
                }
            }
            kad::QueryResult::PutRecord(Err(e)) => {
                debug!(?e, "kad put_record failed");
                state.pending_publishes.remove(&id);
            }
            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(rec))) => {
                let addrs = decode_addrs(&rec.record.value);
                if let Some(peer) = state.pending_lookups.remove(&id) {
                    info!(%peer, ?addrs, "peer found in DHT");
                    let _ = evt_tx.try_send(Event::PeerFound {
                        peer,
                        addrs: addrs.clone(),
                    });
                    for a in &addrs {
                        if let Err(e) = swarm.dial(a.clone()) {
                            debug!(%a, %e, "dial of discovered addr failed");
                        }
                    }
                }
            }
            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FinishedWithNoAdditionalRecord {
                ..
            })) => {
                // quorum reached without further records
            }
            kad::QueryResult::GetRecord(Err(e)) => {
                debug!(?e, "kad get_record failed");
                for (_, peer) in state.pending_lookups.drain() {
                    state.deferred_lookups.push(peer);
                }
            }
            other => {
                debug!(?other, "kad query result");
            }
        },
        SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
            relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
        )) => {
            info!(relay=%relay_peer_id, "relay reservation accepted");
            let _ = evt_tx.try_send(Event::RelayReserved {
                relay: relay_peer_id,
            });
            publish_addresses(swarm, state, self_peer);
        }
        SwarmEvent::Behaviour(BehaviourEvent::RelayClient(ev)) => {
            debug!(?ev, "relay client event");
        }
        SwarmEvent::ListenerClosed { reason, .. } => {
            // The first RESERVE can lose the race against the relay server's
            // own status setup — re-issue the circuit listen a bounded number
            // of times.
            if state.circuit_retries < 5 {
                state.circuit_retries += 1;
                for (peer, base) in state.bootstrap_addrs.clone() {
                    if state.relay_listening.contains(&peer) {
                        let circuit = base
                            .clone()
                            .with(AddrProtocol::P2p(peer))
                            .with(AddrProtocol::P2pCircuit);
                        match swarm.listen_on(circuit) {
                            Ok(_) => {
                                info!(relay=%peer, "circuit listen re-issued");
                                state.relay_listening.remove(&peer);
                            }
                            Err(e) => warn!(relay=%peer, ?e, "circuit re-listen failed"),
                        }
                    }
                }
            } else {
                warn!(?reason, "listener closed (retries exhausted)");
            }
        }
        SwarmEvent::ListenerError { error, .. } => {
            warn!(%error, "listener error");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Dcutr(ev)) => {
            if let Ok(_conn) = &ev.result {
                info!(peer=%ev.remote_peer_id, "hole punch succeeded");
                let _ = evt_tx.try_send(Event::HolePunchSucceeded {
                    peer: ev.remote_peer_id,
                });
            } else {
                debug!(peer=%ev.remote_peer_id, "hole punch failed");
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
            old,
            new,
        })) => {
            info!(?old, ?new, "nat status changed");
            if let autonat::NatStatus::Public(addr) = new {
                let _ = evt_tx.try_send(Event::ReachabilityChanged {
                    public_addr: Some(addr.clone()),
                });
                publish_addresses(swarm, state, self_peer);
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Autonat(ev)) => {
            debug!(?ev, "autonat event");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
            peer,
            message,
            ..
        })) => match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                debug!(%peer, ?request, "control request");
                if request.kind == "ping" {
                    let resp = ControlResponse {
                        ok: true,
                        payload: b"pong".to_vec(),
                    };
                    if let Err(_closed) = swarm
                        .behaviour_mut()
                        .control
                        .send_response(channel, resp.clone())
                    {
                        debug!("control response rejected (channel closed)");
                    }
                }
                let _ = evt_tx.try_send(Event::ControlRequest { peer, request });
            }
            request_response::Message::Response {
                request_id,
                response,
            } => {
                if let Some(peer) = state.pending_controls.remove(&request_id) {
                    let _ = evt_tx.try_send(Event::ControlResponse { peer, response });
                }
            }
        },
        SwarmEvent::Behaviour(other) => {
            debug!(?other, "behaviour event");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Wait (bounded) for the node's first `Listening` event — proof the
    /// loop actually runs and its sockets are bound.
    fn wait_listening(events: &mut mpsc::Receiver<Event>, deadline: Duration) -> Option<Multiaddr> {
        let stop = Instant::now() + deadline;
        while Instant::now() < stop {
            match events.try_recv() {
                Ok(Event::Listening { addr }) => return Some(addr),
                Ok(_) => continue,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        None
    }

    /// 0.11.24 regression: the GUI calls `spawn` from the egui UI thread,
    /// which has NO tokio runtime context — `tokio::spawn` used to panic
    /// there («must be called from the context of a Tokio 1.x runtime»).
    /// The enable flag is persisted, so every app start after enabling
    /// the decentralized mode died before the first frame (0.11.23,
    /// «включил настройки децентрализованный — не запускается вообще»).
    /// This test runs on a plain test thread — exactly that context.
    #[test]
    fn spawn_works_without_ambient_runtime() {
        let (handle, mut events) = spawn(NodeConfig::default()).expect("spawn without runtime");
        assert!(!handle.peer_id().to_string().is_empty());

        let addr = wait_listening(&mut events, Duration::from_secs(15))
            .expect("node loop bound a listener within 15 s");
        assert!(
            addr.to_string().contains("quic-v1") || addr.to_string().contains("tcp"),
            "unexpected listener addr {addr}"
        );

        // A clean shutdown ends the loop instead of leaking the node.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async { handle.shutdown().await })
            .expect("shutdown ok");
    }

    /// The other path: callers already inside a runtime (`p2p-node` CLI
    /// under `#[tokio::main]`) must keep working — sockets and the node
    /// loop land on THEIR runtime, no fallback is created. A
    /// current-thread runtime only progresses while `block_on` is
    /// running, so the whole wait happens inside one `block_on`.
    #[test]
    fn spawn_inside_runtime_uses_it() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async {
            let (handle, mut events) = spawn(NodeConfig::default()).expect("spawn inside runtime");
            let addr = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    match events.recv().await {
                        Some(Event::Listening { addr }) => return addr,
                        Some(_) => continue,
                        None => panic!("event channel closed"),
                    }
                }
            })
            .await
            .expect("listener within 15 s");
            assert!(
                addr.to_string().contains("quic-v1") || addr.to_string().contains("tcp"),
                "unexpected listener addr {addr}"
            );
            handle.shutdown().await.expect("shutdown ok");
        });
    }
}
