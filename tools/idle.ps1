# Idle CPU probe for volant.exe (ASCII only: PS 5.1 reads .ps1 as ANSI).
# Usage: .\tools\idle.ps1 [-Seconds 300] [-Exe .\target\release\volant.exe]
# Prints the process CPU delta per minute (as a percentage of one core) plus a
# total for the whole window. The window is pinned to (0,0), topmost, so the
# number is "visible but untouched", not "occluded".
param(
    [int]$Seconds = 300,
    [string]$Exe = ".\target\release\volant.exe",
    [string]$Home = ".\target\t6\home2"
)

$ErrorActionPreference = "Stop"
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
'@

$exe = (Resolve-Path $Exe).Path
$homeDir = (Resolve-Path $Home).Path
$env:VOLANT_HOME = $homeDir

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.Arguments = "--size 780x480"
$psi.UseShellExecute = $false
$p = [System.Diagnostics.Process]::Start($psi)
Start-Sleep -Seconds 3
[void][W.U]::SetWindowPos($p.MainWindowHandle, [IntPtr](-1), 0, 0, 0, 0, 0x41)
Start-Sleep -Seconds 7   # warm-up: startup frames, font atlas, shader compile

$prev = $p.TotalProcessorTime.TotalMilliseconds
$total = 0.0
for ($i = 0; $i -lt $Seconds; $i += 60) {
    Start-Sleep -Seconds 60
    $p.Refresh()
    $now = $p.TotalProcessorTime.TotalMilliseconds
    $delta = $now - $prev
    $total += $delta
    "minute {0,2}: {1,8:N1} ms  {2:N3} %" -f (($i / 60) + 1), $delta, ($delta / 600)
    $prev = $now
}
"total {0} s: {1:N1} ms  {2:N4} % of one core" -f $Seconds, $total, ($total / ($Seconds * 10))
$p.Kill()
