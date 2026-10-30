# Changelog

All notable changes to OpacusR are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
SemVer-ish (breaking protocol changes bump the minor until 1.0).

## [0.2.0] — 2026-10-30

### Added

- **H.264 video** via Cisco OpenH264 (BSD-licensed, compiled from the
  bundled source): screen-content usage profile, bitrate mode with a
  quality→bpp heuristic, forced IDR on session start and every 120 frames,
  automatic re-init on resolution change. JPEG remains the fallback
  (`codec = auto | h264 | jpeg` in `agent.toml`); a build without the
  `h264` feature gracefully degrades to JPEG end-to-end. The E2E test now
  exercises the real H.264 encode→transport→decode path.
- **OS keychain for the unattended password**: `opacusr-agent passwd`
  stores into Windows Credential Manager / macOS Keychain / Linux Secret
  Service (vendored libdbus — no system dbus package needed) and keeps the
  config file password-free; `passwd --file` forces the old 0600-file
  storage; `passwd-clear` wipes both. `opacusr-agent config` shows the
  storage location. The GUI host panel follows the same policy.
- **Packaging**: Debian packages for the agent (with a systemd user unit)
  and the viewer (with `.desktop` entry and hicolor icon); a Windows MSI
  (WiX v3: viewer + agent + server, Start-menu shortcut, embedded icon);
  a `Release` workflow that builds tar.gz/zip/deb/msi and publishes a
  GitHub Release on every `v*` tag.
- Application icon (PNG/ICO), embedded into the Windows executable.
- `CHANGELOG.md` (this file).

### Changed

- `CodecId` gained an `H264` variant; `SessionAccept` may now announce it.
  Bincode discriminants stay compatible (append-only enum).
- `opacusr-agent` config gained `codec` and `password_in_keychain` keys;
  old configs load unchanged (serde defaults).
- Workspace version bumped to 0.2.0.

### Fixed

- CI on Windows: `Win32Capture` now implements `Send` (safe: the `HWND`
  is process-wide and GDI resources live within one capture call), an
  explicit `Default` impl and a fully-initialised `RGBQUAD` satisfy
  clippy; the unused `allow_test_fallback` on the Windows path is
  silenced with an explicit `let _ =`.
- CI on macOS: `Insert` / `NumLock` / `PrintScreen` key mappings are
  compiled out (enigo 0.6 has no such `Key` variants on macOS); the
  agent reports them as unsupported keys instead of failing the build.

## [0.1.0] — 2026-10-29

### Added

- Rust workspace of 10 crates: proto, crypto, transport (WebRTC via
  webrtc-rs + TCP direct + TCP relay + chunking + secure session +
  file-transfer engine + signaling client), capture (X11 / Win32 GDI /
  test source), codec (JPEG), input (enigo + test doubles), server (axum
  WebSocket signaling + TCP relay hub), agent (sessions, PTY, config,
  CLI), client (viewer state machine), app (eframe/egui GUI).
- End-to-end encryption on every transport: Argon2id PSK, HMAC-SHA256
  challenge-response, HKDF → ChaCha20-Poly1305 with counter nonces and
  strict replay rejection.
- Features: screen streaming (JPEG), remote mouse/keyboard, resumable
  SHA-256-verified file transfer (64 KiB chunks, sliding window 8), chat,
  interactive terminal (ConPTY/openpty), unattended access, attended-mode
  consent prompts.
- Self-hosted signaling + relay server; Docker Compose bundle with coturn
  (STUN/TURN).
- Documentation: architecture, wire protocol, security/threat model,
  platform matrix, roadmap; bilingual README (EN/RU).
- CI (fmt + clippy + tests on Linux/Windows/macOS); 35 tests including a
  full in-process E2E session.
- AGPL-3.0 license.
