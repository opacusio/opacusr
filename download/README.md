# download/

Prebuilt OpacusR binaries. The linux and windows-gnu artifacts are
built in the maintainer's sandbox from the exact commit in the
filename; the macOS and Windows MSVC artifacts are the CI builds from
the `v0.10.0` tag (Apple's SDK cannot be used on a Linux builder).

| file | origin | what's inside |
| --- | --- | --- |
| `opacusr-0.10.0-446b452-linux-x86_64.tar.gz` | sandbox build of `446b452` | `opacusr` (GUI, Rev-3 design), `opacusr-agent`, `opacusr-server` + READMEs + LICENSE |
| `opacusr-0.10.0-446b452-windows-x86_64-gnu.zip` | sandbox mingw-w64 cross-build of `446b452` | `opacusr-agent.exe`, `opacusr-server.exe` + READMEs + LICENSE |
| `opacusr-0.10.0-windows-x86_64.zip` | CI MSVC build (tag v0.10.0) | full Windows set incl. the GUI `opacusr.exe` |
| `opacusr-0.10.0-macos-aarch64.tar.gz` | CI build (tag v0.10.0) | full macOS set (Apple silicon) |

Each artifact has a `.sha256` sidecar with its checksum.

## Notes

- `446b452` = the **Rev-3 UI design** batch (merged home screen,
  violet/teal theme, Carlito fonts, collapsible advanced settings,
  hostname as the default network name). The windows-gnu zip carries
  only agent+server: the GUI app is not cross-buildable under mingw
  (vendored Opus) — use the CI MSVC zip for a Windows GUI, or the
  linux/macOS artifacts elsewhere. A tag-driven CI build of the
  Rev-3 commit for Windows GUI/macOS can be produced on request
  (tag v0.10.1 → release.yml).
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
