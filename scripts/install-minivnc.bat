@echo off
rem Builds MiniVNC from this repository and installs it on Windows with
rem `cargo install` (default: %USERPROFILE%\.cargo\bin, which should be on
rem the PATH): minivnc-viewer and minivnc-passwd. The server and
rem minivnc-session run on Linux; see scripts\install-minivnc.sh.
rem
rem usage: scripts\install-minivnc.bat [--tools] [--root DIR] [--no-turbojpeg]
rem                                    [--target-dir DIR]
rem
rem   --tools           also install rfb-snapshot and rfb-bench
rem   --root DIR        install into DIR\bin instead (cargo install --root)
rem   --no-turbojpeg    decode JPEG with the pure Rust codec instead of
rem                     libjpeg-turbo (which needs CMake and NASM)
rem   --target-dir DIR  build into DIR (default: %CARGO_TARGET_DIR%, or
rem                     target in the repository, so
rem                     scripts\build-minivnc.bat output is reused)
rem
rem The vendored OpenSSL build needs Strawberry Perl (Git's perl does not
rem work); C:\Strawberry\perl\bin is put first on the PATH if it exists.
rem It also fails on long build paths ("files missing"); then build into a
rem short one such as --target-dir C:\t.

setlocal EnableDelayedExpansion

rem (`shift` moves %0 too: the script's own paths come first.)
set "self=%~f0"
for %%d in ("%~dp0..") do set "repo=%%~fd"
set "tools=0"
set "turbojpeg=1"
set "root_args="
set "target_args="
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
if /i "%~1"=="--no-turbojpeg" (
    set "turbojpeg=0"
    shift
    goto args
)
if /i "%~1"=="--target-dir" (
    if "%~2"=="" (
        set "msg=--target-dir needs a value"
        set "code=2"
        goto fail
    )
    set "target_args=--target-dir "%~f2""
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
if "%turbojpeg%"=="1" (
    for %%c in (cmake nasm) do (
        where %%c >nul 2>&1 || (
            set "msg=%%c not found (needed for libjpeg-turbo; or use --no-turbojpeg)"
            goto fail
        )
    )
)

set "packages=bins\minivnc-viewer bins\minivnc-passwd"
if "%tools%"=="1" set "packages=%packages% tools\rfb-snapshot tools\rfb-bench"
set "installed="
for %%p in (%packages%) do (
    rem Only packages with rfb-codec as a direct dependency take the feature.
    set "features="
    if "%turbojpeg%"=="1" (
        if /i "%%~nxp"=="minivnc-viewer" set "features=--features rfb-codec/turbojpeg"
        if /i "%%~nxp"=="rfb-snapshot" set "features=--features rfb-codec/turbojpeg"
    )
    echo == installing %%~nxp
    pushd "%repo%"
    cargo install --locked !features! %target_args% %root_args% --path "%repo%\%%p" || (
        popd
        set "msg=installing %%~nxp failed"
        goto fail
    )
    popd
    set "installed=!installed! %%~nxp"
)
echo installed:%installed%
exit /b 0

:fail
echo install-minivnc.bat: !msg! 1>&2
exit /b %code%
