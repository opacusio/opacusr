# OpacusR Security

This document explains the cryptographic design, the trust boundaries and the threat model of OpacusR v0.1.0, plus known limitations and planned improvements. The message flow it describes is specified in [protocol.md](protocol.md).

## 1. Design goals

1. **The password never crosses the wire.** The host verifies *proof* of knowledge, not the password itself.
2. **Every payload is end-to-end encrypted** regardless of transport — WebRTC, LAN TCP or relay. The relay server forwards sealed bytes it cannot read.
3. **Per-session keys.** Compromising one session's key must not help an attacker against any other session, past or future.
4. **Replay resistance.** A captured, re-sent or re-ordered sealed frame is rejected.
5. **Self-hostable trust.** The only long-lived secret is the access password, chosen and stored by the host owner. No vendor account, no vendor cloud.

## 2. Cryptographic building blocks

| Purpose | Primitive | Crate |
| --- | --- | --- |
| Password stretching (PSK) | Argon2id, 32-byte output | `argon2` |
| Proof of password knowledge | HMAC-SHA256 over a per-session nonce | `hmac`/`sha2` |
| Session key derivation | HKDF-SHA256 (`info = "opacusr-session-v1"`) | `hkdf` |
| Payload encryption | ChaCha20-Poly1305 AEAD | `chacha20poly1305` |
| Comparison of proofs | constant-time equality | manual XOR-OR loop |
| Key hygiene | keys wrapped in `Zeroizing` | `zeroize` |

Default Argon2id parameters (`KdfParams`): **m = 8 MiB, t = 1, p = 1**, sent explicitly in every `AuthChallenge` so both sides agree.

## 3. Authentication flow

```
salt  = random 16 B (per session, host-chosen)
nonce = random 16 B (per session, host-chosen)

PSK          = Argon2id(password, salt)             [both sides]
proof        = HMAC-SHA256(PSK, nonce ‖ salt)       [viewer]
host recomputes and compares in constant time

session_salt = random 16 B (host, delivered only after proof passes)
session_key  = HKDF-SHA256(ikm=PSK, salt=session_salt, info="opacusr-session-v1")
```

Consequences:

- A passive observer of the handshake sees `salt`, `nonce`, `proof` — none of which is reversible to the password. The HMAC proof is bound to this session's nonce, so it cannot be replayed in a later session.
- `session_salt` is only ever sent *after* successful proof verification, so an unauthenticated peer never learns the salt that derives the session key.
- Password change takes effect immediately for new sessions (nothing password-derived is persisted server-side).

## 4. Session encryption

`SessionCrypto` (`opacusr-crypto`) seals every frame after the handshake:

- **Nonce discipline:** 12-byte counter nonce = `4 zero bytes ‖ u64 BE counter`. Independent `send_counter`/`recv_counter` per direction, so counters never collide between the two peers and never repeat under one key.
- **Associated data:** the 8-byte big-endian counter is the AEAD AAD — a frame cannot be transplanted to another sequence position.
- **Replay handling:** strict — `recv_counter` must match the envelope counter exactly. A duplicate, reordered or skipped frame returns `CryptoError::Replay`, and per the implementation's contract a corrupted stream is **fatal**: the peers tear the session down rather than resync. (The underlying transports are reliable and ordered — TCP or SCTP — so a mismatch always means tampering or a broken peer, not ordinary loss. A sliding-window resync mode is a roadmap item if lossy datagram transports are added.)

## 5. Trust boundaries

| Component | Trusts | Sees |
| --- | --- | --- |
| Signaling server | nobody | peer IDs, display names, SDP/ICE blobs (routing metadata), not session content |
| Relay server | nobody | sealed frames, ticket bytes, byte counts; not plaintext |
| Viewer | its user, host's password (entered by user) | host screen, files it accepts |
| Host agent | its owner's password | viewer input events, files it accepts |

What the infrastructure *can* do: correlate peer IDs and timing, refuse service, drop connections. What it *cannot* do: read screens, input, chat, file contents or terminal I/O; inject input (frames are AEAD-authenticated); impersonate a peer (HMAC proof requires the password).

## 6. Threat model

**Considered adversaries:**

1. **Passive network observer** (ISP, Wi-Fi snooper) — defeated by AEAD + key agreement above; sees only sizes/timing.
2. **Malicious relay/signaling operator** — cannot read or modify session content; can DoS (see limitations).
3. **Man in the middle between viewer and host** — authentication is password-based; an MITM that does not know the password cannot produce a valid proof, and cannot hijack after auth without the session key. Note: this model assumes the *user* verifies they are connecting to the intended host (ID/name), as there is no PKI web-of-trust in v0.1.0.
4. **Local attacker on the host** — outside scope for session crypto; mitigations are filesystem-level (config `0600`, see §8).
5. **Replay/injection attacker** — counter+AAD scheme rejects everything except exact in-order continuation.

**Out of scope:** compromises of the endpoints themselves (keyloggers, screen scrapers on the host), side channels, traffic-analysis-resistant cover traffic.

## 7. Known limitations (v0.1.0)

1. **Password storage is plaintext in the agent config** (`agent.toml`, mode `0600`). It must be recoverable to compute Argon2id for every challenge; hashed storage would break unattended verification. OS keychain storage is the planned fix — see roadmap.
2. **No per-user accounts / ACLs.** One access password per host. Role-based access ("view only", "no file transfer") is planned.
3. **No rate limiting / lockout** on auth attempts yet — recommended mitigation while self-hosting: run the server behind a firewall/reverse proxy with rate limiting, and use a strong random password.
4. **WebRTC DTLS certificates are ephemeral** (generated per connection by webrtc-rs). Combined with the password handshake this is acceptable, but pinned/fingerprinted certs would harden the first connection.
5. **TURN credentials** (when using coturn) are shared static/time-limited secrets at the deployment layer; OpacusR does not yet broker per-session TURN credentials.
6. **Strict in-order crypto** means a transport that reorders frames (none today) would require a resync mechanism before use.

## 8. Operational hardening checklist (self-hosting)

- Generate the access password with a password manager (≥ 20 random characters); `opacusr-agent passwd` prompts without echo.
- Keep `agent.toml` permissions at `0600` (default on Unix); on Windows keep it under the user profile, not a shared location.
- Terminate signaling on TLS in production: put `opacusr-server` behind Caddy/nginx (`wss://`) — the server itself speaks plain `ws://`, so TLS is the proxy's job.
- Set a unique coturn `static-auth-secret` (`openssl rand -hex 32`); the bundled config already disables coturn's CLI, see `docker/turnserver.conf`.
- Monitor `/health` and logs (`RUST_LOG=info`); every auth failure is logged by the agent.
- Restrict the relay port (8081) and signaling (8080) at the firewall to the networks that need them.

## 9. Roadmap (security items)

- OS keychain for the unattended password (Windows Credential Manager / macOS Keychain / Secret Service).
- Per-session TURN credential minting (coturn REST API) driven by the signaling server.
- Optional TOFU/pinned host fingerprints shown in the viewer UI.
- Auth attempt rate limiting and lockout in the agent.
- View-only / scoped permission modes.
- Sliding-window replay resync (enables future datagram transports).
