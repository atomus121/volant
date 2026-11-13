# Build through the local toolchain (see docs/HANDOFF.md section 3).
# Usage: .\tools\cargo.ps1 check -p volant-engine
#
# Prefer .toolchains next to the workspace, then the two historical paths.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$tcCandidates = @(
    (Join-Path (Split-Path -Parent $root) ".toolchains"),
    "D:\Documents\deepseek-harness\.toolchains",
    "D:\Documents\deepseek\.toolchains"
)
foreach ($tc in $tcCandidates) {
    if (Test-Path (Join-Path $tc "cargo\bin\cargo.exe")) {
        $env:CARGO_HOME = Join-Path $tc "cargo"
        $env:RUSTUP_HOME = Join-Path $tc "rustup"
        $env:Path = "$tc\cargo\bin;$tc\mingw64\bin;$env:Path"
        break
    }
}
$tmp = Join-Path $root "target\tmp"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TMP = $tmp; $env:TEMP = $tmp; $env:TMPDIR = $tmp
$env:AWS_LC_SYS_NO_JITTER_ENTROPY = "1"
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    [Console]::Error.WriteLine("cargo not found; check .toolchains next to the workspace or PATH.")
    exit 1
}
& $cargo.Source @args
$code = $LASTEXITCODE
if ($null -eq $code) { exit 1 }
exit $code
