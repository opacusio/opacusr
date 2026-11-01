//! OpacusR wire protocol.
//!
//! Everything that crosses the network between two peers or between a peer
//! and the signaling server is defined in this crate: session messages
//! ([`message`]), signaling messages ([`signal`]) and the byte framing used
//! by stream transports ([`framing`]).
//!
//! The protocol is intentionally simple: `serde` + `bincode`, explicit
//! versioning via [`PROTO_VERSION`], and small fixed-size crypto fields.

pub mod framing;
pub mod message;
pub mod signal;

pub use message::*;
pub use signal::*;

/// Bump this when the message enums change in a way that breaks
/// compatibility. Peers must refuse sessions when versions differ.
///
/// v2 (0.3.0): `SessionAccept` gained `view_only`/`screens`/`audio_available`;
/// new messages `LinkStats`, `SelectScreen`, `AudioControl`, `AudioFrame`.
///
/// v3 (0.6.0): `AuthResponse` gained an optional `totp` code (RFC 6238
/// second factor) — hosts that require 2FA reject responses without it.
pub const PROTO_VERSION: u32 = 3;

/// Application name used in handshakes and logs.
pub const APP_NAME: &str = "OpacusR";

/// Audio streaming parameters, fixed by the protocol: Opus, 48 kHz mono,
/// 20 ms frames (960 samples), VoIP bitrate.
pub mod audio {
    pub const SAMPLE_RATE: u32 = 48_000;
    pub const FRAME_MS: usize = 20;
    pub const FRAME_SAMPLES: usize = SAMPLE_RATE as usize * FRAME_MS / 1000;
    pub const BITRATE: i32 = 32_000;
}

/// Error type shared by protocol helpers.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("serialization failed: {0}")]
    Encode(String),
    #[error("deserialization failed: {0}")]
    Decode(String),
    #[error("invalid frame: {0}")]
    BadFrame(String),
    #[error("invalid identifier: {0}")]
    BadId(String),
    #[error("message too large: {size} bytes (limit {limit})")]
    TooLarge { size: usize, limit: usize },
}

/// A peer identifier, e.g. `382 915 074` — the number users type to connect.
///
/// Server-assigned 9-digit number in `[MIN_ID, MAX_ID]`.
pub type PeerId = u64;

/// Lowest assignable peer id (100 000 000).
pub const MIN_PEER_ID: PeerId = 100_000_000;
/// Highest assignable peer id (999 999 999).
pub const MAX_PEER_ID: PeerId = 999_999_999;

/// Returns `true` if `id` is a syntactically valid peer id.
pub fn is_valid_peer_id(id: PeerId) -> bool {
    (MIN_PEER_ID..=MAX_PEER_ID).contains(&id)
}

/// Logical channel of a session. Transports either multiplex both channels
/// over one stream (TCP) or map them onto separate SCTP data channels
/// (WebRTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Channel {
    /// Reliable, ordered, low-volume: auth, input, chat, files, terminal.
    Control = 0,
    /// High-volume media: video frames. Kept separate so a burst of frames
    /// never delays control traffic.
    Media = 1,
}

impl Channel {
    /// Single-byte tag used by stream framing.
    pub fn tag(self) -> u8 {
        match self {
            Channel::Control => 0,
            Channel::Media => 1,
        }
    }

    /// Inverse of [`Channel::tag`].
    pub fn from_tag(tag: u8) -> Result<Self, ProtoError> {
        match tag {
            0 => Ok(Channel::Control),
            1 => Ok(Channel::Media),
            other => Err(ProtoError::BadFrame(format!("unknown channel tag {other}"))),
        }
    }
}

/// Target platform, advertised in handshakes and the signaling registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Platform {
    Windows,
    Linux,
    MacOS,
    Android,
    IOS,
    Other,
}

impl Platform {
    /// Current compile-target platform.
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Platform::MacOS
        }
        #[cfg(target_os = "android")]
        {
            Platform::Android
        }
        #[cfg(target_os = "ios")]
        {
            Platform::IOS
        }
        #[cfg(all(
            not(target_os = "windows"),
            not(target_os = "macos"),
            not(target_os = "android"),
            not(target_os = "ios")
        ))]
        {
            Platform::Linux
        }
    }

    /// Human readable name (English).
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::MacOS => "macOS",
            Platform::Android => "Android",
            Platform::IOS => "iOS",
            Platform::Other => "Other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_range_is_validated() {
        assert!(is_valid_peer_id(100_000_000));
        assert!(is_valid_peer_id(999_999_999));
        assert!(!is_valid_peer_id(99_999_999));
        assert!(!is_valid_peer_id(1_000_000_000));
    }

    #[test]
    fn channel_tags_round_trip() {
        for ch in [Channel::Control, Channel::Media] {
            assert_eq!(Channel::from_tag(ch.tag()).unwrap(), ch);
        }
        assert!(Channel::from_tag(7).is_err());
    }

    #[test]
    fn platform_current_is_sane() {
        // The sandbox/test target is a desktop platform.
        assert_ne!(Platform::current(), Platform::Other);
    }
}
