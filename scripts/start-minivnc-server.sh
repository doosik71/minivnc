#!/usr/bin/env bash
# Runs a MiniVNC desktop session on Linux in the foreground: Xvfb, the
# desktop (~/.vnc/xstartup.minivnc, an xfce4 desktop unless changed) and
# minivnc-server. A wrapper around `minivnc-session -fg`, which does the
# work; logging off, Ctrl+C or the server stopping ends the session.
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
# As TurboVNC's vncserver, ~/.vnc/x509_cert.pem and ~/.vnc/x509_private.pem,
# if they exist, enable the X509 security types. A self-signed pair:
#   openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=$(hostname)" \
#       -keyout ~/.vnc/x509_private.pem -out ~/.vnc/x509_cert.pem
#
# Environment:
#   MINIVNC_SESSION  minivnc-session binary (default: on PATH, then this
#                    repository's target/release)
#   MINIVNC_SERVER   minivnc-server binary (default: next to minivnc-session,
#                    then on PATH)
#   DESKTOP          desktop command of the default xstartup (startxfce4)
#
# The log is ~/.vnc/HOST:N.log; see `minivnc-session -help`.

set -u

display=1
args=()
while getopts "n:g:p:h" opt; do
    case $opt in
        n) display=${OPTARG#:} ;;
        g) args+=(-geometry "$OPTARG") ;;
        p) args+=(-rfbauth "$OPTARG") ;;
        *) sed -n '2,28s/^# \{0,1\}//p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))

script_dir=$(cd "$(dirname "$0")" && pwd)
session=${MINIVNC_SESSION:-$(command -v minivnc-session || echo "$script_dir/../target/release/minivnc-session")}
[ -x "$session" ] || { echo "start-minivnc-server.sh: minivnc-session not found at $session (set MINIVNC_SESSION)" >&2; exit 1; }

exec "$session" ":$display" -fg "${args[@]}" -- "$@"
