# download/

Prebuilt OpacusR binaries — **v0.11.22 is the current release** (2026-11-24).
This folder mirrors the official, checksummed release set from
GitHub Releases: <https://github.com/opacusio/opacusr/releases/tag/v0.11.22>.
Every file here is byte-identical to the release asset (sha256 — see
`SHA256SUMS`).

What is inside:

| File | Platform |
| --- | --- |
| `opacusr-0.11.22-android-arm64.apk` | Android 8+ (arm64) — viewer app |
| `opacusr-0.11.22-linux-x86_64.tar.gz` | Linux x86_64 (X11) |
| `opacusr-0.11.22-linux-wayland-x86_64.tar.gz` | Linux x86_64 (Wayland) |
| `opacusr-0.11.22-macos-aarch64.tar.gz` | macOS Apple Silicon (experimental) |
| `opacusr-0.11.22-windows-x86_64.zip` | Windows x86_64 (portable zip) |
| `opacusr-0.11.22-x64.msi` | Windows x86_64 — the installer |
| `opacusr-app_0.11.22-1_amd64.deb` | Debian/Ubuntu — GUI |
| `opacusr-agent_0.11.22-1_amd64.deb` | Debian/Ubuntu — agent/server |
| `SHA256SUMS` | sha256 of every file above |

v0.11.22 highlights: the connection history keeps ONE row per id (a
repeat connection refreshes and lifts the row, stored duplicates are
collapsed on the first start), and a tap on a history row reconnects —
the partner password is remembered after the first successful session.

## Notes

- The MSVC zip / MSI are built with `+crt-static` — no VC++
  redistributable needed on clean Windows installs.
- **The MSI is the installer**: it registers the agent as a real
  auto-start Windows service (native WiX `ServiceInstall` /
  `ServiceControl`) and starts it during setup; uninstall stops and
  removes it. An HKLM `Run` entry opens the GUI at every logon; when
  the service is running the GUI defers to it (no second agent, no
  duplicate ID). Zip installs get the same service via
  `opacusr-agent.exe service install` or the UAC-elevated button in
  Настройки → Хост → «Служба Windows».
- Release GUI builds are GUI-subsystem binaries: double-clicking
  `opacusr.exe` opens the window directly, no empty console appears
  next to it. Closing the window hides the app to the tray by default
  (TeamViewer model; opt out in Настройки → Хост → «Окно»); tray
  «Выход» exits for real. On Windows the title bar/border use the dark
  palette via DWM.
- Android: install the APK over any 0.11.14+ build without
  uninstalling (same signing key). The version is shown top-right in
  the window (e.g. `v0.11.22`), so it is always clear which binary
  runs. The wire protocol is v9 (bincode 2.x): update the agent and
  the viewer together.

## Quick start (linux)

```bash
tar -xzf opacusr-0.11.22-linux-x86_64.tar.gz
cd opacusr-0.11.22-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
