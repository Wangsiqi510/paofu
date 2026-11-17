@echo off
rem Portable package: the diagnostics script is embedded in 泡浮.exe.
start "" "%~dp0泡浮.exe" --collect-diagnostics
