#!/usr/bin/env bash
# imag-wallpaper-refresh.sh — keep the wall-fallback desktop background CURRENT (#791/#800 family).
#
# WHY: the fallback background (shown by the LED wall whenever OBS is down) is a still of the
# 'resolume imag' scene. A one-shot screenshot goes STALE — live incident 2026-07-18: an evening
# OBS crash exposed a fallback still carrying the PREVIOUS band's logos. This script re-grabs the
# still every run while OBS is healthy, so the fallback is never older than the timer cadence.
#
# Install (systemd user timer, every 5 min):
#   systemctl --user enable --now imag-wallpaper-refresh.timer
set -euo pipefail

OUT="$HOME/Pictures/wall-fallback.png"
TMP="$OUT.tmp"

# OBS down -> keep the last good image (that is the whole point of the fallback).
pgrep -x obs >/dev/null || { echo "obs not running — keeping existing fallback"; exit 0; }

python3 - "$TMP" <<'PY'
import base64
import json
import sys

from websocket import create_connection

ws = create_connection("ws://127.0.0.1:4455", timeout=10)
json.loads(ws.recv())
ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1}}))
json.loads(ws.recv())
ws.send(json.dumps({"op": 6, "d": {"requestType": "GetSourceScreenshot", "requestId": "x",
                                   "requestData": {"sourceName": "resolume imag",
                                                   "imageFormat": "png",
                                                   "imageWidth": 1920, "imageHeight": 1080}}}))
while True:
    m = json.loads(ws.recv())
    if m.get("op") == 7 and m["d"]["requestId"] == "x":
        st = m["d"]["requestStatus"]
        if not st["result"]:
            sys.exit(f"GetSourceScreenshot failed: {st.get('code')} {st.get('comment', '')}")
        data = m["d"]["responseData"]["imageData"].split(",", 1)[1]
        with open(sys.argv[1], "wb") as fh:
            fh.write(base64.b64decode(data))
        break
PY

# Atomic replace + re-apply, so a crash mid-write can never leave a corrupt fallback.
mv -f "$TMP" "$OUT"
export DISPLAY="${DISPLAY:-:0}" XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}"
feh --no-fehbg --bg-fill "$OUT"
echo "fallback refreshed: $(date -Is)"
