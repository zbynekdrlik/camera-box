#!/usr/bin/env bash
# rig-mode.sh — the DETERMINISTIC rig TEST-mode / EVENT-mode switch (#247).
#
# WHY (#247, the #246 live-event disaster): switching the rig between TEST mode (QR/E2E measurement)
# and EVENT mode (clean prod broadcast) used to be AD-HOC — which QR, what size, capture settings,
# burns on/off, genlock config all depended on the operator's/agent's context. That left burns ON in
# the prod Machine env during a LIVE event (QR painted on the broadcast) and genlock in a test state.
# This script is the SINGLE SOURCE OF TRUTH: identical pinned settings every time, no improvisation.
#
# WHAT IT DOES — the CAM side is automated here (ssh to the cam boxes is ALLOWED); the Windows OBS
# burn is toggled DIRECTLY over OBS WebSocket (scripts/obs_burn_filter.py — the harness has WS access);
# the env-free genlock relaunch (no --mode) is PRINTED to run via the win-*
# MCP (ssh/scp to the Windows boxes is DENIED on this rig, same model as recording-verdict-on-stream.sh).
#
#   TEST  : cam2 — free /dev/fb0 WITHOUT killing capture+emit (#291: switch camera-box to a TRANSIENT
#                  no-display systemd drop-in instead of stopping it — display output is the ONLY thing
#                  that grabs fb0; /dev/video0 capture + NDI emit do not), so cam2 stays a MEASURABLE
#                  camera during the test. Then launch the PINNED dual-QR vernier painter
#                  (frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60 --duration-secs N
#                  — #290: 60fps to match the 60fps capture so 60 distinct ticks/s resolve) WITH the
#                  QPSK A/V-sync audio marker (--audio-marker --audio-marker-device hw:CARD=PCH,DEV=3
#                  — #420: TEST mode used to launch the painter WITHOUT this, so the A/V-sync
#                  measurement was silently unmeasured — no marker ever reached the recording), verify
#                  it is up + writing /dev/fb0 AND camera-box is still active + capturing/emitting AND
#                  the marker's ALSA PCM is actually RUNNING (#420: fail loud + kill the painter if
#                  silent). Then PRINT the OBS test step (burns ON, run_id strih 911002 / stream 911004).
#   EVENT : cam2 — stop the painter cleanly (via its PID file — NOT a naive `pkill -f frame-probe`,
#                  which would self-kill a shell whose cmdline contains "frame-probe"); the QPSK audio
#                  marker is a THREAD inside that same process (#420: no separate stop needed), so this
#                  also stops the marker. REMOVE the transient no-display drop-in TEST mode installed
#                  (#291), then reload + restart camera-box and verify the service is active +
#                  --display restored. Then PRINT the OBS event step (burns OFF: the #246 guard; the
#                  wrapper refuses to launch otherwise).
#
# The painter binary on cam2 comes from the CI probe-tools-linux-amd64 artifact:
#   gh run download <latest CI run> -n probe-tools-linux-amd64
#   scp frame-probe root@10.77.9.62:/usr/local/bin/frame-probe
# If it is absent, TEST mode FAILS LOUD telling the operator to deploy it.
#
# cam1 (the SOURCE camera) is NOT reconfigured here: it runs its DEPLOYED camera-box service, which
# already emits a 30 fps NDI ("CAM1 (usb)") at the certified v4l2 controls (the recording-e2e
# harness convention — the real camera is already at the test rate). See the e2e playbook skill.
#
# Idempotent (re-runnable), self-verifying (prints the achieved state + a clear PASS/FAIL), fail-loud
# (set -euo pipefail; any verify mismatch exits non-zero).
#
# Usage:
#   scripts/rig-mode.sh test       # switch the rig INTO test mode (paint QR, print OBS burns-ON step)
#   scripts/rig-mode.sh event      # switch the rig BACK to clean broadcast (stop QR, print OBS burns-OFF step)
#
# Env overrides (all pinned by default — override only for a non-default rig):
#   CAM_PW                 cam-box root password (default: the dev-rig LAN root pw, as in the sibling
#                          e2e scripts; override from your password store for a different rig)
#   PAINTER_IP             cam2 device IP (default 10.77.9.62 — the box with the physical monitor)
#   PAINTER_BIN            painter binary path on cam2 (default /usr/local/bin/frame-probe)
#   QR_SIZE                dual-QR module size px (default 700 — the validated vernier size)
#   PAINTER_FPS            painter frame rate (default 60 — MUST match the 60fps capture, #290; the
#                          painter must paint 60 distinct ticks/s so 60fps optical timing can resolve.
#                          Under the KMS presenter the painter is vblank-locked at the monitor refresh
#                          and this is a no-op; on the fbdev fallback it is what forces the right rate.)
#   PAINTER_DURATION_SECS  painter run length (default 7200 = 2 h; event mode stops it sooner via pidfile)
#   PAINTER_PIDFILE        painter PID file on cam2 (default /run/rig-painter.pid)
#   PAINTER_EXTRA_FLAGS    extra painter flags for a MEASUREMENT run (default empty), e.g.
#                          "--wall-clock --run-id 12345" — the pinned switch paints the vernier; a
#                          full E2E measurement adds these (see scripts/recording-e2e.sh).
#   AUDIO_MARKER_DEVICE    #420: ALSA device for the QPSK A/V-sync audio marker (default
#                          hw:CARD=PCH,DEV=3 — the cam2 BenQ HDMI out with the connected speaker,
#                          confirmed live; card0 is the intercom, held exclusively by camera-box).
#   AUDIO_MARKER_CADENCE_TICKS  emit the marker every N painter ticks (default 180 ≈ 3s @ 60Hz —
#                          the av-sync skill's proven recipe).
#   AUDIO_MARKER_LOG       path on cam2 for the emitted-marker CSV (default
#                          /run/rig-qpsk-markers.csv — pull it off cam2 for recording-verdict
#                          --av-sync).
#
# Exit codes: 0 = mode applied (cam side verified) + OBS step printed; non-zero = cam-side failure or
#             a usage error (exit 2).
set -euo pipefail

# #291/#309: SINGLE SOURCE OF TRUTH for the transient no-display drop-in — the path constant
# (RIG_TEST_DROPIN) AND the clear-on-restore builder (rig_test_dropin_clear_cmds), shared with the
# sibling e2e harnesses (recording-e2e.sh / loopback-e2e.sh) so the path can never desync across the
# three scripts. Sourced here (before the source-guard) so both the executed flow and the unit tests
# that source this script get the constant + builder.
RIG_MODE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/rig-test-dropin.sh
. "$RIG_MODE_DIR/lib/rig-test-dropin.sh"
# #281 Fix#3: the rig-active heartbeat. TEST mode SETS it (deliberate "rig is in a test state"
# marker); EVENT mode CLEARS it. Unlike recording-e2e.sh this is a one-shot write (rig-mode exits),
# so the marker goes STALE after RIG_HEARTBEAT_STALE_SEC (default 10 min) — an idle TEST rig with a
# clear stranded signal then becomes watchdog-actionable (the intended safety net: the rig must not
# sit indefinitely in a test state with prod unprotected). Run an active E2E to keep it fresh.
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$RIG_MODE_DIR/lib/rig-heartbeat.sh"

# --- pinned constants (overridable via env, but DEFAULTS are the single source of truth) -----------
CAM_PW="${CAM_PW:-newlevel}"                 # dev-rig LAN root pw (same as the sibling e2e scripts)
PAINTER_IP="${PAINTER_IP:-10.77.9.62}"       # cam2 — has /dev/fb0 + the monitor the broadcast cam films
CAM1_IP="${CAM1_IP:-10.77.9.61}"             # cam1 — the SOURCE camera (NOT reconfigured here; for the print)
PAINTER_BIN="${PAINTER_BIN:-/usr/local/bin/frame-probe}"
QR_SIZE="${QR_SIZE:-700}"
PAINTER_FPS="${PAINTER_FPS:-60}"             # painter rate — MUST match the 60fps capture (#290)
PAINTER_DURATION_SECS="${PAINTER_DURATION_SECS:-7200}"
PAINTER_PIDFILE="${PAINTER_PIDFILE:-/run/rig-painter.pid}"
PAINTER_EXTRA_FLAGS="${PAINTER_EXTRA_FLAGS:-}"
CAMERA_BOX_BIN="${CAMERA_BOX_BIN:-/usr/local/bin/camera-box}"   # the deployed camera-box binary on cam2
# #420: the QPSK A/V-sync audio marker — a THREAD inside the SAME frame-probe --paint-only process
# (src/probe/qpsk_emit.rs), never a separate daemon. TEST mode used to launch the painter WITHOUT
# these flags at all (live evidence 2026-07-02: no audio-marker process running on cam2), so the
# whole A/V-sync measurement (#188/#398) was silently unmeasured. Defaults match the proven
# av-sync skill recipe (.claude/skills/av-sync).
AUDIO_MARKER_DEVICE="${AUDIO_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}"        # cam2 BenQ HDMI (confirmed: has a connected speaker)
AUDIO_MARKER_CADENCE_TICKS="${AUDIO_MARKER_CADENCE_TICKS:-180}"        # ~3s @ 60Hz painter ticks
AUDIO_MARKER_LOG="${AUDIO_MARKER_LOG:-/run/rig-qpsk-markers.csv}"      # emitted-marker CSV on cam2
# RIG_TEST_DROPIN (the transient no-display drop-in TEST mode installs / EVENT mode removes) is now
# defined in scripts/lib/rig-test-dropin.sh, sourced above — the single source shared with the e2e
# harnesses (#309). install = painter_launch_remote; remove = painter_stop_remote (via the shared
# rig_test_dropin_clear_cmds builder).

# --- PURE functions (no network, no ssh — unit-tested by sourcing this script) --------------------

# painter_launch_remote BIN DUR QR PIDFILE [EXTRA] [FPS] [CBBIN] [DROPIN] [AUDIO_DEV] [AUDIO_CADENCE]
#   [MARKER_LOG] -> the REMOTE bash run on cam2 (over ssh) to enter TEST mode: stop any prior
# painter, free /dev/fb0 WITHOUT killing capture+emit (#291: switch camera-box to a no-display
# drop-in instead of stopping it), fail loud if the painter binary is absent, launch the PINNED
# dual-QR vernier painter WITH the QPSK A/V-sync audio marker (#420: the marker is a thread inside
# this same process — --audio-marker/--audio-marker-device/--audio-marker-cadence-ticks/
# --marker-log — never a separate launch), recording its PID, then verify it is up AND writing
# /dev/fb0 AND (#420) the marker's ALSA PCM is actually RUNNING — fail loud + kill the painter if
# silent (a run with no audible marker is a wasted, unmeasured run). Pure string so a unit test can
# assert the pinned flags + the safety properties without a live cam. Loop vars (\$i, \$!,
# \$PAINTER_PID) are \$-escaped so they run REMOTELY; the ALSA card/dev are parsed from AUDIO_DEV
# LOCALLY (pure bash parameter expansion) so the self-check below is a plain literal path — no
# remote-side parsing needed inside the already-nested heredoc.
painter_launch_remote() {
  local bin="$1" dur="$2" qr="$3" pidfile="$4" extra="${5:-}"
  # #290: painter rate — positional like the other params, with the PAINTER_FPS pinned constant as
  # the fallback (keeps the builder pure; the call site passes "$PAINTER_FPS"). Paint at the 60fps
  # capture rate so the optical tick advances 60 distinct ids/s.
  local fps="${6:-${PAINTER_FPS:-60}}"
  local cbbin="${7:-${CAMERA_BOX_BIN:-/usr/local/bin/camera-box}}"
  local dropin="${8:-$RIG_TEST_DROPIN}"   # path single-sourced in lib/rig-test-dropin.sh (#309)
  local dropin_dir; dropin_dir="$(dirname "$dropin")"
  # #420: the QPSK audio-marker params — same positional-with-env-fallback shape as fps/cbbin/dropin.
  local audio_dev="${9:-${AUDIO_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}}"
  local audio_cadence="${10:-${AUDIO_MARKER_CADENCE_TICKS:-180}}"
  local marker_log="${11:-${AUDIO_MARKER_LOG:-/run/rig-qpsk-markers.csv}}"
  # Parse CARD=<id> / DEV=<n> out of "hw:CARD=<id>,DEV=<n>" with plain bash parameter expansion
  # (LOCALLY, not on the remote box) so the audible self-check below can bake a literal
  # /proc/asound/<id>/pcm<n>p/sub0/status path straight into the remote heredoc.
  local audio_card="${audio_dev#*CARD=}"; audio_card="${audio_card%%,*}"
  local audio_devnum="${audio_dev#*DEV=}"; audio_devnum="${audio_devnum%%,*}"
  local audio_status="/proc/asound/$audio_card/pcm${audio_devnum}p/sub0/status"
  cat <<REMOTE
set -e
# (0) idempotency: stop any painter from a previous TEST run so re-running never stacks two painters on
#     /dev/fb0. pkill -x matches the process NAME only (comm) — it can NEVER self-match the remote
#     shell's cmdline (the naive 'pkill -f frame-probe' self-kill footgun this whole rig avoids).
if [ -f "$pidfile" ]; then
  OLD=\$(cat "$pidfile" 2>/dev/null || true)
  [ -n "\$OLD" ] && kill "\$OLD" 2>/dev/null || true
fi
pkill -x frame-probe 2>/dev/null || true
# (1) free /dev/fb0 WITHOUT killing capture+emit (#291). cam2 does THREE independent things: DISPLAY
#     (--display -> /dev/fb0/HDMI), CAPTURE (/dev/video0) and EMIT (NDI to strih). ONLY display grabs
#     fb0; capture+emit do not. The old switch fully STOPPED the whole service, which killed all three
#     and dropped cam2 as a measurable camera. Instead install a TRANSIENT systemd drop-in that
#     overrides ExecStart to run camera-box WITHOUT --display, then reload + restart: display output
#     stops (fb0 freed for the painter) while capture+emit keep running. The drop-in lives in /run
#     (tmpfs) so a reboot auto-reverts to the deployed --display unit; EVENT mode removes it
#     explicitly. Because the drop-in IS the active ExecStart, the unit's Restart=always now respawns
#     the NO-display command — a restart can never re-grab fb0 (the footgun a naive kill+respawn had).
mkdir -p "$dropin_dir"
{
  echo '[Service]'
  echo 'ExecStart='
  echo "ExecStart=$cbbin"
} > "$dropin"
systemctl daemon-reload
systemctl restart camera-box
# (2) wait until /dev/fb0 is actually free (the no-display camera-box released it; teardown is async).
i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done
if fuser -s /dev/fb0 2>/dev/null; then echo "FAIL: /dev/fb0 still held after switching camera-box to no-display mode" >&2; exit 1; fi
echo "ok: /dev/fb0 free (camera-box NOT stopped — only display output dropped; capture+emit keep running)"
# (2b) #291: verify camera-box is STILL ACTIVE (so capture+emit keep running — the whole point) and
#      now runs WITHOUT --display, so a Restart=always respawn can never re-grab fb0. NOTE: this is a
#      systemd is-active check (Type=simple → 'active' == process forked); it does NOT itself prove the
#      NDI emit reached strih — that optical/network proof is a rig step (see the e2e skill).
i=0; while [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ] && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
if [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ]; then
  echo "FAIL: camera-box not active after switching to no-display mode (capture+emit must keep running)" >&2
  systemctl status camera-box --no-pager >&2 2>/dev/null || true
  exit 1
fi
if systemctl show -p ExecStart --value camera-box 2>/dev/null | grep -q -- '--display'; then
  echo "FAIL: camera-box still launches with --display — fb0 would be re-grabbed" >&2
  exit 1
fi
echo "ok: camera-box ACTIVE in no-display mode (not stopped; capture+emit running) — fb0 free for the painter"
# (3) the painter binary MUST be present — deploy the CI probe-tools-linux-amd64 artifact to it.
if [ ! -x "$bin" ]; then
  echo "FAIL: painter binary $bin missing/not-executable on cam2." >&2
  echo "      Deploy the CI probe-tools-linux-amd64 artifact:" >&2
  echo "        gh run download <latest CI run> -n probe-tools-linux-amd64" >&2
  echo "        scp frame-probe root@$PAINTER_IP:$bin   # then chmod +x" >&2
  exit 1
fi
# (4) launch the PINNED dual-QR vernier painter WITH the QPSK A/V-sync audio marker (#420: both on
#     the SAME process — the marker is a thread inside frame-probe, in lock-step with the painter's
#     frame_id via the shared refresh tick, src/probe/qpsk_emit.rs); record its PID for a clean
#     event-mode stop (stopping this one process stops both painter AND marker).
#     --paint-fps $fps pins the rate to the 60fps capture (#290): the painter must paint 60 distinct
#     ticks/s or no 60fps optical timing can be resolved. Under KMS the painter is vblank-locked at the
#     monitor refresh and the flag is a documented no-op; on the fbdev fallback it forces the rate.
rm -f "$pidfile" 2>/dev/null || true
nohup $bin --paint-only --dual-qr --qr-size $qr --duration-secs $dur --paint-fps $fps \
  --audio-marker --audio-marker-device $audio_dev --audio-marker-cadence-ticks $audio_cadence \
  --marker-log $marker_log $extra >/tmp/rig-painter.log 2>&1 &
echo \$! > "$pidfile"
PAINTER_PID=\$(cat "$pidfile")
sleep 3
# (5) verify the painter is UP and actually writing /dev/fb0.
if ! kill -0 "\$PAINTER_PID" 2>/dev/null; then
  echo "FAIL: painter PID \$PAINTER_PID not alive (see /tmp/rig-painter.log on cam2):" >&2
  tail -n 20 /tmp/rig-painter.log >&2 2>/dev/null || true
  exit 1
fi
i=0; while ! fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
if ! fuser -s /dev/fb0 2>/dev/null; then echo "FAIL: painter PID \$PAINTER_PID alive but NOT writing /dev/fb0" >&2; exit 1; fi
echo "PASS: painter PID \$PAINTER_PID up + painting /dev/fb0 (dual-QR ${qr}px, ${fps}fps, ${dur}s)"
# (6) #420: verify the QPSK audio marker is ACTUALLY producing audio, not just that the process is
#     alive. The emitter is a CONTINUOUS-FEED writer (silence between markers, tone when due — it
#     never lets the ALSA ring drain), so a healthy run means the PCM backing $audio_dev is OPEN and
#     RUNNING right now. #420's root cause was NOTHING opening the device at all (TEST mode never
#     passed --audio-marker), so this is a REAL kernel-reported signal that catches exactly that
#     failure class — never a stub that always passes. FAIL LOUD + kill the just-verified painter:
#     a silent marker means this whole TEST-mode switch produced an unmeasured, wasted run.
i=0; while [ \$i -lt 20 ]; do
  if [ -f "$audio_status" ] && grep -q '^state: RUNNING' "$audio_status" 2>/dev/null; then break; fi
  sleep 0.5; i=\$((i+1))
done
if [ ! -f "$audio_status" ] || ! grep -q '^state: RUNNING' "$audio_status" 2>/dev/null; then
  echo "FAIL: #420 QPSK audio marker ($audio_dev -> $audio_status) is NOT RUNNING — no marker audio is being emitted (silent, unmeasured run)." >&2
  echo "      status file: \$(cat "$audio_status" 2>/dev/null || echo '<missing>')" >&2
  kill "\$PAINTER_PID" 2>/dev/null || true
  exit 1
fi
echo "PASS: #420 QPSK audio marker RUNNING on $audio_dev ($audio_status state=RUNNING, cadence=${audio_cadence} ticks, log=$marker_log)"
REMOTE
}

# painter_stop_remote PIDFILE [DROPIN] -> the REMOTE bash run on cam2 to enter EVENT mode: stop the
# painter cleanly via its PID file (NEVER a 'pkill -f frame-probe' — that matches the remote shell's
# own cmdline and self-kills the cleanup), REMOVE the transient no-display drop-in TEST mode installed
# (#291), then reload + restart camera-box and verify the service is active AND --display is restored
# (camera-box re-grabbed /dev/fb0 to paint the interkom return on the monitor).
painter_stop_remote() {
  local pidfile="$1"
  local dropin="${2:-$RIG_TEST_DROPIN}"   # path single-sourced in lib/rig-test-dropin.sh (#309)
  cat <<REMOTE
set -e
# (1) stop the painter cleanly via its PID file (the self-match-safe path).
if [ -f "$pidfile" ]; then
  PID=\$(cat "$pidfile" 2>/dev/null || true)
  if [ -n "\$PID" ] && kill -0 "\$PID" 2>/dev/null; then kill "\$PID" 2>/dev/null || true; fi
  rm -f "$pidfile" 2>/dev/null || true
fi
# (2) belt-and-suspenders: pkill -x matches the process NAME only (comm), so it can NEVER match the
#     remote shell's own cmdline — immune to the self-match that strands cleanups (NOT pkill -f).
pkill -x frame-probe 2>/dev/null || true
# (3) wait until /dev/fb0 is released by the painter, then RESTORE the deployed --display camera-box
#     (#291): remove the transient no-display drop-in TEST mode installed, reload, and RESTART so the
#     unit's ExecStart reverts to --display and camera-box re-grabs /dev/fb0 for the interkom return.
#     (TEST mode no longer STOPS camera-box — it switches it to no-display — so EVENT mode RESTARTS
#     rather than just starts, to drop the override.)
i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
$(rig_test_dropin_clear_cmds "$dropin")
systemctl restart camera-box
# (4) verify the service is active.
i=0; while [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ] && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
if [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ]; then
  echo "FAIL: camera-box service not active after restart" >&2
  systemctl status camera-box --no-pager >&2 2>/dev/null || true
  exit 1
fi
# (5) verify --display is restored: the EFFECTIVE ExecStart carries --display (same resolved check
#     TEST mode uses — 'systemctl show', NOT 'systemctl cat', so a silently-failed drop-in removal
#     can't false-pass on the base unit's --display line) AND camera-box re-grabbed /dev/fb0.
if ! systemctl show -p ExecStart --value camera-box 2>/dev/null | grep -q -- '--display'; then
  echo "FAIL: camera-box ExecStart has no --display — interkom monitor not restored" >&2
  exit 1
fi
i=0; while ! fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
if ! fuser -s /dev/fb0 2>/dev/null; then
  echo "FAIL: camera-box active but /dev/fb0 not held — --display not painting the interkom return" >&2
  exit 1
fi
echo "PASS: painter stopped, camera-box active + --display restored (holding /dev/fb0)"
REMOTE
}

# --- #257 per-box measurement-burn WebSocket toggle (NO OBS relaunch) ----------------------------
# #257 replaced the launch-shell OBS_BURN_* env (the old --mode test/event relaunch) with a per-source
# `genlock_burn` bool flipped over OBS WebSocket — so TEST/EVENT no longer relaunch OBS to change the
# burn. The harness HAS websocket access to both boxes (ssh does NOT — that's why genlock RELAUNCH is
# still printed, not run), so rig-mode toggles the burn DIRECTLY via scripts/obs_burn_filter.py.
#
# The per-box burn targets (host=ip, input=the program-feeding NDI source the recording captures).
# Overridable; defaults mirror the recording-e2e BURN_TARGETS (the prod program inputs).
STRIH_IP="${STRIH_IP:-10.77.9.202}"
STREAM_IP="${STREAM_IP:-10.77.9.204}"
STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-NDI cam5}"      # strih program input (#246 burn target)
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}" # stream program input (#246 burn target)
OBS_WS_PASSWORD="${OBS_WS_PASSWORD:-}"

# obs_burn_targets -> the host=ip=source burn triples, one per line "ip|source|box".
obs_burn_targets() {
  printf '%s|%s|%s\n' "$STRIH_IP" "$STRIH_PROG_SOURCE" strih
  printf '%s|%s|%s\n' "$STREAM_IP" "$STREAM_PROG_SOURCE" stream
}

# burn_action_for_mode MODE -> the obs_burn_filter.py action (test=add/on, event=remove/off).
burn_action_for_mode() {
  case "${1:-}" in
    test)  printf 'add' ;;
    event) printf 'remove' ;;
    *) echo "burn_action_for_mode: unknown mode '${1:-}' (expected test|event)" >&2; return 2 ;;
  esac
}

# toggle_burn MODE -> flip the per-source genlock_burn on (test) / off (event) on BOTH boxes over
# OBS WebSocket (no relaunch), fail-loud on any box. The genlock build is hard-locked (#257), so the
# only mode-specific OBS state is this burn bool; the genlock render tick is the build default.
toggle_burn() {
  local mode="$1" action here rc=0
  action="$(burn_action_for_mode "$mode")"
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  while IFS='|' read -r ip src box; do
    [ -n "$ip" ] || continue
    echo "[obs ${box} ${ip}] genlock_burn ${action} on '${src}' (WebSocket, no relaunch)"
    python3 "$here/obs_burn_filter.py" "$action" --host "$ip" --input "$src" --password "$OBS_WS_PASSWORD" \
      2>&1 | sed "s/^/    [${box} burn] /" || rc=$?
  done < <(obs_burn_targets)
  return $rc
}

# enforce_strih_ndi_mapping -> set + VERIFY the strih NDI-input→camera mapping (#399) over OBS WS.
# The mapping is fixed + Claude-owned (never a user question): NDI cam5→CAM1, cam1→CAM3, cam3→CAM4,
# cam2→CAM2 (the pins in set-ndi-mapping.py). It drifts (recurring bug: two inputs both on CAM4 → a
# camera shows twice, another missing), and a hot WS rebind does not survive a force-kill relaunch —
# so rig activation ENFORCES it every time here instead of the operator/agent re-doing it by hand.
# Fail-loud (non-zero) if it cannot make all 4 distinct.
enforce_strih_ndi_mapping() {
  local here rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  echo "[obs strih ${STRIH_IP}] #399 enforce NDI-input→camera mapping (4 distinct) over WebSocket:"
  python3 "$here/set-ndi-mapping.py" --host "$STRIH_IP" --password "$OBS_WS_PASSWORD" \
    2>&1 | sed 's/^/    [strih ndi-map] /' || rc=$?
  return $rc
}

# print_genlock_relaunch_note MODE -> the genlock RELAUNCH step (printed, not run — ssh to Windows is
# DENIED so OBS launch goes via the win-* MCP). #257: env-free; the wrapper just verifies the genlock
# render tick is ENABLED (build default). Only needed if OBS is not already running on a box.
print_genlock_relaunch_note() {
  local mode="$1"
  cat <<EOF
# ---- Windows OBS genlock relaunch (only if OBS is not already running; ssh denied -> win-* MCP) ----
# The measurement burn for ${mode} mode was just toggled over WebSocket above (no relaunch). The
# genlock build is hard-locked (render tick + ts-align always ON, latency 3 ms — NO env), so a
# relaunch is only needed to (re)start a stopped/wedged OBS. Per box, paste the printed program into
# that box's win-* MCP Shell:
#   strih  : scripts/launch-obs-genlock.sh --box strih  --force
#   stream : scripts/launch-obs-genlock.sh --box stream --force
# Then confirm (per the e2e / obs-ops playbook skills): the right scene (PHASE2-PROBE for test, prod
# for event), recording NATIVE 1080p (#225), DanteSync locked. The wrapper EXITS 0 only when the OBS
# log shows the genlock render tick ENABLED.
EOF
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------------

usage() {
  cat <<'EOF'
rig-mode.sh — the deterministic rig TEST-mode / EVENT-mode switch (#247).

Usage:
  scripts/rig-mode.sh test     # paint the dual-QR vernier on cam2 + print the OBS burns-ON step
  scripts/rig-mode.sh event    # stop the QR, restore camera-box --display + print the OBS burns-OFF step

The CAM side (cam2 = 10.77.9.62) is applied + verified here over ssh. The OBS burn is toggled DIRECTLY
over OBS WebSocket (scripts/obs_burn_filter.py — no relaunch); the env-free genlock relaunch (no
--mode) is PRINTED to run via the win-* MCP (ssh to Windows is denied). See the script header for
env overrides.

Exit codes: 0 = mode applied (cam side + burn WS toggle) + relaunch note printed; 2 = usage error.
EOF
}

require_sshpass() {
  command -v sshpass >/dev/null 2>&1 || {
    echo "ERROR: sshpass not found — needed to ssh into the cam boxes (apt install sshpass)." >&2
    exit 1
  }
}

cam_ssh() {
  # cam_ssh REMOTE_SCRIPT — run REMOTE_SCRIPT on cam2 as root, fail-loud on a non-zero remote exit.
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 root@"$PAINTER_IP" "$1"
}

do_test() {
  require_sshpass
  # #281 Fix#3: mark the rig as deliberately in a TEST state so the rig-restore watchdog does not
  # fight an in-progress test (until the marker goes stale — see the lib-source note above).
  rig_heartbeat_write "rig-mode:test" 2>/dev/null \
    && echo "[#281] rig-active heartbeat SET ($(rig_heartbeat_path))" \
    || echo "WARNING: could not set rig-active heartbeat (#281)" >&2
  echo "===== rig-mode TEST (#247/#257/#291) — paint dual-QR vernier on cam2, genlock_burn ON downstream ====="
  echo "[cam2 ${PAINTER_IP}] switch camera-box to no-display (free /dev/fb0, keep capture+emit) -> launch PINNED painter (qr=${QR_SIZE}px)"
  cam_ssh "$(painter_launch_remote "$PAINTER_BIN" "$PAINTER_DURATION_SECS" "$QR_SIZE" "$PAINTER_PIDFILE" "$PAINTER_EXTRA_FLAGS" "$PAINTER_FPS" "$CAMERA_BOX_BIN" "$RIG_TEST_DROPIN" "$AUDIO_MARKER_DEVICE" "$AUDIO_MARKER_CADENCE_TICKS" "$AUDIO_MARKER_LOG")"
  echo
  echo "[obs] #257 toggle per-source genlock_burn ON over WebSocket (no relaunch):"
  toggle_burn test
  echo
  echo "[obs] #399 enforce the strih NDI-input→camera mapping (4 distinct):"
  enforce_strih_ndi_mapping
  echo
  print_genlock_relaunch_note test
  echo
  echo "ACHIEVED (cam side): cam2 painting dual-QR ${QR_SIZE}px on /dev/fb0 (pidfile ${PAINTER_PIDFILE})."
  echo "                     cam2 camera-box still ACTIVE in no-display mode (#291: NOT stopped — capture+emit keep running)."
  echo "                     cam2 QPSK audio marker RUNNING+VERIFIED on ${AUDIO_MARKER_DEVICE} (#420: cadence ${AUDIO_MARKER_CADENCE_TICKS} ticks, log ${AUDIO_MARKER_LOG})."
  echo "                     -> verify cam2's NDI actually reaches strih on the rig (this switch does not prove the emit)."
  echo "                     cam1 (${CAM1_IP}) left on its DEPLOYED service (already at the 30 fps test rate)."
  echo "ACHIEVED (obs side): genlock_burn=true on strih + stream program inputs (WebSocket, no relaunch)."
  echo "NEXT: confirm the PHASE2-PROBE scene + native-1080p recording per the e2e/obs-ops skill -> TEST mode."
  echo "RESULT: TEST mode — cam side PASS, burns ON."
}

do_event() {
  require_sshpass
  # #281 Fix#3: clear the rig-active heartbeat — we are returning the rig to a clean prod/EVENT
  # state, so the watchdog need no longer treat it as "a test is running".
  rig_heartbeat_clear 2>/dev/null \
    && echo "[#281] rig-active heartbeat CLEARED" \
    || true
  echo "===== rig-mode EVENT (#247/#257/#291) — stop QR, restore clean broadcast, genlock_burn OFF ====="
  echo "[cam2 ${PAINTER_IP}] stop painter (via pidfile) -> remove no-display drop-in -> restart camera-box -> verify --display restored"
  cam_ssh "$(painter_stop_remote "$PAINTER_PIDFILE")"
  echo
  echo "[obs] #257 toggle per-source genlock_burn OFF over WebSocket (no relaunch — the #246 guard):"
  toggle_burn event
  echo
  echo "[obs] #399 enforce the strih NDI-input→camera mapping (4 distinct — correct for broadcast too):"
  enforce_strih_ndi_mapping
  echo
  print_genlock_relaunch_note event
  echo
  echo "ACHIEVED (cam side): cam2 painter stopped, camera-box active + --display interkom restored."
  echo "ACHIEVED (obs side): genlock_burn=false on strih + stream program inputs (WebSocket, no relaunch)."
  echo "NEXT: confirm the prod scene per the obs-ops skill -> rig in clean EVENT mode (no burn on broadcast)."
  echo "RESULT: EVENT mode — cam side PASS, burns OFF."
}

main() {
  local mode="${1:-}"
  case "$mode" in
    test)      do_test ;;
    event)     do_event ;;
    -h|--help) usage; exit 0 ;;
    *) echo "ERROR: mode must be 'test' or 'event' (got '${mode}')" >&2; usage >&2; exit 2 ;;
  esac
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
