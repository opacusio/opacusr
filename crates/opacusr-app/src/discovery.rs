//! mDNS browser for the viewer: watches `_opacusr._tcp.local.` on a
//! background thread and keeps a deduplicated snapshot of the OpacusR
//! hosts on this network.
//!
//! The thread is best-effort, like the agent's advertiser: no multicast
//! socket or no daemon simply means an empty list. Hosts running a
//! foreign protocol version are filtered out — their entries would only
//! produce handshake failures (see `PROTO_VERSION`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use opacusr_proto::discovery;

/// How long an unrefreshed entry stays listed after the last
/// resolution (mDNS re-announces well inside this window).
const HOST_TTL: Duration = Duration::from_secs(60);

/// One discovered host, as the connect tab shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredHost {
    /// Full mDNS name (dedup key), e.g. `officepc._opacusr._tcp.local.`.
    pub fullname: String,
    /// Human label: the instance part of `fullname`.
    pub label: String,
    /// Signaling endpoint: `ws://<ip>:<port><path>`.
    pub signal_url: String,
    /// Direct-LAN target `ip:port` when the host accepts LAN sessions.
    pub lan_target: Option<String>,
}

/// Background browser with a polled snapshot. Kept alive by the app for
/// the whole session; dropping it stops the daemon.
pub struct Discovery {
    hosts: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>>,
    _daemon: Option<ServiceDaemon>,
}

impl Discovery {
    /// Start browsing (non-blocking, best-effort).
    pub fn start() -> Self {
        let hosts: Arc<Mutex<HashMap<String, (DiscoveredHost, Instant)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(target: "opacusr", "mDNS discovery unavailable: {e}");
                return Self {
                    hosts,
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
        Self {
            hosts,
            _daemon: Some(daemon),
        }
    }

    /// Current host list: deduped by fullname, stale entries dropped,
    /// sorted case-insensitively by label.
    pub fn snapshot(&self) -> Vec<DiscoveredHost> {
        let mut hosts = self.hosts.lock().unwrap();
        hosts.retain(|_, (_, seen)| seen.elapsed() < HOST_TTL);
        let mut out: Vec<DiscoveredHost> = hosts.values().map(|(h, _)| h.clone()).collect();
        out.sort_by_key(|h| h.label.to_lowercase());
        out
    }
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
        signal_url: discovery::signal_url(ip, port, &txt.path),
        lan_target,
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
        let props = discovery::txt_properties("/signal", Some(54000));
        let host = host_from_parts(
            &["192.168.1.7".parse().unwrap()],
            8080,
            "officepc._opacusr._tcp.local.",
            &props,
        )
        .expect("valid host");
        assert_eq!(host.label, "officepc");
        assert_eq!(host.signal_url, "ws://192.168.1.7:8080/signal");
        assert_eq!(host.lan_target.as_deref(), Some("192.168.1.7:54000"));

        // Without the `lan` record there is no direct target.
        let props = discovery::txt_properties("/signal", None);
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
        let mut props = discovery::txt_properties("/signal", None);
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
}
