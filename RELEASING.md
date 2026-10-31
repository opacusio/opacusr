# Releasing

The GitHub Actions workflow (.github/workflows/release.yml) builds Linux
(deb + tar.gz), Windows (msi + zip) and macOS (tar.gz) from a `v*` tag and
publishes a GitHub Release whose body is extracted from CHANGELOG.md.

## Checklist

1. Bump `version` in the root `Cargo.toml` to `X.Y.Z` and let any cargo
   command (e.g. `cargo check`) refresh `Cargo.lock`; commit both.
   - .deb files are versioned from `Cargo.toml`;
   - msi/zip/tar.gz names come from the tag.
   Tag `vX.Y.Z` must match `Cargo.toml` version `X.Y.Z`.
2. Update the **Status** line in `README.md` and `README.ru.md`.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`
   and fill it (Keep a Changelog categories). The publish job hard-fails
   if the section is missing or empty - on purpose.
4. Commit and push `main` first, then tag:
       git tag vX.Y.Z
       git push origin vX.Y.Z
   A tag snapshots a commit - it does not follow the branch, so tag only
   after the release commits are on `main`.
5. Watch the run in Actions: three build jobs, then "Publish GitHub Release".
6. Verify the release page: body from the changelog and all assets
   (linux/macos tar.gz, windows zip + msi, two .deb, SHA256SUMS).

## Gotchas (learned on v0.3.1)

- The publish job checks out the repository itself. CHANGELOG.md must be
  present at the tag commit under the exact name `CHANGELOG.md`
  (Linux runners are case-sensitive).
- Linux and macOS release binaries link Opus statically (built in CI).
  Do not switch audiopus_sys back to a system shared library without a
  smoke test: the binary must start on a clean machine.