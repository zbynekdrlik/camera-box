#!/usr/bin/env bash
# imag-obs-start.sh -- operator's OBS start for the imag box (#788).
#
# WHY: the imag-obs-watchdog used to be the only thing that (re)started OBS after a manual quit,
# and it fought the operator (auto-relaunch loops + false "crashed" alarms, 2026-07-16) so it is
# stopped. This script is the OPERATOR path: openbox root menu "Spustit OBS" runs it; boot uses
# the openbox autostart (which launches obs + seed --bootstrap itself, independent of this file).
#
# Idempotent: OBS already running -> prints a note and exits 0 (never a second instance).
# Full start = the watchdog tier-a recovery semantics: clear crash sentinels -> launch obs ->
# wait for WebSocket -> seed --bootstrap (bindings/mutes/program/Studio enforced on the FRESH
# instance only, #785) -> projectors (wall + multiview).
set -euo pipefail

export DISPLAY="${DISPLAY:-:0}"
export XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}"
SCN=/usr/local/bin/imag_scenes.py
LOG=/tmp/imag-obs-start.log

exec >>"$LOG" 2>&1
echo "=== $(date '+%F %T') imag-obs-start ==="

if pgrep -x obs >/dev/null; then
    echo "OBS uz bezi -- nic nerobim."
    exit 0
fi

rm -rf "$HOME/.config/obs-studio/.sentinel"/* 2>/dev/null || true
taskset -c 2-11 obs --disable-shutdown-check &
echo "obs launched (pid $!)"

deadline=$((SECONDS + 90))
until (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; do
    if ! pgrep -x obs >/dev/null; then
        echo "FAIL: obs proces zmizol pocas nabehu"
        exit 1
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
        echo "FAIL: OBS WebSocket (port 4455) nenabehol do 90 s"
        exit 1
    fi
    sleep 3
done
exec 3<&- 3>&- 2>/dev/null || true
sleep 2   # WS port is up; give the ident handshake layer a moment before the seed connects

python3 "$SCN" --host 127.0.0.1 --bootstrap
python3 "$SCN" --host 127.0.0.1 --projector
echo "OK: OBS bezi, scenes seednute (--bootstrap), projektory otvorene."
