# Synoptic on Windows: one-shot developer setup. Run in an elevated
# PowerShell on the machine (or VM) that will build and run it:
#
#   Set-ExecutionPolicy -Scope Process Bypass; .\tools\windows\setup.ps1
#
# Installs MSYS2 (winget) and git, then runs tools/windows/build.sh inside the
# UCRT64 shell, which installs the GTK4/GStreamer toolchain and builds a
# release binary. Nothing here needs admin apart from the winget installs.
$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")

if (-not (Test-Path "C:\msys64\usr\bin\bash.exe")) {
    winget install --id MSYS2.MSYS2 -e --accept-source-agreements --accept-package-agreements
}
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    winget install --id Git.Git -e --accept-source-agreements --accept-package-agreements
}

$bash = "C:\msys64\usr\bin\bash.exe"
$unixRepo = ($repo.Path -replace '\\', '/') -replace '^([A-Za-z]):', '/$1'
& $bash -lc "pacman -Syu --noconfirm" | Out-Host
& $bash -lc "cd '$unixRepo' && MSYSTEM=UCRT64 bash tools/windows/build.sh" | Out-Host
