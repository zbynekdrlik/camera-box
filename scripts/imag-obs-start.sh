#!/usr/bin/env bash
# imag-obs-start.sh -- operator's OBS start for the imag box (#788).
#
# WHY: the imag-obs-watchdog used to be the only thing that (re)started OBS after a manual quit,
# and it fought the operator (auto-relaunch loops + false "crashed" alarms, 2026-07-16) so it is
# stopped. This script is the OPERATOR path: openbox root menu "Spustit OBS" runs it.
#
# #840: boot now runs THROUGH this SAME script too (IMAG_ISOLATED_CPUS exported by the openbox
# autostart, see setup-imag.sh step 16) -- it used to launch obs + seed independently inline,
# with its own fragile 30s WebSocket wait (no obs-process-liveness check, failures silently
# swallowed by `|| true`), which is exactly what let the boot path silently drop the projector
# self-heal while this script's more robust 90s/liveness-checked wait kept working fine manually.
# One launch mechanism now, not two.
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
# #840/#841: the CPU pin is env-overridable so the boot-time openbox autostart -- which DOES know
# this box's own DERIVED isolated-CPU set (#816) -- can pass it through via IMAG_ISOLATED_CPUS.
# A bare manual invocation (no env set, e.g. the operator's "Spustit OBS" menu entry) falls back
# to the SAME derived value setup-imag.sh persisted to /etc/imag-isolated-cpus.conf when it
# computed the isolation plan for the kernel cmdline -- ONE source of truth. #841 REMOVED the
# previous hardcoded fallback range (the INCUMBENT 16-thread box's hand-tuned pin): it was
# silently wrong on the 12-thread replacement notebook (10.77.9.187), where it overlapped the
# kernel's own irqaffinity IRQ cores and defeated the isolation. Never guess a pin -- fail loud
# when neither source has a value.
ISOLATED_CPUS="${IMAG_ISOLATED_CPUS:-}"
if [ -z "$ISOLATED_CPUS" ] && [ -r /etc/imag-isolated-cpus.conf ]; then
    ISOLATED_CPUS="$(cat /etc/imag-isolated-cpus.conf)"
fi
if [ -z "$ISOLATED_CPUS" ]; then
    echo "FAIL: no derived isolated-CPU set available (IMAG_ISOLATED_CPUS unset and /etc/imag-isolated-cpus.conf missing/empty) -- refuse to guess a taskset pin"
    exit 1
fi
taskset -c "$ISOLATED_CPUS" obs --disable-shutdown-check &
echo "obs launched (pid $!, taskset -c $ISOLATED_CPUS)"

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
