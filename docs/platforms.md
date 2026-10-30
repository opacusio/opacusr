# OpacusR Platform Support

Status per platform for **v0.1.0**. The transport, protocol, crypto, server, file-transfer and terminal layers are fully cross-platform Rust; what differs is **screen capture** and **input injection**.

## Summary

| Platform | Screen capture | Input injection | Terminal (PTY) | GUI | Overall |
| --- | --- | --- | --- | --- | --- |
| Linux (X11) | ✅ `x11rb` GetImage | ✅ enigo (X11) | ✅ openpty | ✅ eframe | **Working** |
| Linux (Wayland) | ❌ falls back to X11/XWayland | ⚠️ partial | ✅ | ✅ | Roadmap (pipewire) |
| Windows 10/11 | ✅ Win32 GDI (BitBlt) | ✅ enigo (SendInput) | ✅ ConPTY | ✅ eframe | **Working** |
| macOS (aarch64/x86_64) | ⚠️ core-graphics partial | ⚠️ enigo (needs permissions) | ✅ openpty | ✅ eframe | **Experimental** |
| Android | ❌ | ❌ | ❌ | 🚧 skeleton crate target | **Planned** |
| iOS | ❌ | ❌ | ❌ | 🚧 skeleton crate target | **Planned** |
| Headless / VM / CI | ✅ `test-source` synthetic pattern | ✅ `NullInjector` | ✅ | n/a | **Working (testing)** |

Every platform compiles the full workspace; capture/input differences are isolated behind traits in `opacusr-capture` and `opacusr-input`, so a missing backend degrades to the deterministic `test-source` / `NullInjector` pair instead of failing the build.

## Linux

- **Capture:** `x11rb::connect` + `GetImage` over the root window; BGRA→RGBA conversion and nearest-neighbor downscale (`resize_nearest`) are applied in-crate. Requires `libx11-dev` and `libxi-dev` at build time (Debian/Ubuntu names).
- **Input:** enigo 0.6 over X11 (keys incl. `Numlock`/`PrintScr`, wheel via `scroll(length, Axis)`).
- **Wayland:** not yet. Under Wayland, run through XWayland, or use the test source. Native capture via `pipewire` + `xdg-desktop-portal` is on the roadmap (needed for screen-capture permission UX).
- **Unattended:** run `opacusr-agent run` under the desktop user's session (it must have X access; `XAUTHORITY`/`DISPLAY` must be set). A systemd user unit is on the roadmap.

## Windows

- **Capture:** GDI `BitBlt` of the primary screen via the `winapi` stack; BGRA→RGBA and downscale shared with other backends.
- **Input:** enigo 0.6 `SendInput`.
- **Terminal:** ConPTY through `portable-pty` (PowerShell by default).
- **Config:** `%APPDATA%\opacusr\agent.toml`.
- **Build:** MSVC toolchain; no additional SDK beyond standard Windows libs. CI builds on `windows-latest`.
- **Known limits:** GDI capture is CPU-copied per frame; DXGI Desktop Duplication (better multi-monitor + cursor info) is a roadmap item.

## macOS

- **Status: experimental.** The workspace builds (`x86_64` and `aarch64`), GUI runs, PTY works.
- **Capture:** partial core-graphics path; full-screen capture permissions (`Screen Recording` TCC prompt) must be granted to the binary.
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
