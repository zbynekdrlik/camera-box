#!/usr/bin/env bash
# imag-obs-stop.sh — #785: GRACEFUL OBS stop for the imag box.
#
# WHY: a bare `pkill obs` loses the operator's live cut — on the next start the seed parked
# program on "Cam 1" no matter what was on the wall at shutdown (user complaint 2026-07-18:
# "vzdy ked ho spustis nanovo je strihnute nieco ine nez tam bolo pri vypnuti"). This script
# saves the CURRENT program scene to ~/.config/imag-last-program BEFORE terminating OBS;
# imag_scenes.py --bootstrap restores it on the next start.
#
# Use this for EVERY deliberate OBS stop (deploys, recovery, operator menu) — never bare pkill.
set -euo pipefail

STATE="$HOME/.config/imag-last-program"

if pgrep -x obs >/dev/null; then
    PROG=$(python3 - <<'PY' 2>/dev/null || true
import json
from websocket import create_connection
ws = create_connection("ws://127.0.0.1:4455", timeout=5)
json.loads(ws.recv()); ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1}})); json.loads(ws.recv())
ws.send(json.dumps({"op": 6, "d": {"requestType": "GetCurrentProgramScene",
                                   "requestId": "x", "requestData": {}}}))
while True:
    m = json.loads(ws.recv())
    if m.get("op") == 7 and m["d"]["requestId"] == "x":
        print(m["d"]["responseData"]["currentProgramSceneName"])
        break
PY
)
    if [ -n "${PROG:-}" ]; then
        printf '%s' "$PROG" > "$STATE"
        echo "saved program scene: $PROG"
    else
        echo "WARN: could not read program scene (WS down?) — keeping previous state file"
    fi
    pkill -TERM -x obs || true
    for _ in $(seq 1 20); do
        pgrep -x obs >/dev/null || { echo "obs stopped"; exit 0; }
        sleep 1
    done
    echo "WARN: obs ignored SIGTERM for 20s — force-killing"
    pkill -KILL -x obs || true
else
    echo "obs not running"
fi
