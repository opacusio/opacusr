//! End-to-end encryption layer on top of any [`Transport`].
//!
//! After the password handshake both peers hold a ChaCha20-Poly1305 session
//! key. When the underlying wire is *not* already confidential (TCP, relay),
//! every message is sealed into a [`CryptoEnvelope`]. On DTLS-protected
//! WebRTC the application may run unencrypted at wire level — call
//! [`SecureSession::new`] with `crypto = None` there, or keep it enabled for
//! defense in depth (config flag).

use opacusr_crypto::SessionCrypto;
use opacusr_proto::framing::{decode_message, encode_message};
use opacusr_proto::message::CryptoEnvelope;
use opacusr_proto::Message;

use crate::{Result, Transport, TransportError};

/// Session pipe with optional E2E encryption.
pub struct SecureSession {
    inner: Box<dyn Transport>,
    crypto: Option<SessionCrypto>,
}

impl SecureSession {
    pub fn new(inner: Box<dyn Transport>, crypto: Option<SessionCrypto>) -> Self {
        Self { inner, crypto }
    }

    /// The negotiated wire (for logs / UI).
    pub fn describe(&self) -> String {
        self.inner.describe()
    }

    /// Whether messages are additionally encrypted at application level.
    pub fn app_encrypted(&self) -> bool {
        self.crypto.is_some()
    }

    /// Whether the wire itself is confidential (WebRTC DTLS).
    pub fn wire_confidential(&self) -> bool {
        self.inner.wire_confidential()
    }

    /// Turn encryption on after the handshake completes. Idempotent: a later
    /// call replaces the cipher (both counters reset) — only do this once,
    /// right after `AuthResult`.
    pub fn enable_crypto(&mut self, session_key: [u8; 32]) {
        self.crypto = Some(SessionCrypto::new(&session_key));
    }

    /// Serialize + (optionally) seal + send.
    pub async fn send_msg(&mut self, msg: &Message) -> Result<()> {
        let channel = msg.channel();
        let raw = encode_message(msg)?;
        match &mut self.crypto {
            Some(crypto) => {
                let envelope = crypto.seal(&raw);
                let bytes = bincode::serialize(&envelope).map_err(|e| {
                    TransportError::Proto(opacusr_proto::ProtoError::Encode(e.to_string()))
                })?;
                self.inner.send(channel, &bytes).await
            }
            None => self.inner.send(channel, &raw).await,
        }
    }

    /// Receive + (optionally) open + deserialize.
    pub async fn recv_msg(&mut self) -> Result<Message> {
        let (channel, payload) = self.inner.recv().await?;
        let raw = match &mut self.crypto {
            Some(crypto) => {
                let envelope: CryptoEnvelope = bincode::deserialize(&payload).map_err(|_| {
                    TransportError::Other("invalid crypto envelope on the wire".into())
                })?;
                crypto.open(&envelope)?
            }
            None => payload,
        };
        let _ = channel; // channels are advisory for control-plane logic
        decode_message(&raw).map_err(TransportError::Proto)
    }

    /// Underlying transport teardown.
    pub async fn close(&mut self) {
        self.inner.close().await;
    }
}
