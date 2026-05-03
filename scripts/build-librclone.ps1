# Build librclone (rclone as a C-shared library) for the current host.
#
# Output: src-tauri\lib\<rust-target-triple>\librclone.{dll,so,dylib} + librclone.h
#
# Required tools (install once via winget):
#   winget install -e --id GoLang.Go
#   winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT.LLVM
#   winget install -e --id Git.Git
#
# Optional env vars:
#   $env:RCLONE_VERSION = "v1.69.0"
#   $env:RCLONE_SRC = "C:\path\to\rclone-source"

$ErrorActionPreference = "Stop"

$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
$RcloneVersion = if ($env:RCLONE_VERSION) { $env:RCLONE_VERSION } else { "v1.69.0" }
$SrcDir = if ($env:RCLONE_SRC) { $env:RCLONE_SRC } else { Join-Path $Root "build\rclone-src" }
$OutBase = Join-Path $Root "src-tauri\lib"

if ($IsWindows -or $env:OS -eq "Windows_NT") {
    $Triple = "x86_64-pc-windows-gnu"
    $LibName = "librclone.dll"
} elseif ($IsLinux) {
    $Triple = "x86_64-unknown-linux-gnu"
    $LibName = "librclone.so"
} elseif ($IsMacOS) {
    $Triple = "x86_64-apple-darwin"
    $LibName = "librclone.dylib"
} else {
    throw "Unsupported platform"
}

$OutDir = Join-Path $OutBase $Triple

Write-Host "[*] target triple : $Triple"
Write-Host "[*] output dir    : $OutDir"
Write-Host "[*] rclone version: $RcloneVersion"

if (-not (Get-Command go -ErrorAction SilentlyContinue)) {
    Write-Host "[!] go not found on PATH" -ForegroundColor Red
    Write-Host "    install with: winget install -e --id GoLang.Go"
    exit 1
}
$GoVer = & go version
Write-Host "[*] go: $GoVer"

if (-not (Get-Command gcc -ErrorAction SilentlyContinue)) {
    Write-Host "[!] gcc not found on PATH (librclone needs CGO)" -ForegroundColor Red
    Write-Host "    install with: winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT.LLVM"
    exit 1
}
$GccVer = (& gcc --version | Select-Object -First 1)
Write-Host "[*] cc: $GccVer"

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Host "[!] git not found" -ForegroundColor Red
    exit 1
}

if ($env:RCLONE_SRC) {
    if (-not (Test-Path $SrcDir)) { throw "RCLONE_SRC=$SrcDir does not exist" }
    Write-Host "[*] using existing source at $SrcDir"
}
elseif (-not (Test-Path (Join-Path $SrcDir ".git"))) {
    $ParentDir = Split-Path $SrcDir -Parent
    if (-not (Test-Path $ParentDir)) {
        New-Item -ItemType Directory -Path $ParentDir | Out-Null
    }
    Write-Host "[*] cloning rclone $RcloneVersion into $SrcDir ..."
    & git clone --depth 1 --branch $RcloneVersion `
        https://github.com/rclone/rclone.git $SrcDir
    if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
}
else {
    Write-Host "[*] reusing $SrcDir"
    Push-Location $SrcDir
    try {
        & git fetch --depth 1 origin $RcloneVersion
        if ($LASTEXITCODE -ne 0) { throw "git fetch failed" }
        & git checkout --quiet FETCH_HEAD
        if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
}

Write-Host "[*] building $LibName ..."
$OutPath = Join-Path $OutDir $LibName
Push-Location $SrcDir
try {
    $env:CGO_ENABLED = "1"
    & go build `
        -buildmode=c-shared `
        -trimpath `
        -ldflags="-s -w" `
        -o $OutPath `
        ./librclone
    if ($LASTEXITCODE -ne 0) { throw "go build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

Write-Host "[*] done"
Get-ChildItem $OutDir | Format-Table Name, Length, LastWriteTime

$HeaderPath = Join-Path $OutDir "librclone.h"
if (-not (Test-Path $HeaderPath)) {
    Write-Host "[!] librclone.h not generated at $HeaderPath" -ForegroundColor Red
    exit 1
}
Write-Host "[*] header: $HeaderPath"
