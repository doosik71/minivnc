@echo off
rem Builds the Windows release archive minivnc-VERSION-windows-ARCH.zip with
rem minivnc-viewer, minivnc-passwd, LICENSE, NOTICE and README.md (VERSION
rem from the workspace Cargo.toml). The Linux archive is made by
rem scripts/create-distro.sh.
rem
rem usage: scripts\create-distro.bat [--out DIR] [--no-turbojpeg]
rem                                   [--target-dir DIR]
rem
rem   --out DIR         write the archive to DIR (default: target\tmp)
rem   --no-turbojpeg    decode JPEG with the pure Rust codec instead of
rem                     libjpeg-turbo (which needs CMake and NASM)
rem   --target-dir DIR  build into DIR (default: %CARGO_TARGET_DIR%, or
rem                     target in the repository, as
rem                     scripts\build-minivnc.bat)
rem
rem The vendored OpenSSL build needs Strawberry Perl (Git's perl does not
rem work); C:\Strawberry\perl\bin is put first on the PATH if it exists.
rem It also fails on long build paths ("files missing"); then build into a
rem short one such as --target-dir C:\t.

setlocal EnableDelayedExpansion

rem (`shift` moves %0 too: the script's own paths come first.)
set "self=%~f0"
for %%d in ("%~dp0..") do set "repo=%%~fd"
set "out=%repo%\target\tmp"
set "turbojpeg=1"
set "target="
:args
if "%~1"=="" goto args_done
if /i "%~1"=="--out" (
    if "%~2"=="" (
        set "msg=--out needs a value"
        set "code=2"
        goto fail
    )
    set "out=%~f2"
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
set "cargo_args=--release --locked -p minivnc-viewer -p minivnc-passwd"
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
    rem A relative CARGO_TARGET_DIR is relative to the workspace.
    pushd "%repo%"
    for %%t in ("%CARGO_TARGET_DIR%") do set "target=%%~ft"
    popd
) else (
    set "target=%repo%\target"
)

rem The first `version = "..."` line is [workspace.package].
set "version="
for /f "tokens=2 delims==" %%v in ('findstr /b /c:"version = " "%repo%\Cargo.toml"') do (
    if not defined version set "version=%%v"
)
if not defined version (
    set "msg=no version in %repo%\Cargo.toml"
    goto fail
)
set "version=%version: =%"
set "version=%version:"=%"
set "arch=x64"
if /i "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "arch=arm64"
set "name=minivnc-%version%-windows-%arch%"
set "bins=minivnc-viewer minivnc-passwd"

echo == building %bins%
pushd "%repo%"
cargo build %cargo_args% || (
    popd
    set "msg=build failed"
    goto fail
)
popd

set "stage=%out%\%name%"
if not exist "%out%" mkdir "%out%" || (
    set "msg=cannot create %out%"
    goto fail
)
if exist "%stage%" rmdir /s /q "%stage%"
mkdir "%stage%"
for %%b in (%bins%) do (
    copy /y "%target%\release\%%b.exe" "%stage%\" >nul || (
        set "msg=copying %%b.exe failed"
        goto fail
    )
)
for %%f in (LICENSE NOTICE README.md) do (
    copy /y "%repo%\%%f" "%stage%\" >nul || (
        set "msg=copying %%f failed"
        goto fail
    )
)
if exist "%out%\%name%.zip" del /q "%out%\%name%.zip"
powershell -NoProfile -Command "Compress-Archive -Path '%stage%' -DestinationPath '%out%\%name%.zip'" || (
    set "msg=Compress-Archive failed"
    goto fail
)
rmdir /s /q "%stage%"
echo created %out%\%name%.zip
exit /b 0

:fail
echo create-distro.bat: !msg! 1>&2
exit /b %code%
