//! OpacusR cryptography: password-based session establishment and
//! end-to-end message encryption.
//!
//! Threat model in one line: the signaling/relay server is *untrusted* —
//! it can see ciphertext only. Confidentiality rests on DTLS (WebRTC mode)
//! plus an application layer built from a shared password:
//!
//! 1. Host sends a random `salt` + `nonce` (see [`opacusr_proto`] protocol).
//! 2. Both sides derive a PSK: Argon2id(password, salt) → 32 bytes.
//! 3. Viewer proves knowledge of the PSK: HMAC-SHA256(PSK, nonce‖salt).
//! 4. Both sides derive the session key: HKDF-SHA256(ikm = PSK,
//!    salt = `session_salt`) → ChaCha20-Poly1305 key.
//! 5. Every message (when the transport is not already confidential) is
//!    sealed with a strictly monotonic counter as the nonce — this gives
//!    replay protection for free.

pub mod totp;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

use opacusr_proto::message::{CryptoEnvelope, KdfParams};

/// Cryptographic failures.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("KDF parameter error: {0}")]
    Kdf(String),
    #[error("decryption failed (wrong key, corrupted or replayed data)")]
    Decrypt,
    #[error("replayed or out-of-order message: got counter {got}, expected {expected}")]
    Replay { got: u64, expected: u64 },
    #[error("random source failure")]
    Random,
}

const AUTH_INFO: &[u8] = b"opacusr-auth-v1";
const SESSION_INFO: &[u8] = b"opacusr-session-v1";

/// Cryptographically secure random bytes (panics only if the OS source is
/// broken, which is unrecoverable anyway).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}

/// Derive the 32-byte pre-shared key from a human password.
///
/// `salt` should be 16 random bytes per session (the host sends them in the
/// [`opacusr_proto::message::AuthChallenge`]).
pub fn derive_psk(
    password: &str,
    salt: &[u8],
    kdf: &KdfParams,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let params = Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.parallelism, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    a2.hash_password_into(password.as_bytes(), salt, out.as_mut())
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

/// Compute the viewer's proof of password knowledge: HMAC-SHA256 over
/// `salt ‖ nonce` keyed with the PSK.
pub fn auth_proof(psk: &[u8; 32], salt: &[u8; 16], nonce: &[u8; 16]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(psk).expect("hmac accepts any key len");
    mac.update(AUTH_INFO);
    mac.update(salt);
    mac.update(nonce);
    let out = mac.finalize().into_bytes();
    let mut proof = [0u8; 32];
    proof.copy_from_slice(&out);
    proof
}

/// Constant-time equality for authentication proofs.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Derive the ChaCha20-Poly1305 session key from the verified PSK and the
/// per-session random salt chosen by the host.
pub fn derive_session_key(psk: &[u8; 32], session_salt: &[u8; 16]) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(session_salt), psk);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(SESSION_INFO, out.as_mut())
        .expect("32 bytes is a valid HKDF length");
    out
}

/// Seals and opens messages with strict monotonic nonces, PER CHANNEL.
///
/// Both peers construct this with the *same* session key. The transport
/// delivers two independent channels (control, media) and — by design —
/// does NOT order one against the other (a viewer drains whichever queue
/// is ready first, tokio::select!). A single shared counter across both
/// channels made every cross-channel interleaving look like a replay
/// (v0.11.24 field report: «replayed or out-of-order message: got counter
/// 4, expected 3» the moment the first frame met a control message in
/// flight). Each channel therefore carries its OWN counter pair, and the
/// channel id is bound into both the nonce and the AAD so an envelope can
/// never be transplanted onto the other channel by the untrusted relay.
pub struct SessionCrypto {
    cipher: ChaCha20Poly1305,
    send_counters: [u64; N_CHANNELS],
    recv_counters: [u64; N_CHANNELS],
}

/// Number of independent channel counters. Channel ids above this are a
/// protocol error (proto defines Control=0, Media=1; room to grow).
pub const N_CHANNELS: usize = 4;

impl SessionCrypto {
    pub fn new(session_key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(session_key)),
            send_counters: [0; N_CHANNELS],
            recv_counters: [0; N_CHANNELS],
        }
    }

    /// Seal `plaintext` on `channel`. The AAD binds channel AND counter
    /// into the tag.
    pub fn seal_channel(&mut self, channel: u8, plaintext: &[u8]) -> CryptoEnvelope {
        let counter = self.send_counters[channel_usize(channel)];
        self.send_counters[channel_usize(channel)] += 1;
        let nonce = counter_nonce(channel, counter);
        let aad = counter_aad(channel, counter);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .expect("chacha20poly1305 cannot fail with valid inputs");
        CryptoEnvelope {
            counter,
            ciphertext,
        }
    }

    /// Open a sealed message from `channel`; enforces strict in-order
    /// delivery WITHIN the channel.
    pub fn open_channel(
        &mut self,
        channel: u8,
        envelope: &CryptoEnvelope,
    ) -> Result<Vec<u8>, CryptoError> {
        let ch = channel_usize(channel);
        if envelope.counter != self.recv_counters[ch] {
            return Err(CryptoError::Replay {
                got: envelope.counter,
                expected: self.recv_counters[ch],
            });
        }
        let nonce = counter_nonce(channel, envelope.counter);
        let aad = counter_aad(channel, envelope.counter);
        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &envelope.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        self.recv_counters[ch] += 1;
        Ok(plaintext)
    }
}

/// Channel id → counter-slot index. Unknown ids are a hard protocol
/// violation (fail closed rather than silently sharing slot 0).
fn channel_usize(channel: u8) -> usize {
    if (channel as usize) < N_CHANNELS {
        channel as usize
    } else {
        panic!("session crypto channel id {channel} out of range");
    }
}

/// 12-byte nonce: `[0, 0, channel, 0] ‖ big-endian counter` — unique per
/// (key, channel, counter); the channel byte keeps the spaces apart.
fn counter_nonce(channel: u8, counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[2] = channel;
    n[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::from(n)
}

/// AAD: channel byte ‖ big-endian counter — an envelope from one channel
/// never verifies on the other.
fn counter_aad(channel: u8, counter: u64) -> [u8; 9] {
    let mut aad = [0u8; 9];
    aad[0] = channel;
    aad[1..].copy_from_slice(&counter.to_be_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kdf() -> KdfParams {
        // Keep tests fast: small costs are fine for round-trip checks.
        KdfParams {
            m_cost_kib: 8 * 1024,
            t_cost: 1,
            parallelism: 1,
        }
    }

    #[test]
    fn psk_is_deterministic_and_password_sensitive() {
        let salt = random_bytes::<16>();
        let a = derive_psk("hunter2", &salt, &kdf()).unwrap();
        let b = derive_psk("hunter2", &salt, &kdf()).unwrap();
        let c = derive_psk("hunter3", &salt, &kdf()).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
        assert_ne!(a.as_ref(), c.as_ref());
        let salt2 = random_bytes::<16>();
        let d = derive_psk("hunter2", &salt2, &kdf()).unwrap();
        assert_ne!(a.as_ref(), d.as_ref());
    }

    #[test]
    fn auth_proof_binds_salt_and_nonce() {
        let psk = derive_psk("pw", &random_bytes::<16>(), &kdf()).unwrap();
        let salt = random_bytes::<16>();
        let nonce = random_bytes::<16>();
        let p1 = auth_proof(&psk, &salt, &nonce);
        let p2 = auth_proof(&psk, &salt, &nonce);
        assert_eq!(p1, p2);
        assert_ne!(p1, auth_proof(&psk, &salt, &random_bytes::<16>()));
        assert!(ct_eq(&p1, &p2));
        let mut other = p2;
        other[0] ^= 1;
        assert!(!ct_eq(&p1, &other));
        assert!(!ct_eq(&p1, &p1[..31]));
    }

    #[test]
    fn session_key_depends_on_salt() {
        let psk = derive_psk("pw", &random_bytes::<16>(), &kdf()).unwrap();
        assert_ne!(
            derive_session_key(&psk, &random_bytes::<16>()).as_ref(),
            derive_session_key(&psk, &random_bytes::<16>()).as_ref()
        );
    }

    const CTL: u8 = 0;
    const MED: u8 = 1;

    #[test]
    fn cipher_round_trip_and_tamper_detection() {
        let key = derive_session_key(
            &derive_psk("pw", &random_bytes::<16>(), &kdf()).unwrap(),
            &random_bytes::<16>(),
        );
        let mut alice = SessionCrypto::new(&key);
        let mut bob = SessionCrypto::new(&key);

        let env = alice.seal_channel(CTL, b"hello opacusr");
        assert_eq!(bob.open_channel(CTL, &env).unwrap(), b"hello opacusr");

        // Tampered ciphertext must fail.
        let mut env2 = alice.seal_channel(CTL, b"secret frame data");
        env2.ciphertext[3] ^= 0xFF;
        assert!(matches!(
            bob.open_channel(CTL, &env2),
            Err(CryptoError::Decrypt)
        ));

        // Replay must fail even if ciphertext is intact. Note: the failed
        // tamper attempt above did NOT advance the receive counter, so the
        // next valid message must arrive at counter 1 — anything else is
        // rejected (a broken stream is fatal by design).
        let env3 = alice.seal_channel(CTL, b"again");
        assert!(matches!(
            bob.open_channel(CTL, &env3),
            Err(CryptoError::Replay {
                got: 2,
                expected: 1
            })
        ));
        let env4 = alice.seal_channel(CTL, b"fresh after error");
        assert!(bob.open_channel(CTL, &env4).is_err());
        assert!(matches!(
            bob.open_channel(CTL, &alice.seal_channel(CTL, b"resync")),
            Err(CryptoError::Replay { .. })
        ));
    }

    #[test]
    fn counters_are_independent_per_direction() {
        let key = derive_session_key(&random_bytes::<32>(), &random_bytes::<16>());
        let mut a = SessionCrypto::new(&key);
        let mut b = SessionCrypto::new(&key);
        let e1 = a.seal_channel(CTL, b"one");
        let e2 = b.seal_channel(CTL, b"uno");
        // Interleaved directions must both decrypt fine.
        assert_eq!(a.open_channel(CTL, &e2).unwrap(), b"uno");
        assert_eq!(b.open_channel(CTL, &e1).unwrap(), b"one");
    }

    /// THE v0.11.24 field bug: the host ships video frames on the media
    /// channel and control traffic on the control channel; the viewer's
    /// receive loop drains whichever queue is ready first, so envelopes
    /// from the two channels arrive ARBITRARILY interleaved. With per-
    /// channel counters every interleaving decrypts cleanly.
    #[test]
    fn cross_channel_interleaving_never_looks_like_replay() {
        let key = derive_session_key(&random_bytes::<32>(), &random_bytes::<16>());
        let mut host = SessionCrypto::new(&key);
        let mut viewer = SessionCrypto::new(&key);

        // Sealed in the natural send order: ctl 0..3, then frames+ctl.
        let ctl0 = host.seal_channel(CTL, b"welcome");
        let ctl1 = host.seal_channel(CTL, b"accept");
        let ctl2 = host.seal_channel(CTL, b"quality");
        let ctl3 = host.seal_channel(CTL, b"input-ack");
        let med0 = host.seal_channel(MED, b"frame0");
        let med1 = host.seal_channel(MED, b"frame1");

        // Delivered in a shuffled cross-channel order — exactly what a
        // tokio::select! receive loop produces under load.
        assert_eq!(viewer.open_channel(CTL, &ctl0).unwrap(), b"welcome");
        assert_eq!(viewer.open_channel(MED, &med0).unwrap(), b"frame0");
        assert_eq!(viewer.open_channel(MED, &med1).unwrap(), b"frame1");
        assert_eq!(viewer.open_channel(CTL, &ctl1).unwrap(), b"accept");
        assert_eq!(
            viewer
                .open_channel(MED, &host.seal_channel(MED, b"frame2"))
                .unwrap(),
            b"frame2"
        );
        assert_eq!(viewer.open_channel(CTL, &ctl2).unwrap(), b"quality");
        assert_eq!(viewer.open_channel(CTL, &ctl3).unwrap(), b"input-ack");
    }

    /// An envelope transplanted onto the OTHER channel (the untrusted
    /// relay could try) must fail authentication — channel id is in the
    /// AAD, not only in the nonce space.
    #[test]
    fn envelopes_cannot_move_across_channels() {
        let key = derive_session_key(&random_bytes::<32>(), &random_bytes::<16>());
        let mut host = SessionCrypto::new(&key);
        let mut viewer = SessionCrypto::new(&key);

        let on_media = host.seal_channel(MED, b"frame");
        // Same counter value on the control channel — different nonce AND
        // AAD; verification must fail either way.
        assert!(matches!(
            viewer.open_channel(CTL, &on_media),
            Err(CryptoError::Decrypt)
        ));
        // And the legit delivery still works afterwards.
        assert_eq!(viewer.open_channel(MED, &on_media).unwrap(), b"frame");
    }

    #[test]
    #[should_panic]
    fn unknown_channel_id_fails_closed() {
        let key = derive_session_key(&random_bytes::<32>(), &random_bytes::<16>());
        let mut s = SessionCrypto::new(&key);
        let _ = s.seal_channel(200, b"x");
    }
}
