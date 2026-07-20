# Build a distributable FurryManager installer.
# ASCII only: PowerShell 5.1 reads .ps1 without BOM as ANSI.
#
# Usage (from project root):
#   powershell -ExecutionPolicy Bypass -File release.ps1
#   powershell -ExecutionPolicy Bypass -File release.ps1 -SkipSidecar
#
# Result: src-tauri\target\release\bundle\nsis\FurryManager_<ver>_x64-setup.exe

param([switch]$SkipSidecar)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$sidecarExe = "sidecar\dist\fm-sidecar.exe"

if (-not $SkipSidecar) {
    if (Test-Path $sidecarExe) {
        Write-Host "== Sidecar already built, reusing ==" -ForegroundColor Cyan
        Write-Host "   (delete sidecar\dist to force a rebuild)"
    } else {
        Write-Host "== Building standalone sidecar (long) ==" -ForegroundColor Cyan
        & powershell -ExecutionPolicy Bypass -File "sidecar\build.ps1"
    }
}

if (Test-Path $sidecarExe) {
    Write-Host "== Sidecar WILL be bundled: end user needs no Python ==" -ForegroundColor Green
} else {
    Write-Host "== No sidecar binary: app will need Python for AI features ==" -ForegroundColor Yellow
}

Write-Host "== Building app + installer ==" -ForegroundColor Cyan
cargo tauri build

$out = Get-ChildItem "src-tauri\target\release\bundle\nsis\*setup.exe" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($out) {
    $mb = [math]::Round($out.Length / 1MB, 1)
    Write-Host ""
    Write-Host "== Done ==" -ForegroundColor Green
    Write-Host "Installer: $($out.FullName)  ($mb MB)"
    Write-Host "Installing it over an older version upgrades in place and keeps settings and library."
} else {
    Write-Host "Installer not found - check cargo tauri build output above" -ForegroundColor Red
    exit 1
}
