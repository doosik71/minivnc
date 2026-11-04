@echo off
rem Builds MiniVNC from this repository on Windows with `cargo build`:
rem minivnc-viewer and minivnc-passwd. The server and minivnc-session run
rem on Linux; see scripts/build-minivnc.sh.
rem
rem usage: scripts\build-minivnc.bat [--debug] [--tools] [--no-turbojpeg]
rem                                  [--target-dir DIR]
rem
rem   --debug           build the dev profile instead of release
rem   --tools           also build rfb-snapshot and rfb-bench
rem   --no-turbojpeg    decode JPEG with the pure Rust codec instead of
rem                     libjpeg-turbo (which needs CMake and NASM)
rem   --target-dir DIR  build into DIR (default: %CARGO_TARGET_DIR%, or
rem                     target in the repository)
rem
rem The vendored OpenSSL build needs Strawberry Perl (Git's perl does not
rem work); C:\Strawberry\perl\bin is put first on the PATH if it exists.
rem It also fails on long build paths ("files missing"); then build into a
rem short one such as --target-dir C:\t.

setlocal EnableDelayedExpansion

rem (`shift` moves %0 too: the script's own paths come first.)
set "self=%~f0"
for %%d in ("%~dp0..") do set "repo=%%~fd"
set "profile=release"
set "tools=0"
set "turbojpeg=1"
set "target="
:args
if "%~1"=="" goto args_done
if /i "%~1"=="--debug" (
    set "profile=debug"
    shift
    goto args
)
if /i "%~1"=="--tools" (
    set "tools=1"
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
    set "target=%~f2"
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
set "cargo_args=--locked"
if "%profile%"=="release" set "cargo_args=%cargo_args% --release"
if "%turbojpeg%"=="1" (
    for %%c in (cmake nasm) do (
        where %%c >nul 2>&1 || (
            set "msg=%%c not found (needed for libjpeg-turbo; or use --no-turbojpeg)"
            goto fail
        )
    )
    set "cargo_args=!cargo_args! --features rfb-codec/turbojpeg"
)
if defined target (
    set "cargo_args=!cargo_args! --target-dir "!target!""
) else if defined CARGO_TARGET_DIR (
    set "target=%CARGO_TARGET_DIR%"
) else (
    set "target=%repo%\target"
)

set "packages=minivnc-viewer minivnc-passwd"
if "%tools%"=="1" set "packages=%packages% rfb-snapshot rfb-bench"
for %%p in (%packages%) do set "cargo_args=!cargo_args! -p %%p"

echo == building %packages% (%profile%)
pushd "%repo%"
cargo build %cargo_args% || (
    popd
    set "msg=build failed"
    goto fail
)
rem A relative CARGO_TARGET_DIR is relative to the workspace.
for %%t in ("%target%") do set "target=%%~ft"
popd
echo built in %target%\%profile%: %packages%
exit /b 0

:fail
echo build-minivnc.bat: !msg! 1>&2
exit /b %code%
