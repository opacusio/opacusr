# OpacusR

[![Release](https://img.shields.io/github/v/release/opacusio/opacusr)](https://github.com/opacusio/opacusr/releases/latest)
[![CI](https://github.com/opacusio/opacusr/actions/workflows/ci.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/ci.yml)
[![Release build](https://github.com/opacusio/opacusr/actions/workflows/release.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/opacusio/opacusr)](LICENSE)

**Open-source remote desktop** — screen sharing, control, file transfer, chat and terminal over a self-hostable transport. Built in Rust: an alternative to TeamViewer / AnyDesk where *you* own the infrastructure.

> 🇷🇺 Russian documentation: [README.ru.md](README.ru.md)

- **License:** AGPL-3.0
- **Status:** v0.3.0 — working core, actively developed
- **Platforms:** Linux (X11 / Wayland via PipeWire), Windows (DXGI/GDI), macOS (partial), Android/iOS (skeleton)
- **Repository home:** [github.com/opacusio](https://github.com/opacusio)

---

## Why OpacusR

Commercial remote-desktop tools route your sessions through the vendor's cloud. OpacusR is designed so that the **signaling and relay servers are yours**: one small binary (or two Docker containers), WebRTC data channels for direct peer-to-peer traffic, and an encrypted TCP relay as the automatic fallback when a direct connection is impossible.

### Feature set (MVP, implemented)

| Feature | State |
| --- | --- |
| Screen streaming — H.264 (Cisco OpenH264, screen-content tuned) with JPEG fallback; adjustable quality / scale / fps | ✅ |
| **Adaptive quality** — RTT/FPS-driven tiers, fast-degrade / slow-upgrade hysteresis, manual override wins | ✅ |
| **Multi-monitor** — enumerate + switch monitors mid-session (X11 RandR, GDI, DXGI) | ✅ |
| **Clipboard sync** — both directions with echo suppression | ✅ |
| **View-only mode + ACL** — full / view-only / deny per session and per viewer | ✅ |
| **Audio** — Windows: system sound (WASAPI loopback); Linux/macOS: mic; Opus 48 kHz mono ~32 kbps | ✅ |
| Remote mouse & keyboard control | ✅ |
| File transfer (chunked, resumable, SHA-256 verified) | ✅ |
| Chat | ✅ |
| Interactive terminal (PTY: ConPTY / openpty) | ✅ |
| Unattended access (password, challenge-response) | ✅ |
| Password in the OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service), 0600-file fallback | ✅ |
| WebRTC transport (webrtc-rs, DTLS-secured data channels) | ✅ |
| TCP relay fallback + direct LAN TCP | ✅ |
| E2E encryption on every transport (ChaCha20-Poly1305) | ✅ |
| Signaling + relay self-hosted server | ✅ |
| Docker Compose bundle (signaling + coturn) | ✅ |
| Packages: deb (agent + viewer, systemd unit, .desktop) · MSI (WiX) · release CI | ✅ |
| Capture: DXGI Desktop Duplication (Windows), X11 (Linux), Wayland via PipeWire portal (experimental) | ✅ |

## Quick start

### 1. Run a server (self-hosted)

```bash
# from source
cargo run --release -p opacusr-server
# signaling : ws://0.0.0.0:8080/signal
# health    : http://0.0.0.0:8080/health
# tcp relay : 0.0.0.0:8081

# or with Docker (server + coturn STUN/TURN)
cd docker
sed -i "s/CHANGE_ME_opacusr_turn_secret/$(openssl rand -hex 32)/" turnserver.conf
docker compose up -d
```

### 2. Host side (the machine to be controlled)

```bash
cargo run --release -p opacusr-agent -- run --server ws://your-server:8080/signal

# unattended access
opacusr-agent passwd            # prompts for a password
opacusr-agent id                # print this host's registered ID
opacusr-agent config            # print config location & summary
```

Settings live in `~/.config/opacusr/agent.toml` (Linux), `%APPDATA%\opacusr\agent.toml` (Windows), `~/Library/Application Support/opacusr/agent.toml` (macOS). Notable keys: `server_url`, `name`, `password` / `password_in_keychain`, `fps`, `jpeg_quality`, `scale_percent`, `auto_accept_files`, `download_dir`, `lan_port`, `ice_urls`, `codec` (`auto` | `h264` | `jpeg`), `clipboard_sync`, `adaptive_quality`, `audio`, `audio_device`, `capture` (`auto` | `gdi` | `dxgi`, Windows), `default_permission` (`full` | `view-only` | `deny`), `acl_rules` (per-viewer overrides).

### 3. Viewer side (the controller)

```bash
cargo run --release -p opacusr-app
```

In the GUI: enter the signaling URL, connect, pick the host from the list (or type its ID), enter the access password, and the remote desktop opens. The session window includes the video stream, chat, file-transfer and terminal panels.

### Installing (packages)

**Download:** [latest release](https://github.com/opacusio/opacusr/releases/latest) вЂ” `.deb` for Debian/Ubuntu, `.msi` / `.zip` for Windows, `.tar.gz` for macOS and portable Linux. Every asset has a SHA-256 checksum in `SHA256SUMS` (verify with `sha256sum -c SHA256SUMS`).

| Platform | Package | Notes |
| --- | --- | --- |
| Debian/Ubuntu | `opacusr-app_*.deb`, `opacusr-agent_*.deb` | viewer ships a `.desktop` entry + icon; agent ships a systemd **user** unit (`systemctl --user enable --now opacusr-agent`) |
| Windows | `opacusr-*-x64.msi` | installs viewer + agent + server into `Program Files\OpacusR\bin`, Start-menu shortcut, icon embedded |
| macOS / portable | `.tar.gz` per release | unpack and run |

Assets are produced automatically on every `v*` tag by [.github/workflows/release.yml](.github/workflows/release.yml) (Linux deb + tar.gz, Windows MSI + zip, macOS tar.gz). To build locally: `cargo deb -p opacusr-app -p opacusr-agent` (needs `cargo install cargo-deb`); MSI: see `wix/main.wxs` header.

### Building

```bash
cargo build --release          # everything
cargo test  --workspace        # 40 tests incl. a full E2E session (H.264 in the loop)
```

Rust 1.75+ (edition 2021). Linux builds need X11 client libraries (`libx11-dev`, `libxi-dev` on Debian/Ubuntu) and `libasound2-dev` (audio); NASM is optional (speeds up the bundled OpenH264). Windows capture uses DXGI Desktop Duplication with a GDI fallback (`--capture gdi` to force); macOS uses core-graphics where available (see [docs/platforms.md](docs/platforms.md)).

Optional builds:

```bash
cargo build --release --features opacusr-capture/pipewire   # Wayland via the portal (needs libpipewire-0.3-dev, libspa-0.2-dev)
cargo build --release --no-default-features                 # no audio (e.g. windows-gnu cross builds)
```

## Architecture in one minute

```
┌──────────┐  signaling (WS)  ┌────────────┐  signaling (WS)  ┌──────────┐
│  Viewer  │◄────────────────►│  opacusr-  │◄────────────────►│   Host   │
│  (GUI)   │                  │   server   │                  │  (agent) │
│          │   WebRTC data channels (preferred, DTLS)        │          │
│          │◄═══════════════════════════════════════════════►│          │
│          │   TCP direct (LAN) or TCP relay :8081 (fallback)│          │
└──────────┘◄═══════════════════════════════════════════════►└──────────┘
```

- **Signaling** brokers introductions: hosts `Register`, viewers `RequestSession`, both receive each other's SDP/ICE via the server.
- **WebRTC** carries two data channels: `Control` (auth, input, chat, files, terminal) and `Media` (video frames) so a frame burst never delays input.
- **TCP relay** is the fallback: same wire format, multiplexed by 16-byte tickets; the server copies bytes both ways without reading them (they are already E2E-encrypted).
- **Everything above the transport is E2E-encrypted**: Argon2id derives a PSK from the access password, the viewer proves it with an HMAC-SHA256 challenge response, HKDF derives a ChaCha20-Poly1305 session key with counter nonces and replay protection.

Details: [docs/architecture.md](docs/architecture.md) · wire format: [docs/protocol.md](docs/protocol.md) · threat model: [docs/security.md](docs/security.md).

## Workspace layout

| Crate | Purpose |
| --- | --- |
| `opacusr-proto` | Messages, framing, signaling payloads (bincode) |
| `opacusr-crypto` | PSK derivation, challenge-response, session crypto |
| `opacusr-transport` | WebRTC / TCP / relay transports, chunking, secure session, file-transfer engine, signaling client |
| `opacusr-capture` | Screen capture: X11 (RandR multi-monitor), DXGI Desktop Duplication + GDI (Windows), PipeWire portal (Wayland, experimental), test source |
| `opacusr-codec` | JPEG + H.264 (OpenH264) video codecs behind a `VideoEncoder/Decoder` trait |
| `opacusr-input` | Input injection (enigo), clipboard worker, test doubles |
| `opacusr-server` | axum WebSocket signaling + TCP relay hub |
| `opacusr-agent` | Host daemon: sessions, PTY, audio capture, adaptive quality, ACL, CLI |
| `opacusr-client` | Viewer session state machine (frames, input, audio playback, clipboard) |
| `opacusr-app` | eframe/egui desktop GUI (viewer + host panel) |

## Security notes (short version)

- Passwords are never transmitted; the host verifies an HMAC proof of a per-session nonce.
- Session keys are unique per session (per-session salts); nonce counters prevent replay.
- The relay server never sees plaintext — it forwards sealed frames.
- Config files with credentials are written with mode `0600`.

Read [docs/security.md](docs/security.md) for the full threat model and roadmap items (TURN credentials handling, rate limiting).

## Documentation

| Document | Content |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | Crates, data flow, transports, threading model |
| [docs/protocol.md](docs/protocol.md) | Framing, signaling handshake, session handshake, message reference |
| [docs/security.md](docs/security.md) | Crypto design, threat model, known limitations |
| [docs/platforms.md](docs/platforms.md) | Per-platform capture/input status and build notes |
| [docs/roadmap.md](docs/roadmap.md) | Planned features: VP9/AV1, audio routing, mobile UI… |

## Contributing

PRs are welcome at [github.com/opacusio/opacusr](https://github.com/opacusio/opacusr). Run `cargo fmt` and `cargo clippy --workspace` before submitting; CI enforces both plus the test suite on Linux, Windows and macOS.

## License

AGPL-3.0 — see [LICENSE](LICENSE). By contributing you agree that your contributions are licensed under the same license.
