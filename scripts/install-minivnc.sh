#!/usr/bin/env bash
# Builds MiniVNC from this repository and installs it with `cargo install`
# (default: ~/.cargo/bin, which should be on the PATH).
#
# usage: scripts/install-minivnc.sh [--tools] [--root DIR] [--no-turbojpeg]
#                                   [--target-dir DIR]
#
#   --tools           also install rfb-snapshot and rfb-bench
#   --root DIR        install into DIR/bin instead (cargo install --root)
#   --no-turbojpeg    Windows: decode JPEG with the pure Rust codec instead
#                     of libjpeg-turbo (which needs CMake and NASM); Linux
#                     always uses libjpeg-turbo
#   --target-dir DIR  build into DIR (default: $CARGO_TARGET_DIR, or target
#                     in the repository, so scripts/build-minivnc.sh output
#                     is reused)
#
# Linux installs minivnc-server, minivnc-session, minivnc-passwd and
# minivnc-viewer; Windows (Git Bash) installs minivnc-viewer and
# minivnc-passwd. minivnc-session looks for minivnc-server and
# minivnc-passwd next to itself, so they are installed together. On Windows
# the vendored OpenSSL build fails on long build paths ("files missing");
# then build into a short one such as --target-dir C:/t.
#
# Running sessions keep the old binaries until restarted:
#   minivnc-session -kill :N && minivnc-session ...

set -u

tools=0
turbojpeg=1
cargo_args=(--locked)
while [ $# -gt 0 ]; do
    case $1 in
        --tools) tools=1 ;;
        --root) [ $# -ge 2 ] || { echo "install-minivnc.sh: --root needs a value" >&2; exit 2; }
                cargo_args+=(--root "$2"); shift ;;
        --no-turbojpeg) turbojpeg=0 ;;
        --target-dir) [ $# -ge 2 ] || { echo "install-minivnc.sh: --target-dir needs a value" >&2; exit 2; }
                      mkdir -p "$2" && cargo_args+=(--target-dir "$(cd "$2" && pwd)") ||
                          { echo "install-minivnc.sh: cannot create $2" >&2; exit 1; }
                      shift ;;
        *) sed -n '2,25s/^# \{0,1\}//p' "$0"; exit 2 ;;
    esac
    shift
done

repo=$(cd "$(dirname "$0")/.." && pwd)
fail() { echo "install-minivnc.sh: $*" >&2; exit 1; }
command -v cargo >/dev/null || fail "cargo not found (install Rust from https://rustup.rs)"

case $(uname -s) in
    Linux)
        packages=(minivnc-server minivnc-session minivnc-passwd minivnc-viewer)
        # Build requirements (README "빌드"): libjpeg-turbo is built from
        # source, OpenSSL and PAM come from the system.
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
        turbojpeg=0  # always libjpeg-turbo
        for c in Xvfb xauth; do
            command -v "$c" >/dev/null ||
                echo "install-minivnc.sh: warning: $c not found; minivnc-session needs it (sudo apt install xvfb xauth)" >&2
        done
        ;;
    MINGW* | MSYS* | CYGWIN*)
        packages=(minivnc-viewer minivnc-passwd)
        if [ $turbojpeg = 1 ]; then
            for c in cmake nasm; do
                command -v "$c" >/dev/null ||
                    fail "$c not found (needed for libjpeg-turbo; or use --no-turbojpeg)"
            done
        fi
        ;;
    *) fail "unsupported system $(uname -s)" ;;
esac
[ $tools = 1 ] && packages+=(rfb-snapshot rfb-bench)

for p in "${packages[@]}"; do
    case $p in
        rfb-*) path=$repo/tools/$p ;;
        *) path=$repo/bins/$p ;;
    esac
    # Only packages with rfb-codec as a direct dependency take the feature.
    features=()
    case $p in
        minivnc-viewer | rfb-snapshot) [ $turbojpeg = 1 ] && features=(--features rfb-codec/turbojpeg) ;;
    esac
    echo "== installing $p"
    cargo install "${cargo_args[@]}" "${features[@]}" --path "$path" || fail "installing $p failed"
done

echo "installed: ${packages[*]}"
if command -v minivnc-session >/dev/null && minivnc-session -list 2>/dev/null | grep -q '^:'; then
    echo "running sessions keep the old binaries until restarted (minivnc-session -list)"
fi
