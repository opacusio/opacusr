# OpacusR Roadmap

Where OpacusR is heading. Items are grouped by milestone; security items from [security.md](security.md) §9 are folded in where they belong.

## Done

- **v0.1.0** — WebRTC + TCP/relay transports, JPEG streaming, remote input, file transfer, chat, PTY terminal, unattended access, self-hosted signaling/relay, Docker Compose (server + coturn), CI on three OSes.
- **v0.2.0** — H.264 codec (Cisco OpenH264, screen-content tuned, JPEG fallback, `codec` config), unattended password in the OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service, 0600-file fallback), packaging: deb (agent + viewer with systemd unit and .desktop), Windows MSI (WiX), release pipeline producing tar.gz/zip/deb/msi on every `v*` tag.

## M1 — Media & experience (next)

- **Quality adaptation** — drive quality/scale/fps from measured RTT (`Ping/Pong`) and send-queue depth; frame pacing.
- **DXGI Desktop Duplication** capture on Windows (multi-monitor, cursor compositing, higher fps at lower CPU).
- **Wayland capture** via PipeWire/xdg-desktop-portal on Linux.
- **Clipboard sync** end-to-end (`ClipboardText` message exists; wire to platform clipboards with size limits).
- **Session UX**: multi-monitor selection, fullscreen hotkeys, quality presets in the viewer UI.
- **VP9/AV1 evaluation** behind the existing codec traits (web-optimized fallback for slow links).
- **Audio streaming** (microphone + system sound, Opus over a dedicated WebRTC channel) — the single most requested remote-desktop feature after video.

## M2 — Trust & accounts

- **Per-user access control**: multiple credentials per host with scopes (`view-only`, `no-files`, `no-terminal`), session logging with consent prompts.
- **Auth rate limiting & lockout** in the agent; optional TOFU/pinned host fingerprints in the viewer.
- **Per-session TURN credentials** minted by the signaling server (coturn REST API) instead of a shared secret.
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
