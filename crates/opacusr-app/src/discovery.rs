//! LAN discovery for the viewer: mDNS browsing **plus** a UDP-broadcast
//! beacon listener (see `opacusr_proto::beacon`), both feeding one
//! deduplicated snapshot of the OpacusR hosts on this network.
//!
//! Two channels on purpose: mDNS rides on IP multicast, which is exactly
//! what cheap Wi-Fi APs filter and what Windows Firewall blocks for
//! unknown executables; plain subnet broadcast survives both. Either
//! channel alone is enough — the snapshot merges whatever arrives.
//!
//! Both threads are best-effort, like the agent's advertisers: no
//! multicast socket or no daemon simply means an empty list. Hosts
//! running a foreign protocol version are filtered out — their entries
//! would only produce handshake failures (see `PROTO_VERSION`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use opacusr_proto::beacon;
use opacusr_proto::discovery;

/// How long an unrefreshed entry stays listed after the last
/// resolution (mDNS re-announces well inside this window).
const HOST_TTL: Duration = Duration::from_secs(60);

/// One discovered host, as the connect tab shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredHost {
    /// Full mDNS name (dedup key), e.g. `officepc._opacusr._tcp.local.`;
    /// beacon entries use a synthetic `beacon://<ip>/<id>` key.
    pub fullname: String,
    /// Human label: the instance part of `fullname`, or the beacon's
    /// display name.
    pub label: String,
    /// Signaling endpoint: `ws://<ip>:<port><path>`; `None` when the
    /// host runs no local signaling server (a beacon host with
    /// `signal_port = 0` — only its direct-LAN target is usable).
    pub signal_url: Option<String>,
    /// Direct-LAN target `ip:port` when the host accepts LAN sessions.
    pub lan_target: Option<String>,
    /// The host's assigned 9-digit ID when known — from the mDNS `id`
    /// TXT record or the beacon payload. The connect ladder uses it to
    /// resolve «connect by ID» straight onto this host's endpoints.
    pub peer_id: Option<u64>,
}

/// Background browser with a polled snapshot. Kept alive by the app for
/// the whole session; dropping it stops the daemon.
pub struct Discovery {
    hosts: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>>,
    /// This app's own ID: beacons carrying it are the machine's own
    /// agent looped back over broadcast — skip them so the host never
    /// lists itself. Written by the UI when the ID becomes known.
    pub own_id: Arc<AtomicU64>,
    _daemon: Option<ServiceDaemon>,
}

impl Discovery {
    /// Start browsing (non-blocking, best-effort).
    pub fn start() -> Self {
        let hosts: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let own_id = Arc::new(AtomicU64::new(0));
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(target: "opacusr", "mDNS discovery unavailable: {e}");
                start_beacon_listener(hosts.clone(), own_id.clone());
                return Self {
                    hosts,
                    own_id,
                    _daemon: None,
                };
            }
        };
        match daemon.browse(discovery::SERVICE_TYPE) {
            Ok(rx) => {
                let store = hosts.clone();
                let spawned = std::thread::Builder::new()
                    .name("opacusr-mdns".into())
                    .spawn(move || browse_loop(rx, store));
                if spawned.is_err() {
                    tracing::debug!(target: "opacusr", "mDNS discovery thread failed to spawn");
                }
            }
            Err(e) => {
                tracing::debug!(target: "opacusr", "mDNS browse failed: {e}");
            }
        }
        start_beacon_listener(hosts.clone(), own_id.clone());
        Self {
            hosts,
            own_id,
            _daemon: Some(daemon),
        }
    }

    /// Current host list: deduped by fullname, stale entries dropped,
    /// sorted case-insensitively by label.
    pub fn snapshot(&self) -> Vec<DiscoveredHost> {
        let mut hosts = self.hosts.lock().unwrap();
        hosts.retain(|key, (_, seen)| {
            if key.starts_with("beacon://") {
                seen.elapsed() < beacon::BEACON_TTL
            } else {
                seen.elapsed() < HOST_TTL
            }
        });
        let mut out: Vec<DiscoveredHost> = hosts.values().map(|(h, _)| h.clone()).collect();
        out.sort_by_key(|h| h.label.to_lowercase());
        out
    }
}

/// Bind the UDP beacon listener and spawn its thread. Best-effort: a
/// bound socket on the shared discovery port may fail (another OpacusR
/// instance already holds it — with SO_REUSEADDR this is rare; on some
/// Android builds the port is restricted), and losing one channel never
/// blocks the other.
fn start_beacon_listener(
    store: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>>,
    own_id: Arc<AtomicU64>,
) {
    let spawned = std::thread::Builder::new()
        .name("opacusr-beacon".into())
        .spawn(move || {
            let socket = std::net::UdpSocket::bind(("0.0.0.0", beacon::BEACON_PORT));
            let socket = match socket {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(target: "opacusr",
                        "beacon listener unavailable on udp {}: {e}",
                        beacon::BEACON_PORT);
                    return;
                }
            };
            let mut buf = [0u8; 256];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((n, src)) => {
                        if let Some(host) = beacon_host(&buf[..n], src.ip()) {
                            // The machine's own agent looped back over
                            // broadcast — never list ourselves.
                            if host.peer_id != Some(own_id.load(Ordering::Relaxed)) {
                                store
                                    .lock()
                                    .unwrap()
                                    .insert(host.fullname.clone(), (host, Instant::now()));
                            }
                        }
                    }
                    Err(_) => return,
                }
            }
        });
    if spawned.is_err() {
        tracing::debug!(target: "opacusr", "beacon listener thread failed to spawn");
    }
}

/// Build a [`DiscoveredHost`] from a received beacon packet. `None` for
/// foreign-protocol or unregistered senders.
fn beacon_host(buf: &[u8], src: IpAddr) -> Option<DiscoveredHost> {
    let b = beacon::parse(buf)?;
    if b.proto != opacusr_proto::PROTO_VERSION || b.peer_id == 0 {
        return None;
    }
    let ip_str = host_str(src);
    let label = if b.name.trim().is_empty() {
        b.peer_id.to_string()
    } else {
        b.name.trim().to_string()
    };
    let host = DiscoveredHost {
        fullname: format!("beacon://{ip_str}/{}", b.peer_id),
        label,
        signal_url: (b.signal_port != 0)
            .then(|| discovery::signal_url(src, b.signal_port, "/signal")),
        lan_target: (b.lan_port != 0).then(|| format!("{ip_str}:{}", b.lan_port)),
        peer_id: Some(b.peer_id),
    };
    // A beacon with neither endpoint usable carries no resolution at all.
    if host.signal_url.is_none() && host.lan_target.is_none() {
        return None;
    }
    Some(host)
}

/// Long-lived browse loop: resolve events refresh entries, removal
/// events drop them. Blocking `recv()` is fine — this is a dedicated
/// thread; an error means the daemon is gone and the loop ends.
fn browse_loop(
    rx: mdns_sd::Receiver<ServiceEvent>,
    store: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>>,
) {
    loop {
        match rx.recv() {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if let Some(host) = host_from_info(&info) {
                    store
                        .lock()
                        .unwrap()
                        .insert(host.fullname.clone(), (host, Instant::now()));
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                store.lock().unwrap().remove(&fullname);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

/// Build a [`DiscoveredHost`] from a resolved service. `None` when the
/// advertisement is not a usable OpacusR endpoint: no usable address,
/// no/foreign protocol version, malformed TXT.
fn host_from_info(info: &ServiceInfo) -> Option<DiscoveredHost> {
    let props: Vec<(String, String)> = info
        .get_properties()
        .iter()
        .map(|p| (p.key().to_string(), p.val_str().to_string()))
        .collect();
    let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
    host_from_parts(&addrs, info.get_port(), info.get_fullname(), &props)
}

/// Pure builder shared with the tests: address preference, TXT parsing
/// and the protocol filter all live here.
fn host_from_parts(
    addrs: &[IpAddr],
    port: u16,
    fullname: &str,
    props: &[(String, String)],
) -> Option<DiscoveredHost> {
    let txt = discovery::parse_txt(props)?;
    // Foreign protocol versions would fail the handshake anyway — do
    // not offer them in the UI.
    if txt.proto != opacusr_proto::PROTO_VERSION {
        return None;
    }
    let ip = pick_addr(addrs)?;
    let label = fullname.split('.').next().unwrap_or(fullname).to_string();
    if label.is_empty() {
        return None;
    }
    let lan_target = txt.lan_port.map(|p| format!("{}:{p}", host_str(ip)));
    Some(DiscoveredHost {
        fullname: fullname.to_string(),
        label,
        signal_url: Some(discovery::signal_url(ip, port, &txt.path)),
        lan_target,
        peer_id: txt.peer_id,
    })
}

/// IP inside an `ip:port` target: bracket IPv6.
fn host_str(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Address preference for connecting: a normal (non-loopback,
/// non-link-local) IPv4 first — mDNS is a LAN feature — then a usable
/// IPv6, then whatever is left.
fn pick_addr(addrs: &[IpAddr]) -> Option<IpAddr> {
    let usable_v4 = |ip: &IpAddr| {
        matches!(
            ip,
            IpAddr::V4(v) if !v.is_loopback() && !v.is_unspecified() && !v.is_link_local()
        )
    };
    let usable_v6 = |ip: &IpAddr| {
        matches!(
            ip,
            IpAddr::V6(v) if !v.is_loopback() && !v.is_unspecified() && !v.is_unicast_link_local()
        )
    };
    addrs
        .iter()
        .find(|ip| usable_v4(ip))
        .or_else(|| addrs.iter().find(|ip| usable_v6(ip)))
        .or_else(|| addrs.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_addr_prefers_global_ipv4() {
        let v4 = "192.168.1.7".parse().unwrap();
        let v6 = "fe80::1".parse().unwrap();
        // Loopback + link-local + good v4 → the good v4 wins.
        let addrs = ["127.0.0.1".parse().unwrap(), v4, v6];
        assert_eq!(pick_addr(&addrs), Some(v4));
        // No usable v4 → usable v6.
        let addrs = [
            "127.0.0.1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "fd00::5".parse().unwrap(),
        ];
        assert_eq!(pick_addr(&addrs), Some("fd00::5".parse().unwrap()));
        // Only loopback → loopback (better than nothing).
        assert_eq!(
            pick_addr(&["127.0.0.1".parse().unwrap()]),
            Some("127.0.0.1".parse().unwrap())
        );
        // Nothing at all.
        assert_eq!(pick_addr(&[]), None);
    }

    #[test]
    fn host_from_parts_builds_url_label_and_lan_target() {
        let props = discovery::txt_properties("/signal", Some(54000), Some(691721284));
        let host = host_from_parts(
            &["192.168.1.7".parse().unwrap()],
            8080,
            "officepc._opacusr._tcp.local.",
            &props,
        )
        .expect("valid host");
        assert_eq!(host.label, "officepc");
        assert_eq!(
            host.signal_url.as_deref(),
            Some("ws://192.168.1.7:8080/signal")
        );
        assert_eq!(host.lan_target.as_deref(), Some("192.168.1.7:54000"));
        assert_eq!(host.peer_id, Some(691721284));

        // Without the `lan` record there is no direct target.
        let props = discovery::txt_properties("/signal", None, None);
        let host = host_from_parts(
            &["192.168.1.7".parse().unwrap()],
            8080,
            "officepc._opacusr._tcp.local.",
            &props,
        )
        .unwrap();
        assert_eq!(host.lan_target, None);

        // No usable address → not listed.
        assert!(host_from_parts(&[], 8080, "officepc._opacusr._tcp.local.", &props).is_none());
    }

    #[test]
    fn foreign_proto_is_filtered() {
        let mut props = discovery::txt_properties("/signal", None, None);
        // Rewrite the proto record to a foreign version.
        for p in props.iter_mut() {
            if p.0 == discovery::TXT_PROTO {
                p.1 = "1".into();
            }
        }
        assert!(host_from_parts(
            &["192.168.1.7".parse().unwrap()],
            8080,
            "old-host._opacusr._tcp.local.",
            &props
        )
        .is_none());

        // A payload without a proto record at all is equally useless.
        let no_proto: Vec<(String, String)> = props
            .into_iter()
            .filter(|(k, _)| k != discovery::TXT_PROTO)
            .collect();
        assert!(host_from_parts(
            &["192.168.1.7".parse().unwrap()],
            8080,
            "weird._opacusr._tcp.local.",
            &no_proto
        )
        .is_none());
    }

    #[test]
    fn beacon_packet_becomes_host_entry() {
        let ip: IpAddr = "192.168.2.25".parse().unwrap();
        let pkt = beacon::encode(&beacon::Beacon {
            proto: opacusr_proto::PROTO_VERSION,
            peer_id: 950_239_493,
            name: "Телефон".into(),
            signal_port: 8080,
            lan_port: 54000,
        });
        let host = beacon_host(&pkt, ip).expect("valid beacon");
        assert_eq!(host.fullname, "beacon://192.168.2.25/950239493");
        assert_eq!(host.label, "Телефон");
        assert_eq!(
            host.signal_url.as_deref(),
            Some("ws://192.168.2.25:8080/signal")
        );
        assert_eq!(host.lan_target.as_deref(), Some("192.168.2.25:54000"));
        assert_eq!(host.peer_id, Some(950_239_493));

        // No ports at all → nothing usable, not listed.
        let pkt = beacon::encode(&beacon::Beacon {
            proto: opacusr_proto::PROTO_VERSION,
            peer_id: 950_239_493,
            name: "x".into(),
            signal_port: 0,
            lan_port: 0,
        });
        assert!(beacon_host(&pkt, ip).is_none());

        // Foreign protocol version → filtered.
        let pkt = beacon::encode(&beacon::Beacon {
            proto: opacusr_proto::PROTO_VERSION - 1,
            peer_id: 950_239_493,
            name: "x".into(),
            signal_port: 8080,
            lan_port: 0,
        });
        assert!(beacon_host(&pkt, ip).is_none());

        // Unregistered sender (id = 0) → filtered.
        let pkt = beacon::encode(&beacon::Beacon {
            proto: opacusr_proto::PROTO_VERSION,
            peer_id: 0,
            name: "x".into(),
            signal_port: 8080,
            lan_port: 0,
        });
        assert!(beacon_host(&pkt, ip).is_none());
    }
}
