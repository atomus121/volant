# Build release binaries and produce the two distributables:
#   dist\volant-<ver>-portable.zip  - unzip anywhere, run volant.exe
#   dist\volant-<ver>-setup.zip     - unzip, double-click install.cmd (per-user install)
# ASCII only: PowerShell 5.1 reads this file as ANSI.
param(
    [string]$Version = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Version) {
    $line = Select-String -Path "crates\app\Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1
    $Version = $line.Matches[0].Groups[1].Value
}

Write-Host "== build $Version"
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\cargo.ps1 build --release -p volant-app --bins
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }

$stage = "dist\stage"
$portable = "dist\volant-$Version-portable"
$setup = "dist\volant-$Version"
foreach ($d in @($stage, $portable, $setup)) {
    if (Test-Path $d) { Remove-Item $d -Recurse -Force }
    New-Item -ItemType Directory -Path $d | Out-Null
}

$exe = "target\release\volant.exe"
$hostExe = "target\release\volant-host.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
if (-not (Test-Path $hostExe)) { throw "missing $hostExe" }

# Portable: everything needed to run and to load the extension by hand.
Copy-Item $exe (Join-Path $portable "volant.exe")
Copy-Item $hostExe (Join-Path $portable "volant-host.exe")
Copy-Item -Recurse extension (Join-Path $portable "extension")
Copy-Item docs\PRD.md (Join-Path $portable "README-PRD.md")
[System.IO.File]::WriteAllText(
    (Join-Path $portable "VERSION"),
    $Version,
    (New-Object System.Text.UTF8Encoding $false)
)

# Setup: same payload plus the installer.
Copy-Item -Recurse $portable\* $setup
Copy-Item tools\install.ps1 (Join-Path $setup "install.ps1")
Copy-Item tools\uninstall.ps1 (Join-Path $setup "uninstall.ps1")
Copy-Item tools\install.cmd (Join-Path $setup "install.cmd")

$portableZip = "dist\volant-$Version-portable.zip"
$setupZip = "dist\volant-$Version-setup.zip"
foreach ($z in @($portableZip, $setupZip)) { if (Test-Path $z) { Remove-Item $z -Force } }
Compress-Archive -Path "$portable\*" -DestinationPath $portableZip
Compress-Archive -Path "$setup\*" -DestinationPath $setupZip

$raw = (Get-Item $exe).Length + (Get-Item $hostExe).Length
foreach ($z in @($portableZip, $setupZip)) {
    $mb = [math]::Round((Get-Item $z).Length / 1MB, 2)
    Write-Host ("{0}  {1} MB" -f (Split-Path -Leaf $z), $mb)
}
Write-Host ("raw exe total {0} MB" -f [math]::Round($raw / 1MB, 2))
