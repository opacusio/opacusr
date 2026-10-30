 $ErrorActionPreference = "Stop"
Set-Location D:\opacusr

git checkout main
if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
git pull origin main
if ($LASTEXITCODE -ne 0) { throw "git pull failed" }

 $path = ".github/workflows/release.yml"
 $text = (Get-Content $path -Raw) -replace "`r`n", "`n"

# --- fix 1: copy .deb to workspace root after cargo deb ---
 $old1 = @'
      - name: Build debs
        run: |
          cargo install cargo-deb
          cargo deb --no-build -p opacusr-agent
          cargo deb --no-build -p opacusr-app
'@ -replace "`r`n", "`n"

 $new1 = @'
      - name: Build debs
        run: |
          cargo install cargo-deb
          cargo deb --no-build -p opacusr-agent
          cargo deb --no-build -p opacusr-app
          cp target/debian/*.deb .
'@ -replace "`r`n", "`n"

# --- fix 2: upload flat *.deb so they land at artifact root ---
 $old2 = @'
            opacusr-*-linux-x86_64.tar.gz
            target/debian/*.deb
'@ -replace "`r`n", "`n"

 $new2 = @'
            opacusr-*-linux-x86_64.tar.gz
            *.deb
'@ -replace "`r`n", "`n"

if (-not $text.Contains($old1)) { throw "Build debs block not found" }
if (-not $text.Contains($old2)) { throw "upload-artifact block not found" }

 $text = $text.Replace($old1, $new1).Replace($old2, $new2)
[IO.File]::WriteAllText((Join-Path (Get-Location) $path), $text)

git add $path
git commit -m "fix(release): copy .deb to workspace root so dist/*.deb glob matches on publish"
if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
git push origin main
if ($LASTEXITCODE -ne 0) { throw "git push failed" }

Write-Host "DONE. The fix applies to FUTURE tag releases (v0.3.1+)."
Write-Host "For current v0.3.0 attach the 2 .deb files manually, or re-push the tag to rebuild."