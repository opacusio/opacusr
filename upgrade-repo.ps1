 $ErrorActionPreference = "Stop"
Set-Location D:\opacusr

git checkout main
if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
git pull origin main
if ($LASTEXITCODE -ne 0) { throw "git pull failed" }

function Apply([string]$file, [string]$old, [string]$new) {
  $full = Join-Path (Get-Location) $file
  $text = [IO.File]::ReadAllText($full) -replace "`r`n", "`n"
  $old = $old -replace "`r`n", "`n"
  $new = $new -replace "`r`n", "`n"
  if (-not $text.Contains($old)) { throw "Anchor not found in $file" }
  [IO.File]::WriteAllText($full, $text.Replace($old, $new))
  Write-Host "OK: patched $file"
}

# ===== 1. release.yml: smoke tests =====
Apply ".github/workflows/release.yml" @'
      - name: Test
        run: cargo test --workspace
      - name: Build debs
'@ @'
      - name: Test
        run: cargo test --workspace
      - name: Smoke test
        run: |
          ./target/release/opacusr-agent --version
          ./target/release/opacusr-server --version
      - name: Build debs
'@

Apply ".github/workflows/release.yml" @'
      - name: Test
        run: cargo test --workspace
      - name: Install WiX Toolset v3
'@ @'
      - name: Test
        run: cargo test --workspace
      - name: Smoke test
        shell: pwsh
        run: |
          & target\release\opacusr-agent.exe --version
          & target\release\opacusr-server.exe --version
      - name: Install WiX Toolset v3
'@

Apply ".github/workflows/release.yml" @'
        run: cargo build --release -p opacusr-app -p opacusr-agent -p opacusr-server
      - name: Package tar.gz
'@ @'
        run: cargo build --release -p opacusr-app -p opacusr-agent -p opacusr-server
      - name: Smoke test
        run: |
          ./target/release/opacusr-agent --version
          ./target/release/opacusr-server --version
      - name: Package tar.gz
'@

# ===== 1. release.yml: SHA256SUMS =====
Apply ".github/workflows/release.yml" @'
          path: dist
      - uses: softprops/action-gh-release@v2
'@ @'
          path: dist
      - name: Generate checksums
        run: |
          cd dist
          sha256sum * > SHA256SUMS
          cat SHA256SUMS
      - uses: softprops/action-gh-release@v2
'@

Apply ".github/workflows/release.yml" @'
            dist/*.deb
          generate_release_notes: true
'@ @'
            dist/*.deb
            dist/SHA256SUMS
          generate_release_notes: true
'@

# ===== 2. README.md: badges + install link =====
Apply "README.md" @'
# OpacusR

**Open-source remote desktop**
'@ @'
# OpacusR

[![Release](https://img.shields.io/github/v/release/opacusio/opacusr)](https://github.com/opacusio/opacusr/releases/latest)
[![CI](https://github.com/opacusio/opacusr/actions/workflows/ci.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/ci.yml)
[![Release build](https://github.com/opacusio/opacusr/actions/workflows/release.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/opacusio/opacusr)](LICENSE)

**Open-source remote desktop**
'@

Apply "README.md" @'
### Installing (packages)

| Platform | Package | Notes |
'@ @'
### Installing (packages)

**Download:** [latest release](https://github.com/opacusio/opacusr/releases/latest) — `.deb` for Debian/Ubuntu, `.msi` / `.zip` for Windows, `.tar.gz` for macOS and portable Linux. Every asset has a SHA-256 checksum in `SHA256SUMS` (verify with `sha256sum -c SHA256SUMS`).

| Platform | Package | Notes |
'@

# ===== 2. README.ru.md: badges + install link =====
Apply "README.ru.md" @'
# OpacusR

**
'@ @'
# OpacusR

[![Release](https://img.shields.io/github/v/release/opacusio/opacusr)](https://github.com/opacusio/opacusr/releases/latest)
[![CI](https://github.com/opacusio/opacusr/actions/workflows/ci.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/ci.yml)
[![Release build](https://github.com/opacusio/opacusr/actions/workflows/release.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/opacusio/opacusr)](LICENSE)

**
'@

Apply "README.ru.md" @'
### Установка (пакеты)

| Платформа | Пакет | Примечания |
'@ @'
### Установка (пакеты)

**Скачивание:** [последний релиз](https://github.com/opacusio/opacusr/releases/latest) — `.deb` для Debian/Ubuntu, `.msi` / `.zip` для Windows, `.tar.gz` для macOS и портативного Linux. У каждого файла есть SHA-256 контрольная сумма в `SHA256SUMS` (проверка: `sha256sum -c SHA256SUMS`).

| Платформа | Пакет | Примечания |
'@

# ===== 4. SECURITY.md =====
 $security = @'
# Security Policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.3.x | yes |
| < 0.3 | no |

## Reporting a vulnerability

OpacusR is remote-control software - treat any security issue with priority.

**Do not open a public issue for a security report.**

Use GitHub private vulnerability reporting:
https://github.com/opacusio/opacusr/security/advisories/new

Please include: affected version or commit, platform, component (viewer / agent / server / transport / crypto), reproduction steps or PoC, and impact assessment.

You will get an acknowledgment within 72 hours and a status update at least every 7 days until the issue is resolved. Fixes land as a patch release; reporters are credited in the release notes unless they prefer otherwise.

## Scope notes

- All transports are expected to be E2E-encrypted (ChaCha20-Poly1305); findings that break transport confidentiality or integrity are critical.
- The signaling/relay server is designed for self-hosting; reports about metadata leakage to third parties are in scope.
- The unattended-access password flow (challenge-response, keychain storage) is in scope.
'@
[IO.File]::WriteAllText((Join-Path (Get-Location) "SECURITY.md"), $security)
Write-Host "OK: wrote SECURITY.md"

# ===== 4. Issue templates =====
New-Item -ItemType Directory -Force ".github/ISSUE_TEMPLATE" | Out-Null

 $bugReport = @'
---
name: Bug report
about: Report a problem with OpacusR
labels: ["bug"]
---
<!-- Please search existing issues first -->

**OpacusR version / commit:** (e.g. v0.3.0, or `git rev-parse --short HEAD`)

**Component:** viewer (`opacusr`) / agent (`opacusr-agent`) / server (`opacusr-server`) / docker bundle

**Platform:** (OS + version, e.g. Ubuntu 24.04, Windows 11, macOS 15; X11 or Wayland where relevant)

**What happened?**

**Steps to reproduce:**
1.
2.

**Expected behavior:**

**Logs:** (agent: `journalctl --user -u opacusr-agent`; server console output; sanitize tokens and passwords)
'@
[IO.File]::WriteAllText((Join-Path (Get-Location) ".github/ISSUE_TEMPLATE/bug_report.md"), $bugReport)

 $featureRequest = @'
---
name: Feature request
about: Suggest an idea for OpacusR
labels: ["enhancement"]
---

**Problem to solve:** (what can't you do today?)

**Proposed solution:**

**Alternatives considered:**

**Additional context:**
'@
[IO.File]::WriteAllText((Join-Path (Get-Location) ".github/ISSUE_TEMPLATE/feature_request.md"), $featureRequest)

 $configYml = @'
blank_issues_enabled: false
'@
[IO.File]::WriteAllText((Join-Path (Get-Location) ".github/ISSUE_TEMPLATE/config.yml"), $configYml)
Write-Host "OK: wrote issue templates"

# ===== 5. Changelog categories =====
 $releaseCfg = @'
changelog:
  categories:
    - title: Features
      labels: [enhancement]
    - title: Bug fixes
      labels: [bug]
    - title: Documentation
      labels: [documentation]
    - title: Other changes
      labels: ["*"]
'@
[IO.File]::WriteAllText((Join-Path (Get-Location) ".github/release.yml"), $releaseCfg)
Write-Host "OK: wrote .github/release.yml"

# ===== Commit & push =====
git add -A
git diff --cached --stat
git commit -m "ci: SHA256SUMS + smoke tests in release workflow; docs: README badges and install links; add SECURITY.md, issue templates, changelog categories"
if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
git push origin main
if ($LASTEXITCODE -ne 0) { throw "git push failed" }

Write-Host ""
Write-Host "ALL DONE. Live immediately: badges, install links, SECURITY.md, issue templates."
Write-Host "On the NEXT tag: smoke tests + SHA256SUMS appear automatically."