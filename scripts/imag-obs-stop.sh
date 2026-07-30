#!/usr/bin/env bash
# imag-obs-stop.sh — #785: GRACEFUL OBS stop for the imag box.
#
# WHY: a bare `pkill obs` loses the operator's live cut — on the next start the seed parked
# program on "Cam 1" no matter what was on the wall at shutdown (user complaint 2026-07-18:
# "vzdy ked ho spustis nanovo je strihnute nieco ine nez tam bolo pri vypnuti"). This script
# saves the CURRENT program scene to ~/.config/imag-last-program BEFORE terminating OBS;
# imag_scenes.py --bootstrap restores it on the next start.
#
# #840: SIGTERM never runs OBS's own clean-shutdown save path (live-verified: with both
# projectors open, a SIGTERM-only stop followed by re-reading the scene collection JSON showed
# `saved_projectors: []` every time) — OBS only persists `saved_projectors` and
# `DockState`/`geometry` (in global.ini) on a real Qt application exit. This script now attempts
# a GRACEFUL window close first (`wmctrl -c`, the same EWMH `_NET_CLOSE_WINDOW` request a window
# manager's own close button sends, which OBS handles as `Controls -> Exit`), and only falls back
# to the pre-existing SIGTERM/SIGKILL ladder if OBS does not exit on its own within budget.
#
# Use this for EVERY deliberate OBS stop (deploys, recovery, operator menu) — never bare pkill.
#
# #882: a PLAIN invocation (no --exec-stop) FIRST checks whether imag-obs.service (the new
# systemd supervision unit) is active, and if so delegates to `systemctl --user stop` instead of
# running the ladder below directly. This matters because systemd's Restart=on-failure only
# suppresses itself for a stop IT initiated — an external `pkill`/direct kill of the tracked obs
# process looks like an unexpected crash and gets auto-relaunched regardless of how "graceful" the
# kill was. That is EXACTLY the bug the stood-down imag-obs-watchdog.py had (issue 788: it fought
# the operator on every deliberate manual quit) — routing every deliberate stop through systemctl
# is what keeps the NEW supervision unit from reintroducing it. `--exec-stop` is the mode the
# unit's own ExecStop= line passes (systemd is ALREADY the one stopping it there) and skips this
# delegation to avoid recursing back into `systemctl stop` mid-stop.
set -euo pipefail

EXEC_STOP_MODE=0
if [ "${1:-}" = "--exec-stop" ]; then
    EXEC_STOP_MODE=1
fi

if [ "$EXEC_STOP_MODE" -eq 0 ] && systemctl --user is-active --quiet imag-obs.service 2>/dev/null; then
    echo "imag-obs.service je aktivny -- zastavujem cez systemd (systemctl stop), aby to Restart=on-failure nebral ako pad"
    systemctl --user stop imag-obs.service
    exit 0
fi

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
    # #840: try a GRACEFUL window close FIRST -- this is what makes OBS actually run its own
    # clean-shutdown save path (saved_projectors, DockState/geometry). "OBS Studio" is a stable
    # prefix of the MAIN window's title only (live-verified `wmctrl -l`:
    # "OBS Studio 32.1.2 - newlevel.media build ... - Profile: imag-60fps - Scenes: Untitled");
    # the two Projector windows are titled "Projector - Program"/"Projector - Multiview" and never
    # match this substring, so this can never accidentally close a projector instead.
    if command -v wmctrl >/dev/null 2>&1; then
        DISPLAY="${DISPLAY:-:0}" wmctrl -c "OBS Studio" 2>/dev/null || true
        for _ in $(seq 1 15); do
            pgrep -x obs >/dev/null || { echo "obs closed cleanly (wmctrl -c)"; exit 0; }
            sleep 1
        done
        echo "WARN: obs did not exit within 15s of the graceful window close -- escalating"
    else
        echo "WARN: wmctrl not installed -- cannot request a graceful window close, escalating straight to SIGTERM"
    fi

    pkill -TERM -x obs || true
    for _ in $(seq 1 20); do
        pgrep -x obs >/dev/null || { echo "obs stopped (SIGTERM)"; exit 0; }
        sleep 1
    done
    echo "WARN: obs ignored SIGTERM for 20s — force-killing"
    pkill -KILL -x obs || true
else
    echo "obs not running"
fi
