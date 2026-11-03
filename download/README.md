# download/

Prebuilt **linux-x86_64** binaries of OpacusR, built from this exact
commit in the maintainer's sandbox and published per request.
sha256: `319cd1e5c1df4257d73f9cdcd63691aadf4a9aa0b36e5a7d06f6b1afabae1ff8`

| file | what's inside |
| --- | --- |
| `opacusr-0.10.0-d8f2056-linux-x86_64.tar.gz` | `opacusr` (GUI client/app, 33.8 MB), `opacusr-agent` (host service, 19.9 MB), `opacusr-server` (relay, 3.9 MB) + README.md / README.ru.md / LICENSE |
| `opacusr-0.10.0-d8f2056-linux-x86_64.tar.gz.sha256` | SHA-256 checksum |

## What this build is

- Version **0.10.0** (wire protocol v9, bincode 2.x codec), commit
  `d8f2056` — the same code as the `v0.10.0` tag **plus** the full-audit
  batch `d313284` (cargo-audit policy in `.cargo/audit.toml`, 16
  semver-compatible dependency bumps).
- Built with `cargo build --release --workspace` on Ubuntu (x86_64),
  rustc 1.99.1, real ALSA (`libasound2` 1.2.14); X11 capture/audio
  stack linked statically or from base distro libs as in the CI
  `tar.gz` job. Verified before publishing: `cargo fmt` clean,
  clippy matrix ×9 clean (`-D warnings`, incl. windows-msvc /
  apple-darwin / windows-gnu cross targets), **176 tests passed,
  0 failed**.
- Smoke-tested: `opacusr-agent --version`, `opacusr-server --version`
  → `0.10.0`.

## Quick start

```bash
tar -xzf opacusr-0.10.0-d8f2056-linux-x86_64.tar.gz
cd opacusr-0.10.0-d8f2056-linux-x86_64
./opacusr            # GUI client (Connect / Host)
./opacusr-agent run  # host service; add --help for flags
./opacusr-server     # self-hosted signal/relay server
```

## Platform installers

Windows (zip + MSI), macOS (aarch64), Linux wayland variant and
`.deb` packages are attached to the GitHub Releases:
<https://github.com/opacusio/opacusr/releases> — they are built by
the tag-triggered `release.yml` pipeline. This folder only carries the
linux-x86_64 rebuild of the audited HEAD so the exact bytes of this
commit are always available.
