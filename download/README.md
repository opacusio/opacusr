# download/

Prebuilt OpacusR binaries. **v0.11.4 is the current release** — the
«всё должно работать» batch on top of the v0.11.1/0.11.2 fixes: the
tray icon can no longer linger after «Выход», the MSI autostart opens
the window at logon (no more invisible-in-tray starts), showing the
window from the tray / a second launch un-minimizes it first, and the
GUI defers to a running `OpacusRAgent` service instead of registering
a duplicate ID (the home card shows «служба работает»). The build
version is displayed top-right in the window (e.g. `v0.11.4`), so it
is always clear which binary runs. The wire protocol is v9 (bincode
2.x): update the agent and the viewer together.

The official, checksummed release set (Windows MSVC zip + MSI, Linux
tar.gz + Wayland variant + debs, macOS tar.gz, SHA256SUMS) lives in
GitHub Releases: <https://github.com/opacusio/opacusr/releases/tag/v0.11.4>.

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
  next to it. Closing the window QUITS the app by default
  (hide-to-tray is opt-in: Настройки → Хост → «Окно»); tray «Выход»
  exits for real and removes the icon. On Windows the title
  bar/border use the dark palette via DWM; the dark Rev-3 theme is
  pinned before the first frame (no white flash) and re-asserted
  every frame.

## Quick start (linux)

```bash
tar -xzf opacusr-0.11.4-linux-x86_64.tar.gz
cd opacusr-0.11.3-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
