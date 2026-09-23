@echo off
setlocal
echo ===================================================
echo   auxbookmark Build Script (Windows)
echo ===================================================

echo [1/2] Building release binary (LTO + Strip)...
cargo build --release
if %ERRORLEVEL% neq 0 (
    echo [ERROR] Build failed!
    exit /b %ERRORLEVEL%
)

echo [2/2] Deploying binary...
if exist "target\release\auxbookmark.exe" (
    copy /y "target\release\auxbookmark.exe" ".\auxbookmark.exe" >nul
)

if exist "auxbookmark.exe" (
    echo ===================================================
    echo   BUILD SUCCESS!
    echo   Output: .\auxbookmark.exe
    for %%F in (auxbookmark.exe) do echo   Size  : %%~zF bytes
    echo ===================================================
) else (
    echo [WARNING] auxbookmark.exe built, but could not auto-copy to current dir.
)

endlocal
