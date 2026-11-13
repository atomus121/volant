@echo off
setlocal
set "ROOT=%~dp0.."
set "VS_SETUP=%ROOT%\target\tmp\vs_installer_payload\Contents\setup.exe"

if not exist "%VS_SETUP%" (
  echo Visual Studio installer payload is missing.
  echo Ask Codex to download it again, or install Visual Studio 2022 Build Tools manually.
  pause
  exit /b 1
)

echo Requesting administrator permission for Visual Studio Build Tools...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Start-Process -FilePath '%VS_SETUP%' -Verb RunAs -ArgumentList 'install --quiet --norestart --nocache --installPath C:\BuildTools --productId Microsoft.VisualStudio.Product.BuildTools --channelId VisualStudio.17.Release --channelUri https://aka.ms/vs/17/release/channel --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.VC.CMake.Project'"
exit /b %errorlevel%
