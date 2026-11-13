# T1 验收：按 PRD 6.1 的流程夹着裸写基向量一遍。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\t1.ps1
#
# 为什么要脚本：T1 的绝对值主要由「这块盘刚被写了多少」决定，同一份代码能测出
# 1686 / 1477 / 1076 MB/s。判据只看比值（引擎 ≥ 前后两次裸写平均的 80%），而比值
# 只有按固定顺序量才有意义：静置 → 裸写 A → 引擎 B → 裸写 C。
param([int]$Idle = 300, [long]$Size = 10737418240)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$bench = ".\target\release\examples\bench.exe"
if (-not (Test-Path $bench)) {
    throw "先编译：powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\cargo.ps1 build --release -p volant-labserver --bins -p volant-engine --examples"
}
$py = $env:VOLANT_PYTHON
if (-not $py) { $py = "C:\Users\EvanShaw\.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\python\python.exe" }

# 引擎那边用同一个尺寸（bench t1 读 VOLANT_SIZE），两边量的必须是同一件事。
$env:VOLANT_SIZE = "$Size"

New-Item -ItemType Directory -Force .\target\bench | Out-Null

function Get-RawWrite([string]$path) {
    $out = & $py .\tools\rawwrite.py $path $Size
    $mb = [double]([regex]::Match($out, "(\d+) MB/s").Groups[1].Value)
    Write-Host "  $out"
    return $mb
}

Write-Host "静置 $Idle 秒，让 SSD 缓存恢复……"
Start-Sleep -Seconds $Idle

Write-Host "裸写基线 A："
$a = Get-RawWrite ".\target\bench\raw-a.bin"

Write-Host "引擎 T1："
$t1 = & $bench t1 | Select-String "T1"
Write-Host "  $t1"
$b = [double]([regex]::Match($t1, "(\d+) MB/s").Groups[1].Value)

Write-Host "裸写基线 C："
$c = Get-RawWrite ".\target\bench\raw-c.bin"

$base = ($a + $c) / 2
$ratio = $b / $base
Write-Host ""
Write-Host ("引擎 {0:N0} MB/s ÷ 基线 {1:N0} MB/s（{2:N0} + {3:N0} 平均）= {4:P0}，判据 ≥ 80%" -f $b, $base, $a, $c, $ratio)
if ($ratio -ge 0.8) {
    Write-Host "T1 达标"
    exit 0
} else {
    Write-Host "T1 未达标"
    exit 1
}
