@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0package-gpui.ps1" %*
exit /b %errorlevel%
