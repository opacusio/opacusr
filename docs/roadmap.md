# OpacusR Roadmap

Where OpacusR is heading. Items are grouped by milestone; security items from [security.md](security.md) §10 are folded in where they belong.

## Done

- **v0.1.0** — WebRTC + TCP/relay transports, JPEG streaming, remote input, file transfer, chat, PTY terminal, unattended access, self-hosted signaling/relay, Docker Compose (server + coturn), CI on three OSes.
- **v0.2.0** — H.264 codec (Cisco OpenH264, screen-content tuned, JPEG fallback, `codec` config), unattended password in the OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service, 0600-file fallback), packaging: deb (agent + viewer with systemd unit and .desktop), Windows MSI (WiX), release pipeline producing tar.gz/zip/deb/msi on every `v*` tag.
- **v0.3.0** — clipboard sync (both directions, echo-suppressed), view-only mode + ACL policy, adaptive quality (RTT/FPS tiers with hysteresis), DXGI Desktop Duplication (Windows) and PipeWire portal capture (Wayland, experimental), multi-monitor selection, audio streaming (WASAPI loopback on Windows, cpal mic on Linux/macOS, Opus 48 kHz mono).
- **main (unreleased)** — startup update check, Wayland fail-fast error, macOS ad-hoc codesign + Gatekeeper first-launch docs, download verification docs (SHA256SUMS + SmartScreen), server hardening: per-IP rate limits, peer/relay-ticket caps, time-limited TURN credentials (`GET /turn`).

## M1 — Media & experience (next)

- **Audio polish**: Linux system-sound capture without manual device picking (PipeWire monitor), Windows per-device selection, viewer-side volume/mute, WASAPI loopback auto-recovery after default-device changes.
- **Wayland hardening**: promote the PipeWire backend to default after field testing; multi-monitor portal dialogs; DmaBuf zero-copy path.
- **Cursor rendering**: compositor cursor overlay (DXGI pointer shapes / X11 cursor events) so remote viewers always see the pointer.
- **Session UX**: fullscreen hotkeys, quality presets, session stats panel (tier, encode time, packet loss).
- **VP9/AV1 evaluation** behind the existing codec traits (web-optimized fallback for slow links).
- **H.264 incremental/region encode** for lower CPU on static screens (dirty-rect driven).

## M2 — Trust & accounts

- **Strong ACL identities**: per-viewer keys / device fingerprints so rules do not depend on self-reported names (today the password is the real gate, see security.md).
- **Agent-side** auth rate limiting & lockout (server-side connection/ticket limits are done); optional TOFU/pinned host fingerprints in the viewer.
- **Automatic per-session TURN credential fetch** in clients (server-side minting at `GET /turn` is done).
- **Windows service / macOS launchd** installs for the agent (the systemd user unit and MSI already ship; service-mode is the remaining gap).

## M3 — Scale & operations

- **Server v2**: multi-node signaling with a shared registry (Redis/Postgres), horizontal relay scaling, connection routing by region.
- **TLS everywhere by default**: built-in `wss://` with ACME, or documented single-command reverse-proxy setup (Caddy).
- **Metrics & admin UI**: Prometheus endpoint on the server, session/relay dashboards.
- **Meeting-style features**: multi-viewer observation of one host (viewer count policies), session recording (opt-in, host-side).
- **Auto-update** channel for desktop clients (check-on-start + in-app "update available" notice; signed artifacts already produced by the release pipeline).

## M4 — Mobile & embed

- **Android**: MediaProjection capture, touch-input mapping, FFI bindings for `SessionClient`, published APKs.
- **iOS**: ReplayKit broadcast capture, input bridging where iOS allows, TestFlight builds.
- **Remote keyboard/text input polish** for touch (soft keyboard over `TextInput`).

## Always

- Keep the E2E test green on every new transport/codec.
- Keep the relay unable to read session content.
- Keep self-hosting simple: one binary + one compose file must remain enough.
- AGPL-3.0 stays.

## Non-goals

- Vendor cloud as the default signaling path (a public relay may be *offered* later, but self-hosting remains first-class).
- Closed-source components or SDKs that would compromise AGPL compliance.
- Sneaking telemetry into clients — diagnostics are opt-in and documented.
