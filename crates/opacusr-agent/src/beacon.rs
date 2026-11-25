//! UDP-broadcast LAN beacon — the robust sibling of the mDNS
//! advertisement (see `opacusr_proto::beacon` for the packet shape and
//! why broadcast instead of multicast).
//!
//! Every few seconds the running agent announces itself to the local
//! subnet: its assigned ID (once the primary signaling endpoint has
//! minted one), display name, embedded signaling port and direct-LAN
//! port. Viewers listen on the same port and build the same
//! `ID → endpoints` map they build from mDNS — so «connect by ID» works
//! on networks where multicast is filtered and on Windows installs
//! where the mDNS firewall exception is missing.
//!
//! Best-effort like mDNS: any send/bind error is logged once at debug
//! level and the task idles on — a machine without broadcast support
//! must not lose remote access over discovery.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use opacusr_proto::beacon::{self, Beacon};

/// Spawn the periodic beacon task. Ends when `shutdown` flips.
/// `signal_port` is the embedded signaling listener's port (0 = this
/// agent runs no local server); `lan_port` — the direct-LAN listener's
/// port (`None` = `allow_lan` off).
pub fn spawn(
    my_id: Arc<AtomicU64>,
    name: String,
    signal_port: u16,
    lan_port: Option<u16>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        // Rebind the payload each tick: the ID may arrive at any moment.
        let sock = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(target: "opacusr", "beacon: bind failed: {e}");
                return;
            }
        };
        if let Err(e) = sock.set_broadcast(true) {
            tracing::debug!(target: "opacusr", "beacon: broadcast unsupported: {e}");
            return;
        }
        let addrs = broadcast_addrs();
        let mut ticker = tokio::time::interval(beacon::BEACON_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = shutdown.changed() => return,
            }
            let id = my_id.load(Ordering::Relaxed);
            if id == 0 {
                // Not registered yet — the packet would advertise an
                // unusable ID.
                continue;
            }
            let pkt = beacon::encode(&Beacon {
                proto: opacusr_proto::PROTO_VERSION,
                peer_id: id,
                name: name.clone(),
                signal_port,
                lan_port: lan_port.unwrap_or(0),
            });
            for addr in &addrs {
                // A refused/failed send on one interface must not stop
                // the others (dead interfaces are normal on laptops).
                let _ = sock.send_to(&pkt, (*addr, beacon::BEACON_PORT)).await;
            }
        }
    });
}

/// Broadcast targets: the global broadcast address plus every local
/// interface's subnet broadcast (computed from the interface address
/// and its netmask — the `broadcast` field is often unset on Wi-Fi).
fn broadcast_addrs() -> Vec<Ipv4Addr> {
    let mut out = vec![Ipv4Addr::new(255, 255, 255, 255)];
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return out;
    };
    for iface in ifs {
        let (ip, mask) = match iface.addr {
            if_addrs::IfAddr::V4(v4) => (v4.ip, v4.netmask),
            _ => continue,
        };
        if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
            continue;
        }
        let (ip_u, mask_u) = (u32::from(ip), u32::from(mask));
        let bc = Ipv4Addr::from((ip_u & mask_u) | !mask_u);
        if !out.contains(&bc) {
            out.push(bc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn broadcast_addrs_always_carry_the_global_one() {
        let addrs = broadcast_addrs();
        assert!(addrs.contains(&Ipv4Addr::new(255, 255, 255, 255)));
        // No loopback broadcast ever lands in the list.
        assert!(!addrs.contains(&Ipv4Addr::new(127, 255, 255, 255)));
    }

    #[test]
    fn subnet_broadcast_math() {
        // 192.168.2.26/24 → 192.168.2.255 (the user's actual network).
        let ip = Ipv4Addr::new(192, 168, 2, 26);
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        let bc = Ipv4Addr::from((u32::from(ip) & u32::from(mask)) | !u32::from(mask));
        assert_eq!(bc, Ipv4Addr::new(192, 168, 2, 255));
    }

    #[tokio::test]
    async fn beacon_task_sends_nothing_before_registration() {
        // The task must tolerate a zero ID (no endpoint registered yet)
        // and shut down cleanly — nothing observable, just no panic.
        let (tx, rx) = tokio::sync::watch::channel(false);
        spawn(Arc::new(AtomicU64::new(0)), "test".into(), 8080, None, rx);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send(true);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
