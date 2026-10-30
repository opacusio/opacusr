# OpacusR Roadmap

Where OpacusR is heading after v0.1.0. Items are grouped by milestone; security items from [security.md](security.md) §9 are folded in where they belong.

## M1 — Media & experience (next)

- **Better video codecs.** The `VideoEncoder`/`VideoDecoder` traits already isolate the codec. Add hardware-friendly H.264 (via `openh264` or platform encoders) and VP9/AV1 evaluation; add real keyframe/interval logic and quality adaptation driven by RTT and loss estimates.
- **DXGI Desktop Duplication** capture on Windows (multi-monitor, cursor compositing, higher fps at lower CPU).
- **Wayland capture** via PipeWire/xdg-desktop-portal on Linux.
- **Clipboard sync** end-to-end (`ClipboardText` message exists; wire to platform clipboards with size limits).
- **Frame pacing & bandwidth estimation** — adaptive jpeg quality/scale/fps from measured RTT (`Ping/Pong`) and send-queue depth.
- **Session UX**: multi-monitor selection, fullscreen hotkeys, quality presets in the viewer UI.

## M2 — Trust & accounts

- **OS keychain** for the unattended password (Windows Credential Manager, macOS Keychain, Secret Service), replacing plaintext `agent.toml` storage.
- **Per-user access control**: multiple credentials per host with scopes (`view-only`, `no-files`, `no-terminal`), session logging with consent prompts.
- **Auth rate limiting & lockout** in the agent; optional TOFU/pinned host fingerprints in the viewer.
- **Per-session TURN credentials** minted by the signaling server (coturn REST API) instead of a shared secret.
- **System integration**: systemd user unit for the agent, Windows service installer, macOS launchd agent, signed/notarized packages.

## M3 — Scale & operations

- **Server v2**: multi-node signaling with a shared registry (Redis/Postgres), horizontal relay scaling, connection routing by region.
- **TLS everywhere by default**: built-in `wss://` with ACME, or documented single-command reverse-proxy setup (Caddy).
- **Metrics & admin UI**: Prometheus endpoint on the server, session/relay dashboards.
- **Meeting-style features**: multi-viewer observation of one host (viewer count policies), session recording (opt-in, host-side).

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
