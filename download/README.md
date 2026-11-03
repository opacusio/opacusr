# download/

Prebuilt OpacusR binaries for **linux-x86_64**, **windows-x86_64** and
**macos-aarch64**. Two origins:

1. **Sandbox builds** (named with the commit sha, e.g. `-d8f2056-`) —
   compiled from this exact audited HEAD in the maintainer's sandbox,
   checksums below.
2. **CI builds** (plain `opacusr-0.10.0-*` names) — produced by the
   tag-triggered `release.yml` pipeline on the `v0.10.0` tag (commit
   `4fd7c27`), checksums re-verified against the release `SHA256SUMS`.

| file | origin | contents |
| --- | --- | --- |
| `opacusr-0.10.0-d8f2056-linux-x86_64.tar.gz` | sandbox, HEAD `d8f2056` | `opacusr` 33.8M + `opacusr-agent` 19.9M + `opacusr-server` 3.9M (X11 + real ALSA) |
| `opacusr-0.10.0-bed4104-windows-x86_64-gnu.zip` | sandbox cross-build (mingw-w64), HEAD `bed4104` | `opacusr-agent.exe` 18.7M + `opacusr-server.exe` 3.3M — **no GUI app, no Opus audio** (see note) |
| `opacusr-0.10.0-windows-x86_64.zip` | CI, tag `v0.10.0` (MSVC) | `opacusr.exe` 24.8M + `opacusr-agent.exe` 19.6M + `opacusr-server.exe` 3.6M — full build with GUI and audio |
| `opacusr-0.10.0-macos-aarch64.tar.gz` | CI, tag `v0.10.0` (macos runner) | `opacusr` + `opacusr-agent` + `opacusr-server`, Apple Silicon, full build |

Every artifact has a sibling `.sha256` file.

## Why the sandbox windows build has no app and no audio

`opacusr-app` (GUI) depends on `opacusr-client` with the `audio`
feature, and the vendored Opus (`audiopus_sys`) does not build for the
`windows-gnu` toolchain — a documented limitation of the cross-build
(see `crates/opacusr-agent/Cargo.toml` `[features]`). The same applies
to `opacusr-agent`'s `audio` feature: the sandbox `opacusr-agent.exe`
is built with `--no-default-features` (control/capture/screen-share
work, audio streaming does not). For everyday use on Windows take the
CI MSVC zip — it is the canonical full build. Tag a new release
(`v0.10.1`) to get full MSVC/macOS builds of a newer HEAD via CI.

macOS cross-compilation from Linux is not possible (Apple's SDK cannot
be redistributed), so macOS binaries are the CI-built ones.

## Windows sandbox build recipe (reproducible)

```
rustup target add x86_64-pc-windows-gnu
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_AR=x86_64-w64-mingw32-ar
cargo build --release -p opacusr-server --target x86_64-pc-windows-gnu
cargo build --release -p opacusr-agent --no-default-features \
            --target x86_64-pc-windows-gnu
```

Smoke check: both exes are valid `PE32+ executable … x86-64` per `file`.

## Checksums

```
319cd1e5c1df4257d73f9cdcd63691aadf4a9aa0b36e5a7d06f6b1afabae1ff8  opacusr-0.10.0-d8f2056-linux-x86_64.tar.gz
329b8f07097d54e7772f801e9ecd30876360ee4c7e87cfbecdd93426a3cc6ae9  opacusr-0.10.0-bed4104-windows-x86_64-gnu.zip
```

CI artifacts: see the sibling `.sha256` files (values match the
release `SHA256SUMS` of `v0.10.0`).

## Quick start (windows example, CI zip)

```powershell
Expand-Archive opacusr-0.10.0-windows-x86_64.zip
cd opacusr-0.10.0-windows-x86_64
.\opacusr.exe             # GUI client (Connect / Host)
.\opacusr-agent.exe run   # host service
.\opacusr-server.exe      # self-hosted signal/relay server
```

Platform notes (launchd service, privacy mode, printing, protocol v9):
see the root `README.md` and `docs/`.
