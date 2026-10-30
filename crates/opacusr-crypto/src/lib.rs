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

/// Seals and opens messages with strict monotonic nonces.
///
/// Both peers construct this with the *same* session key; each instance
/// keeps independent send/recv counters, so counters never collide between
/// directions.
pub struct SessionCrypto {
    cipher: ChaCha20Poly1305,
    send_counter: u64,
    recv_counter: u64,
}

impl SessionCrypto {
    pub fn new(session_key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(session_key)),
            send_counter: 0,
            recv_counter: 0,
        }
    }

    /// Seal `plaintext`. The AAD binds the counter into the tag.
    pub fn seal(&mut self, plaintext: &[u8]) -> CryptoEnvelope {
        let counter = self.send_counter;
        self.send_counter += 1;
        let nonce = counter_nonce(counter);
        let aad = counter.to_be_bytes();
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

    /// Open a sealed message; enforces strict in-order delivery.
    pub fn open(&mut self, envelope: &CryptoEnvelope) -> Result<Vec<u8>, CryptoError> {
        if envelope.counter != self.recv_counter {
            return Err(CryptoError::Replay {
                got: envelope.counter,
                expected: self.recv_counter,
            });
        }
        let nonce = counter_nonce(envelope.counter);
        let aad = envelope.counter.to_be_bytes();
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
        self.recv_counter += 1;
        Ok(plaintext)
    }
}

/// 12-byte nonce: 4 zero bytes ‖ big-endian counter (unique per key).
fn counter_nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::from(n)
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

    #[test]
    fn cipher_round_trip_and_tamper_detection() {
        let key = derive_session_key(
            &derive_psk("pw", &random_bytes::<16>(), &kdf()).unwrap(),
            &random_bytes::<16>(),
        );
        let mut alice = SessionCrypto::new(&key);
        let mut bob = SessionCrypto::new(&key);

        let env = alice.seal(b"hello opacusr");
        assert_eq!(bob.open(&env).unwrap(), b"hello opacusr");

        // Tampered ciphertext must fail.
        let mut env2 = alice.seal(b"secret frame data");
        env2.ciphertext[3] ^= 0xFF;
        assert!(matches!(bob.open(&env2), Err(CryptoError::Decrypt)));

        // Replay must fail even if ciphertext is intact. Note: the failed
        // tamper attempt above did NOT advance the receive counter, so the
        // next valid message must arrive at counter 1 — anything else is
        // rejected (a broken stream is fatal by design).
        let env3 = alice.seal(b"again");
        assert!(matches!(
            bob.open(&env3),
            Err(CryptoError::Replay {
                got: 2,
                expected: 1
            })
        ));
        let env4 = alice.seal(b"fresh after error");
        assert!(bob.open(&env4).is_err());
        assert!(matches!(
            bob.open(&alice.seal(b"resync")),
            Err(CryptoError::Replay { .. })
        ));
    }

    #[test]
    fn counters_are_independent_per_direction() {
        let key = derive_session_key(&random_bytes::<32>(), &random_bytes::<16>());
        let mut a = SessionCrypto::new(&key);
        let mut b = SessionCrypto::new(&key);
        let e1 = a.seal(b"one");
        let e2 = b.seal(b"uno");
        // Interleaved directions must both decrypt fine.
        assert_eq!(a.open(&e2).unwrap(), b"uno");
        assert_eq!(b.open(&e1).unwrap(), b"one");
    }
}
