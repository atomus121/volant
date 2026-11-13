@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0gpui-metrics.ps1" %*
exit /b %errorlevel%
