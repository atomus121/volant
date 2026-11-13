# Build GPUI crates with the MSVC toolchain and Visual Studio developer environment.
# Usage: .\tools\cargo-gpui.ps1 check --manifest-path crates\app-gpui\Cargo.toml
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$tc = "D:\Documents\deepseek-harness\.toolchains"
$cargo = Join-Path $tc "cargo\bin\cargo.exe"
$rustup = Join-Path $tc "cargo\bin\rustup.exe"

$vsCandidates = @(
    "C:\BuildTools",
    "C:\Program Files\Microsoft Visual Studio\2022\BuildTools",
    "C:\Program Files\Microsoft Visual Studio\2022\Community"
)
$vsRoot = $vsCandidates | Where-Object {
    Test-Path (Join-Path $_ "Common7\Tools\VsDevCmd.bat")
} | Select-Object -First 1
if (-not $vsRoot) {
    [Console]::Error.WriteLine("No Visual Studio C++ developer environment found.")
    [Console]::Error.WriteLine("Install VS 2022 Build Tools with the Desktop development with C++ workload.")
    exit 1
}

$env:CARGO_HOME = Join-Path $tc "cargo"
$env:RUSTUP_HOME = Join-Path $tc "rustup"
$env:AWS_LC_SYS_NO_JITTER_ENTROPY = "1"

$vsDev = Join-Path $vsRoot "Common7\Tools\VsDevCmd.bat"
$environment = & cmd.exe /d /s /c "`"$vsDev`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
if ($LASTEXITCODE -ne 0) {
    [Console]::Error.WriteLine("VsDevCmd.bat failed: $LASTEXITCODE")
    exit 1
}
foreach ($line in $environment) {
    $split = $line.IndexOf("=")
    if ($split -lt 1) { continue }
    $name = $line.Substring(0, $split)
    $value = $line.Substring($split + 1)
    Set-Item -Path "Env:$name" -Value $value
}

$tmp = Join-Path $root "target\tmp"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TMP = $tmp
$env:TEMP = $tmp
$env:TMPDIR = $tmp

& $cargo "+stable-x86_64-pc-windows-msvc" @CargoArgs
exit $LASTEXITCODE
