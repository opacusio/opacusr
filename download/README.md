# download/

Prebuilt OpacusR binaries. The linux and windows-gnu artifacts are
built in the maintainer's sandbox from the exact commit in the
filename; the macOS and Windows MSVC artifacts are the CI builds from
the `v0.10.0` tag (Apple's SDK cannot be used on a Linux builder).

| file | origin | what's inside |
| --- | --- | --- |
| `opacusr-0.10.0-446b452-linux-x86_64.tar.gz` | sandbox build of `446b452` | `opacusr` (GUI, Rev-3 design), `opacusr-agent`, `opacusr-server` + READMEs + LICENSE |
| `opacusr-0.10.0-3af81e6-windows-x86_64-gnu.zip` | sandbox mingw-w64 cross-build of `3af81e6` | full set: `opacusr.exe` (GUI, Rev-3 design), `opacusr-agent.exe` (with audio), `opacusr-server.exe` + `README-WINDOWS.txt` + READMEs + LICENSE |
| `opacusr-0.10.0-windows-x86_64.zip` | CI MSVC build (tag v0.10.0) | full Windows set incl. the GUI `opacusr.exe` (pre-Rev-3 UI) |
| `opacusr-0.10.0-macos-aarch64.tar.gz` | CI build (tag v0.10.0) | full macOS set (Apple silicon, pre-Rev-3 UI) |

Each artifact has a `.sha256` sidecar with its checksum.

## Notes

- The windows-gnu zip is **self-contained**: libgcc/libstdc++ are
  linked statically and Opus is compiled as a static library — no
  MinGW runtime DLLs are needed, only system Windows 10/11 components.
  This fixes the `libstdc++-6.dll is missing` startup error reported
  for earlier zips. `3af81e6` = that fix on top of the Rev-3 UI code
  (`446b452`); the zip also gained the GUI app (see README-WINDOWS.txt
  inside for what each exe is for).
- The macOS / MSVC zip still predate the Rev-3 UI (built from the
  v0.10.0 tag). A tag-driven CI rebuild (v0.10.1) refreshes them with
  the new UI + a static CRT (no VC redist needed) — on request.
- Platform installers (Windows MSI, macOS, wayland variant, debs)
  live in GitHub Releases: <https://github.com/opacusio/opacusr/releases>.

## Quick start (linux)

```bash
tar -xzf opacusr-0.10.0-446b452-linux-x86_64.tar.gz
cd opacusr-0.10.0-446b452-linux-x86_64
./opacusr            # GUI client
./opacusr-agent run  # host service
./opacusr-server     # self-hosted signal/relay server
```
