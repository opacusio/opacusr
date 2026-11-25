//! Zero-infra LAN discovery over mDNS/DNS-SD.
//!
//! Agents advertise `_opacusr._tcp.local.` so viewers on the same network
//! can list hosts without any signaling server or typed ID (Phase 3 of
//! docs/serverless.md). The advertisement carries three TXT records:
//!
//! * `path`  — URL path of the signaling endpoint (`/signal`);
//! * `proto` — the advertising peer's [`PROTO_VERSION`];
//! * `lan`   — the direct-LAN TCP port (`ip:port` sessions), present only
//!   when the host accepts them (`--allow-lan`);
//! * `id`    — the advertising peer's 9-digit ID, once the signaling
//!   server has assigned one. Lets a viewer resolve «connect by ID»
//!   straight onto this host's endpoints instead of asking every
//!   discovered signaling server about a stranger.
//!
//! Only the pure pieces live here — service name, TXT keys and the
//! helpers both sides share — so the byte-shape of a discovery record has
//! one definition and is testable without a network. The `mdns-sd`
//! plumbing stays in the advertiser (agent) and the browser (GUI).

/// mDNS service type advertised and browsed by OpacusR peers.
pub const SERVICE_TYPE: &str = "_opacusr._tcp.local.";

use crate::PROTO_VERSION;

/// TXT key: URL path of the signaling endpoint.
pub const TXT_PATH: &str = "path";

/// TXT key: the advertising peer's [`PROTO_VERSION`] (decimal string).
pub const TXT_PROTO: &str = "proto";

/// TXT key: direct-LAN TCP port (decimal string) when the host accepts
/// `ip:port` sessions.
pub const TXT_LAN: &str = "lan";

/// TXT key: the advertising peer's assigned 9-digit ID (decimal
/// string), present once the signaling server has minted one.
pub const TXT_ID: &str = "id";

/// Longest instance name we advertise (RFC 6763 §6 keeps single labels
/// practical; DNS labels cap at 63 bytes — stay comfortably inside).
const MAX_INSTANCE_LEN: usize = 60;

/// Sanitize a display/host name into a valid mDNS instance name.
///
/// mDNS splits service names on `.` into DNS labels, so an instance name
/// must not contain one: a host that reports itself as `office.local`
/// (the common hostname form) would otherwise advertise an empty
/// instance label under a foreign domain. Folds every `.` out of the
/// name and trims it to a sane length.
pub fn sanitize_instance_name(name: &str) -> String {
    let folded: String = name.chars().filter(|c| *c != '.').collect();
    let trimmed = folded.trim();
    if trimmed.is_empty() {
        return "opacusr-host".into();
    }
    let mut out: String = trimmed.chars().take(MAX_INSTANCE_LEN).collect();
    // Labels are byte-bound in DNS; a multi-byte char cut at a char
    // boundary is fine, but stay under the 63-byte DNS limit.
    while out.len() > MAX_INSTANCE_LEN {
        out.pop();
    }
    out
}

/// Parsed TXT payload of a discovered OpacusR service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTxt {
    /// URL path of the signaling endpoint (e.g. `/signal`).
    pub path: String,
    /// The advertising peer's protocol version. Callers compare against
    /// [`PROTO_VERSION`] and skip foreign-version hosts.
    pub proto: u32,
    /// Direct-LAN TCP port when the host accepts `ip:port` sessions.
    pub lan_port: Option<u16>,
    /// The advertising peer's assigned ID, when known.
    pub peer_id: Option<crate::PeerId>,
}

/// Build the TXT properties for an advertiser. `lan_port` is `None`
/// unless the host also accepts direct LAN sessions; `peer_id` is
/// `None` until the signaling server has assigned an ID.
pub fn txt_properties(
    path: &str,
    lan_port: Option<u16>,
    peer_id: Option<crate::PeerId>,
) -> Vec<(String, String)> {
    let mut props = vec![
        (TXT_PATH.to_string(), path.to_string()),
        (TXT_PROTO.to_string(), PROTO_VERSION.to_string()),
    ];
    if let Some(port) = lan_port {
        props.push((TXT_LAN.to_string(), port.to_string()));
    }
    if let Some(id) = peer_id {
        props.push((TXT_ID.to_string(), id.to_string()));
    }
    props
}

/// Parse the TXT map of a discovered service. `None` when the required
/// records are missing or malformed (not an OpacusR advertisement).
pub fn parse_txt(props: &[(String, String)]) -> Option<ServiceTxt> {
    let mut path = None;
    let mut proto = None;
    let mut lan = None;
    let mut id = None;
    for (k, v) in props {
        match k.as_str() {
            TXT_PATH => path = Some(v.trim().to_string()),
            TXT_PROTO => proto = v.trim().parse().ok(),
            TXT_LAN => lan = v.trim().parse().ok(),
            TXT_ID => id = v.trim().parse().ok(),
            _ => {}
        }
    }
    let path = path?;
    let proto = proto?;
    if path.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(ServiceTxt {
        path,
        proto,
        lan_port: lan,
        peer_id: id,
    })
}

/// Signaling URL for a discovered host: `ws://<host>:<port><path>`,
/// with IPv6 addresses bracketed.
pub fn signal_url(ip: std::net::IpAddr, port: u16, path: &str) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => format!("ws://{v4}:{port}{path}"),
        std::net::IpAddr::V6(v6) => format!("ws://[{v6}]:{port}{path}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_are_dot_free() {
        assert_eq!(sanitize_instance_name("office.local"), "officelocal");
        assert_eq!(sanitize_instance_name("my.pc.name"), "mypcname");
        assert_eq!(sanitize_instance_name("plain"), "plain");
        assert_eq!(sanitize_instance_name("  spaced  "), "spaced");
        // Empty / dot-only names fall back to something usable.
        assert!(!sanitize_instance_name("").contains('.'));
        assert!(!sanitize_instance_name("...").contains('.'));
        // No name can smuggle in a second DNS label or overflow it.
        for long in ["a".repeat(500), "в".repeat(500)] {
            let s = sanitize_instance_name(&long);
            assert!(s.len() <= MAX_INSTANCE_LEN);
            assert!(!s.contains('.'));
        }
    }

    #[test]
    fn txt_and_url_round_trip() {
        let props = txt_properties("/signal", Some(54000), Some(691721284));
        let txt = parse_txt(&props).expect("valid payload");
        assert_eq!(txt.path, "/signal");
        assert_eq!(txt.proto, PROTO_VERSION);
        assert_eq!(txt.lan_port, Some(54000));
        assert_eq!(txt.peer_id, Some(691721284));

        // No `lan` record → direct LAN is not offered.
        let no_lan = parse_txt(&txt_properties("/signal", None, None)).unwrap();
        assert_eq!(no_lan.lan_port, None);
        assert_eq!(no_lan.peer_id, None);

        // A malformed `id` record is ignored (older peers never send it).
        let props = txt_properties("/signal", None, None);
        let props = [
            props.as_slice(),
            &[(TXT_ID.to_string(), "not-a-number".to_string())],
        ]
        .concat();
        let txt = parse_txt(&props).unwrap();
        assert_eq!(txt.peer_id, None);

        // Malformed or foreign payloads are rejected by the parser.
        assert!(parse_txt(&[]).is_none());
        assert!(parse_txt(&[(TXT_PATH.into(), "signal".into())]).is_none());
        assert!(parse_txt(&[
            (TXT_PATH.into(), "/signal".into()),
            (TXT_PROTO.into(), "not-a-number".into())
        ])
        .is_none());

        // URL shape: plain IPv4 and bracketed IPv6.
        assert_eq!(
            signal_url("192.168.1.7".parse().unwrap(), 8080, "/signal"),
            "ws://192.168.1.7:8080/signal"
        );
        assert_eq!(
            signal_url("2001:db8::1".parse().unwrap(), 8080, "/signal"),
            "ws://[2001:db8::1]:8080/signal"
        );
    }
}
