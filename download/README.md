# download/

Prebuilt OpacusR binaries. **v0.11.0 is the current release** — every
platform ships the TeamViewer-style compact UI, the AnyDesk-style
contacts/history windows, the system tray (close-to-tray) and the same
violet/teal palette. The wire protocol is v9 (bincode 2.x): update the
agent and the viewer together.

The official, checksummed release set (Windows MSVC zip + MSI, Linux
tar.gz + Wayland variant + debs, macOS tar.gz, SHA256SUMS) lives in
GitHub Releases: <https://github.com/opacusio/opacusr/releases/tag/v0.11.0>.

## Local copies

| file | origin | what's inside |
| --- | --- | --- |
| `opacusr-0.11.0-windows-x86_64.zip` | CI MSVC build, tag `v0.11.0` (sha256-verified) | full Windows set: `opacusr.exe` (GUI, tray, no console), `opacusr-agent.exe` (audio), `opacusr-server.exe`, `README-WINDOWS.txt`, READMEs, LICENSE |
| `opacusr-0.11.0-x64.msi` | CI MSVC build, tag `v0.11.0` | Windows installer |
| `opacusr-0.11.0-macos-aarch64.tar.gz` | CI build, tag `v0.11.0`, ad-hoc signed | full macOS set (Apple silicon) |
| `opacusr-0.11.0-linux-x86_64.tar.gz` | CI build, tag `v0.11.0` | `opacusr`, `opacusr-agent`, `opacusr-server` + READMEs + LICENSE |
| `opacusr-0.11.0-linux-wayland-x86_64.tar.gz` | CI build, tag `v0.11.0` | same + PipeWire/portal backend for native Wayland |
| `opacusr-agent_0.11.0-1_amd64.deb`, `opacusr-app_0.11.0-1_amd64.deb` | CI build, tag `v0.11.0` | Debian/Ubuntu packages |
| `SHA256SUMS` | release job | checksums of everything above |
| `opacusr-0.11.0-258d20e-windows-x86_64-gnu.zip` | sandbox mingw-w64 cross-build of `258d20e` | the gnu variant (not in the official release): same three exes, libgcc/libstdc++ static, Opus static — only system DLLs |

All `0.11.0` files were re-verified against the release `SHA256SUMS`
after download. The older `opacusr-0.10.0-*` files below are kept for
reference only.

## Notes

- The MSVC zip / MSI are built with `+crt-static` — no VC++
  redistributable needed on clean Windows installs.
- The windows-gnu zip is self-contained the same way (static
  libgcc/libstdc++/Opus): only system Windows 10/11 components. This
  fixes the `libstdc++-6.dll is missing` startup error reported for
  earlier zips. See `README-WINDOWS.txt` inside for what each exe is
  for.
- Release GUI builds are GUI-subsystem binaries: double-clicking
  `opacusr.exe` opens the window directly, no empty console appears
  next to it; closing the window hides it to the tray (agent keeps
  serving, exit via the tray menu).

## Quick start (linux)

```bash
tar -xzf opacusr-0.11.0-linux-x86_64.tar.gz
cd opacusr-0.11.0-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
