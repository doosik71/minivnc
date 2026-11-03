#!/usr/bin/env bash
# Runs a MiniVNC desktop session on Linux: Xvfb + an xfce4 desktop +
# minivnc-server. Like TurboVNC's vncserver, logging off ends the whole
# session; so do Ctrl+C, killing this script, or the server stopping.
#
# usage: scripts/start-minivnc-server.sh [-n N] [-g WxH] [-p PASSWD] [-- SERVER OPTIONS...]
#
#   -n N       X display :N and VNC display :N (port 5900+N)   default: 1
#   -g WxH     desktop size, also the largest size viewers can  default: 3840x2160
#              resize to (Xvfb cannot grow past its initial size;
#              viewers with -DesktopSize=Auto shrink it to fit)
#   -p FILE    VNC password file (minivnc-passwd)               default: ~/.vnc/passwd
#   -- ...     further minivnc-server options, e.g. -- -localhost -viewonly
#
# Environment:
#   MINIVNC_SERVER   minivnc-server binary (default: on PATH, then this
#                    repository's target/release)
#   DESKTOP          desktop command (default: startxfce4)
#   LOG_DIR          where the logs go (default: ~/.cache/minivnc)

set -u

display=1
geometry=3840x2160
passwd=$HOME/.vnc/passwd
while getopts "n:g:p:h" opt; do
    case $opt in
        n) display=${OPTARG#:} ;;
        g) geometry=$OPTARG ;;
        p) passwd=$OPTARG ;;
        *) sed -n '2,21s/^# \{0,1\}//p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))

script_dir=$(cd "$(dirname "$0")" && pwd)
server=${MINIVNC_SERVER:-$(command -v minivnc-server || echo "$script_dir/../target/release/minivnc-server")}
desktop=${DESKTOP:-startxfce4}
log_dir=${LOG_DIR:-$HOME/.cache/minivnc}
mkdir -p "$log_dir"

fail() { echo "start-minivnc-server.sh: $*" >&2; exit 1; }
[ -x "$server" ] || fail "minivnc-server not found at $server (set MINIVNC_SERVER)"
if [ ! -r "$passwd" ]; then
    # minivnc-passwd creates ~/.vnc only when given no file name.
    [ "$passwd" = "$HOME/.vnc/passwd" ] && hint="minivnc-passwd" \
        || hint="mkdir -p $(dirname "$passwd") && minivnc-passwd $passwd"
    fail "no password file $passwd (create one: $hint)"
fi
command -v Xvfb >/dev/null || fail "Xvfb is not installed"
[ -e "/tmp/.X$display-lock" ] && fail "display :$display is in use (/tmp/.X$display-lock)"

xvfb_pid= desktop_pid= server_pid=
cleanup() {
    trap - EXIT INT TERM
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null
    # Ending X also ends whatever is left of the desktop.
    [ -n "$desktop_pid" ] && kill "$desktop_pid" 2>/dev/null
    [ -n "$xvfb_pid" ] && kill "$xvfb_pid" 2>/dev/null
    wait 2>/dev/null
    echo "session :$display ended"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# On WSLg, /tmp/.X11-unix is read-only: Xvfb warns that it cannot create its
# socket file and listens on the abstract socket only, which works.
Xvfb ":$display" -screen 0 "${geometry}x24" -nolisten tcp >"$log_dir/xvfb-$display.log" 2>&1 &
xvfb_pid=$!
sleep 1
kill -0 "$xvfb_pid" 2>/dev/null || fail "Xvfb did not start; see $log_dir/xvfb-$display.log"

# The desktop; it ends when the user logs off. Keep GTK apps off WSLg's
# Wayland display.
env -u WAYLAND_DISPLAY GDK_BACKEND=x11 XDG_SESSION_TYPE=x11 DISPLAY=":$display" \
    dbus-launch --exit-with-session "$desktop" >>"$log_dir/desktop-$display.log" 2>&1 &
desktop_pid=$!

echo "session :$display: desktop up to $geometry, VNC port $((5900 + display)); logs in $log_dir"
# In the background, so a signal to this script runs the cleanup at once.
"$server" ":$display" -display ":$display" -rfbauth "$passwd" "$@" \
    > >(tee "$log_dir/server-$display.log") 2>&1 &
server_pid=$!

# Whichever ends first, the desktop (logging off) or the server, ends the
# session.
wait -n "$desktop_pid" "$server_pid"
if ! kill -0 "$desktop_pid" 2>/dev/null; then
    echo "start-minivnc-server.sh: the desktop ended (logged off)"
fi
