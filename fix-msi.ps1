 $ErrorActionPreference = "Stop"
Set-Location D:\opacusr

git checkout main
if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
git pull origin main
if ($LASTEXITCODE -ne 0) { throw "git pull failed" }

 $path = ".github/workflows/release.yml"
 $text = (Get-Content $path -Raw) -replace "`r`n", "`n"

 $old = @'
      - name: Build MSI
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Force build | Out-Null
          & wix\candle.exe -arch x64 -dMsiVersion=$($env:GITHUB_REF_NAME.Substring(1)) wix\main.wxs -o build\main.wixobj
          & wix\light.exe -ext WixUIExtension build\main.wixobj -o opacusr-$($env:GITHUB_REF_NAME.Substring(1))-x64.msi
'@ -replace "`r`n", "`n"

 $new = @'
      - name: Build MSI
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Force build | Out-Null
          $ver = $env:GITHUB_REF_NAME.Substring(1)
          & wix\candle.exe -arch x64 "-dMsiVersion=$ver" wix\main.wxs -o build\main.wixobj
          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
          & wix\light.exe -ext WixUIExtension build\main.wixobj -o "opacusr-$ver-x64.msi"
          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
'@ -replace "`r`n", "`n"

if (-not $text.Contains($old)) { throw "Build MSI block not found - edit the file manually" }
[IO.File]::WriteAllText((Join-Path (Get-Location) $path), $text.Replace($old, $new))

git add $path
git commit -m "fix(release): pass -dMsiVersion to candle as one quoted argument"
if ($LASTEXITCODE -ne 0) { throw "git commit failed" }
git push origin main
if ($LASTEXITCODE -ne 0) { throw "git push failed" }

git fetch origin
git tag -f v0.3.0 origin/main
git push -f origin v0.3.0
if ($LASTEXITCODE -ne 0) { throw "tag move failed" }

Write-Host "DONE. Release workflow started:"
Write-Host "https://github.com/opacusio/opacusr/actions"