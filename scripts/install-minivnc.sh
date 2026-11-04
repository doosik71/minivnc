#!/usr/bin/env bash
# Builds MiniVNC from this repository and installs it with `cargo install`
# (default: ~/.cargo/bin, which should be on the PATH).
#
# usage: scripts/install-minivnc.sh [--tools] [--root DIR]
#
#   --tools     also install rfb-snapshot and rfb-bench
#   --root DIR  install into DIR/bin instead (cargo install --root)
#
# Linux installs minivnc-server, minivnc-session, minivnc-passwd and
# minivnc-viewer; Windows (Git Bash) installs minivnc-viewer and
# minivnc-passwd. minivnc-session looks for minivnc-server and
# minivnc-passwd next to itself, so they are installed together.
#
# Running sessions keep the old binaries until restarted:
#   minivnc-session -kill :N && minivnc-session ...

set -u

tools=0
cargo_args=(--locked)
while [ $# -gt 0 ]; do
    case $1 in
        --tools) tools=1 ;;
        --root) [ $# -ge 2 ] || { echo "install-minivnc.sh: --root needs a value" >&2; exit 2; }
                cargo_args+=(--root "$2"); shift ;;
        *) sed -n '2,16s/^# \{0,1\}//p' "$0"; exit 2 ;;
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
        for c in Xvfb xauth; do
            command -v "$c" >/dev/null ||
                echo "install-minivnc.sh: warning: $c not found; minivnc-session needs it (sudo apt install xvfb xauth)" >&2
        done
        ;;
    MINGW* | MSYS* | CYGWIN*)
        packages=(minivnc-viewer minivnc-passwd)
        # cargo install builds in a temporary directory, whose long path
        # breaks the vendored OpenSSL build ("files missing").
        cargo_args+=(--target-dir "${CARGO_TARGET_DIR:-C:/t}")
        ;;
    *) fail "unsupported system $(uname -s)" ;;
esac
[ $tools = 1 ] && packages+=(rfb-snapshot rfb-bench)

for p in "${packages[@]}"; do
    case $p in
        rfb-*) path=$repo/tools/$p ;;
        *) path=$repo/bins/$p ;;
    esac
    echo "== installing $p"
    cargo install "${cargo_args[@]}" --path "$path" || fail "installing $p failed"
done

echo "installed: ${packages[*]}"
if command -v minivnc-session >/dev/null && minivnc-session -list 2>/dev/null | grep -q '^:'; then
    echo "running sessions keep the old binaries until restarted (minivnc-session -list)"
fi
