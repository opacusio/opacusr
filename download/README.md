# download/

Prebuilt OpacusR binaries. **v0.11.1 is the current release** — the
whole «белый фон» batch: one TeamViewer-style home card (allow +
control in a single window), the assigned ID always visible (persisted
in `agent.toml`), services that really start automatically (the MSI
registers and starts the `OpacusRAgent` service; a per-machine Run
entry launches the GUI hidden to the tray), and the dark Rev-3 theme
pinned everywhere — including the native Windows title bar/border
(DWM) and a startup that shows the window only after the first dark
frame is painted (no white flash). The wire protocol is v9 (bincode
2.x): update the agent and the viewer together.

The official, checksummed release set (Windows MSVC zip + MSI, Linux
tar.gz + Wayland variant + debs, macOS tar.gz, SHA256SUMS) lives in
GitHub Releases: <https://github.com/opacusio/opacusr/releases/tag/v0.11.1>.

All 8 `v0.11.1` assets were downloaded to the session `download/`
folder and verified against the release `SHA256SUMS` (7× OK). The
`v0.11.0` set below stays published for reference.

## Notes

- The MSVC zip / MSI are built with `+crt-static` — no VC++
  redistributable needed on clean Windows installs.
- The MSI now installs the agent as a real auto-start Windows service
  (native WiX `ServiceInstall`/`ServiceControl`, launched as
  `opacusr-agent.exe service run`) and starts it immediately during
  setup; uninstall stops and removes it. An HKLM `Run` entry starts
  the GUI with `--tray` at every login, and the GUI starts the host
  agent itself (Настройки → Хост → «Автозапуск»).
- Release GUI builds are GUI-subsystem binaries: double-clicking
  `opacusr.exe` opens the window directly, no empty console appears
  next to it; closing the window hides it to the tray (agent keeps
  serving, exit via the tray menu). On Windows the title bar/border
  are switched to the dark palette via DWM; on Windows 10 the
  immersive-dark-mode flag is used (caption color needs Windows 11).

## Quick start (linux)

```bash
tar -xzf opacusr-0.11.1-linux-x86_64.tar.gz
cd opacusr-0.11.1-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
