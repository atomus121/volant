# Measure one process' CPU cost from the outside.
#
#   .\tools\cpu.ps1 -Exe .\target\release\volant-cli.exe -ArgList 'get','http://...','-c','8','--rate','125M'
#
# Prints per-window CPU% (one window per -SampleMs) and a summary line:
#   cpu=7.42s over 16.02s -> 46.3% of one core, peak 61.2%
#
# Why external sampling: the engine's own cpu_avg_pct was caught reporting 44% and 120%
# for the same run (docs/HANDOFF.md section 6). Get-Process above the process boundary
# is the only number we trust.
#
# Use [System.Diagnostics.Process]::Start, not Start-Process: on this box Start-Process
# dies with "key NO_PROXY already added to dictionary".
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [string[]]$ArgList = @(),
  [int]$SampleMs = 250,
  [string]$WorkDir = "",
  [switch]$Quiet
)

$ErrorActionPreference = "Stop"

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path $Exe).Path
# Windows PowerShell 5.1 is .NET Framework: no ArgumentList, so quote by hand.
$psi.Arguments = ($ArgList | ForEach-Object { '"' + ($_ -replace '"', '\"') + '"' }) -join ' '
if ($WorkDir -ne "") { $psi.WorkingDirectory = (Resolve-Path $WorkDir).Path }
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true

$proc = [System.Diagnostics.Process]::Start($psi)
$outTask = $proc.StandardOutput.ReadToEndAsync()
$errTask = $proc.StandardError.ReadToEndAsync()

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$prevCpu = 0.0
$prevT = 0.0
$windows = New-Object System.Collections.Generic.List[double]
$threads = @()
while (-not $proc.HasExited) {
  Start-Sleep -Milliseconds $SampleMs
  if ($proc.HasExited) { break }
  $cpu = $proc.TotalProcessorTime.TotalSeconds
  $t = $sw.Elapsed.TotalSeconds
  $dt = $t - $prevT
  if ($dt -ge 0.2) {
    $windows.Add((($cpu - $prevCpu) / $dt) * 100.0)
    $prevCpu = $cpu
    $prevT = $t
    # Per-thread CPU: which thread burns the core? Sampled while still alive.
    $threads = $proc.Threads | ForEach-Object {
      [pscustomobject]@{ Id = $_.Id; Cpu = $_.TotalProcessorTime.TotalSeconds }
    }
  }
}
$sw.Stop()
$proc.WaitForExit()
$cpu = $proc.TotalProcessorTime.TotalSeconds
$user = $proc.UserProcessorTime.TotalSeconds
$priv = $proc.PrivilegedProcessorTime.TotalSeconds
$wall = $sw.Elapsed.TotalSeconds

$err = $errTask.Result
$out = $outTask.Result
if (-not $Quiet) {
  if ($out.Trim() -ne "") { Write-Output "--- stdout"; Write-Output $out.TrimEnd() }
  if ($err.Trim() -ne "") { Write-Output "--- stderr"; Write-Output $err.TrimEnd() }
}

$mean = if ($wall -gt 0) { $cpu / $wall * 100.0 } else { 0 }
$peak = if ($windows.Count -gt 0) { ($windows | Measure-Object -Maximum).Maximum } else { 0 }
$list = ($windows | ForEach-Object { "{0:N0}" -f $_ }) -join " "
Write-Output ("cpu={0:N2}s over {1:N2}s -> {2:N1}% of one core, peak {3:N1}%  [{4}]" -f $cpu, $wall, $mean, $peak, $list)
if ($wall -gt 0) {
  Write-Output ("split: user={0:N2}s ({1:N1}%) kernel={2:N2}s ({3:N1}%)" -f $user, ($user / $wall * 100), $priv, ($priv / $wall * 100))
}
if ($threads.Count -gt 0) {
  $top = $threads | Sort-Object Cpu -Descending | Select-Object -First 6
  $t = ($top | ForEach-Object { "t{0}={1:N2}s" -f $_.Id, $_.Cpu }) -join " "
  Write-Output ("threads(top6 of {0}): {1}" -f $threads.Count, $t)
}
Write-Output ("exit={0}" -f $proc.ExitCode)
