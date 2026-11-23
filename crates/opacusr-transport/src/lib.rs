//! OpacusR transports and session plumbing.
//!
//! Layers (bottom → top):
//! 1. [`Transport`] — byte pipes with two logical channels
//!    ([`opacusr_proto::Channel`]): TCP ([`tcp`]), server relay
//!    ([`relay`]) and WebRTC data channels ([`webrtc`]).
//! 2. [`secure::SecureSession`] — optional end-to-end encryption on top of
//!    any transport (skipped when the wire is already confidential).
//! 3. [`signaling::SignalingClient`] — WebSocket client for the OpacusR
//!    signaling server.

pub mod auth;
pub mod chunk;
pub mod ft;
pub mod lockout;
pub mod proxy;
pub mod relay;
pub mod secure;
pub mod signaling;
pub mod tcp;
pub mod turn;
pub mod update;
pub mod webrtc;

pub use opacusr_proto::{message::Message, signal::*, Channel, PeerId, ProtoError, PROTO_VERSION};
pub use proxy::Proxy;
pub use secure::SecureSession;

use async_trait::async_trait;

/// Install the process-wide rustls crypto provider.
///
/// The workspace compiles rustls with two crypto backends: the WebRTC
/// stack wants **ring** while rust-libp2p (opacusr-libp2p) pulls in
/// **aws-lc-rs**. With both feature sets unified rustls refuses to
/// auto-pick a process default and panics on the first `builder()` call
/// ("no process-level CryptoProvider available"). Pinning ring on the
/// first transport use removes the ambiguity; a second install attempt
/// is a harmless no-op.
pub fn ensure_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Transport failures, unified across backends.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),
    #[error("crypto error: {0}")]
    Crypto(#[from] opacusr_crypto::CryptoError),
    #[error("connection closed")]
    Closed,
    #[error("operation timed out")]
    Timeout,
    #[error("webrtc error: {0}")]
    WebRtc(String),
    #[error("signaling error: {0}")]
    Signaling(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = TransportError> = std::result::Result<T, E>;

/// A bidirectional pipe carrying framed payloads on two channels.
///
/// `payload` passed to [`Transport::send`] / returned from
/// [`Transport::recv`] is an *unframed* bincode `Message` (or an encrypted
/// envelope thereof — that decision belongs to [`SecureSession`]).
#[async_trait]
pub trait Transport: Send {
    async fn send(&mut self, channel: Channel, payload: &[u8]) -> Result<()>;
    async fn recv(&mut self) -> Result<(Channel, Vec<u8>)>;
    /// `true` when the wire itself provides confidentiality (WebRTC DTLS),
    /// so application-layer encryption becomes optional.
    fn wire_confidential(&self) -> bool {
        false
    }
    /// Short description for logs and UI.
    fn describe(&self) -> String;
    /// Shut the transport down gracefully.
    async fn close(&mut self);
}
