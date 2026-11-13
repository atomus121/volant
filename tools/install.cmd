@echo off
rem Double-click entry point for the per-user installer.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
if errorlevel 1 (
  echo.
  echo Install failed. See the messages above.
  pause
)
