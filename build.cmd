@echo off
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"
echo [build] cargo build -p inputshare --release
cargo build -p inputshare --release
if errorlevel 1 exit /b %ERRORLEVEL%
if not exist "dist" mkdir dist
copy /Y "target\release\INTmouse.exe" "dist\INTmouse.exe" >nul
echo.
echo  OK  dist\INTmouse.exe
echo.
exit /b 0
