# Build standalone ML sidecar for FurryManager (PyInstaller).
# ASCII only on purpose: PowerShell 5.1 reads .ps1 without BOM as ANSI,
# and Cyrillic text would break the parser.
#
# Usage (from project root):
#   powershell -ExecutionPolicy Bypass -File sidecar\build.ps1
#
# Result: sidecar\dist\fm-sidecar.exe (+ libraries next to it).
# The app picks it up automatically - no Python needed for the end user.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "== Checking Python ==" -ForegroundColor Cyan
python --version

Write-Host "== Installing sidecar deps and PyInstaller ==" -ForegroundColor Cyan
python -m pip install --upgrade pip
python -m pip install -r sidecar\requirements.txt
python -m pip install pyinstaller

Write-Host "== Building (takes a while: onnxruntime is big) ==" -ForegroundColor Cyan
python -m PyInstaller sidecar\fm-sidecar.spec --noconfirm `
    --distpath sidecar\dist_build --workpath sidecar\build

$built = "sidecar\dist_build\fm-sidecar"
if (-not (Test-Path "$built\fm-sidecar.exe")) {
    Write-Host "Build failed: $built\fm-sidecar.exe not found" -ForegroundColor Red
    exit 1
}

# Flatten into sidecar\dist (this is where the app and the installer look)
if (Test-Path "sidecar\dist") { Remove-Item "sidecar\dist" -Recurse -Force }
New-Item -ItemType Directory -Path "sidecar\dist" | Out-Null
Copy-Item "$built\*" "sidecar\dist\" -Recurse -Force
Remove-Item "sidecar\dist_build" -Recurse -Force

$size = [math]::Round((Get-ChildItem "sidecar\dist" -Recurse |
    Measure-Object -Property Length -Sum).Sum / 1MB, 1)

Write-Host ""
Write-Host "== Done ==" -ForegroundColor Green
Write-Host "Binary: sidecar\dist\fm-sidecar.exe  (package ~$size MB)"
Write-Host "Restart the app: Settings -> Storage -> 'AI module' should say Python is not required."
