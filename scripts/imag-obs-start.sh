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
# #882: log the deployed genlock build sha at every start -- so a future incident never again
# needs cross-referencing git history + a separately-read file just to know what's actually
# running (exactly the archaeology this ticket's own investigation had to do by hand).
echo "genlock build sha: $(cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null || echo unknown)"

if pgrep -x obs >/dev/null; then
    echo "OBS uz bezi -- nic nerobim."
    exit 0
fi

# #1156: preflight the seed's Python import chain BEFORE launching OBS. This script launches OBS and
# only AFTERWARD runs `python3 imag_scenes.py --bootstrap`; if a sibling module the seed imports is
# missing on the box (e.g. imag_record_encoder, when setup-imag.sh's install list drifted), the seed
# dies on ModuleNotFoundError AFTER OBS is already up -> set -e aborts this script -> Restart=on-failure
# relaunches -> a HEALTHY OBS flaps on the live projection 1700x (the incident this guards). Failing
# HERE, before any launch, fails the unit cleanly and never touches a running OBS. Loading imag_scenes
# from the REAL on-box install dir transitively validates imag_record_encoder + the websocket dep too.
if ! python3 -c "import sys; sys.path.insert(0, '/usr/local/bin'); import imag_scenes"; then
    echo "FAIL: imag_scenes import preflight failed -- a seed dependency is missing on the box (e.g. the imag_record_encoder sibling module, or python3-websocket). Refusing to launch OBS (a broken seed would Restart-loop it). Fix: re-run setup-imag.sh to reinstall /usr/local/bin/imag_*.py."
    exit 1
fi

# issue 1152 M4: DRM-lease tolerance. With ~/.camera-box/drm-output.json ENABLED the vendored OBS
# leases the HDMI connector OUT of the X layout at startup and page-flips the Program onto it
# directly (render->scanout, obs-drm-output.md) -- so this wrapper must NOT require the HDMI
# display in X. Two jobs here, both LOUD and both best-effort (never a new unit-abort path -- the
# issue-866 start-path discipline; the projector-step tolerance itself lives in imag_scenes.py's
# own lease-mode branch, which EVERY caller of the seed inherits):
#   1. take the config's connector out of the X layout BEFORE the launch (the idle-connector
#      lease precondition: never lease an output X is actively displaying), reboot-durable
#      without touching the openbox autostart;
#   2. announce the mode, so this log always names WHY no X Program projector opens.
# Config absent/disabled -> DRM_LEASE_MODE stays 0 and behaviour is byte-identical to before.
DRM_OUTPUT_CONF="$HOME/.camera-box/drm-output.json"
DRM_LEASE_MODE=0
if [ -r "$DRM_OUTPUT_CONF" ] && LC_ALL=C grep -aqE '"enabled"[[:space:]]*:[[:space:]]*true' "$DRM_OUTPUT_CONF"; then
    DRM_LEASE_MODE=1
    DRM_CONNECTOR="$(LC_ALL=C sed -nE 's/.*"connector"[[:space:]]*:[[:space:]]*"([^"]*)".*/\1/p' "$DRM_OUTPUT_CONF" | head -1 || true)"
    DRM_CONNECTOR="${DRM_CONNECTOR:-HDMI-1}"
    echo "issue 1152 drm-lease mode ENABLED (${DRM_OUTPUT_CONF}): Program goes out via the DRM-leased ${DRM_CONNECTOR} scanout -- taking ${DRM_CONNECTOR} out of the X layout; only the panel Multiview projector will open X-side"
    xrandr --output "$DRM_CONNECTOR" --off 2>/dev/null \
        || echo "WARN #1152: xrandr --output ${DRM_CONNECTOR} --off failed -- if ${DRM_CONNECTOR} is still active in X the in-OBS lease may fail; the drm_output drift facet will name it (continuing, never aborting the unit)"
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
OBS_PID=$!
echo "obs launched (pid $OBS_PID, taskset -c $ISOLATED_CPUS)"

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
if [ "$DRM_LEASE_MODE" = "1" ]; then
    echo "issue 1152 drm-lease mode: X-side seeding done -- the Program is on the DRM scanout, only the panel Multiview projector is X-side"
fi
echo "OK: OBS bezi, scenes seednute (--bootstrap), projektory otvorene."

# #882: BLOCK here until obs itself exits, then propagate ITS OWN exit status. This makes obs --
# not this wrapper script -- the process a systemd Type=simple unit tracks as its "main process"
# (imag-obs.service, ExecStart=this script): a segfault (killed by a signal) reports non-zero and
# Restart=on-failure relaunches within seconds; a clean exit(0) (the operator quitting OBS's own
# UI) reports 0 and is correctly left alone, never fought. Never reached on the idempotent
# "already running" early exit above -- that path never backgrounds anything of its own to wait on.
echo "supervising obs (pid $OBS_PID) -- exit propagated to systemd for Restart=on-failure (#882)"
wait "$OBS_PID"
OBS_EXIT=$?
echo "=== $(date '+%F %T') obs exited (code $OBS_EXIT) ==="
exit "$OBS_EXIT"
