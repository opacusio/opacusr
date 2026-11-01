# Changelog

All notable changes to OpacusR are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
SemVer-ish (breaking protocol changes bump the minor until 1.0).

## [Unreleased]

### Added

- **Opt-in session recording (host-side)** — a host checkbox
  (`Host tab → Recording`) or `opacusr-agent --record` muxes every
  accepted session to a timestamped MP4
  (`<record_dir>/<date>-<time>-<peer>.mp4`, default
  `~/OpacusR-Recordings`): the encoded H.264 bitstream is repackaged
  Annex-B → AVCC and written sample-by-sample with real frame timings
  (90 kHz timescale, no transcoding). Streams rotate to `-partN` files
  on monitor-switch resolution changes; recording starts at the first
  keyframe and stops fail-open (the session itself never breaks).
  Viewers see a `● REC` indicator — `SessionAccept` gained a
  `recording` flag, `PROTO_VERSION` bumps to 4. JPEG-codec sessions are
  announced as unrecordable; audio is not muxed yet.
- **TOTP two-factor authentication (optional)** — hosts can enable a
  second factor (`Host tab → Two-factor (TOTP) → Enable`): a random
  base32 secret is generated and copied to the clipboard
  (`otpauth://totp/OpacusR?secret=…` for authenticator apps; RFC 6238,
  HMAC-SHA1, 30 s step, 6 digits, ±1 window for clock drift). Viewers
  then type a 6-digit code in the Connect tab; hosts verify it *in
  addition* to the password and feed wrong codes into the same
  lockout as wrong passwords. `PROTO_VERSION` bumps to 3 (old peers are
  refused cleanly). Verified against the RFC 6238 test vectors.
- **Viewer address book** — save contacts (label, 9-digit ID or `ip:port`,
  optional per-contact signaling endpoint tried before the global list),
  favorites, one-click connect from the list. Stored locally in
  `addressbook.toml` next to `agent.toml` (atomic tmp+rename writes;
  passwords are never stored).
- **Connection history** — the last 100 connection attempts with UTC
  timestamp, target, outcome (connected/failed) and session duration;
  viewable and clearable in the Connect tab.
- **Contact renaming** — every saved contact gets a `✎` button: edit the
  name inline (Enter or ✔ to apply, ✖ to cancel) and give the remote your
  own label instead of the ID-derived default. Target, endpoint and
  favorite are untouched.
- **Session statistics panel** — toggle `📊` in the session toolbar (or
  `Ctrl+Shift+S`): remote name, stream resolution, negotiated codec,
  actually received fps vs the host's fps hint, compressed wire bitrate
  (2 s sliding window) and RTT.
- **Session hotkeys** — `F11` fullscreen, `Ctrl+Shift+P` pause,
  `Ctrl+Shift+D` disconnect, `Ctrl+Shift+C/F/T/S` toggle the
  chat/files/terminal/stats panels. Letter shortcuts are ignored while a
  text field has focus. `ClientEvent::Connected` now carries the negotiated
  codec + fps hint; `ClientEvent::Frame` carries the compressed wire size.
- **Host → viewer file send** — while a session is live the Host tab offers
  `⬆ Send file to viewer`: a per-session command channel feeds
  `HostCommand::SendFile` into the agent session loop and the same FtEngine
  drives the push (sha256-verified on arrival). The viewer's Files panel
  gains a `⬆ Send files…` picker next to drag & drop.

## [0.5.0] — 2026-11-01

### Added

- **Host physical-input lock** (`--lock-input` / `lock_input` in `agent.toml` / GUI
  checkbox) — TeamViewer-style: while a viewer has full control the host's
  physical keyboard/mouse is swallowed while input injected through the session
  still passes. Windows uses low-level hooks (only non-injected events are
  dropped; **Ctrl+Alt+Del stays available**), Linux grabs `/dev/input/event*`
  (`EVIOCGRAB`, needs the `input` group). View-only sessions never lock; the
  lock is fail-open (a platform that cannot take it warns and continues) and
  crash-safe (released on drop and on process death). macOS: not yet.
- **Audio-free builds** — `cpal` is optional behind the existing `audio`
  feature on both the agent and the client, so `cargo build
  --no-default-features` and the test suite work on hosts without ALSA dev
  headers (the GUI opts back in via `features = ["audio"]`).
- **Zero-infra networking, phases 1–2** ([docs/serverless.md](docs/serverless.md)):
  - **Multi-endpoint signaling fan-out on the agent** — `signaling_urls` in
    `agent.toml`: the agent registers on every endpoint simultaneously, one
    supervised reconnect loop per endpoint (capped exponential backoff), so a
    flapping endpoint never takes the others down. Each endpoint assigns its
    own ID; `opacusr-agent id` prints all of them. The signaling accept path
    shares one auth-failure budget across all endpoints (lockout unchanged).
  - **Embedded signaling server in the agent** — `signal_listen = true` (or
    `opacusr-agent run --signal-listen`) serves the peer registry, `/health`,
    `GET /turn` and the TCP relay inside the agent process
    (`signal_listen_port`, relay on port +1; same env knobs as the standalone
    server: `OPACUSR_RATE_MAX`, `OPACUSR_MAX_PEERS`, `OPACUSR_TURN_SECRET`).
    The agent registers on the local endpoint automatically — no separate
    server binary needed; expose it behind a free tunnel for remote access.
  - **Viewer-side endpoint ladder** — the GUI tries the configured signaling
    endpoints in order (per-endpoint TURN credential minting, 25 s budget
    each) and the first handshake that completes wins; per-endpoint progress
    and failures show up in the session status line.
  - **Tunnel cookbook** — ready-to-copy cloudflared and Tailscale configs
    for the zero-infra setup (docs/serverless.md §7).


## [0.4.0] — 2026-10-31

### Added

- **Server hardening: rate limits + time-limited TURN credentials** — per-IP limits on new signaling connections and `/turn` requests (`OPACUSR_RATE_MAX`, default 20 per 10 s), a registered-peer cap (`OPACUSR_MAX_PEERS`, default 1024), relay-ticket caps (8 per peer, 4096 server-wide), and `GET /turn` minting coturn REST-API credentials from `OPACUSR_TURN_SECRET` (HMAC-SHA1, `OPACUSR_TURN_TTL_SECS`).

- **Agent-side auth lockout** — failed password proofs are counted per accept path (signaling / LAN): after `auth_max_attempts` (default 5) failures within `auth_window_secs` (default 300 s) the agent refuses new handshakes for `auth_lockout_secs` (default 60 s, doubling per consecutive lockout, capped at 15 min; `0` disables). The budget is shared across all peers of the path, so rotating peer IDs or display names no longer helps an online brute-force — with defaults it is capped at ~20 guesses per hour even under sustained attack. Tunables in `agent.toml`; every failure and lockout is logged.

- **Automatic time-limited TURN credentials in clients** — the agent and the GUI viewer fetch the coturn REST pair from the server's `GET /turn` at WebRTC session setup (`turn_api = true` in `agent.toml`), cache it for the server-declared TTL (a 503 disables the fetch for the run), and fall back to STUN-only on any failure. Completes the `OPACUSR_TURN_SECRET` flow: coturn serves per-session credentials instead of a shared static secret.


- **Docs: checksum verification on every OS + Windows SmartScreen first run** —
  README.md / README.ru.md gain per-OS verification commands (`sha256sum -c`,
  `shasum -a 256 -c`, PowerShell `Get-FileHash`, `certutil -hashfile`) and the
  expected SmartScreen prompt for the unsigned MSI with the More info ->
  Run anyway steps.


- **macOS: ad-hoc signing + Gatekeeper docs** — release binaries are ad-hoc
  codesigned (`codesign --force --sign -`) in the release workflow before
  packaging, so they run on Apple Silicon; README.md / README.ru.md document
  the expected one-time Gatekeeper prompt and the ways to clear it (`xattr
  -cr`, right-click Open, Privacy & Security -> Open Anyway).

- Update check: the GUI app and the agent query GitHub for the latest published release at startup and show a notice when a newer version is available. No download and no auto-install; disable with OPACUSR_NO_UPDATE_CHECK=1.

- **Docs: zero-infra networking design** ([docs/serverless.md](docs/serverless.md)) — how to run OpacusR without renting servers: automatic connection ladder (LAN direct → P2P/STUN → TURN → TCP relay), signaling embedded in the agent behind free tunnels (cloudflared / Tailscale Funnel), mesh-VPN mode, mDNS discovery and manual "meeting code" fallback; phased implementation plan.


### Changed
- Wayland hosts fail fast with a clear error (PipeWire portal feature or an X11 session; `OPACUSR_ALLOW_X11_ON_WAYLAND=1` forces the old behavior) instead of a silent blank XWayland capture, and agents print a startup warning under Wayland.

### Fixed
- **An agent with no configured password no longer accepts an empty one.** The host previously verified proofs against an empty string when no unattended password was set (`effective_password().unwrap_or_default()`), so a viewer submitting an empty password could authenticate in attended mode (e.g. direct LAN sessions). Now the missing password propagates as `None` and the handshake is rejected with "no password configured on the host".

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
