#!/usr/bin/env bash
# Builds the Linux release archive minivnc-VERSION-linux-ARCH.tar.gz with
# minivnc-server, minivnc-session, minivnc-passwd, minivnc-viewer, LICENSE,
# NOTICE and README.md (VERSION from the workspace Cargo.toml).
#
# usage: scripts/create-distro.sh [--out DIR]
#
#   --out DIR   write the archive to DIR (default: target/tmp)
#
# The Windows archive is made by scripts\create-distro.bat.
#
# The binaries need the glibc of the build system or newer, so build on the
# oldest distribution to support; the required version is printed at the end.

set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
out=$repo/target/tmp
while [ $# -gt 0 ]; do
    case $1 in
        --out) [ $# -ge 2 ] || { echo "create-distro.sh: --out needs a value" >&2; exit 2; }
               out=$2; shift ;;
        *) sed -n '2,13s/^# \{0,1\}//p' "$0"; exit 2 ;;
    esac
    shift
done

fail() { echo "create-distro.sh: $*" >&2; exit 1; }
[ "$(uname -s)" = Linux ] || fail "run on Linux (Windows: scripts\\create-distro.bat)"
command -v cargo >/dev/null || fail "cargo not found (install Rust from https://rustup.rs)"
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

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)
[ -n "$version" ] || fail "no version in $repo/Cargo.toml"
name=minivnc-$version-linux-$(uname -m)
bins=(minivnc-server minivnc-session minivnc-passwd minivnc-viewer)
target=${CARGO_TARGET_DIR:-$repo/target}

pkg_args=()
for b in "${bins[@]}"; do pkg_args+=(-p "$b"); done
echo "== building ${bins[*]}"
(cd "$repo" && cargo build --release --locked "${pkg_args[@]}") || fail "build failed"

mkdir -p "$out" || fail "cannot create $out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d) || fail "mktemp failed"
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
for b in "${bins[@]}"; do
    install -m 755 "$target/release/$b" "$stage/$name/" || fail "copying $b failed"
done
for f in LICENSE NOTICE README.md; do
    install -m 644 "$repo/$f" "$stage/$name/" || fail "copying $f failed"
done
chmod 755 "$stage/$name"
tar -C "$stage" -czf "$out/$name.tar.gz" --owner=0 --group=0 "$name" || fail "tar failed"

echo "created $out/$name.tar.gz"
if command -v objdump >/dev/null; then
    glibc=$(cd "$stage/$name" && objdump -T "${bins[@]}" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1)
    echo "requires ${glibc/_/ } or newer"
fi
