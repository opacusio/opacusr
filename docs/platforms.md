# OpacusR Platform Support

Status per platform for **main** (post-0.17.0). The transport, protocol, crypto, server, file-transfer and terminal layers are fully cross-platform Rust; what differs is **screen capture**, **audio capture** and **input injection**.

## Summary

| Platform | Screen capture | Audio | Input injection | Terminal (PTY) | GUI | Overall |
| --- | --- | --- | --- | --- | --- | --- |
| Linux (X11) | ✅ RandR multi-monitor | ✅ mic (cpal/ALSA) | ✅ enigo (X11) | ✅ openpty | ✅ eframe | **Working** |
| Linux (Wayland) | ⚠️ PipeWire portal (`--features pipewire`, experimental) else X11/XWayland | ✅ mic | ✅ uinput virtual devices (native Wayland; needs `/dev/uinput` write access) | ✅ | ✅ eframe | **Experimental** |
| Windows 10/11 | ✅ DXGI Desktop Duplication (auto) with GDI fallback; both multi-monitor | ✅ system sound (WASAPI loopback) | ✅ enigo (SendInput) | ✅ ConPTY | ✅ eframe | **Working** |
| macOS (aarch64) | ✅ CoreGraphics snapshots (multi-monitor, Retina; Screen Recording permission required) | ✅ mic (cpal/CoreAudio) | ✅ enigo (needs Accessibility permission) | ✅ openpty | ✅ eframe | **Working** |
| Android | ✅ MediaProjection (host) | ❌ | ⚠️ host-side via accessibility shim (drag/long-press/swipe-scroll/Back/paste; Android 8+ for drag) | ❌ | ✅ NativeActivity + egui | **Working (viewer + phone-host)** |
| iOS | ❌ | ❌ | ❌ | ❌ | ✅ egui (static shell, needs an Xcode app bundle to run) | **Viewer core — in progress** |
| Headless / VM / CI | ✅ `test-source` synthetic pattern | n/a (feature compiles) | ✅ `NullInjector` | ✅ | n/a | **Working (testing)** |

Every platform compiles the full workspace; capture/input differences are isolated behind traits in `opacusr-capture` and `opacusr-input`, so a missing backend degrades to the deterministic `test-source` / `NullInjector` pair instead of failing the build.

## Linux

- **Capture:** `x11rb::connect` + `GetImage` over a RandR monitor rect; monitors are enumerated via RandR `get_monitors` (primary first), switching mid-session is supported. BGRA→RGBA conversion and nearest-neighbor downscale (`resize_nearest`) are applied in-crate. Requires `libx11-dev`, `libxi-dev` and `libasound2-dev` at build time (Debian/Ubuntu names).
- **Wayland (experimental):** build with `--features opacusr-capture/pipewire` (needs `libpipewire-0.3-dev`, `libspa-0.2-dev`). A native Wayland session (`WAYLAND_DISPLAY` set) then prefers the xdg-desktop-portal ScreenCast: the user picks the monitor in the system dialog, the agent consumes the PipeWire stream (BGRx/RGBA). On portal denial/unavailability the agent falls back to X11/XWayland with a warning.
- **Audio:** `cpal` over ALSA — microphone by default; on PulseAudio systems a monitor source (system sound) can be selected by name: `audio_device = "monitor"`. Native WASAPI-loopback-style capture without extra config is a roadmap item (via PipeWire).
- **Input:** enigo 0.6 over X11 (keys incl. `Numlock`/`PrintScr` on X11, wheel via `scroll(length, Axis)`); macOS lacks enigo variants for Insert/NumLock/PrintScreen — those are reported as unsupported. On a **native Wayland session** the agent instead creates two virtual evdev devices via `/dev/uinput` (absolute pointer + keyboard): the compositor's libinput picks them up like real hardware, so remote mouse, keys, wheel and ASCII text work without any X server. Opening `/dev/uinput` needs write access — the deb ships `usr/lib/udev/rules.d/60-opacusr-uinput.rules` (mode 0660, group `input`, same group the physical input lock already needs; `sudo usermod -aG input $USER` + replug/reboot). Without it the agent falls back to X11/XWayland input with a warning. v1 notes: single-monitor correctness (compositors map absolute pointers across the whole desktop — multi-monitor mapping is a roadmap item); `text()` types printable ASCII (+ `\n`, `\t`) — Unicode text needs a Wayland IME path (roadmap).
- **Unattended:** run `opacusr-agent run` under the desktop user's session (it must have X access; `XAUTHORITY`/`DISPLAY` must be set). The deb ships a systemd **user** unit: `systemctl --user enable --now opacusr-agent`.

## Windows

- **Capture:** DXGI Desktop Duplication by default (`IDXGIOutputDuplication` + D3D11 staging readback, BGRA→RGBA): frames come straight out of DWM, work with hardware overlays and fullscreen apps, and the last frame is re-served while the desktop is idle. `ACCESS_LOST` (UAC prompt, desktop switch, mode change) triggers an automatic rebuild. Monitors come from the DXGI adapter enumeration; `capture = auto | gdi | dxgi` in `agent.toml` or `--capture` on the CLI — `gdi` uses `EnumDisplayMonitors` + `BitBlt` (works from service sessions, CPU-copied per frame).
- **Audio:** WASAPI shared-mode **loopback** of the default output device — the viewer hears exactly what the machine plays. The mix format (usually f32 stereo 48 kHz) is converted to mono internally; `audio_device` filtering is not supported on Windows yet.
- **Input:** enigo 0.6 `SendInput`.
- **Terminal:** ConPTY through `portable-pty` (PowerShell by default).
- **Config:** `%APPDATA%\opacusr\agent.toml`.
- **Build:** MSVC toolchain; no additional SDK beyond standard Windows libs. CI builds on `windows-latest` (MSVC links the prebuilt opus shipped by `audiopus_sys`). Cross-builds with the GNU toolchain should disable the audio feature: `cargo build --no-default-features`.

## macOS

- **Status: working host AND viewer** (since 0.14.0). The workspace builds (aarch64), GUI runs, PTY works, and the ScreenCaptureKit/CoreGraphics capture ladder streams the screen with the same multi-monitor / Retina guarantees as the other desktop platforms.
- **Capture:** ScreenCaptureKit stream first (`SCStream`, macOS 12.3+; hardware-backed on Apple Silicon, push-based — an idle desktop delivers nothing, so the stream costs nothing on static content), CoreGraphics snapshots as the fallback (`CGDisplayCreateImage` per frame; deprecated by macOS 15 — the ladder keeps older or stubborn setups working). The ObjC glue lives in one auditable shim (`opacusr-capture/shim/sck_shim.m`, compiled by build.rs on macOS only); the Rust side is pure C FFI, no ObjC runtime calls. Multi-monitor via `CGGetOnlineDisplayList` (the main display is the default; mid-session switching restarts the stream for the new display), Retina-native frames (device pixels), `showsCursor = NO` on both paths — the viewer draws the cursor from the overlay metadata. Honest TCC failure: without the **Screen Recording** permission (`Системные настройки → Конфиденциальность и безопасность → Запись экрана`) both paths fail and the session ends with an explicit hint instead of a black stream; `backend()` reports `macos-sck` or `macos-cg` for the log.
- **Coordinate contract:** captured frames are device pixels, Quartz events are points — `origin()` and cursor overlay positions are global top-left device pixels (converted from the CG bottom-left geometry), and remote mouse input converts back via `opacusr_capture::macos::px_to_pt_global` (per-display scale, mixed-DPI aware).
- **Cursor overlay:** position tracking via `CGEventGetLocation`; the shape is the built-in standard arrow sprite (per-app shapes — I-beam, resize — need the ObjC `NSCursor` dance, roadmap). The capture API drops the pointer, so the overlay is the only cursor the viewer sees.
- **Audio:** `cpal` captures the default input (microphone). System-sound loopback needs macOS 14.6+ (CoreAudio) or a virtual device (BlackHole) selected via `audio_device` — roadmap.
- **Input:** enigo works but requires the `Accessibility` permission for the process (System Settings → Privacy & Security → Accessibility).
- Config: `~/Library/Application Support/opacusr/agent.toml`. CI builds on `macos-latest`.

## Android / iOS

**Android ships**: viewer (chromeless fullscreen, touch layer, soft keyboard + control-keys strip, cursor overlay) AND phone-as-host via MediaProjection (consent dialog + FGS `CaptureService`; a Java shim compiled by CI).

**Phone-host remote input** (Java shims in classes.dex, `InputService.java` + `KeyboardService.java`, driven from `opacusr_input::android_bridge`): two independent opt-ins, each degrades on its own —

- **Gestures + paste**: the user enables "OpacusR" once in **Settings → Accessibility**; the consent dialog says when it is off. Mapping of the desktop protocol onto touch reality:
  - left button down/move/up → a **drag** (one continued `GestureDescription` stroke per move segment, `StrokeDescription.continueStroke`; needs **Android 8+** — on older devices a plain tap fallback covers single clicks);
  - right button → **long press** (the touch equivalent of right click);
  - wheel → a **short swipe** at the last pointer position (protocol positive dy = wheel UP → finger swipes DOWN, mirroring uinput.rs); suppressed while a drag is running;
  - `Escape` → **Back** (`GLOBAL_ACTION_BACK` — editors cannot consume Esc anyway).
- **Keys + text (v2, IME)**: the user selects the silent «OpacusR» keyboard once as the current input method (it renders NO UI; local typing is inert while selected — switch keyboards from quick settings to type locally, the AnyDesk tradeoff). Text rides `InputConnection.commitText` — real Unicode (emoji, CJK) WITHOUT clobbering the clipboard; editing keys (Enter, Tab, Backspace/Delete, arrows, Home/End, PgUp/PgDn, Insert, Space, F1–F12) ride `InputConnection.sendKeyEvent` with SHIFT/CTRL/ALT/META state (Ctrl+C/V/X/A work in editors that handle them). Modifier chords on letters/digits become real key events; punctuation chords stay unsupported (typed punctuation arrives as `text()` anyway); CapsLock/NumLock/PrintScreen/ContextMenu are unsupported. When the keyboard is not selected (or no editor has focus), `text()` falls back to the v1 clipboard + `ACTION_PASTE` path (clobbers the clipboard) and keys report unsupported honestly.

Hover moves without a held button are dropped (touch screens have no hover). The device paths (real gesture dispatch, real IME commits on hardware) are untested in CI — CI proves javac/aapt2/d8 compile the shims and the manifest links; the first live run should be watched. Audio capture is not implemented for phones.

**iOS (viewer core, this release)**: the whole viewer dependency graph compiles for `aarch64-apple-ios` in CI — proto → crypto → transport → codec → capture/input stubs → client → agent → egui app → the new `opacusr-ios` static shell. The WebRTC data-channel stack is **not compiled** for iOS: webrtc 0.14 → webrtc-util 0.12 → nix 0.26 references `libc::sockaddr_ctl`, which exists for macOS but not iOS, so the module is cfg-gated off there (`TransportChoice::Auto` degrades to the TCP relay, `WebRtcOnly` fails closed, an iOS host answers raw offers with an explicit `Reject`). LAN-direct TCP works as-is. The renderer is **wgpu (Metal)**: glutin — the glow backend — has no iOS support, so eframe is per-target (workspace entry `default-features = false`; desktop/Android spell out the glow set). What is still missing for a runnable app: **signing** — CI already builds the app shell (UIKit entry + `libopacusr_ios.a` linked via `native-static-libs`, sanity-checked Mach-O) and packages an unsigned IPA (`opacusr-<version>-ios-unsigned.ipa`, self-sign with a free Personal Team via Xcode / AltStore / Sideloadly — see `ios/README.md`); a TestFlight/App Store build needs a paid Apple Developer account. ReplayKit host capture stays a roadmap item.

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
| `aarch64-linux-android` / `aarch64-apple-ios` | Android: full APK; iOS: full viewer graph (no webrtc — TCP relay / LAN direct) |

CI (`.github/workflows/ci.yml`) runs fmt, clippy and the test suite on Linux, Windows and macOS.
