//! Experimental decentralized mode (phase 2): the embedded rust-libp2p
//! node surfaced in the GUI.
//!
//! Desktop only — the mobile shells build `opacusr-app` without this
//! module (the libp2p stack pulls aws-lc-rs, which is not wired for
//! mobile cross-builds yet). The node publishes this machine's addresses
//! under a stable `PeerId` in the Kademlia DHT, learns its reachability
//! from AutoNAT, reserves relay circuits from the bootstrap list and
//! answers control pings. Phase 2 carries the control channel only —
//! full screen sessions over libp2p are the next step.

use std::str::FromStr;
use std::time::{Duration, Instant};

use opacusr_libp2p::{ControlRequest, Event, Multiaddr, NodeConfig, PeerId};

use crate::app::App;

/// The node is listening on this fixed pair of ports so invite addresses
/// stay stable across restarts.
const QUIC_PORT: u16 = 4001;
const TCP_PORT: u16 = 4001;
/// How long a «Пинг» may take before the UI declares it failed.
const PING_TIMEOUT: Duration = Duration::from_secs(60);

/// UI-side state of the embedded node.
pub struct P2pUi {
    /// `None` after shutdown (the node task drains and exits).
    pub handle: Option<opacusr_libp2p::Handle>,
    /// Our stable identity, base58 — what partners dial by.
    pub peer_id: String,
    /// `/ip4/<lan>/udp/4001/quic-v1/p2p/<peer>` — paste into the other
    /// machine's bootstrap list when both are on the same network.
    pub invite_addr: String,
    /// Non-wildcard listeners as reported by the swarm (bounded).
    pub listen_addrs: Vec<String>,
    /// AutoNAT / relay picture of how the internet sees this node.
    pub reachability: String,
    /// Last notable event, for the «События» row.
    pub status: String,
    /// UI-side end of the event bridge (node task → GUI thread).
    pub events: Option<std::sync::mpsc::Receiver<Event>>,
    pub ping_status: String,
    /// In-flight «Пинг»: target + when it started (drives Connected →
    /// SendControl and the timeout).
    pub(crate) pending_ping: Option<(PeerId, Instant)>,
    /// The current ping path upgraded to a direct link (DCUtR).
    pub(crate) punched: bool,
}

/// Start the embedded node (does nothing when it already runs).
pub(crate) fn start_node(app: &mut App) {
    if app.p2p.as_ref().is_some_and(|p| p.handle.is_some()) {
        return;
    }

    let bootstrap: Vec<Multiaddr> = app
        .config
        .p2p_bootstrap
        .iter()
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let standalone = bootstrap.is_empty();

    let key_path = opacusr_agent::config::config_root().join("p2p_identity.key");
    let keypair = match opacusr_libp2p::keypair::load_or_create(&key_path) {
        Ok(kp) => Some(kp),
        Err(e) => {
            app.status_message = format!("Ключ узла P2P не создан: {e}");
            None
        }
    };

    let cfg = NodeConfig {
        bootstrap,
        listen: vec![
            format!("/ip4/0.0.0.0/udp/{QUIC_PORT}/quic-v1")
                .parse()
                .expect("static multiaddr"),
            format!("/ip4/0.0.0.0/tcp/{TCP_PORT}")
                .parse()
                .expect("static multiaddr"),
        ],
        keypair,
        // Without a bootstrap list the node is its own seed: a Kademlia
        // server offering a relay circuit. Enough for two machines on
        // one LAN — the second machine gets the invite address of the
        // first. With a list: regular client mode.
        kad_server: standalone,
        serve_relay: standalone,
        publish_interval: Duration::from_secs(120),
    };

    match opacusr_libp2p::spawn(cfg) {
        Ok((handle, mut evt_rx)) => {
            let (tx, rx) = std::sync::mpsc::channel();
            app.rt.spawn(async move {
                while let Some(ev) = evt_rx.recv().await {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            });
            let peer = handle.peer_id();
            let invite_addr = crate::theme::lan_ipv4()
                .map(|lan| format!("/ip4/{lan}/udp/{QUIC_PORT}/quic-v1/p2p/{peer}"))
                .unwrap_or_default();
            app.p2p = Some(P2pUi {
                handle: Some(handle),
                peer_id: peer.to_string(),
                invite_addr,
                listen_addrs: Vec::new(),
                reachability: if standalone {
                    "самостоятельный seed (kad-сервер + релей), ждёт партнёров".into()
                } else {
                    "подключается к bootstrap-узлам…".into()
                },
                status: "узел запущен".into(),
                events: Some(rx),
                ping_status: String::new(),
                pending_ping: None,
                punched: false,
            });
            app.status_message = "Узел P2P запущен.".into();
        }
        Err(e) => {
            app.status_message = format!("Узел P2P не запущен: {e}");
        }
    }
}

/// Stop the embedded node and drop the UI state.
pub(crate) fn stop_node(app: &mut App) {
    if let Some(p) = app.p2p.take() {
        if let Some(h) = p.handle {
            app.rt.spawn(async move {
                let _ = h.shutdown().await;
            });
        }
    }
}

/// «Проверить связь»: DHT lookup by PeerId, dial, control ping — the
/// result lines land in `ping_status` via [`poll_events`].
pub(crate) fn ping_clicked(app: &mut App) {
    let Some(p2p) = app.p2p.as_mut() else {
        return;
    };
    let target = app.p2p_ping_input.trim().to_string();
    let peer = match PeerId::from_str(&target) {
        Ok(p) => p,
        Err(_) => {
            p2p.ping_status = "неверный PeerId (ожидается base58 вида 12D3KooW…)".into();
            return;
        }
    };
    let Some(handle) = p2p.handle.clone() else {
        return;
    };
    p2p.pending_ping = Some((peer, Instant::now()));
    p2p.punched = false;
    p2p.ping_status = "поиск в DHT и дозвон…".into();
    app.rt.spawn(async move {
        let _ = handle.connect_by_peer_id(peer).await;
    });
}

/// Drain the event bridge on every frame and update the UI strings.
pub(crate) fn poll_events(app: &mut App, ctx: &egui::Context) {
    let Some(rx) = app.p2p.as_ref().and_then(|p| p.events.as_ref()) else {
        return;
    };
    let mut evs = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        evs.push(ev);
    }
    if evs.is_empty() {
        return;
    }
    ctx.request_repaint();

    let Some(p2p) = app.p2p.as_mut() else {
        return;
    };
    for ev in evs {
        match ev {
            Event::Listening { addr } => {
                let s = addr.to_string();
                if !p2p.listen_addrs.contains(&s) {
                    p2p.listen_addrs.push(s.clone());
                    if p2p.listen_addrs.len() > 8 {
                        p2p.listen_addrs.remove(0);
                    }
                }
                if p2p.invite_addr.is_empty() {
                    if let Some(lan) = crate::theme::lan_ipv4() {
                        let is_quic = s.contains("quic-v1");
                        if is_quic {
                            if let Some(port) = s.split("/udp/").nth(1).and_then(|rest| {
                                rest.split('/').next().and_then(|p| p.parse::<u16>().ok())
                            }) {
                                p2p.invite_addr =
                                    format!("/ip4/{lan}/udp/{port}/quic-v1/p2p/{}", p2p.peer_id);
                            }
                        }
                    }
                }
                p2p.status = format!("слушает {s}");
            }
            Event::RelayReserved { relay } => {
                p2p.status = format!("резервация релея {relay}");
            }
            Event::ReachabilityChanged { public_addr } => {
                p2p.reachability = match public_addr {
                    Some(a) => format!("внешний адрес подтверждён: {a}"),
                    None => "внешний адрес пока не подтверждён".into(),
                };
            }
            Event::AddressesPublished { addrs } => {
                if !addrs.is_empty() {
                    p2p.status = "адреса опубликованы в DHT".into();
                }
            }
            Event::PeerFound { peer, addrs } => {
                p2p.status = format!("найден {peer}, адреса: {addrs:?}");
            }
            Event::LookupFailed { peer } => {
                if p2p.pending_ping.map(|(id, _)| id) == Some(peer) {
                    p2p.pending_ping = None;
                    p2p.ping_status =
                        "не найден в DHT — узел offline, или bootstrap-узлы не совпадают".into();
                }
            }
            Event::Connected { peer } => {
                p2p.status = format!("соединение: {peer}");
                if p2p.pending_ping.map(|(id, _)| id) == Some(peer) {
                    if let Some(h) = p2p.handle.clone() {
                        app.rt.spawn(async move {
                            let _ = h
                                .send_control(
                                    peer,
                                    ControlRequest {
                                        kind: "ping".into(),
                                        payload: vec![9],
                                    },
                                )
                                .await;
                        });
                    }
                }
            }
            Event::HolePunchSucceeded { peer } => {
                p2p.punched = true;
                p2p.status = format!("прямой пробой NAT удался ({peer})");
            }
            Event::ControlResponse { peer, response } => {
                if let Some((_, t0)) = p2p.pending_ping.filter(|(id, _)| *id == peer) {
                    p2p.pending_ping = None;
                    let ms = t0.elapsed().as_millis();
                    p2p.ping_status = if response.ok {
                        format!(
                            "ответ за {ms} мс — {}",
                            if p2p.punched {
                                "прямое соединение (пробой NAT)"
                            } else {
                                "соединение через релей или прямое LAN"
                            }
                        )
                    } else {
                        "узел ответил ошибкой".into()
                    };
                }
            }
            Event::Disconnected { peer } => {
                p2p.status = format!("отсоединился {peer}");
            }
            Event::ControlRequest { .. } => {}
        }
    }

    if let Some((_, t0)) = p2p.pending_ping {
        if t0.elapsed() > PING_TIMEOUT {
            p2p.pending_ping = None;
            p2p.ping_status = "нет ответа за 60 с — узел offline или сеть недоступна".into();
        }
    }
}
