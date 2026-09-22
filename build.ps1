# auxbookmark Build Script (PowerShell)

Write-Host "===================================================" -ForegroundColor Cyan
Write-Host "  auxbookmark Build Script (PowerShell)" -ForegroundColor Cyan
Write-Host "===================================================" -ForegroundColor Cyan

# 1. Build release binary
Write-Host "[1/2] Building release binary (LTO + Strip)..." -ForegroundColor Yellow
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "[ERROR] Build failed!" -ForegroundColor Red
    exit $LASTEXITCODE
}

# 2. Deploy binary
Write-Host "[2/2] Deploying binary to current directory..." -ForegroundColor Yellow
$targetBin = "target\release\auxbookmark.exe"

if (Test-Path $targetBin) {
    Copy-Item $targetBin ".\auxbookmark.exe" -Force
    $fileInfo = Get-Item ".\auxbookmark.exe"
    $sizeKb = [math]::Round($fileInfo.Length / 1KB, 1)
    Write-Host "===================================================" -ForegroundColor Green
    Write-Host "  BUILD SUCCESS!" -ForegroundColor Green
    Write-Host "  Binary: .\auxbookmark.exe" -ForegroundColor Green
    Write-Host "  Size  : $($fileInfo.Length) bytes ($sizeKb KB)" -ForegroundColor Green
    Write-Host "===================================================" -ForegroundColor Green
} else {
    Write-Warning "Build finished, but could not locate release binary."
}
