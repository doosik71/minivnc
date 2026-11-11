#!/usr/bin/env bash
# Measures one workload on TurboVNC's Xvnc or on MiniVNC (Xvfb + minivnc-server)
# with rfb-bench, as recorded in docs/bench.md.
#
# usage: scripts/bench-compare.sh turbovnc|minivnc WORKLOAD PRESET [SECS] [-- SERVER OPTIONS...]
#
#   WORKLOAD   mandel | testsrc   mpv playing $VIDEO_DIR/WORKLOAD.mp4 at 1280x720
#              term               x11perf -ftext (core font text, over and over)
#   PRESET     rfb-bench preset: perceptual, medium, low, lossless, lossless-zlib, raw
#   SECS       measuring time (default 15)
#
# Both servers run on display :21 (port 5921), 1920x1080x24, -deferupdate 1,
# with their profiling on (TVNC_PROFILE=1 / -profile). Prints the rfb-bench
# total, the CPU the server used (with the X server for MiniVNC) and the
# server's last profile lines.
#
# The test videos (20 s, deterministic):
#   ffmpeg -f lavfi -i mandelbrot=size=1280x720:rate=30 -t 20 -pix_fmt yuv420p -c:v libx264 -crf 18 mandel.mp4
#   ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 20 -pix_fmt yuv420p -c:v libx264 -crf 18 testsrc.mp4
#
# Environment:
#   XVNC            TurboVNC Xvnc (default: /opt/TurboVNC/bin/Xvnc)
#   MINIVNC_SERVER  minivnc-server (default: this repository's target/release)
#   RFB_BENCH       rfb-bench (default: this repository's target/release)
#   VIDEO_DIR       where the test videos are (default: ~/bench)
#   LOG_DIR         server logs (default: ~/.cache/minivnc)

set -u
[ $# -ge 3 ] || { sed -n '2,13s/^# \{0,1\}//p' "$0"; exit 2; }
server=$1 workload=$2 preset=$3
secs=${4:-15}
shift $(($# < 4 ? $# : 4))
[ "${1:-}" = "--" ] && shift

script_dir=$(cd "$(dirname "$0")" && pwd)
xvnc=${XVNC:-/opt/TurboVNC/bin/Xvnc}
minivnc=${MINIVNC_SERVER:-$script_dir/../target/release/minivnc-server}
bench=${RFB_BENCH:-$script_dir/../target/release/rfb-bench}
videos=${VIDEO_DIR:-$HOME/bench}
log_dir=${LOG_DIR:-$HOME/.cache/minivnc}
mkdir -p "$log_dir"
log=$log_dir/bench-server.log

D=21
export DISPLAY=:$D
unset WAYLAND_DISPLAY
srv= xvfb= wl=
cleanup() { kill $wl $srv $xvfb 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

case $server in
turbovnc)
    TVNC_PROFILE=1 "$xvnc" :$D -geometry 1920x1080 -depth 24 \
        -securitytypes none -deferupdate 1 -nolisten tcp "$@" >"$log" 2>&1 &
    srv=$!
    cpupids=$srv ;;
minivnc)
    Xvfb :$D -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
    xvfb=$!
    sleep 1
    "$minivnc" :$D -display :$D -securitytypes none -deferupdate 1 -profile "$@" >"$log" 2>&1 &
    srv=$!
    cpupids="$srv $xvfb" ;;
*) echo "unknown server $server" >&2; exit 2 ;;
esac
sleep 1.5
case $workload in
mandel | testsrc)
    mpv --really-quiet --no-audio --vo=x11 --loop=inf --geometry=1280x720+0+0 \
        "$videos/$workload.mp4" >/dev/null 2>&1 &
    wl=$! ;;
term)
    x11perf -repeat 100000 -time 1 -ftext >/dev/null 2>&1 &
    wl=$! ;;
*) echo "unknown workload $workload" >&2; exit 2 ;;
esac
sleep 3

ticks() {
    local s=0 f
    for p in $cpupids; do
        read -ra f </proc/$p/stat
        s=$((s + f[13] + f[14]))
    done
    echo $s
}
t0=$(ticks)
result=$("$bench" -preset "$preset" -time "$secs" -interval 1000 localhost:$D | grep total)
t1=$(ticks)
cpu=$(awk -v a=$t0 -v b=$t1 -v hz=$(getconf CLK_TCK) -v s=$secs 'BEGIN { printf "%.0f", (b - a) / hz / s * 100 }')
echo "$server $workload $preset: ${result#total: }, server CPU ${cpu}%"
grep -aE "updates/sec|ime/update|capture:" "$log" | sed 's/\x1b\[[0-9;]*m//g' | tail -3 | sed 's/^/    /'
