# End-to-end check for the browser capture chain, without a browser.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\capture-e2e.ps1
#
# what it does:
#   1. start volant.exe with an isolated VOLANT_HOME
#   2. speak native messaging to volant-host.exe on stdin (length-prefixed JSON),
#      exactly like Chrome does when the extension calls connectNative()
#   3. read back the latency the app logged (VOLANT_CAPTURE_LOG) and the row it made
#
# the frame's `t` is Date.now() in the extension, so the number printed here is
# "browser click -> row in the app" minus the browser itself.

param(
    [string]$Url = "http://127.0.0.1:9431/file/10485760",
    [string]$AppPath = "target\release\volant.exe",
    [string]$HostExe = "target\release\volant-host.exe",
    [string]$VolantHome = "target\m5\homeE",
    [int]$WaitMs = 9000
)

$ErrorActionPreference = "Stop"

# a stale volant.exe still owns the capture pipe and would eat our frame
Get-Process volant, volant-host -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
$root = (Get-Location).Path
function Abs([string]$p) { if ([System.IO.Path]::IsPathRooted($p)) { $p } else { Join-Path $root $p } }
$AppPath = Abs $AppPath
$HostExe = Abs $HostExe
$VolantHome = Abs $VolantHome
$log = Join-Path $VolantHome "cap.log"

if (Test-Path $VolantHome) { Remove-Item -Recurse -Force $VolantHome }
New-Item -ItemType Directory -Force -Path $VolantHome | Out-Null
$dir = Join-Path $VolantHome "dl"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$state = @{
    version = 1; next_id = 1
    settings = @{
        limit = 3; connections = 8; rate = 0
        dir = $dir; clipboard = $false
    }
    rows = @()
} | ConvertTo-Json -Depth 6
[System.IO.File]::WriteAllText((Join-Path $VolantHome "state.json"), $state)

$env:VOLANT_HOME = $VolantHome
$env:VOLANT_CAPTURE_LOG = $log
$appPsi = New-Object System.Diagnostics.ProcessStartInfo
$appPsi.FileName = $AppPath
$appPsi.UseShellExecute = $false
$appPsi.WorkingDirectory = $root
$appProc = [System.Diagnostics.Process]::Start($appPsi)
Start-Sleep -Milliseconds 1500

# ---- one native messaging frame ----
$payload = @{
    url = $Url
    referer = "http://example.test/page"
    cookie = "sid=e2e; theme=dark"
    user_agent = "capture-e2e/1.0"
    t = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
} | ConvertTo-Json -Compress
$bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
$head = [BitConverter]::GetBytes([uint32]$bytes.Length)

$hostPsi = New-Object System.Diagnostics.ProcessStartInfo
$hostPsi.FileName = $HostExe
$hostPsi.UseShellExecute = $false
$hostPsi.RedirectStandardInput = $true
$hostPsi.WorkingDirectory = $root
$hostProc = [System.Diagnostics.Process]::Start($hostPsi)
$stdin = $hostProc.StandardInput.BaseStream
$stdin.Write($head, 0, 4)
$stdin.Write($bytes, 0, $bytes.Length)
$stdin.Flush()
$sent = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
$stdin.Close()
$hostProc.WaitForExit(5000) | Out-Null
$hostExit = if ($hostProc.HasExited) { $hostProc.ExitCode } else { -1 }

Start-Sleep -Milliseconds $WaitMs

# ---- what the app saw ----
$lag = "none"
if (Test-Path $log) {
    $line = Get-Content $log -Encoding UTF8 | Select-Object -First 1
    if ($line) { $lag = ($line -split "`t")[0] }
}
$saved = Get-Content (Join-Path $VolantHome "state.json") -Raw -Encoding UTF8 | ConvertFrom-Json
$row = $saved.rows | Select-Object -First 1
$status = if ($row) { $row.status } else { "no row" }
$size = if ($row) { "$($row.downloaded)/$($row.size)" } else { "-" }

if ($appProc -is [System.Diagnostics.Process] -and -not $appProc.HasExited) { $appProc.Kill(); $appProc.WaitForExit(3000) | Out-Null }

Write-Output "host:  exit=$hostExit  frame=$($bytes.Length)B"
Write-Output "app:   lag=${lag}ms  status=$status  bytes=$size"
if ($lag -eq "none") { Write-Output "FAIL: app did not log a capture"; exit 1 }
if (-not $row) { Write-Output "FAIL: no row in state.json"; exit 1 }
exit 0
