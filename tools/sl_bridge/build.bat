@echo off
rem Builds victory_sl_bridge.dll from sl_bridge.cpp with the Streamline SDK headers.
rem   build.bat <streamline sdk folder> <output folder>
setlocal
set SDK=%~1
set OUT=%~2
if "%SDK%"=="" set SDK=G:\victory\nvidia-rtx\streamline-sdk-2.14.16\sdk
if "%OUT%"=="" set OUT=%~dp0out
if not exist "%OUT%" mkdir "%OUT%"
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cl /nologo /std:c++17 /O2 /EHsc /MT /W3 /LD /I "%SDK%\include" "%~dp0sl_bridge.cpp" /Fo"%OUT%\\" /Fe"%OUT%\victory_sl_bridge.dll" /link /NOLOGO user32.lib || exit /b 1
echo built %OUT%\victory_sl_bridge.dll
