//! Best-effort «какой у меня LAN IPv4» picker, shared by the GUI invite
//! card and the decentralized node's invite address.
//!
//! A connected UDP socket resolves the outbound interface from the
//! routing table WITHOUT sending a packet — usually exactly the address
//! LAN peers can dial. On Windows, though, virtual switches win that
//! race depressingly often: WSL2 / Hyper-V NAT adapters habitually live
//! in 172.16/12, and the v0.11.24 field report shows the invite card and
//! the signaling URL built from a WSL adapter (`ws://172.22.144.1:8080`)
//! — an address nothing else on the Wi-Fi can reach.
//!
//! Rule: trust the outbound pick UNLESS it sits in the habitual virtual
//! range AND a plausible physical-LAN candidate (192.168/16 or 10/8)
//! exists among the interface addresses. Pure function — the enumeration
//! and the UDP trick live with the callers.

use std::net::Ipv4Addr;

/// The range home to WSL2 / Hyper-V NAT and Docker default bridges.
/// Legit LANs inside 172.16/12 still work: when this range is the ONLY
/// usable answer, it is returned.
pub fn is_habitual_virtual_range(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 172 && (16..=31).contains(&o[1])
}

/// Plausibility rank for LAN peers: home Wi-Fi (192.168/16) first, then
/// corporate 10/8, everything else (incl. the virtual range, public
/// addresses) last.
fn rank(ip: Ipv4Addr) -> u8 {
    let o = ip.octets();
    if o[0] == 192 && o[1] == 168 {
        0
    } else if o[0] == 10 {
        1
    } else {
        2
    }
}

fn usable(ip: Ipv4Addr) -> bool {
    !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified())
}

/// Pick the LAN IPv4 to advertise. `outbound` — the UDP-connect default
/// route pick (may be `None`); `candidates` — every interface IPv4
/// (raw, may contain junk: loopback, link-local, virtual adapters).
pub fn pick_lan_ipv4(outbound: Option<Ipv4Addr>, candidates: &[Ipv4Addr]) -> Option<Ipv4Addr> {
    let ok_out = outbound.filter(|ip| usable(*ip));
    if let Some(out) = ok_out {
        if !is_habitual_virtual_range(out) {
            return Some(out);
        }
    }
    // The outbound pick was virtual (or unusable): rank all usable
    // candidates, keeping the outbound in the pool so a machine with
    // ONLY virtual adapters still gets an answer.
    let mut pool: Vec<Ipv4Addr> = candidates
        .iter()
        .copied()
        .filter(|ip| usable(*ip))
        .collect();
    if let Some(out) = ok_out {
        if !pool.contains(&out) {
            pool.push(out);
        }
    }
    pool.into_iter().min_by_key(|ip| (rank(*ip), *ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn outbound_wins_when_plausible() {
        // Normal home network: the routing-table pick is right even with
        // virtual adapters around.
        assert_eq!(
            pick_lan_ipv4(Some(v("192.168.1.4")), &[v("172.22.144.1"), v("127.0.0.1")]),
            Some(v("192.168.1.4"))
        );
        // Corporate default route beats a LAN candidate.
        assert_eq!(
            pick_lan_ipv4(Some(v("10.8.0.2")), &[v("192.168.1.4")]),
            Some(v("10.8.0.2"))
        );
    }

    #[test]
    fn wsl_outbound_is_replaced_by_a_lan_candidate() {
        // THE v0.11.24 field case: WSL adapter wins the default route.
        assert_eq!(
            pick_lan_ipv4(
                Some(v("172.22.144.1")),
                &[v("172.22.144.1"), v("192.168.100.7")]
            ),
            Some(v("192.168.100.7"))
        );
    }

    #[test]
    fn virtual_range_kept_when_nothing_better_exists() {
        // Only WSL present: better an un-dialable-ish address than none.
        assert_eq!(
            pick_lan_ipv4(Some(v("172.22.144.1")), &[v("127.0.0.1")]),
            Some(v("172.22.144.1"))
        );
        // No outbound at all: rank candidates (192.168 beats 172.16/12).
        assert_eq!(
            pick_lan_ipv4(
                None,
                &[
                    v("169.254.3.4"),
                    v("127.0.0.1"),
                    v("172.20.0.1"),
                    v("192.168.0.2")
                ]
            ),
            Some(v("192.168.0.2"))
        );
        assert_eq!(pick_lan_ipv4(None, &[]), None);
    }
}
