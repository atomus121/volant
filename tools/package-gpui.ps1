# Build the GPUI application and produce isolated preview packages:
#   dist\volant-<ver>-gpui-portable.zip
#   dist\volant-<ver>-gpui-setup.zip
# The egui packages produced by package.ps1 are intentionally left untouched.
# ASCII only: PowerShell 5.1 reads this file as ANSI.
param(
    [string]$Version = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Version) {
    $line = Select-String -Path "crates\app-gpui\Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1
    $Version = $line.Matches[0].Groups[1].Value
}

Write-Host "== GPUI build $Version"
& ".\tools\cargo-gpui.cmd" build --release `
    --manifest-path "crates\app-gpui\Cargo.toml" `
    --target-dir "crates\app-gpui\target" `
    --bin volant
if ($LASTEXITCODE -ne 0) { throw "GPUI cargo build failed: $LASTEXITCODE" }

Write-Host "== native messaging host"
& powershell -NoProfile -ExecutionPolicy Bypass -File ".\tools\cargo.ps1" `
    build --release -p volant-app --bin volant-host
if ($LASTEXITCODE -ne 0) { throw "host build failed: $LASTEXITCODE" }

$exe = "crates\app-gpui\target\release\volant.exe"
$hostExe = "target\release\volant-host.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
if (-not (Test-Path $hostExe)) { throw "missing $hostExe" }

# The debug host was previously copied into a package by mistake. Refuse any
# host large enough to be the debug binary before it reaches a distributable.
$hostBytes = (Get-Item $hostExe).Length
if ($hostBytes -gt 2MB) {
    throw "refusing $hostExe ($hostBytes bytes): expected the release host"
}

$portable = "dist\volant-$Version-gpui-portable"
$setup = "dist\volant-$Version-gpui-setup"
foreach ($dir in @($portable, $setup)) {
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    New-Item -ItemType Directory -Path $dir | Out-Null
}

Copy-Item $exe (Join-Path $portable "volant.exe")
Copy-Item $hostExe (Join-Path $portable "volant-host.exe")
Copy-Item -Recurse extension (Join-Path $portable "extension")
Copy-Item docs\PRD.md (Join-Path $portable "README-PRD.md")
Copy-Item docs\GPUI_MIGRATION.md (Join-Path $portable "README-GPUI.md")
Copy-Item docs\GPUI_P5_REPORT.md (Join-Path $portable "GPUI_P5_REPORT.md")
[System.IO.File]::WriteAllText(
    (Join-Path $portable "VERSION"),
    $Version,
    (New-Object System.Text.UTF8Encoding $false)
)

Copy-Item -Recurse (Join-Path $portable "*") $setup
Copy-Item tools\install.ps1 (Join-Path $setup "install.ps1")
Copy-Item tools\uninstall.ps1 (Join-Path $setup "uninstall.ps1")
Copy-Item tools\install.cmd (Join-Path $setup "install.cmd")

$portableZip = "dist\volant-$Version-gpui-portable.zip"
$setupZip = "dist\volant-$Version-gpui-setup.zip"
foreach ($zip in @($portableZip, $setupZip)) {
    if (Test-Path $zip) { Remove-Item $zip -Force }
}
Compress-Archive -Path (Join-Path $portable "*") -DestinationPath $portableZip
Compress-Archive -Path (Join-Path $setup "*") -DestinationPath $setupZip

foreach ($zip in @($portableZip, $setupZip)) {
    $mb = [math]::Round((Get-Item $zip).Length / 1MB, 2)
    Write-Host ("{0}  {1} MB" -f (Split-Path -Leaf $zip), $mb)
}
$raw = (Get-Item $exe).Length + $hostBytes
Write-Host ("raw exe total {0} MB" -f [math]::Round($raw / 1MB, 2))
