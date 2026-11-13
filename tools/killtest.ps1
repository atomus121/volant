# 崩溃续传压测：连着杀几次，每次都用「盘上真有的字节 − 状态文件记的字节」对账。
#
#   .\tools\killtest.ps1                       # 4 GiB，杀 3 次，全速
#   .\tools\killtest.ps1 -DiskRate 8M          # 慢盘，逼写队列积压
#   .\tools\killtest.ps1 -Cycles 5 -KillAfterMs 300
#
# 盘上真有的字节 = part 文件的已分配区间之和（稀疏文件，没写过的部分是洞）；
# 状态文件记的字节 = killresume --show 读出来的 downloaded()。
# 判据（PRD 6.3）：每一轮的 lost 都 <= 1 MiB，最后一轮逐字节校验通过。
param(
    [string]$Url = "",
    [int64]$Size = 4294967296,
    [string]$Dir = "target\killtest",
    [int]$Cycles = 3,
    [int]$KillAfterMs = 400,
    [string]$DiskRate = "",
    [int]$Conns = 8,
    [switch]$NoVerify
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# URL 默认跟着 -Size 走，否则改了 Size 还是在量上一次那个文件。
if (-not $Url) { $Url = "http://127.0.0.1:47777/file/$Size" }

$exe = "target\release\examples\killresume.exe"
if (-not (Test-Path $exe)) { throw "缺 $exe，先 cargo build --release -p volant-engine --examples" }

if ($DiskRate) { $env:VOLANT_DISK_RATE = $DiskRate } else { Remove-Item Env:VOLANT_DISK_RATE -ErrorAction SilentlyContinue }
$env:VOLANT_CONNS = "$Conns"

if (Test-Path $Dir) { Remove-Item -Recurse -Force $Dir }
New-Item -ItemType Directory -Path $Dir -Force | Out-Null

# 后台起一个下载。杀进程按映像名来：从文件里解析 pid 会读到写了一半的行，靠不住。
function Start-Download([string]$out, [switch]$Verify) {
    if (Test-Path $out) { Remove-Item $out -Force }
    $extra = if ($Verify) { " --verify" } else { "" }
    cmd /c "start `"`" /b $exe $Url $Dir$extra > $out 2>&1"
    for ($i = 0; $i -lt 400; $i++) {
        Start-Sleep -Milliseconds 25
        if ((Test-Path $out) -and (Select-String -Path $out -Pattern '^pid .*\d' -Quiet)) { return }
    }
    throw "没等到 pid，看 $out"
}

# 状态文件里记了多少字节
function Read-Recorded {
    $state = Get-ChildItem $Dir -Filter *.state -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $state) { return $null }
    $line = & $exe --show $state.FullName
    if ($line -match '^downloaded=(\d+)') { return [int64]$Matches[1] }
    throw "读不出 $($state.FullName): $line"
}

# part 文件真正占了多少字节（稀疏文件里写过的部分才有分配）
function Read-Allocated {
    $part = Get-ChildItem $Dir -Filter *.part -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $part) { return $null }
    $sum = [int64]0
    foreach ($line in (& fsutil file queryAllocRanges offset=0 length=$Size $part.FullName)) {
        if ($line -match 'Length: 0x([0-9a-fA-F]+)') { $sum += [Convert]::ToInt64($Matches[1], 16) }
    }
    $sum
}

$rows = @()

for ($c = 1; $c -le $Cycles; $c++) {
    $out = "target\killtest-$c.out"
    Start-Download $out
    Start-Sleep -Milliseconds $KillAfterMs
    try { & taskkill /F /IM killresume.exe 2>$null | Out-Null } catch { }
    Start-Sleep -Milliseconds 400
    $rec = Read-Recorded
    $alloc = Read-Allocated
    $lost = if ($null -ne $rec -and $null -ne $alloc) { $alloc - $rec } else { $null }
    $rows += [pscustomobject]@{ Cycle = $c; Recorded = $rec; OnDisk = $alloc; Lost = $lost }
    Write-Host ("第 {0} 轮：状态记了 {1}，盘上有 {2}，差 {3}" -f $c, $rec, $alloc, $lost)
    # 这一轮还没杀就下完了（文件小、速度高）：再杀也没意义，直接去收尾。
    if ($null -ne $rec -and $rec -ge $Size) {
        Write-Host "已经下完，不再杀了"
        break
    }
}

# 最后一轮不杀，跑完 + 校验
$out = "target\killtest-final.out"
Start-Download $out -Verify
Write-Host "最后一轮已起，等它跑完…"
$deadline = (Get-Date).AddMinutes(20)
while ((Get-Process -Name killresume -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
}
$tail = Get-Content -Path $out -Encoding UTF8 -ErrorAction SilentlyContinue
$rows += [pscustomobject]@{ Cycle = "final"; Recorded = $null; OnDisk = $null; Lost = $null }

Write-Host ""
$rows | Format-Table -AutoSize | Out-String | Write-Host
$tail | Select-Object -Last 3 | ForEach-Object { Write-Host $_ }

$bad = @($rows | Where-Object { $null -ne $_.Lost -and $_.Lost -gt 1MB })
if ($bad.Count -gt 0) {
    Write-Host ("超预算的轮次：" + (($bad | ForEach-Object { "$($_.Cycle)=$($_.Lost)B" }) -join ", "))
    exit 1
}
if (-not $NoVerify) {
    if ($tail -match '校验通过') { Write-Host "全部通过"; exit 0 }
    Write-Host "没看到校验通过"
    exit 1
}
Write-Host "没校验（-NoVerify）"
