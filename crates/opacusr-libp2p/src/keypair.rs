//! Persistent node identity for the decentralized mode.
//!
//! A stable key means a stable `PeerId` — the address partners dial by.
//! The file holds the libp2p protobuf encoding of the keypair; it is
//! created (mode 0600 where the platform honors it) on first use.

use std::io;
use std::path::Path;

use libp2p::identity::Keypair;

/// Load the node identity from `path`, creating and persisting a fresh
/// ed25519 keypair when the file does not exist (or is unreadable).
pub fn load_or_create(path: &Path) -> io::Result<Keypair> {
    if let Ok(bytes) = std::fs::read(path) {
        match Keypair::from_protobuf_encoding(&bytes) {
            Ok(kp) => return Ok(kp),
            // Fall through and regenerate: a corrupt identity file must
            // not wedge the node (the PeerId changes, partners re-add it).
            Err(e) => tracing::debug!(path = %path.display(), %e, "keypair decode failed"),
        }
    }

    let kp = Keypair::generate_ed25519();
    let bytes = kp
        .to_protobuf_encoding()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(kp)
}

/// Best-effort local IPv4 address of this machine. Primary pick: the
/// route the OS would take towards the internet (connected UDP socket,
/// no packets sent); if that lands on a habitual virtual adapter (WSL2 /
/// Hyper-V, 172.16/12) while a plausible LAN address exists, the LAN
/// candidate wins instead (see `opacusr_proto::netpick`). `None` without
/// any IPv4 route.
pub fn lan_ipv4() -> Option<String> {
    let outbound = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|sock| sock.connect("8.8.8.8:80").map(|_| sock).ok())
        .and_then(|sock| match sock.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(v4) => Some(v4),
            std::net::IpAddr::V6(_) => None,
        });
    let candidates = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|ifc| match ifc.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4.ip),
            _ => None,
        })
        .collect::<Vec<_>>();
    opacusr_proto::netpick::pick_lan_ipv4(outbound, &candidates).map(|ip| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_persists_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("opacusr-libp2p-test-{}", std::process::id()));
        let path = dir.join("identity.key");
        let _ = std::fs::remove_dir_all(&dir);

        let first = load_or_create(&path).expect("create");
        let second = load_or_create(&path).expect("reload");
        assert_eq!(
            first.public().to_peer_id(),
            second.public().to_peer_id(),
            "the same file must yield the same PeerId"
        );

        std::fs::write(&path, b"garbage").unwrap();
        let third = load_or_create(&path).expect("regenerate on corrupt file");
        assert_ne!(
            second.public().to_peer_id(),
            third.public().to_peer_id(),
            "a corrupt identity file regenerates a new key"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lan_ipv4_is_either_none_or_a_parseable_ipv4() {
        if let Some(ip) = lan_ipv4() {
            assert!(ip.parse::<std::net::Ipv4Addr>().is_ok());
        }
    }
}
