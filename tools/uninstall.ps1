# Undo install.ps1: remove shortcuts, uninstall entry and the program folder.
# User data (%LOCALAPPDATA%\Volant: state.json, logs) is kept unless -DataToo.
# ASCII only (PowerShell 5.1 reads .ps1 as ANSI).
param([switch]$DataToo)
$ErrorActionPreference = "Continue"
$dest = Join-Path $env:LOCALAPPDATA "Programs\Volant"

Write-Host "removing $dest"
foreach ($lnk in @(
    (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Volant.lnk"),
    (Join-Path ([Environment]::GetFolderPath("Desktop")) "Volant.lnk")
)) {
    if (Test-Path $lnk) { Remove-Item $lnk -Force }
}
Remove-Item "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Volant" -Recurse -Force -ErrorAction SilentlyContinue

# Native messaging keys point at manifests in the data folder; drop them too,
# otherwise the extension keeps pointing at a host that no longer exists.
$hostKey = "com.volant.host"
foreach ($parent in @(
    "Software\Google\Chrome\NativeMessagingHosts",
    "Software\Microsoft\Edge\NativeMessagingHosts",
    "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    "Software\Vivaldi\NativeMessagingHosts",
    "Software\Chromium\NativeMessagingHosts",
    "Software\Mozilla\NativeMessagingHosts"
)) {
    Remove-Item ("HKCU:\$parent\$hostKey") -Recurse -Force -ErrorAction SilentlyContinue
}
$native = Join-Path $env:LOCALAPPDATA "Volant\native"
if (Test-Path $native) { Remove-Item $native -Recurse -Force -ErrorAction SilentlyContinue }

if (Test-Path $dest) { Remove-Item $dest -Recurse -Force -ErrorAction SilentlyContinue }
if ($DataToo) {
    $data = Join-Path $env:LOCALAPPDATA "Volant"
    if (Test-Path $data) { Remove-Item $data -Recurse -Force -ErrorAction SilentlyContinue }
}
Write-Host "done."
