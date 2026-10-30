//! Signaling messages exchanged with an OpacusR server over WebSocket
//! (binary frames, bincode-encoded [`SignalMessage`]).
//!
//! The server never inspects SDP blobs — it routes opaque payloads between
//! registered peers and keeps a registry of public peer ids.

use serde::{Deserialize, Serialize};

use crate::{PeerId, Platform};

/// Client → server: register under a fresh public id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Register {
    pub platform: Platform,
    pub display_name: String,
    /// `true` = host agent (accepts sessions), `false` = viewer.
    pub is_agent: bool,
    /// `true` = host answers session requests without human consent
    /// (unattended password set).
    pub unattended: bool,
}

/// Server → client: registration result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub id: PeerId,
    pub server_version: String,
    /// Recommended heartbeat interval in seconds.
    pub heartbeat_secs: u16,
}

/// Viewer → server: ask peer `peer` for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSession {
    pub peer: PeerId,
    pub display_name: String,
}

/// Server → host: somebody wants a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIncoming {
    pub from: PeerId,
    pub display_name: String,
    pub platform: Platform,
}

/// Server → viewer: the request could not even reach the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFailed {
    pub peer: PeerId,
    pub reason: String,
}

/// Opaque payload routed between two peers (server does not interpret it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerPayload {
    /// Host agrees to the session.
    Accept,
    /// Host declines.
    Reject { reason: String },
    /// WebRTC offer (non-trickle: gathered SDP).
    SdpOffer { sdp: String },
    /// WebRTC answer (non-trickle: gathered SDP).
    SdpAnswer { sdp: String },
    /// Switch to the TCP relay fallback: the sender allocated a ticket and
    /// is about to connect to `host:port` with role=Host; the receiver
    /// must connect with the same ticket and role=Viewer.
    RelayUse { ticket: [u8; 16], port: u16 },
}

/// Both ways: forward `payload` to peer `to` / deliver from peer `from`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSend {
    pub to: PeerId,
    pub payload: PeerPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecv {
    pub from: PeerId,
    pub payload: PeerPayload,
}

/// Server → peer: the other side unregistered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PeerGone {
    pub peer: PeerId,
}

/// Client → server: allocate a relay ticket (TCP fallback when direct
/// WebRTC is impossible).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RelayAlloc;

/// Server → client: ticket for the TCP relay listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayTicket {
    pub ticket: [u8; 16],
    /// TCP port of the relay listener on the same server host.
    pub port: u16,
}

/// Every signaling message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalMessage {
    Register(Register),
    Welcome(Welcome),
    /// Client liveness; server prunes peers silent for 3× interval.
    Heartbeat,
    RequestSession(RequestSession),
    SessionIncoming(SessionIncoming),
    SessionFailed(SessionFailed),
    PeerSend(PeerSend),
    PeerRecv(PeerRecv),
    PeerGone(PeerGone),
    RelayAlloc(RelayAlloc),
    RelayTicket(RelayTicket),
    Error {
        message: String,
    },
}
