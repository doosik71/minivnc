#!/usr/bin/env bash
# Builds MiniVNC from this repository with `cargo build` after checking the
# build requirements, and prints where the binaries are.
#
# usage: scripts/build-minivnc.sh [--debug] [--tools] [--no-turbojpeg]
#                                 [--target-dir DIR]
#
#   --debug           build the dev profile instead of release
#   --tools           also build rfb-snapshot and rfb-bench
#   --no-turbojpeg    Windows: decode JPEG with the pure Rust codec instead
#                     of libjpeg-turbo (which needs CMake and NASM); Linux
#                     always uses libjpeg-turbo
#   --target-dir DIR  build into DIR (default: $CARGO_TARGET_DIR, or target
#                     in the repository)
#
# Linux builds minivnc-server, minivnc-session, minivnc-passwd and
# minivnc-viewer; Windows (Git Bash) builds minivnc-viewer and
# minivnc-passwd. On Windows the vendored OpenSSL build fails on long build
# paths ("files missing"); then build into a short one such as
# --target-dir C:/t. Windows command prompt: scripts\build-minivnc.bat.

set -u

profile=release
tools=0
turbojpeg=1
target=
while [ $# -gt 0 ]; do
    case $1 in
        --debug) profile=debug ;;
        --tools) tools=1 ;;
        --no-turbojpeg) turbojpeg=0 ;;
        --target-dir) [ $# -ge 2 ] || { echo "build-minivnc.sh: --target-dir needs a value" >&2; exit 2; }
                      target=$2; shift ;;
        *) sed -n '2,20s/^# \{0,1\}//p' "$0"; exit 2 ;;
    esac
    shift
done

repo=$(cd "$(dirname "$0")/.." && pwd)
fail() { echo "build-minivnc.sh: $*" >&2; exit 1; }
command -v cargo >/dev/null || fail "cargo not found (install Rust from https://rustup.rs)"

cargo_args=(--locked)
[ $profile = release ] && cargo_args+=(--release)
case $(uname -s) in
    Linux)
        packages=(minivnc-server minivnc-session minivnc-passwd minivnc-viewer)
        # Build requirements (README "빌드"), as in install-minivnc.sh.
        missing=()
        for c in cc cmake nasm pkg-config; do
            command -v "$c" >/dev/null || missing+=("$c")
        done
        pkg-config --exists openssl 2>/dev/null || missing+=("OpenSSL headers")
        [ -e /usr/include/security/pam_appl.h ] || missing+=("PAM headers")
        if [ ${#missing[@]} -gt 0 ]; then
            fail "missing ${missing[*]}
  Debian/Ubuntu: sudo apt install build-essential cmake nasm pkg-config libssl-dev libpam0g-dev"
        fi
        ;;
    MINGW* | MSYS* | CYGWIN*)
        packages=(minivnc-viewer minivnc-passwd)
        # The vendored OpenSSL build needs Strawberry Perl, not Git's perl.
        [ -x /c/Strawberry/perl/bin/perl.exe ] && PATH=/c/Strawberry/perl/bin:$PATH
        if [ -z "${OPENSSL_SRC_PERL:-}" ]; then
            perl -e 'exit($^X =~ /strawberry/i ? 0 : 1)' 2>/dev/null ||
                fail "Strawberry Perl not found (https://strawberryperl.com, or set OPENSSL_SRC_PERL)"
        fi
        if [ $turbojpeg = 1 ]; then
            for c in cmake nasm; do
                command -v "$c" >/dev/null ||
                    fail "$c not found (needed for libjpeg-turbo; or use --no-turbojpeg)"
            done
            cargo_args+=(--features rfb-codec/turbojpeg)
        fi
        ;;
    *) fail "unsupported system $(uname -s)" ;;
esac
[ $tools = 1 ] && packages+=(rfb-snapshot rfb-bench)
for p in "${packages[@]}"; do cargo_args+=(-p "$p"); done
if [ -n "$target" ]; then
    # Relative to where the script was run, not the workspace.
    mkdir -p "$target" && target=$(cd "$target" && pwd) || fail "cannot create $target"
    cargo_args+=(--target-dir "$target")
else
    target=${CARGO_TARGET_DIR:-target}
fi

echo "== building ${packages[*]} ($profile)"
(cd "$repo" && cargo build "${cargo_args[@]}") || fail "build failed"

# A relative CARGO_TARGET_DIR is relative to the workspace.
case $target in
    /* | [A-Za-z]:*) ;;
    *) target=$repo/$target ;;
esac
echo "built in $target/$profile: ${packages[*]}"
