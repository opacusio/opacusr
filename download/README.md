# download/

Prebuilt OpacusR binaries — **v0.11.25 is the current release** (2026-11-24).
This folder mirrors the official, checksummed release set from
GitHub Releases: <https://github.com/opacusio/opacusr/releases/tag/v0.11.25>.
Every file here is byte-identical to the release asset (sha256 — see
`SHA256SUMS`).

What is inside:

| File | Platform |
| --- | --- |
| `opacusr-0.11.25-android-arm64.apk` | Android 8+ (arm64) — viewer app + P2P node |
| `opacusr-0.11.25-linux-x86_64.tar.gz` | Linux x86_64 (X11) |
| `opacusr-0.11.25-linux-wayland-x86_64.tar.gz` | Linux x86_64 (Wayland) |
| `opacusr-0.11.25-macos-aarch64.tar.gz` | macOS Apple Silicon (experimental) |
| `opacusr-0.11.25-windows-x86_64.zip` | Windows x86_64 (portable zip) |
| `opacusr-0.11.25-x64.msi` | Windows x86_64 — the installer |
| `opacusr-app_0.11.25-1_amd64.deb` | Debian/Ubuntu — GUI |
| `opacusr-agent_0.11.25-1_amd64.deb` | Debian/Ubuntu — agent/server |
| `SHA256SUMS` | sha256 of every file above |

v0.11.25 highlights: the phone→PC session no longer dies a few
messages in with «crypto error: replayed or out-of-order message: got
counter 4, expected 3» — the session cipher now keeps PER-CHANNEL
counters (control and media are delivered in arbitrary cross order by
design; a shared counter misread the first interleaving as a replay).
The PC invite card / signaling URL no longer shows a WSL2/Hyper-V
virtual-adapter address (172.16/12) when a real LAN address exists.
Hosts without a capture backend (the phone) now CLOSE the session with
a clear on-wire reason instead of leaving the viewer in «Ожидание
первого кадра» forever. Screen streaming FROM the phone remains on the
roadmap (MediaProjection host backend). All versions aligned at
0.11.25.

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
  uninstalling (same signing key, sha256 fingerprint
  89:4F:15:87…EF:19:44). The version is shown top-right in
  the window (e.g. `v0.11.24`), so it is always clear which binary
  runs. The wire protocol is v9 (bincode 2.x): update the agent and
  the viewer together.

## Quick start (linux)

```bash
tar -xzf opacusr-0.11.25-linux-x86_64.tar.gz
cd opacusr-0.11.25-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
