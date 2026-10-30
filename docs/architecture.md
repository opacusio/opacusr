# OpacusR Architecture

This document describes how OpacusR is structured: the crate graph, the data flows for a live session, the three transports and the threading model. The wire format itself is specified in [protocol.md](protocol.md).

## 1. The big picture

OpacusR is a Cargo workspace with ten crates, layered from the wire up:

```
┌─────────────────────────────────────────────────────────────┐
│  opacusr-app        eframe/egui GUI (viewer + host panel)   │
├─────────────────────────────────────────────────────────────┤
│  opacusr-client     viewer session state machine            │
│  opacusr-agent      host daemon: sessions, PTY, config, CLI │
├─────────────────────────────────────────────────────────────┤
│  opacusr-transport  WebRTC / TCP / relay + chunking +       │
│                     secure session + file transfer engine   │
├─────────────────────────────────────────────────────────────┤
│  opacusr-capture    opacusr-codec    opacusr-input          │
│  screen grab        JPEG codec       input injection        │
├─────────────────────────────────────────────────────────────┤
│  opacusr-crypto     PSK derivation, session crypto          │
│  opacusr-proto      messages, framing, signaling payloads   │
└─────────────────────────────────────────────────────────────┘
        opacusr-server (standalone: signaling + TCP relay hub)
```

Dependency rules (enforced by crate boundaries):

- `proto` and `crypto` depend on nothing but serialization/KDF crates.
- `transport` depends on `proto` + `crypto` and knows *nothing* about screens.
- `capture`, `codec`, `input` are peers; the session layer wires them together.
- `client` (viewer logic) and `agent` (host logic) are symmetric around `transport`.
- `app` owns the GUI event loop and drives `client`/`agent` through typed command/event channels.

## 2. Session data flow (host side)

`opacusr-agent/src/session.rs` runs one connected host session. It is a `tokio::select!` loop over four event sources:

1. **Transport inbound** — decrypted `Message`s from the viewer (input, chat, file-transfer, terminal, quality changes).
2. **Capture tick** — at `fps` cadence, grab the screen (`opacusr-capture`), encode JPEG (`opacusr-codec`), send `VideoFrame` on the Media channel.
3. **Input events** — from the GUI viewer, forwarded into `opacusr-input` injection.
4. **PTY output** — terminal bytes from `opacusr-agent/src/pty.rs` (a `portable-pty` reader pumping an unbounded channel so a chatty shell never blocks the session loop).

Files are handled by `opacusr-transport/src/ft.rs`: `FtEngine` chunks at 64 KiB, keeps a sliding window of 8 in-flight chunks, processes cumulative acks and buffers out-of-order arrivals in a `BTreeMap`, then verifies SHA-256 on completion. Resume is supported via the `offset` in `FtAccept`.

## 3. Session data flow (viewer side)

`opacusr-client/src/session.rs` (`SessionClient`) mirrors the host: a command channel (`Command`) in, an event channel (`ClientEvent`) out, one `run` loop consuming both plus transport inbound. The GUI (`opacusr-app/src/view.rs`) renders frames as they arrive, scales remote pixels to the widget, converts local widget input to `MouseMove/MouseButton/MouseWheel/KeyEvent` messages and forwards chat/files/terminal traffic. `Ping/Pong` round-trips are timed to display RTT.

`connect_via_server` and `connect_direct` produce the same `Transport` object from either path, so the rest of the viewer is transport-agnostic.

## 4. Transports

All transports implement one trait (`opacusr-transport/src/lib.rs`):

```rust
#[async_trait]
pub trait Transport: Send {
    async fn send(&self, frame: &[u8]) -> Result<()>;
    async fn recv(&self) -> Result<Vec<u8>>;
    // ...
}
```

### 4.1 WebRTC (preferred)

`webrtc.rs` on **webrtc-rs 0.14**. Two ordered/reliable data channels:

- `Control` — auth, input, chat, files, terminal (low volume, latency-sensitive).
- `Media` — video frames (high volume, droppable content) so bursts never delay control traffic.

webrtc-rs 0.14 has no *negotiated* channels, so the standard pattern is used: the offerer creates channels with fixed IDs; the answerer binds to the same IDs via `on_data_channel` after `accept_offer`. A subtlety handled in code: the answerer's channel callbacks fire only after DTLS is up, and `announce` arrives after that — so channel collection happens in `finish()`, not at offer time (the `AnswerPending` pattern). Messages larger than ~30 KiB are split by `chunk.rs` (`Chunker`/`Reassembler`) because SCTP message size limits are browser-parity, not spec-mandated.

### 4.2 TCP direct (LAN)

`tcp.rs`: plain stream framing, used when viewer and host are reachable directly (same network or port-forward). The agent listens on `lan_port` (default 54000) when started with `--allow-lan`.

### 4.3 TCP relay (fallback)

`relay.rs` + server-side `RelayHub` (`opacusr-server/src/lib.rs`). When ICE fails, the viewer asks signaling for `RelayAlloc`, gets a random 16-byte ticket, and the host connects to the relay port with the same ticket. The hub keeps two half-connections per ticket and copies bytes each way with two independent copy tasks (a deliberate replacement for `tokio::io::copy_bidirectional`, which tears down both directions on a single EOF). Preamble on the wire: `[ticket:16][role:1]` where role is `0 = host`, `1 = viewer`.

### 4.4 Secure session wrapper

`secure.rs` wraps *any* transport in `SecureSession`: every frame is sealed with ChaCha20-Poly1305 under a key derived by HKDF from the authenticated PSK (see [security.md](security.md)). `auth.rs` implements the viewer/host handshake over the Control channel before any other traffic.

## 5. Signaling

`opacusr-server/src/lib.rs` runs two listeners:

- **WebSocket (axum 0.8), port 8080** — `/signal` for peers, `/health` for monitoring. Messages are bincode-encoded `SignalMessage`s: `Register` → `Welcome(peer_id)`; `RequestSession` → `SessionIncoming`; SDP/ICE are relayed opaquely inside `PeerPayload::WebrtcSdp/WebrtcIce`; `RelayUse` tells both sides to fall back to the relay.
- **TCP relay, port 8081** — `RelayHub`, described above.

The registry maps `PeerId → connection`, prunes hosts whose heartbeat has lapsed, and never touches session content.

## 6. Threading model

- **GUI**: `eframe` owns the main thread; a tokio runtime is created inside the app and handles are shared with the UI through `Arc<Mutex<…>>`/watch channels. Session handles (`SessionHandle`, `HostHandle`) expose `pub` fields intentionally so the view layer can drive them.
- **Agent**: one tokio task per session, plus the capture tick and the PTY pump; the CLI (`run/passwd/passwd-clear/id/config`) is a thin front over the same library code, which is what makes the E2E test possible without a GUI.
- **Server**: one task per WebSocket connection and per relay half; shared state is `Arc<AppState>` with a `Mutex<HashMap>` registry.

## 7. Testing strategy

- Unit tests per crate (framing round-trips, crypto seal/open/replay, chunker, config).
- Server integration tests (`crates/opacusr-server/tests/integration.rs`) — registry + relay behavior.
- WebRTC loopback tests (`crates/opacusr-transport/tests/loopback.rs`) — real DTLS handshake in-process, 100 KiB chunks.
- **E2E** (`crates/opacusr-agent/tests/full_session.rs`) — a viewer and a host in the same process over TCP: authentication, 5 real frames at 320×200, input injection through a `SharedInjector` test double, chat, a 129 KiB file upload verified by SHA-256, a PTY terminal session with an output marker, RTT measurement and clean disconnect.
