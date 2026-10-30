 $ErrorActionPreference = "Stop"
Set-Location D:\opacusr

function Apply([string]$file, [string]$old, [string]$new) {
  $full = Join-Path (Get-Location) $file
  $text = [IO.File]::ReadAllText($full) -replace "`r`n", "`n"
  $old = $old -replace "`r`n", "`n"
  $new = $new -replace "`r`n", "`n"
  if ($text.Contains($new)) { Write-Host "SKIP: $file already patched"; return }
  if (-not $text.Contains($old)) { throw "Anchor not found in $file" }
  [IO.File]::WriteAllText($full, $text.Replace($old, $new))
  Write-Host "OK: patched $file"
}

# ===== README.ru.md: install link (Cyrillic via Base64 - encoding-proof) =====
 $oldB64 = "IyMjINCj0YHRgtCw0L3QvtCy0LrQsCAo0L/QsNC60LXRgtGLKQoKfCDQn9C70LDRgtGE0L7RgNC80LAgfCDQn9Cw0LrQtdGCIHwg0J/RgNC40LzQtdGH0LDQvdC40Y8gfA=="
 $newB64 = "IyMjINCj0YHRgtCw0L3QvtCy0LrQsCAo0L/QsNC60LXRgtGLKQoKKirQodC60LDRh9C40LLQsNC90LjQtToqKiBb0L/QvtGB0LvQtdC00L3QuNC5INGA0LXQu9C40LddKGh0dHBzOi8vZ2l0aHViLmNvbS9vcGFjdXNpby9vcGFjdXNyL3JlbGVhc2VzL2xhdGVzdCkg4oCUIGAuZGViYCDQtNC70Y8gRGViaWFuL1VidW50dSwgYC5tc2lgIC8gYC56aXBgINC00LvRjyBXaW5kb3dzLCBgLnRhci5nemAg0LTQu9GPIG1hY09TINC4INC/0L7RgNGC0LDRgtC40LLQvdC+0LPQviBMaW51eC4g0KMg0LrQsNC20LTQvtCz0L4g0YTQsNC50LvQsCDQtdGB0YLRjCBTSEEtMjU2INC60L7QvdGC0YDQvtC70YzQvdCw0Y8g0YHRg9C80LzQsCDQsiBgU0hBMjU2U1VNU2AgKNC/0YDQvtCy0LXRgNC60LA6IGBzaGEyNTZzdW0gLWMgU0hBMjU2U1VNU2ApLgoKfCDQn9C70LDRgtGE0L7RgNC80LAgfCDQn9Cw0LrQtdGCIHwg0J/RgNC40LzQtdGH0LDQvdC40Y8gfA=="
 $old = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($oldB64))
 $new = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($newB64))
Apply "README.ru.md" $old $new

# ===== SECURITY.md =====
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

# ===== Issue templates =====
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

# ===== Changelog categories =====
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

# ===== Commit & push everything (9 patches from the previous run included) =====
git add -A
git diff --cached --stat
git commit -m "ci: SHA256SUMS + smoke tests in release workflow; docs: README badges and install links; add SECURITY.md, issue templates, changelog categories"
if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
git push origin main
if ($LASTEXITCODE -ne 0) { throw "git push failed" }

Write-Host ""
Write-Host "ALL DONE."