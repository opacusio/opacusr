//! Minimal CLI around `opacusr-libp2p` for field tests.
//!
//! Run a seed/relay node on a machine reachable from the internet:
//!
//! ```text
//! p2p-node --seed --key /var/lib/opacusr/p2p.key
//! ```
//!
//! …or dial another node by `PeerId` and ping it (the phase-2 check of
//! the DHT + AutoNAT + Relay + DCUtR ladder on two real networks):
//!
//! ```text
//! p2p-node --bootstrap /ip4/HOST/udp/4001/quic-v1/p2p/SEED_PEER_ID \
//!          --ping TARGET_PEER_ID
//! ```
//!
//! Without `--ping` the client node just runs and reports its addresses
//! (useful on the second machine of a LAN test).

use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};

use opacusr_libp2p::keypair::{lan_ipv4, load_or_create};
use opacusr_libp2p::{ControlRequest, Event, Multiaddr, NodeConfig, PeerId};

const PING_DEADLINE: Duration = Duration::from_secs(75);

struct Args {
    seed: bool,
    key: String,
    listen: Vec<Multiaddr>,
    bootstrap: Vec<Multiaddr>,
    ping: Option<PeerId>,
}

fn usage() -> &'static str {
    "usage:
  p2p-node --seed [--key <file>] [--listen <multiaddr>]...
  p2p-node [--key <file>] [--listen <multiaddr>]...
           [--bootstrap <multiaddr>]... [--ping <peer-id>]

--listen defaults to /ip4/0.0.0.0/udp/4001/quic-v1 + /ip4/0.0.0.0/tcp/4001"
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        seed: false,
        key: "p2p-node.key".to_string(),
        listen: Vec::new(),
        bootstrap: Vec::new(),
        ping: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take_value = |what: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{what} needs a value"))
        };
        match flag.as_str() {
            "--seed" => a.seed = true,
            "--key" => a.key = take_value("--key")?,
            "--listen" => {
                let v = take_value("--listen")?;
                a.listen
                    .push(v.parse().map_err(|e| format!("bad multiaddr {v:?}: {e}"))?);
            }
            "--bootstrap" => {
                let v = take_value("--bootstrap")?;
                a.bootstrap
                    .push(v.parse().map_err(|e| format!("bad multiaddr {v:?}: {e}"))?);
            }
            "--ping" => {
                let v = take_value("--ping")?;
                a.ping = Some(PeerId::from_str(&v).map_err(|e| format!("bad peer id {v:?}: {e}"))?);
            }
            "--help" | "-h" => return Err(usage().to_string()),
            other => return Err(format!("unknown flag {other:?}\n{}", usage())),
        }
    }
    if a.ping.is_some() && a.bootstrap.is_empty() {
        return Err("--ping needs at least one --bootstrap (the DHT entry point)".to_string());
    }
    if a.listen.is_empty() {
        a.listen = ["/ip4/0.0.0.0/udp/4001/quic-v1", "/ip4/0.0.0.0/tcp/4001"]
            .iter()
            .map(|s| s.parse().expect("static multiaddr"))
            .collect();
    }
    Ok(a)
}

/// The multiaddr another node should put into its bootstrap list: the
/// LAN-reachable QUIC listener plus our PeerId.
fn invite_line(listen_event: &Multiaddr, peer: PeerId) -> Option<String> {
    let lan = lan_ipv4()?;
    let port = listen_event.iter().find_map(|p| match p {
        opacusr_libp2p::Protocol::Udp(port) => Some(port),
        _ => None,
    })?;
    let is_quic = listen_event
        .iter()
        .any(|p| matches!(p, opacusr_libp2p::Protocol::QuicV1));
    if !is_quic {
        return None;
    }
    Some(format!("/ip4/{lan}/udp/{port}/quic-v1/p2p/{peer}"))
}

fn report(ev: &Event) {
    match ev {
        Event::Listening { addr } => println!("listening on {addr}"),
        Event::RelayReserved { relay } => println!("relay reservation from {relay}"),
        Event::ReachabilityChanged { public_addr } => {
            println!("externally reachable at {public_addr:?}")
        }
        Event::AddressesPublished { addrs } => {
            if !addrs.is_empty() {
                println!("published to the DHT: {addrs:?}")
            }
        }
        Event::Connected { peer } => println!("connected: {peer}"),
        Event::Disconnected { peer } => println!("disconnected: {peer}"),
        Event::ControlRequest { peer, request } => {
            println!("control request from {peer}: {request:?}")
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let keypair = match load_or_create(std::path::Path::new(&args.key)) {
        Ok(kp) => kp,
        Err(e) => {
            eprintln!("cannot load key {:?}: {e}", args.key);
            return ExitCode::from(2);
        }
    };

    let cfg = NodeConfig {
        bootstrap: args.bootstrap.clone(),
        listen: args.listen.clone(),
        keypair: Some(keypair),
        // A seed is a Kademlia server + relay; clients stay client-mode.
        kad_server: args.seed,
        serve_relay: args.seed,
        publish_interval: Duration::from_secs(120),
    };

    let (handle, mut events) = match opacusr_libp2p::spawn(cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("node spawn failed: {e}");
            return ExitCode::from(2);
        }
    };
    let peer = handle.peer_id();
    println!("PeerId: {peer}");
    println!(
        "mode:   {}",
        if args.seed {
            "seed (kad server + relay)"
        } else {
            "client"
        }
    );

    if args.seed {
        seed_loop(peer, &mut events).await
    } else {
        client_loop(handle, peer, &mut events, args).await
    }
}

/// Seed mode: run until Ctrl+C, print notable events including the
/// bootstrap address other nodes should paste into their lists.
async fn seed_loop(peer: PeerId, events: &mut tokio::sync::mpsc::Receiver<Event>) -> ExitCode {
    let mut invited = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("shutdown by signal");
                return ExitCode::SUCCESS;
            }
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                if let Event::Listening { addr } = &ev {
                    if !invited {
                        if let Some(line) = invite_line(addr, peer) {
                            println!("bootstrap address for other nodes: {line}");
                            invited = true;
                        }
                    }
                }
                report(&ev);
            }
        }
    }
    ExitCode::SUCCESS
}

/// Client mode: dial, optionally ping by PeerId, report, exit.
async fn client_loop(
    handle: opacusr_libp2p::Handle,
    peer: PeerId,
    events: &mut tokio::sync::mpsc::Receiver<Event>,
    args: Args,
) -> ExitCode {
    let Some(target) = args.ping else {
        // No ping: keep running and report, like the seed (minus serving).
        println!("running; Ctrl+C to stop");
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("shutdown by signal");
                    let _ = handle.shutdown().await;
                    let _ = peer;
                    return ExitCode::SUCCESS;
                }
                ev = events.recv() => {
                    let Some(ev) = ev else { break };
                    report(&ev);
                }
            }
        }
        return ExitCode::SUCCESS;
    };

    let deadline = Instant::now() + PING_DEADLINE;
    if let Err(e) = handle.connect_by_peer_id(target).await {
        eprintln!("cannot request connect: {e}");
        return ExitCode::from(1);
    }

    let mut punched = false;
    let mut connected = false;
    while Instant::now() < deadline {
        tokio::select! {
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => break,
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match &ev {
                    Event::PeerFound { addrs, .. } => {
                        println!("peer found in DHT, addresses: {addrs:?}");
                    }
                    Event::Connected { peer } if *peer == target => {
                        connected = true;
                        println!("connected to {peer}");
                        let _ = handle
                            .send_control(
                                target,
                                ControlRequest {
                                    kind: "ping".into(),
                                    payload: vec![9],
                                },
                            )
                            .await;
                    }
                    Event::HolePunchSucceeded { peer } if *peer == target => {
                        punched = true;
                        println!("hole punch succeeded — the link upgraded to a direct one");
                    }
                    Event::LookupFailed { peer } if *peer == target => {
                        // Not fatal: the node re-issues the lookup when a
                        // new connection comes up, and the record may
                        // appear at any moment (the target publishes right
                        // after its own kad bootstrap).
                        println!("lookup failed for {peer} — retrying while connected");
                    }
                    Event::ControlResponse { peer, response } if *peer == target => {
                        let ok = response.ok;
                        println!("pong ok={ok} punched={punched} connected={connected}");
                        let _ = handle.shutdown().await;
                        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
                    }
                    _ => report(&ev),
                }
            }
        }
    }
    eprintln!("deadline: connected={connected} punched={punched} — no pong");
    let _ = handle.shutdown().await;
    ExitCode::from(1)
}
