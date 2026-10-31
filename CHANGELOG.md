# Changelog

All notable changes to OpacusR are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
SemVer-ish (breaking protocol changes bump the minor until 1.0).

## [0.3.1] — 2026-10-31

Release engineering, documentation and community tooling. **No functional
changes to the `opacusr-agent`, `opacusr-server` or GUI app binaries** —
upgrading is optional.

### Added

- All release artifacts now ship with a `SHA256SUMS` checksum file;
  verify your download with `sha256sum -c SHA256SUMS`.
- The Release workflow smoke-tests every build (`--version` for the agent
  and server binaries) on Linux, Windows and macOS before publishing, so
  a broken build can no longer reach a release.
- `SECURITY.md`: security policy with private vulnerability reporting,
  a 72-hour acknowledgment target, and the scope (transport crypto,
  relay, password flow).
- GitHub issue templates for bug reports and feature requests; blank
  issues are disabled.
- Categorized release-note configuration (`.github/release.yml`) and the
  release body now generated from this changelog (`body_path`).

### Changed

- `README.md` / `README.ru.md`: added Release / CI / Release build /
  License badges and a Download section.

### Fixed

- Linux: release binaries failed to start with "libopus.so.0: cannot open
  shared object file" — they linked a vendored shared Opus without an
  embedded runtime path. The new release smoke test caught this; the
  v0.3.0 Linux tar.gz was affected the same way. Opus is now built as a
  static PIC library (as on macOS), so the binaries, the tar.gz and the
  .deb packages are self-contained and no longer require a system Opus.

## [0.3.0] — 2026-10-30

Protocol version 2 (`SessionAccept` gained `view_only`/`screens`/
`audio_available`; new messages `LinkStats`, `SelectScreen`,
`AudioControl`, `AudioFrame`). Mixed 0.2/0.3 versions are rejected
cleanly with a version-mismatch message.

### Added

- **Clipboard sync** — both directions with echo suppression, driven by a
  dedicated clipboard worker thread (`opacusr-input::clipboard`); 512 KiB
  size cap; live toggle in the viewer toolbar; `clipboard_sync` in
  `agent.toml`. Headless hosts degrade to no-op.
- **View-only sessions + ACL** — the host-side grant is authoritative:
  input is ignored and file/terminal requests are explicitly refused in
  view-only sessions. Consent is now three-way (full / view-only / deny)
  in both the GUI dialog and the CLI prompt. Unattended policy:
  `default_permission = full | view-only | deny` plus per-viewer
  `acl_rules` (matched on the self-reported display name — the session
  password remains the real gate).
- **Adaptive quality** — the viewer reports `LinkStats` (RTT + received
  FPS) on every pong; the host maps reports onto four quality tiers with
  fast-degrade / slow-upgrade hysteresis. A manual `QualitySet` disables
  adaptation for the session. `adaptive_quality` in `agent.toml`.
- **Windows: DXGI Desktop Duplication capture** — frames straight from
  DWM (hardware overlays, fullscreen apps), BGRA→RGBA readback through a
  staging texture, idle-timeout frame caching, automatic rebuild on
  `ACCESS_LOST` (UAC prompts, desktop switches). Backend selection via
  `capture = auto | gdi | dxgi` and `--capture`; GDI remains the fallback.
- **Wayland: PipeWire screencast (experimental)** — behind the non-default
  `pipewire` cargo feature: xdg-desktop-portal ScreenCast consent, then a
  PipeWire consumer stream (BGRx/RGBA/RGB). A native Wayland session
  prefers the portal and falls back to X11/XWayland.
- **Multi-monitor** — backends enumerate monitors (`ScreenCapture::screens`,
  RandR on X11, `EnumDisplayMonitors` on GDI, DXGI adapter outputs);
  the viewer picks one in the toolbar (`SelectScreen`); input rescaling
  follows the streamed monitor.
- **Audio streaming** — Opus 48 kHz mono, 20 ms frames, ~32 kbps.
  Windows: WASAPI shared-mode *loopback* of the default output device
  (the machine's actual sound). Linux/macOS: `cpal` capture (microphone,
  or a Pulse monitor by name via `audio_device =`). The viewer plays
  through the default output with a small prebuffer; toolbar toggle when
  the host advertises `audio_available`.
- `cargo-deb` agent package now depends on `libasound2`; CI installs
  `libasound2-dev` on Linux.

### Changed

- `SessionAccept` extended (see protocol v2 above); old peers are refused
  with a clean version mismatch.
- `opacusr-agent --run` gained `--capture auto|gdi|dxgi`.
- Agent audio is a default cargo feature (`audio`); opt out with
  `--no-default-features` where the vendored opus build is unavailable
  (e.g. windows-gnu cross builds). Windows MSVC links the prebuilt opus
  shipped by `audiopus_sys`.

### Fixed

- Windows MSI: the WiX authoring used the WiX v4 element
  `<StandardDirectory>`, which the WiX v3.11 toolchain installed by the
  Release workflow rejects with CNDL0005 (and light then failed on the
  missing `main.wixobj`, LGHT0103). Rewritten with the classic
  `TARGETDIR` directory tree and all components marked `Win64` (avoids
  ICE80 for files under `ProgramFiles64Folder`). The MSI product version
  now comes from the release tag (`candle -dMsiVersion=<tag>` with a
  fallback default), so it no longer has to be bumped by hand.
- Windows build: the WASAPI loopback code in `opacusr-agent/src/audio.rs`
  did not compile on Windows. The `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE`
  imports were function-local while `parse_wave_format` needed them too
  (E0425); `AUDCLNT_BUFFERFLAGS_SILENT` is generated as `i32` by the
  `windows` crate and was ANDed with a `u32` counter (E0308/E0277); and
  rustc (E0793) rejects reading the `SubFormat` field of the packed
  `WAVEFORMATEXTENSIBLE` through a reference. Imports moved to module
  scope, the flag is cast to `u32`, the GUID is read via
  `addr_of!` + `read_unaligned`.
- macOS build: `audiopus_sys` fell back to building the vendored Opus
  with autotools, but macOS runners ship no `autoreconf`, so the build
  script panicked ("Failed to autogen Opus"). CI and Release now install
  `opus` + `pkg-config` via Homebrew first; `audiopus_sys` probes
  pkg-config before vendoring and links the system Opus.
- Release (macOS): the tar.gz job now builds Opus from the official
  release tarball (`./configure --enable-static`, ships a pre-generated
  `configure`, so no autotools are needed) instead of the Homebrew
  bottle. The binaries link Opus statically and run on machines without
  Homebrew; CI keeps the brew install, which is enough for clippy and
  tests.

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
  signaling + relay), agent (PTY, unattended), client (viewer session
  logic), app (egui GUI).
- Security model: Argon2id PSK → HMAC-SHA256 proof → HKDF-SHA256 →
  ChaCha20-Poly1305 with counter nonces; replay = fatal.
- Docker Compose (signaling + coturn), CI matrix (Linux/Windows/macOS),
  bilingual README, docs (architecture, protocol, security, platforms,
  roadmap), AGPL-3.0.
