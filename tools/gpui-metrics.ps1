# Measure GPUI startup and steady-state process cost with one consistent method.
# "startup" ends when Windows exposes a main window handle; it is a proxy for
# interactive, not a measured first painted frame.
param(
    [string]$Exe = "crates\app-gpui\target\release\volant.exe",
    [int]$WarmupSeconds = 15,
    [int]$SampleSeconds = 30,
    [string[]]$ArgList = @(),
    [switch]$Demo
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Add-Type -Namespace VolantMetrics -Name Native -MemberDefinition @'
[DllImport("user32.dll")]
public static extern bool SetWindowPos(
    IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
'@

$exePath = (Resolve-Path $Exe).Path
if ($Demo) { $ArgList = @("--demo") }
$runId = [guid]::NewGuid().ToString("N")
$homeDir = Join-Path $root "target\gpui-metrics\$runId"
New-Item -ItemType Directory -Force -Path $homeDir | Out-Null

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exePath
$psi.Arguments = ($ArgList | ForEach-Object {
    '"' + ($_ -replace '"', '\"') + '"'
}) -join ' '
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$psi.EnvironmentVariables["VOLANT_HOME"] = $homeDir

$clock = [System.Diagnostics.Stopwatch]::StartNew()
$process = [System.Diagnostics.Process]::Start($psi)
while ($clock.Elapsed.TotalSeconds -lt 10) {
    Start-Sleep -Milliseconds 10
    $process.Refresh()
    if ($process.HasExited) { throw "Volant exited during startup: $($process.ExitCode)" }
    if ($process.MainWindowHandle -ne [IntPtr]::Zero) { break }
}
if ($process.MainWindowHandle -eq [IntPtr]::Zero) {
    $process.Kill()
    throw "no main window handle within 10 seconds"
}
$startupMs = $clock.Elapsed.TotalMilliseconds

# Keep the window visible and unoccluded during the idle sample.
[void][VolantMetrics.Native]::SetWindowPos(
    $process.MainWindowHandle, [IntPtr](-1), 0, 0, 0, 0, 0x41)
Start-Sleep -Seconds $WarmupSeconds

$process.Refresh()
$idleWorkingSet = $process.WorkingSet64
$idlePrivate = $process.PrivateMemorySize64
$cpuStart = $process.TotalProcessorTime.TotalSeconds
$sampleClock = [System.Diagnostics.Stopwatch]::StartNew()
$peakWorkingSet = $idleWorkingSet
$peakPrivate = $idlePrivate

while ($sampleClock.Elapsed.TotalSeconds -lt $SampleSeconds) {
    Start-Sleep -Milliseconds 500
    $process.Refresh()
    if ($process.HasExited) { throw "Volant exited during sampling: $($process.ExitCode)" }
    $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.WorkingSet64)
    $peakPrivate = [Math]::Max($peakPrivate, $process.PrivateMemorySize64)
}

$process.Refresh()
$cpuSeconds = $process.TotalProcessorTime.TotalSeconds - $cpuStart
$elapsed = $sampleClock.Elapsed.TotalSeconds
$cpuPercent = if ($elapsed -gt 0) { $cpuSeconds / $elapsed * 100 } else { 0 }

"startup_window_ms={0:N1}" -f $startupMs
"idle_working_set_mb={0:N1}" -f ($idleWorkingSet / 1MB)
"idle_private_mb={0:N1}" -f ($idlePrivate / 1MB)
"peak_working_set_mb={0:N1}" -f ($peakWorkingSet / 1MB)
"peak_private_mb={0:N1}" -f ($peakPrivate / 1MB)
"idle_cpu_pct_one_core={0:N3}" -f $cpuPercent
"sample_seconds={0:N1}" -f $elapsed
"home={0}" -f $homeDir

$process.Kill()
$process.WaitForExit()
