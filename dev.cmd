@echo off
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"
if "%~1"=="" goto gui
if /i "%~1"=="gui" goto gui
if /i "%~1"=="status" goto status
if /i "%~1"=="test" goto test
if /i "%~1"=="init" goto init
if /i "%~1"=="run" goto run
if /i "%~1"=="help" goto help
echo Unknown: %~1
goto help
:gui
echo [dev] UI + tray
cargo run -p inputshare
exit /b %ERRORLEVEL%
:status
cargo run -p inputshare -- status
exit /b %ERRORLEVEL%
:init
cargo run -p inputshare -- init-config
exit /b %ERRORLEVEL%
:run
cargo run -p inputshare -- run
exit /b %ERRORLEVEL%
:test
cargo test --workspace
exit /b %ERRORLEVEL%
:help
echo.
echo   InputShare - dev
echo   dev.cmd           UI + tray
echo   dev.cmd status
echo   dev.cmd init
echo   dev.cmd test
echo   build.cmd         release exe
echo.
exit /b 0

