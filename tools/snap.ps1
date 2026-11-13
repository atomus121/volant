# One-shot screenshot/metrics runner: isolated VOLANT_HOME (seeded demo rows),
# waits for the PNG to appear and then kills the process.
#
# Why not WaitForExit on the process: a GUI-subsystem exe started via
# Start-Process -PassThru hangs there (docs/HANDOFF.md section 10.4). The app's
# --shot mode writes the file itself and exits, so polling the file is enough.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Size = "",
    [string]$Extra = "",
    [string]$HomeDir = "",
    [int]$TimeoutSec = 60
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if ($HomeDir -eq "") { $HomeDir = Join-Path $root "target\shot\home" }

$outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
if (Test-Path $outPath) { Remove-Item $outPath -Force }

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path $Exe).Path
if ($Size -eq "") { $psi.Arguments = "--shot `"$outPath`" --wait 1200 $Extra" } else { $psi.Arguments = "--shot `"$outPath`" --size $Size --wait 1200 $Extra" }
$psi.UseShellExecute = $false
$psi.EnvironmentVariables["VOLANT_HOME"] = (Resolve-Path $HomeDir).Path
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$p = [System.Diagnostics.Process]::Start($psi)
$null = $p.StandardOutput.ReadToEndAsync()
$null = $p.StandardError.ReadToEndAsync()

$deadline = (Get-Date).AddSeconds($TimeoutSec)
while ((Get-Date) -lt $deadline) {
    if (Test-Path $outPath) {
        Start-Sleep -Milliseconds 250
        if (-not $p.HasExited) { $p.Kill() }
        Write-Host ("saved {0} ({1} bytes)" -f $outPath, (Get-Item $outPath).Length)
        exit 0
    }
    if ($p.HasExited) { break }
    Start-Sleep -Milliseconds 200
}

if (-not $p.HasExited) { $p.Kill() }
Write-Host "no screenshot: $outPath"
exit 1
