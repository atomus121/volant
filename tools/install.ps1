# Per-user install: copy into %LOCALAPPDATA%\Programs\Volant, register the
# native messaging host, create shortcuts, add an uninstall entry.
# No admin rights needed. ASCII only (PowerShell 5.1 reads .ps1 as ANSI).
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA "Programs\Volant"
$exe = Join-Path $dest "volant.exe"

Write-Host "installing to $dest"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item (Join-Path $here "volant.exe") $exe -Force
Copy-Item (Join-Path $here "volant-host.exe") (Join-Path $dest "volant-host.exe") -Force
if (Test-Path (Join-Path $here "extension")) {
    # Copy-Item -Recurse into an existing directory creates extension\extension on
    # reinstall, leaving the browser pointing at the previous version.
    if (Test-Path (Join-Path $dest "extension")) {
        Remove-Item (Join-Path $dest "extension") -Recurse -Force
    }
    Copy-Item -Recurse (Join-Path $here "extension") (Join-Path $dest "extension") -Force
}

# Browser side: manifests + HKCU registry keys. volant.exe is a GUI binary, so
# its stdout is not visible; ask it to do the work, then check the result below.
cmd /c "`"$exe`" --install-host" | Out-Null
$reg = "HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.volant.host"
if (Test-Path $reg) {
    Write-Host "  browser host registered: $((Get-ItemProperty $reg).'(default)')"
} else {
    Write-Host "  WARNING: browser host not registered."
    Write-Host "  Run this once from a normal console: `"$exe`" --install-host"
}

function New-Shortcut($path, $target) {
    $dir = Split-Path -Parent $path
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($path)
    $lnk.TargetPath = $target
    $lnk.WorkingDirectory = Split-Path -Parent $target
    $lnk.IconLocation = $target
    $lnk.Save()
}
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Volant.lnk"
New-Shortcut $startMenu $exe
$desktop = Join-Path ([Environment]::GetFolderPath("Desktop")) "Volant.lnk"
New-Shortcut $desktop $exe

$ver = (Get-Item $exe).VersionInfo.FileVersion
if (-not $ver) {
    $versionFile = Join-Path $here "VERSION"
    if (Test-Path $versionFile) {
        $ver = (Get-Content $versionFile -Raw).Trim()
    }
}
if (-not $ver) { $ver = "unknown" }
$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Volant"
New-Item -Path $key -Force | Out-Null
Set-ItemProperty -Path $key -Name "DisplayName" -Value "Volant"
Set-ItemProperty -Path $key -Name "DisplayVersion" -Value $ver
Set-ItemProperty -Path $key -Name "Publisher" -Value "Volant"
Set-ItemProperty -Path $key -Name "InstallLocation" -Value $dest
Set-ItemProperty -Path $key -Name "DisplayIcon" -Value $exe
$uninstall = Join-Path $dest "uninstall.ps1"
Copy-Item (Join-Path $here "uninstall.ps1") $uninstall -Force
Set-ItemProperty -Path $key -Name "UninstallString" `
    -Value "powershell -NoProfile -ExecutionPolicy Bypass -File `"$uninstall`""

Write-Host ""
Write-Host "done. launch from the Start Menu shortcut, or:"
Write-Host "  $exe"
Write-Host "The browser extension lives in $dest\extension (load unpacked)."
