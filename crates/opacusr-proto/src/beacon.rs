//! Zero-infra LAN discovery over UDP subnet broadcast — the robust
//! sibling of the mDNS advertisement.
//!
//! mDNS rides on IP multicast, which is exactly the traffic class that
//! cheap Wi-Fi APs rate-limit or drop and that Windows Firewall blocks
//! for unknown executables. Plain subnet broadcast survives both: APs
//! forward it inside the cell, and an MSI-installed OpacusR already
//! carries program-scoped inbound firewall exceptions. Every agent (and
//! every app that hosts) sends a small fixed-shape packet once every
//! few seconds; viewers listen and learn `ID → endpoints` mappings the
//! same way they do from mDNS.
//!
//! Packet shape (little-endian, no allocation on parse):
//!
//! ```text
//! magic      4 bytes  "OPCB"
//! version    1 byte   packet format version (1)
//! proto      4 bytes  PROTO_VERSION
//! peer_id    8 bytes  assigned 9-digit ID (0 = not registered yet)
//! name_len   1 byte   display name length (0..=60)
//! name       n bytes  UTF-8 display name
//! signal     2 bytes  embedded signaling port (0 = none)
//! lan        2 bytes  direct-LAN TCP port (0 = none)
//! ```
//!
//! Only the pure pieces live here (encode/parse/constants) so both the
//! sender (agent) and the receiver (GUI) share one byte-shape, testable
//! without a network.

/// UDP port the beacons are sent to and listened on.
pub const BEACON_PORT: u16 = 54001;

/// How often a running host re-announces itself.
pub const BEACON_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// How long a viewer keeps an unheard beacon alive.
pub const BEACON_TTL: std::time::Duration = std::time::Duration::from_secs(15);

/// Packet format magic ("OpacusR Connection Beacon").
pub const MAGIC: &[u8; 4] = b"OPCB";

/// Packet format version this build encodes and accepts.
pub const PACKET_VERSION: u8 = 1;

/// Longest display name carried in one packet (mirrors the mDNS
/// instance-name cap).
const MAX_NAME_LEN: usize = 60;

/// One decoded beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    /// The sender's [`crate::PROTO_VERSION`]; receivers drop foreign
    /// versions exactly like foreign mDNS TXT records.
    pub proto: u32,
    /// The sender's assigned ID; `0` = not registered yet (skip).
    pub peer_id: crate::PeerId,
    /// The sender's display name (already truncated by the encoder).
    pub name: String,
    /// Embedded signaling port; `0` = this host runs no local server.
    pub signal_port: u16,
    /// Direct-LAN TCP port; `0` = this host accepts no `ip:port`.
    pub lan_port: u16,
}

/// Encode a beacon into a UDP payload. Names are truncated to
/// [`MAX_NAME_LEN`] bytes on a char boundary; `signal_port`/`lan_port`
/// of `None` are encoded as `0`.
pub fn encode(b: &Beacon) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + MAX_NAME_LEN);
    out.extend_from_slice(MAGIC);
    out.push(PACKET_VERSION);
    out.extend_from_slice(&b.proto.to_le_bytes());
    out.extend_from_slice(&b.peer_id.to_le_bytes());
    // Truncate the name on a char boundary, then cap the length byte.
    let mut name = b.name.as_bytes();
    while name.len() > MAX_NAME_LEN {
        name = &name[..name.len() - 1];
    }
    debug_assert!(name.len() <= u8::MAX as usize);
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out.extend_from_slice(&b.signal_port.to_le_bytes());
    out.extend_from_slice(&b.lan_port.to_le_bytes());
    out
}

/// Parse a UDP payload into a [`Beacon`]. `None` on anything that is
/// not a current-shape beacon: wrong magic/version, truncated, a name
/// length that overruns the packet, or a foreign protocol version is
/// left to the caller to filter (it needs `proto` to compare).
pub fn parse(buf: &[u8]) -> Option<Beacon> {
    if buf.len() < 22 || &buf[..4] != MAGIC || buf[4] != PACKET_VERSION {
        return None;
    }
    let proto = u32::from_le_bytes(buf[5..9].try_into().ok()?);
    let peer_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
    let name_len = buf[17] as usize;
    if 18 + name_len + 4 > buf.len() {
        return None;
    }
    let name = String::from_utf8_lossy(&buf[18..18 + name_len]).into_owned();
    let signal_port = u16::from_le_bytes(buf[18 + name_len..20 + name_len].try_into().ok()?);
    let lan_port = u16::from_le_bytes(buf[20 + name_len..22 + name_len].try_into().ok()?);
    Some(Beacon {
        proto,
        peer_id,
        name,
        signal_port,
        lan_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let b = Beacon {
            proto: crate::PROTO_VERSION,
            peer_id: 691_721_284,
            name: "office-pc".into(),
            signal_port: 8080,
            lan_port: 54000,
        };
        let buf = encode(&b);
        assert_eq!(parse(&buf).unwrap(), b);
        // Zero ports (feature disabled) survive the round trip.
        let b = Beacon {
            signal_port: 0,
            lan_port: 0,
            ..b
        };
        assert_eq!(parse(&encode(&b)).unwrap(), b);
    }

    #[test]
    fn long_names_truncate_on_char_boundary() {
        let b = Beacon {
            proto: crate::PROTO_VERSION,
            peer_id: 950_239_493,
            // 200 Cyrillic two-byte chars — far past the cap.
            name: "ф".repeat(200),
            signal_port: 8080,
            lan_port: 0,
        };
        let buf = encode(&b);
        assert!(buf.len() <= 22 + MAX_NAME_LEN);
        let parsed = parse(&buf).unwrap();
        assert!(parsed.name.chars().count() <= MAX_NAME_LEN);
        assert!(parsed.name.chars().all(|c| c == 'ф'));
    }

    #[test]
    fn garbage_is_rejected() {
        let mut buf = encode(&Beacon {
            proto: crate::PROTO_VERSION,
            peer_id: 1,
            name: "x".into(),
            signal_port: 8080,
            lan_port: 54000,
        });
        assert!(parse(&buf).is_some());
        // Wrong magic.
        buf[0] = b'X';
        assert!(parse(&buf).is_none());
        // Wrong packet version.
        buf[0] = b'O';
        buf[4] = 99;
        assert!(parse(&buf).is_none());
        // Truncated payload.
        assert!(parse(&buf[..10]).is_none());
        // Name length overruns the packet.
        buf[4] = PACKET_VERSION;
        buf[17] = 200;
        assert!(parse(&buf).is_none());
        // Empty buffer.
        assert!(parse(&[]).is_none());
    }
}
