# OpacusR Roadmap

Where OpacusR is heading. Items are grouped by milestone; security items from [security.md](security.md) §10 are folded in where they belong.

## Done

- **v0.1.0** — WebRTC + TCP/relay transports, JPEG streaming, remote input, file transfer, chat, PTY terminal, unattended access, self-hosted signaling/relay, Docker Compose (server + coturn), CI on three OSes.
- **v0.2.0** — H.264 codec (Cisco OpenH264, screen-content tuned, JPEG fallback, `codec` config), unattended password in the OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service, 0600-file fallback), packaging: deb (agent + viewer with systemd unit and .desktop), Windows MSI (WiX), release pipeline producing tar.gz/zip/deb/msi on every `v*` tag.
- **v0.3.0** — clipboard sync (both directions, echo-suppressed), view-only mode + ACL policy, adaptive quality (RTT/FPS tiers with hysteresis), DXGI Desktop Duplication (Windows) and PipeWire portal capture (Wayland, experimental), multi-monitor selection, audio streaming (WASAPI loopback on Windows, cpal mic on Linux/macOS, Opus 48 kHz mono).
- **v0.4.0** — startup update check, Wayland fail-fast error, macOS ad-hoc codesign + Gatekeeper first-launch docs, download verification docs (SHA256SUMS + SmartScreen), server hardening: per-IP rate limits, peer/relay-ticket caps, time-limited TURN credentials (`GET /turn`), agent-side auth lockout (sliding-window failure counter with escalating lockout), zero-infra networking design ([serverless.md](serverless.md)).

- **v0.5.0 (unreleased)** — zero-infra networking phases 1–2 shipped: agent `signaling_urls` fan-out, embedded signaling (`--signal-listen`: registry + relay + `/turn` in the agent), viewer `connect = "auto"` endpoint ladder; **host physical-input lock** (`--lock-input` / GUI checkbox, TeamViewer-style: Windows LL hooks swallow physical events while injected remote input passes, Linux evdev grabs, Ctrl+Alt+Del stays available, view-only sessions never lock, fail-open, crash-safe); `--no-default-features` builds for hosts without audio dev headers.

## M1 — Media & experience (next)

- **Audio polish**: Linux system-sound capture without manual device picking (PipeWire monitor), Windows per-device selection, viewer-side volume/mute, WASAPI loopback auto-recovery after default-device changes.
- **Wayland hardening**: promote the PipeWire backend to default after field testing; multi-monitor portal dialogs; DmaBuf zero-copy path.
- **Cursor rendering**: compositor cursor overlay (DXGI pointer shapes / X11 cursor events) so remote viewers always see the pointer.
- **Session UX**: fullscreen hotkeys, quality presets, session stats panel (tier, encode time, packet loss).
- **VP9/AV1 evaluation** behind the existing codec traits (web-optimized fallback for slow links).
- **H.264 incremental/region encode** for lower CPU on static screens (dirty-rect driven).

## M2 — Trust & accounts

- **Strong ACL identities**: per-viewer keys / device fingerprints so rules do not depend on self-reported names (today the password is the real gate, see security.md).
- ~~**Agent-side** auth rate limiting & lockout~~ **done (v0.4.0)** — per-path sliding-window counter with escalating lockout (security.md §8.3). Optional TOFU/pinned host fingerprints in the viewer remains.
- ~~**Automatic per-session TURN credential fetch** in clients~~ **done (v0.4.0)** — both sides fetch `GET /turn` at session setup and cache by TTL; server-side minting shipped earlier.
- **Windows service / macOS launchd** installs for the agent (the systemd user unit and MSI already ship; service-mode is the remaining gap).

## Zero-infra networking (no rented servers)

Design in [serverless.md](serverless.md); goal: a working connection with **0 ₽/mo** of rented infrastructure, with the clients automatically switching to the first path that works.

- **Path ladder** (viewer `connect = "auto"`): LAN direct → P2P/STUN → TURN → TCP relay, with the active mode shown in the UI.
- ~~**Phase 1**: `signaling_urls` list on the agent (fan-out registration), viewer auto-connect ladder~~ **done** — supervised per-endpoint reconnect loops; the viewer tries endpoints in order with per-endpoint TURN minting.
- ~~**Phase 2**: embedded signaling listener in the agent + tunnel cookbook (cloudflared / Tailscale Funnel)~~ **done** — `signal_listen = true` serves registry + relay + `/turn` in the agent; cookbook in [serverless.md](serverless.md) §7.
- **Phase 3**: mDNS discovery (`hostname.local`) for LAN addresses.
- **Phase 4**: manual offer/answer "meeting codes" as the fully serverless fallback.

## M3 — Scale & operations

- **Server v2**: multi-node signaling with a shared registry (Redis/Postgres), horizontal relay scaling, connection routing by region.
- **TLS everywhere by default**: built-in `wss://` with ACME, or documented single-command reverse-proxy setup (Caddy).
- **Metrics & admin UI**: Prometheus endpoint on the server, session/relay dashboards.
- **Meeting-style features**: multi-viewer observation of one host (viewer count policies), ~~session recording (opt-in, host-side)~~ (done, v0.6.0 WIP).
- **Auto-update** channel for desktop clients (check-on-start + in-app "update available" notice; signed artifacts already produced by the release pipeline).

## M4 — Mobile & embed

- **Android**: MediaProjection capture, touch-input mapping, FFI bindings for `SessionClient`, published APKs.
- **iOS**: ReplayKit broadcast capture, input bridging where iOS allows, TestFlight builds.
- **Remote keyboard/text input polish** for touch (soft keyboard over `TextInput`).

## Gap analysis (vs TeamViewer / AnyDesk)

Full matrix in [comparison.md](comparison.md). Top of the list:

- **P0**: ~~lock host input during a session~~ (done, v0.5.0); ~~address book + recent connections~~ (done, v0.6.0 WIP); ~~drag-and-drop files~~ (done: both directions + file pickers; folder drops → P1); session stats panel and fullscreen hotkeys (M1).
- **P1**: ~~TOTP second factor for unattended access~~ (done, v0.6.0 WIP — RFC 6238, lockout-integrated); ~~opt-in session recording~~ (done, v0.6.0 WIP — host-side MP4, viewer REC indicator); TCP tunneling over the session channel; reboot-and-reconnect (needs service mode); proxy support for signaling.
- **P2**: privacy mode (dummy display driver — research); remote printing (evaluate); mobile viewer (M4); in-app auto-update install.

## Always

- Keep the E2E test green on every new transport/codec.
- Keep the relay unable to read session content.
- Keep self-hosting simple: one binary + one compose file must remain enough.
- AGPL-3.0 stays.

## Non-goals

- Vendor cloud as the default signaling path (a public relay may be *offered* later, but self-hosting remains first-class).
- Closed-source components or SDKs that would compromise AGPL compliance.
- Sneaking telemetry into clients — diagnostics are opt-in and documented.
