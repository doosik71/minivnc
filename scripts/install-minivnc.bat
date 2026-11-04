@echo off
rem Builds MiniVNC from this repository and installs it on Windows with
rem `cargo install` (default: %USERPROFILE%\.cargo\bin, which should be on
rem the PATH): minivnc-viewer and minivnc-passwd. The server and
rem minivnc-session run on Linux; see scripts\install-minivnc.sh.
rem
rem usage: scripts\install-minivnc.bat [--tools] [--root DIR]
rem
rem   --tools     also install rfb-snapshot and rfb-bench
rem   --root DIR  install into DIR\bin instead (cargo install --root)
rem
rem The vendored OpenSSL build needs Strawberry Perl (Git's perl does not
rem work); C:\Strawberry\perl\bin is put first on the PATH if it exists.
rem It also fails on long build paths, so the build goes to
rem %CARGO_TARGET_DIR%, or C:\t if that is not set.

setlocal EnableDelayedExpansion

rem (`shift` moves %0 too: the script's own paths come first.)
set "self=%~f0"
for %%d in ("%~dp0..") do set "repo=%%~fd"
set "tools=0"
set "root_args="
:args
if "%~1"=="" goto args_done
if /i "%~1"=="--tools" (
    set "tools=1"
    shift
    goto args
)
if /i "%~1"=="--root" (
    if "%~2"=="" (
        set "msg=--root needs a value"
        set "code=2"
        goto fail
    )
    set "root_args=--root "%~2""
    shift
    shift
    goto args
)
for /f "usebackq tokens=* delims=" %%l in (`findstr /b "rem" "%self%"`) do (
    set "line=%%l"
    set "line=!line:~4!"
    if "!line:~0,4!"=="The " goto usage_done
    echo(!line!
)
:usage_done
exit /b 2
:args_done

rem Failures go through :fail; `exit /b` inside a block that goes on
rem after it loses its code under `cmd /c`.
set "code=1"
where cargo >nul 2>&1 || (
    set "msg=cargo not found (install Rust from https://rustup.rs)"
    goto fail
)
if exist "C:\Strawberry\perl\bin\perl.exe" set "PATH=C:\Strawberry\perl\bin;%PATH%"
if not defined OPENSSL_SRC_PERL (
    perl -e "exit($^X =~ /strawberry/i ? 0 : 1)" >nul 2>&1 || (
        set "msg=Strawberry Perl not found (https://strawberryperl.com, or set OPENSSL_SRC_PERL)"
        goto fail
    )
)
set "target=%CARGO_TARGET_DIR%"
if not defined target set "target=C:\t"

set "packages=bins\minivnc-viewer bins\minivnc-passwd"
if "%tools%"=="1" set "packages=%packages% tools\rfb-snapshot tools\rfb-bench"
set "installed="
for %%p in (%packages%) do (
    echo == installing %%~nxp
    cargo install --locked --target-dir "%target%" %root_args% --path "%repo%\%%p" || (
        set "msg=installing %%~nxp failed"
        goto fail
    )
    set "installed=!installed! %%~nxp"
)
echo installed:%installed%
exit /b 0

:fail
echo install-minivnc.bat: !msg! 1>&2
exit /b %code%
