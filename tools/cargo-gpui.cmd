@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0cargo-gpui.ps1" %*
exit /b %errorlevel%
