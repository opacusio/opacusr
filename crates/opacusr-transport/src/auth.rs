//! Shared password handshake between viewer and host.
//!
//! Flow (control channel, plaintext-safe: only public values cross the wire
//! before encryption is enabled):
//! 1. both sides exchange `Hello` (protocol version check);
//! 2. host → viewer: `AuthChallenge { salt, nonce, kdf }`;
//! 3. viewer → host: `AuthResponse { proof = HMAC-SHA256(psk, salt‖nonce) }`;
//! 4. host → viewer: `AuthResult { ok, session_salt }`;
//! 5. both derive the ChaCha20-Poly1305 session key via HKDF and enable the
//!    application-level cipher in [`SecureSession`].

use std::time::Duration;

use opacusr_crypto::{auth_proof, ct_eq, derive_psk, derive_session_key, random_bytes};
use opacusr_proto::message::{AuthChallenge, AuthResponse, AuthResult, Hello, KdfParams};
use opacusr_proto::Message;

use crate::lockout::{AuthGate, GateDecision};
use crate::{SecureSession, TransportError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a completed handshake on the viewer side.
pub struct ViewerHandshake {
    pub host_hello: Hello,
    /// Session key already installed into the [`SecureSession`].
    pub session_key: [u8; 32],
}

/// Viewer side of the handshake.
pub async fn viewer_handshake(
    session: &mut SecureSession,
    password: &str,
    display_name: &str,
) -> std::result::Result<ViewerHandshake, TransportError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let hello = Hello {
            proto_version: opacusr_proto::PROTO_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            platform: opacusr_proto::Platform::current(),
            display_name: display_name.into(),
            is_agent: false,
        };
        session.send_msg(&Message::Hello(hello)).await?;

        let host_hello = match session.recv_msg().await? {
            Message::Hello(h) => h,
            Message::SessionReject(r) => {
                return Err(TransportError::Other(format!(
                    "host rejected the session: {}",
                    r.reason
                )));
            }
            other => {
                return Err(TransportError::Other(format!(
                    "expected Hello, got {other:?}"
                )));
            }
        };
        if host_hello.proto_version != opacusr_proto::PROTO_VERSION {
            return Err(TransportError::Other(format!(
                "protocol version mismatch: host {}, viewer {}",
                host_hello.proto_version,
                opacusr_proto::PROTO_VERSION
            )));
        }

        let challenge = match session.recv_msg().await? {
            Message::AuthChallenge(c) => c,
            other => {
                return Err(TransportError::Other(format!(
                    "expected AuthChallenge, got {other:?}"
                )))
            }
        };
        let psk = derive_psk(password, &challenge.salt, &challenge.kdf)
            .map_err(|e| TransportError::Other(format!("kdf: {e}")))?;
        let proof = auth_proof(&psk, &challenge.salt, &challenge.nonce);
        session
            .send_msg(&Message::AuthResponse(AuthResponse { proof }))
            .await?;

        let result = match session.recv_msg().await? {
            Message::AuthResult(r) => r,
            other => {
                return Err(TransportError::Other(format!(
                    "expected AuthResult, got {other:?}"
                )))
            }
        };
        if !result.ok {
            return Err(TransportError::Other(
                result
                    .error
                    .unwrap_or_else(|| "authentication failed".into()),
            ));
        }

        let key = derive_session_key(&psk, &result.session_salt);
        session.enable_crypto(*key);
        Ok(ViewerHandshake {
            host_hello,
            session_key: *key,
        })
    })
    .await
    .map_err(|_| TransportError::Timeout)?
}

/// Host-side decision about an incoming viewer.
pub struct HostAuth {
    pub viewer_hello: Hello,
    pub session_key: [u8; 32],
}

/// Host side of the handshake. `password: Option<&str>` — `None` rejects
/// the session outright (no unattended password configured; an empty
/// string is *not* accepted either — pass `None` when nothing is set).
///
/// `gate: Option<&AuthGate>` — shared auth-failure limiter. When set, the
/// handshake is refused before any challenge material is sent while the
/// gate is locked, and wrong proofs are recorded (a wrong proof is the
/// only thing that counts — see [`crate::lockout`]).
pub async fn host_handshake(
    session: &mut SecureSession,
    password: Option<&str>,
    display_name: &str,
    gate: Option<&AuthGate>,
) -> std::result::Result<HostAuth, TransportError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        if let Some(gate) = gate {
            if let GateDecision::Locked(wait) = gate.check() {
                let secs = wait.as_secs().max(1);
                tracing::warn!(target: "opacusr",
                    "auth lockout active; refused a handshake ({secs} s left)");
                let _ = session
                    .send_msg(&Message::SessionReject(
                        opacusr_proto::message::SessionReject {
                            reason: format!(
                                "authentication locked out after repeated failures; retry in {secs} s"
                            ),
                        },
                    ))
                    .await;
                return Err(TransportError::Other(
                    "authentication locked out".into(),
                ));
            }
        }

        let viewer_hello = match session.recv_msg().await? {
            Message::Hello(h) => h,
            other => {
                return Err(TransportError::Other(format!(
                    "expected Hello, got {other:?}"
                )))
            }
        };
        if viewer_hello.proto_version != opacusr_proto::PROTO_VERSION {
            let _ = session
                .send_msg(&Message::SessionReject(
                    opacusr_proto::message::SessionReject {
                        reason: format!(
                            "protocol version mismatch (host {}, viewer {})",
                            opacusr_proto::PROTO_VERSION,
                            viewer_hello.proto_version
                        ),
                    },
                ))
                .await;
            return Err(TransportError::Other("protocol version mismatch".into()));
        }

        let hello = Hello {
            proto_version: opacusr_proto::PROTO_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            platform: opacusr_proto::Platform::current(),
            display_name: display_name.into(),
            is_agent: true,
        };
        session.send_msg(&Message::Hello(hello)).await?;

        let password = match password {
            Some(p) => p,
            None => {
                let _ = session
                    .send_msg(&Message::SessionReject(
                        opacusr_proto::message::SessionReject {
                            reason: "no password configured on the host".into(),
                        },
                    ))
                    .await;
                return Err(TransportError::Other(
                    "no password configured on the host".into(),
                ));
            }
        };

        let salt = random_bytes::<16>();
        let nonce = random_bytes::<16>();
        let kdf = KdfParams::default();
        session
            .send_msg(&Message::AuthChallenge(AuthChallenge { salt, nonce, kdf }))
            .await?;

        let response = match session.recv_msg().await? {
            Message::AuthResponse(r) => r,
            other => {
                return Err(TransportError::Other(format!(
                    "expected AuthResponse, got {other:?}"
                )))
            }
        };
        let psk = derive_psk(password, &salt, &kdf)
            .map_err(|e| TransportError::Other(format!("kdf: {e}")))?;
        let expected = auth_proof(&psk, &salt, &nonce);

        let (ok, error) = if ct_eq(&expected, &response.proof) {
            (true, None)
        } else {
            if let Some(gate) = gate {
                match gate.record_failure() {
                    Some(lock) => tracing::warn!(target: "opacusr",
                        "auth failed: wrong password (lockout engaged for {} s)",
                        lock.as_secs()),
                    None => tracing::warn!(target: "opacusr", "auth failed: wrong password"),
                }
            }
            (false, Some("wrong password".to_string()))
        };
        let session_salt = random_bytes::<16>();
        session
            .send_msg(&Message::AuthResult(AuthResult {
                ok,
                error,
                session_salt,
            }))
            .await?;
        if !ok {
            return Err(TransportError::Other("viewer failed authentication".into()));
        }
        if let Some(gate) = gate {
            gate.record_success();
        }

        let key = derive_session_key(&psk, &session_salt);
        session.enable_crypto(*key);
        Ok(HostAuth {
            viewer_hello,
            session_key: *key,
        })
    })
    .await
    .map_err(|_| TransportError::Timeout)?
}
