# OpacusR Platform Support

Status per platform for **v0.3.0**. The transport, protocol, crypto, server, file-transfer and terminal layers are fully cross-platform Rust; what differs is **screen capture**, **audio capture** and **input injection**.

## Summary

| Platform | Screen capture | Audio | Input injection | Terminal (PTY) | GUI | Overall |
| --- | --- | --- | --- | --- | --- | --- |
| Linux (X11) | ✅ RandR multi-monitor | ✅ mic (cpal/ALSA) | ✅ enigo (X11) | ✅ openpty | ✅ eframe | **Working** |
| Linux (Wayland) | ⚠️ PipeWire portal (`--features pipewire`, experimental) else X11/XWayland | ✅ mic | ⚠️ partial | ✅ | ✅ eframe | **Experimental** |
| Windows 10/11 | ✅ DXGI Desktop Duplication (auto) with GDI fallback; both multi-monitor | ✅ system sound (WASAPI loopback) | ✅ enigo (SendInput) | ✅ ConPTY | ✅ eframe | **Working** |
| macOS (aarch64/x86_64) | ⚠️ core-graphics partial | ✅ mic (cpal/CoreAudio) | ⚠️ enigo (needs permissions) | ✅ openpty | ✅ eframe | **Experimental** |
| Android | ❌ | ❌ | ❌ | ❌ | 🚧 skeleton crate target | **Planned** |
| iOS | ❌ | ❌ | ❌ | ❌ | 🚧 skeleton crate target | **Planned** |
| Headless / VM / CI | ✅ `test-source` synthetic pattern | n/a (feature compiles) | ✅ `NullInjector` | ✅ | n/a | **Working (testing)** |

Every platform compiles the full workspace; capture/input differences are isolated behind traits in `opacusr-capture` and `opacusr-input`, so a missing backend degrades to the deterministic `test-source` / `NullInjector` pair instead of failing the build.

## Linux

- **Capture:** `x11rb::connect` + `GetImage` over a RandR monitor rect; monitors are enumerated via RandR `get_monitors` (primary first), switching mid-session is supported. BGRA→RGBA conversion and nearest-neighbor downscale (`resize_nearest`) are applied in-crate. Requires `libx11-dev`, `libxi-dev` and `libasound2-dev` at build time (Debian/Ubuntu names).
- **Wayland (experimental):** build with `--features opacusr-capture/pipewire` (needs `libpipewire-0.3-dev`, `libspa-0.2-dev`). A native Wayland session (`WAYLAND_DISPLAY` set) then prefers the xdg-desktop-portal ScreenCast: the user picks the monitor in the system dialog, the agent consumes the PipeWire stream (BGRx/RGBA). On portal denial/unavailability the agent falls back to X11/XWayland with a warning.
- **Audio:** `cpal` over ALSA — microphone by default; on PulseAudio systems a monitor source (system sound) can be selected by name: `audio_device = "monitor"`. Native WASAPI-loopback-style capture without extra config is a roadmap item (via PipeWire).
- **Input:** enigo 0.6 over X11 (keys incl. `Numlock`/`PrintScr` on X11, wheel via `scroll(length, Axis)`); macOS lacks enigo variants for Insert/NumLock/PrintScreen — those are reported as unsupported.
- **Unattended:** run `opacusr-agent run` under the desktop user's session (it must have X access; `XAUTHORITY`/`DISPLAY` must be set). The deb ships a systemd **user** unit: `systemctl --user enable --now opacusr-agent`.

## Windows

- **Capture:** DXGI Desktop Duplication by default (`IDXGIOutputDuplication` + D3D11 staging readback, BGRA→RGBA): frames come straight out of DWM, work with hardware overlays and fullscreen apps, and the last frame is re-served while the desktop is idle. `ACCESS_LOST` (UAC prompt, desktop switch, mode change) triggers an automatic rebuild. Monitors come from the DXGI adapter enumeration; `capture = auto | gdi | dxgi` in `agent.toml` or `--capture` on the CLI — `gdi` uses `EnumDisplayMonitors` + `BitBlt` (works from service sessions, CPU-copied per frame).
- **Audio:** WASAPI shared-mode **loopback** of the default output device — the viewer hears exactly what the machine plays. The mix format (usually f32 stereo 48 kHz) is converted to mono internally; `audio_device` filtering is not supported on Windows yet.
- **Input:** enigo 0.6 `SendInput`.
- **Terminal:** ConPTY through `portable-pty` (PowerShell by default).
- **Config:** `%APPDATA%\opacusr\agent.toml`.
- **Build:** MSVC toolchain; no additional SDK beyond standard Windows libs. CI builds on `windows-latest` (MSVC links the prebuilt opus shipped by `audiopus_sys`). Cross-builds with the GNU toolchain should disable the audio feature: `cargo build --no-default-features`.

## macOS

- **Status: experimental.** The workspace builds (`x86_64` and `aarch64`), GUI runs, PTY works.
- **Capture:** partial core-graphics path; full-screen capture permissions (`Screen Recording` TCC prompt) must be granted to the binary.
- **Audio:** `cpal` captures the default input (microphone). System-sound loopback needs macOS 14.6+ (CoreAudio) or a virtual device (BlackHole) selected via `audio_device` — roadmap.
- **Input:** enigo works but requires `Accessibility` permission for the process.
- Config: `~/Library/Application Support/opacusr/agent.toml`. CI builds on `macos-latest`.

## Android / iOS

Skeleton targets exist so the core (proto/crypto/transport/client) can be compiled for mobile from day one (`opacusr-app` is kept GUI-agnostic where practical). Planned delivery: FFI/UniFFI bindings exposing `SessionClient`, then a thin UI per platform. Screen capture on Android requires the MediaProjection API; on iOS, ReplayKit broadcast — both are roadmap items.

## Headless / CI usage

Both sides can run without any real display or input device:

- Host: `opacusr-agent run --test-source` streams the deterministic synthetic pattern (no X/Win32 needed) — this is exactly what the E2E test does in-process.
- Input: `NullInjector` logs/discards events, keeping the session loop honest without touching a desktop.
- A full viewer↔host session (auth, frames, input, chat, files, terminal) can be exercised on any CI runner; see `crates/opacusr-agent/tests/full_session.rs`.

## Build matrix

| Target | Notes |
| --- | --- |
| `x86_64-unknown-linux-gnu` | primary; X11 dev packages required |
| `aarch64-unknown-linux-gnu` | cross-compiles; same deps on target |
| `x86_64-pc-windows-msvc` | primary Windows target |
| `aarch64-apple-darwin` / `x86_64-apple-darwin` | experimental |
| `aarch64-linux-android` / `aarch64-apple-ios` | core crates only, today |

CI (`.github/workflows/ci.yml`) runs fmt, clippy and the test suite on Linux, Windows and macOS.
