#!/bin/bash
# Start a virtual X server, expose it over VNC, bridge that to the browser
# with noVNC, then launch LinuxCNC with the PC Turn 50 simulator config.
set -euo pipefail

CONFIG="${LINUXCNC_CONFIG:-/repo/linuxcnc/sim/emco-pcturn50/emco-pcturn50.ini}"

if [[ ! -f "$CONFIG" ]]; then
    echo "No config at $CONFIG — is the repository bind-mounted at /repo?" >&2
    exit 1
fi

# A restarted container keeps its filesystem, so LinuxCNC finds its old lock
# file and stops to ask "LinuxCNC is still running. Restart it? [Y/n]" on a
# terminal nobody is watching. Nothing from the previous run survives a
# container restart, so clearing these is always safe here.
rm -f /tmp/linuxcnc.lock /tmp/linuxcnc.print "$HOME/.rtapi_fifo" 2>/dev/null || true

cleanup() { kill $(jobs -p) 2>/dev/null || true; }
trap cleanup EXIT INT TERM

Xvfb :1 -screen 0 1280x900x24 -nolisten tcp &
for _ in $(seq 1 50); do xdpyinfo -display :1 >/dev/null 2>&1 && break; sleep 0.2; done

openbox &
x11vnc -display :1 -forever -shared -nopw -quiet -rfbport 5900 &
websockify --web=/usr/share/novnc 8080 localhost:5900 &

echo
echo "  LinuxCNC simulator: http://localhost:8080/vnc.html?autoconnect=1&resize=scale"
echo
echo "  This is a SIMULATOR. It does not control the real lathe."
echo "  Home both axes before running anything — the real machine loses its"
echo "  reference at every power-off, so build the habit here."
echo

exec linuxcnc "$CONFIG"
