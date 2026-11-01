# OpacusR Wire Protocol (v1)

All multi-byte integers are little-endian. Everything above the transport is encoded with `bincode` (serde). This document covers: stream framing, the signaling exchange, the session handshake and the full message reference. Implementations live in `opacusr-proto` and `opacusr-transport`.

## 1. Stream framing

Every frame on a transport (WebRTC data channel, TCP stream, relay leg) is:

```
┌──────────┬───────────────┬──────────────────┐
│ channel  │ payload len   │ payload (bincode │
│ u8       │ u32 (LE)      │ Message)         │
│ 1 byte   │ 4 bytes       │ len bytes        │
└──────────┴───────────────┴──────────────────┘
```

`channel` is `0 = Control`, `1 = Media` (see below). A `FrameDecoder` accumulator handles partial reads on TCP; on WebRTC each datagram already contains exactly one frame.

### Channels

| Tag | Name | Carries |
| --- | --- | --- |
| 0 | Control | auth, input, chat, file transfer, terminal, session control |
| 1 | Media | `VideoFrame` only |

The split exists so a burst of frames can never delay input or file acks. On WebRTC these are two SCTP channels with fixed IDs; on TCP the tag demultiplexes.

## 2. Signaling protocol

Transport: WebSocket to `ws://<server>:8080/signal`. Payloads are bincode `SignalMessage`s. The server is stateless with respect to sessions — it only relays opaque payloads.

```
Host                                Server                    Viewer
────┘ Register {name}               ──►  ┌ registry
◄──  Welcome {peer_id, tick}        ┄┄┘  │
                                    ◄────────────── RequestSession {target}
SessionIncoming {from, name}        ──►  host
└─ accepts
PeerPayload::WebrtcSdp (offer)      ──►  ──►  relay to viewer
PeerPayload::WebrtcSdp (answer)     ◄──  ◄──  relay back
PeerPayload::WebrtcIce (candidates) ◄──►  ◄─►  both ways
     ┌──── ICE succeeded ────┐
     │ direct WebRTC session │
     └───────────────────────┘
     ┌──── ICE failed ───────┐
Viewer RelayAlloc            ──►  ticket=16 random bytes
◄──  RelayTicket {ticket}         │
host  RelayUse {ticket}      ◄──  ──►  notify host
both connect to tcp relay :8081
preamble: [ticket:16][role:1]  (role 0 = host, 1 = viewer)
     └───────────────────────┘
```

Errors surface as `SessionFailed {reason}`; a peer going offline is `PeerGone`.

Heartbeats keep the registry entry alive; the server prunes lapsed hosts.

## 3. Session handshake (inside a transport connection)

Once two peers have a raw transport (WebRTC channel, LAN TCP, or relay leg), all frames are bincode `Message`s. The handshake:

```
Viewer                              Host
────┘ Hello {proto_version, app_version, platform, display_name, is_agent=false}
                                    ◄──  Hello {…, is_agent=true}
AuthChallenge {salt:16, nonce:16, kdf: KdfParams}   ◄──
     Argon2id(password, salt) → PSK
     proof = HMAC-SHA256(PSK, nonce ‖ salt)
AuthResponse {proof: 32}            ──►
     host recomputes; mismatch → AuthResult{ok:false} + close
AuthResult {ok, session_salt:16}    ◄──
     session_key = HKDF-SHA256(PSK, salt=session_salt, info="opacusr-session-v1") → 32
     every following frame is ChaCha20-Poly1305 sealed (SecureSession)
SessionAccept {screen_w, screen_h, view_only, screens[], audio_available}  ◄──   capabilities of the host
```

`proto_version` must match; otherwise the peer rejects with a descriptive `AuthResult`/`SessionReject`. Authentication is always initiated by the viewer toward the host's access password (interactive *and* unattended). See [security.md](security.md) for parameter choices (Argon2id memory/iterations, nonce discipline, replay window).

## 4. Message reference

Serialized with bincode over the framed channels. Grouped by purpose:

### Session control

| Message | Direction | Fields (essentials) | Notes |
| --- | --- | --- | --- |
| `Hello` | both | `proto_version`, `app_version`, `platform`, `display_name`, `is_agent` | first frame both ways |
| `AuthChallenge` | host→viewer | `salt:16`, `nonce:16`, `kdf: KdfParams` | per-session randomness |
| `AuthResponse` | viewer→host | `proof:32` | HMAC over `nonce‖salt` |
| `AuthResult` | host→viewer | `ok`, `error`, `session_salt:16` | salt keys the session |
| `SessionAccept` | host→viewer | `screen_w`, `screen_h`, `codec`, `fps_hint`, `view_only`, `screens: Vec<ScreenInfo>`, `audio_available` | capabilities; `screens` lists selectable monitors (id, name, w×h, active) |
| `SessionReject` | host→viewer | reason | busy / rejected |
| `QualitySet` | viewer→host | jpeg quality, scale | live quality control; disables adaptive quality for the session |
| `LinkStats` | viewer→host | `rtt_ms`, `recv_fps` | sent on every pong; feeds the adaptive-quality controller |
| `SelectScreen` | viewer→host | `id` | switch the streamed monitor (ids from `SessionAccept.screens`) |
| `AudioControl` | viewer→host | `enable` | start/stop the audio stream |
| `Pause(bool)` | viewer→host | — | pause/resume stream |
| `Ping(u64)` / `Pong(u64)` | both | monotonic counter | RTT + liveness |
| `Bye(ByeReason)` | both | reason | clean disconnect |

### Video

| Message | Fields | Notes |
| --- | --- | --- |
| `VideoFrame` | `seq:u64`, `width`, `height`, `keyframe:bool`, `codec: CodecId`, `data` | Media channel; `CodecId::Jpeg` or `CodecId::H264` (v0.2+) |

### Audio (v0.3+)

Fixed wire format: **Opus, 48 kHz mono, 20 ms frames (960 samples), VoIP bitrate** (`opacusr_proto::audio`).

| Message | Direction | Fields | Notes |
| --- | --- | --- | --- |
| `AudioControl` | viewer→host | `enable` | start/stop; only honoured when `SessionAccept.audio_available` |
| `AudioFrame` | host→viewer | `seq:u32`, `data` | Media channel; viewer applies a small prebuffer, silence on underrun |

### Input

| Message | Fields | Notes |
| --- | --- | --- |
| `MouseMove` | absolute x/y in remote-screen pixels | viewer scales from widget size |
| `MouseButton` | button, down/up | left/right/middle |
| `MouseWheel` | delta, axis | horizontal + vertical |
| `KeyEvent` | key code, down/up | enigo key codes |
| `TextInput` | UTF-8 text | used by chat-driven typing and mobile |
| `ClipboardText` | text | clipboard sync both ways (≤512 KiB, echo-suppressed on both sides) |

### Chat

| Message | Fields |
| --- | --- |
| `ChatMessage` | sender name, text, timestamp |

### File transfer

Initiated by either side with `FtOffer`. Chunks flow strictly from sender to receiver; acks flow back on the Control channel.

```
sender                              receiver
────┘ FtOffer {id, name, size, chunk_size, sha256:32}  ──►
◄──  FtAccept {id, offset}        (offset enables resume)
FtChunk {id, seq, data≤64KiB}   ──►   (sliding window ≤ 8 in flight)
◄──  FtAck {id, acked_seq}        cumulative: everything ≤ acked_seq is durably written
FtChunk …                        ──►
◄──  FtComplete {id, ok, hash}    receiver verifies SHA-256
     (either side may abort: FtCancel {id, reason})
```

Out-of-order chunks are buffered (`BTreeMap` by seq) and written in FIFO order; a gap stalls acks (TCP/WebRTC channels are reliable, so this is a safety net, not a loss-recovery mechanism).

### Terminal

| Message | Direction | Fields |
| --- | --- | --- |
| `TermOpen` | viewer→host | requested cols/rows |
| `TermOpened` | host→viewer | ok/err |
| `TermData` | both | raw PTY bytes (viewer keystrokes → host; host output → viewer) |
| `TermResize` | viewer→host | cols, rows |
| `TermClose` | both | — |

The host runs the shell via `portable-pty` (ConPTY on Windows, openpty elsewhere); output is pumped through an unbounded channel so a chatty process cannot block the session loop.

### TCP tunnel (v0.6+, proto v5)

SSH-style local forwarding: the viewer binds `127.0.0.1:<local port>`; every local connection becomes a `TunnelOpen` on the Control channel, and the host connects to the target.

| Message | Direction | Fields |
| --- | --- | --- |
| `TunnelOpen` | viewer→host | id, target host, port |
| `TunnelOpened` | host→viewer | id, ok/err |
| `TunnelData` | both | id, raw bytes |
| `TunnelClose` | both | id |

The host gates every open by its `tunnel` policy: `off` refuses everything, `loopback` (default) requires the target to resolve to a loopback address, `any` is unrestricted. View-only sessions never announce or accept tunnels (`SessionAccept.tunnel`). Closing follows a TCP-style half-close: the side that finishes first sends `TunnelClose`, the peer keeps draining until its own EOF arrives (pump tasks stay alive until both halves are done).

## 5. Versioning

- `proto_version` (u32) in `Hello` must match exactly for now; incompatible bumps will be accompanied by a negotiated fallback in a future release.
- The framed format (`[channel][len][payload]`) is stable; new `Message` variants must be appended to the enum to keep bincode discriminants compatible within a minor version.
