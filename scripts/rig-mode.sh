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
# MCP (same model as recording-verdict-on-stream.sh's default planner mode — a GUI relaunch is
# exactly what the win-* MCP is for; #701 proved plain scp/ssh reaches strih/stream, but that
# doesn't drive/verify a GUI app launch).
#
#   TEST  : cam2 — stop the PERMANENT cam2-painter.service if installed (#440: it and this script's
#                  transient emitter-painter both write /dev/fb0 — left running it made the displayed
#                  QR alternate between the two painters' run_ids, breaking --av-sync frame_id
#                  pairing), guarded so a box without the unit is unaffected. Then free /dev/fb0
#                  WITHOUT killing capture+emit (#291: switch camera-box to a TRANSIENT no-display
#                  systemd drop-in instead of stopping it — display output is the ONLY thing that
#                  grabs fb0; /dev/video0 capture + NDI emit do not), so cam2 stays a MEASURABLE
#                  camera during the test. Verify the deployed painter binary is present, WARN
#                  (never fail) if it looks stale (#440: mtime printed — a pre-#431 build silently
#                  fails the #431 emission check below), then launch the PINNED dual-QR vernier
#                  painter (frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60
#                  --duration-secs N — #290: 60fps to match the 60fps capture so 60 distinct ticks/s
#                  resolve) WITH the QPSK A/V-sync audio marker (--audio-marker
#                  --audio-marker-device hw:CARD=PCH,DEV=3 — #420: TEST mode used to launch the
#                  painter WITHOUT this, so the A/V-sync measurement was silently unmeasured — no
#                  marker ever reached the recording), verify it is up + writing /dev/fb0 AND
#                  camera-box is still active + capturing/emitting AND the marker's ALSA PCM is
#                  actually RUNNING (#420: fail loud + kill the painter if silent). Then PRINT the
#                  OBS test step (burns ON, run_id strih 911002 / stream 911004).
#   EVENT : cam2 — stop the painter cleanly (via its PID file — NOT a naive `pkill -f frame-probe`,
#                  which would self-kill a shell whose cmdline contains "frame-probe"); the QPSK audio
#                  marker is a THREAD inside that same process (#420: no separate stop needed), so this
#                  also stops the marker. STOP + DISABLE the permanent cam2-painter.service if present
#                  (#892: EVENT mode must NEVER (re)start it — a live-broadcast QR hazard, and
#                  disabling closes the reboot path too; superseded the old #440 "restore" call).
#                  REMOVE the transient CAMERA_BOX_NO_DISPLAY=1 drop-in TEST mode installed
#                  (#291/#528), then reload + restart camera-box and verify the service is active +
#                  the unconditional HDMI preview restored (on a box WITHOUT a dedicated painter
#                  unit — a painter box's monitor stays owned by the now-stopped painter, #863).
#                  Then PRINT the OBS event step (burns OFF: the #246 guard; the wrapper refuses to
#                  launch otherwise).
#
# The painter binary on cam2 comes from the CI probe-tools-linux-amd64 artifact:
#   gh run download <latest CI run> -n probe-tools-linux-amd64
#   scp frame-probe root@10.77.9.62:/usr/local/bin/frame-probe
# If it is absent, TEST mode FAILS LOUD telling the operator to deploy it.
#
# The SOURCE camera (#1135: DERIVED via camera_source_box — cam1 on a cam1-first set, cam3 once cam1
# is retired; RIG_SOURCE_BOX below) is NOT reconfigured here: it runs its DEPLOYED camera-box
# service, which already emits a 30 fps NDI ("CAMN (usb)") at the certified v4l2 controls (the
# recording-e2e harness convention — the real camera is already at the test rate). See the e2e
# playbook skill.
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
# #827 (binding owner directive): camera-set.sh's CAMERA_ACTIVE_SET is the ONE declared list of
# cameras physically installed today — enforce_strih_ndi_mapping() below passes it straight
# through to set-ndi-mapping.py's own --active flag, and event_mode_assert()'s fleet sweep
# derives its box list from it too, so re-enabling a retired camera never needs a change here.
# shellcheck source=scripts/camera-set.sh
. "$RIG_MODE_DIR/camera-set.sh"
# #1135: SINGLE SOURCE OF TRUTH for imag-nb's own scene + NDI-input name per physical camera
# (imag_scene_for_camera / imag_source_for_camera). Sourced so the source-role derivation below can
# resolve the imag PROGRAM route off the DERIVED source box, not a hard-pinned cam1 (same lib
# recording-e2e.sh already uses for its imag scene).
# shellcheck source=scripts/lib/imag-scene-route.sh
. "$RIG_MODE_DIR/lib/imag-scene-route.sh"
# shellcheck source=scripts/lib/rig-test-dropin.sh
. "$RIG_MODE_DIR/lib/rig-test-dropin.sh"
# #420/#421: SINGLE SOURCE OF TRUTH for the QPSK audio-marker AUDIBLE self-check (ALSA CARD/DEV
# parsing + the `state: RUNNING` poll + fail-loud diagnostic), shared with recording-e2e.sh's
# AV_RESTART_GATE painter (#421) so both launches can never drift on what "audible" means.
# shellcheck source=scripts/lib/audio-marker-check.sh
. "$RIG_MODE_DIR/lib/audio-marker-check.sh"
# #725: SINGLE SOURCE OF TRUTH for resolving the QPSK audio-marker's ALSA device DYNAMICALLY
# from the live `aplay -l` output (which HDMI device carries a genuine connected-monitor EDID
# name right now) — see scripts/lib/marker-device-resolve.sh for the full #725 story. A hardcoded
# device silently plays into a dead pin after any HDMI renegotiation.
# shellcheck source=scripts/lib/marker-device-resolve.sh
. "$RIG_MODE_DIR/lib/marker-device-resolve.sh"
# #723: SINGLE SOURCE OF TRUTH for the rig-test LEDGER — anything a test/worker starts on the
# rig registers durably here; EVENT mode cleans BY LEDGER (kill-by-PID, immune to a process
# rename — the #721 incident's root link: a RENAMED painter that every name-based cleanup
# missed). See scripts/lib/rig-test-ledger.sh for the full #723 story.
# shellcheck source=scripts/lib/rig-test-ledger.sh
. "$RIG_MODE_DIR/lib/rig-test-ledger.sh"
# #722: SINGLE SOURCE OF TRUTH for the two fleet-wide EVENT-mode CONTRACT builders that have no
# existing tool (per-box paint-process/service/stray-unit status, artifacts-existing check) —
# see scripts/lib/event-assert.sh for the full #722 story. event_mode_assert() below
# orchestrates these + the existing OBS-side tools into scripts/event_assert.py's decision.
# shellcheck source=scripts/lib/event-assert.sh
. "$RIG_MODE_DIR/lib/event-assert.sh"
# #724: EVENT-mode Discord confirmation sender — after the #722 assert phase, ONE Slovak message
# lands in the owner's Discord thread (pass=confirmation, fail=warning naming what failed). See
# scripts/lib/event-mode-discord-confirm.sh for the full #724 story.
# shellcheck source=scripts/lib/event-mode-discord-confirm.sh
. "$RIG_MODE_DIR/lib/event-mode-discord-confirm.sh"
# #464: SINGLE SOURCE OF TRUTH for the presenter-aware painter-liveness check (KMS page-flip vs
# fbdev) — see scripts/lib/presenter-liveness-check.sh for the full #464 story. Mirrors
# src/presenter_kind.rs::resolve_presenter_kind's "will this run ever touch /dev/fb0?" answer.
# shellcheck source=scripts/lib/presenter-liveness-check.sh
. "$RIG_MODE_DIR/lib/presenter-liveness-check.sh"
# #1008/#937: the durable steady-state handoff -- TEST mode ends by handing the painter to the
# PERMANENT supervised cam2-painter.service instead of leaving a disposable 2h nohup. Reuses
# audio_marker_emission_check_cmds (sourced above) for the marker-growth assert.
# shellcheck source=scripts/lib/cam2-painter-handoff.sh
. "$RIG_MODE_DIR/lib/cam2-painter-handoff.sh"
# #1075: SINGLE SOURCE OF TRUTH for the transient cam2-painter dead-man arm/disarm builders
# (shared with recording-e2e.sh). EVENT mode must DISARM the timer (a SIGKILLed recording-e2e run
# leaves it armed and it periodically restarts cam2-painter -- resurrecting the QR on air), and
# TEST mode re-arms it as the standing "never dark" net; event_mode_assert catches a still-armed
# one as a STRAY unit. Sourced here (before the source-guard) so both the executed flow and the
# unit tests that source this script get the builders.
# shellcheck source=scripts/lib/cam2-painter-deadman.sh
. "$RIG_MODE_DIR/lib/cam2-painter-deadman.sh"
# #281 Fix#3: the rig-active heartbeat. TEST mode SETS it (deliberate "rig is in a test state"
# marker); EVENT mode CLEARS it. Unlike recording-e2e.sh this is a one-shot write (rig-mode exits),
# so the marker goes STALE after RIG_HEARTBEAT_STALE_SEC (default 10 min) — an idle TEST rig with a
# clear stranded signal then becomes watchdog-actionable (the intended safety net: the rig must not
# sit indefinitely in a test state with prod unprotected). Run an active E2E to keep it fresh.
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$RIG_MODE_DIR/lib/rig-heartbeat.sh"
# #901: SINGLE SOURCE OF TRUTH for the measurement-audio-arrival tier decision (dead/quiet/
# audible + message builders) — shared with recording-e2e.sh's own STRICT [4b2/8] preflight
# (audio_preflight_is_silent etc., untouched), which sources the SAME file for the parse/probe
# helpers verify_measurement_audio_arrives (below) reuses.
# shellcheck source=scripts/lib/audio-presence-preflight.sh
. "$RIG_MODE_DIR/lib/audio-presence-preflight.sh"
# #901: win_ssh_run — the plain-ssh PowerShell execution helper (#703/#701) verify_measurement_
# audio_arrives (below) reuses to run ffmpeg volumedetect on stream, mirroring recording-e2e.sh's
# own [4b2/8] preflight call shape exactly.
# shellcheck source=scripts/lib/win-ssh-exec.sh
. "$RIG_MODE_DIR/lib/win-ssh-exec.sh"
# issue 1171: the SAME offline-ack mechanism recording-e2e.sh's [0/8] uses (#758/#827/#1013) — the
# #789 imag-genlock TEST-entry gate consults cambox_offline_ack_reason "imag" so a legitimately
# acked-offline imag SKIPS the gate instead of fail-closing. Pure functions, no side effects at
# source time.
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$RIG_MODE_DIR/lib/cambox-offline-ack.sh"

# --- pinned constants (overridable via env, but DEFAULTS are the single source of truth) -----------
CAM_PW="${CAM_PW:-newlevel}"                 # dev-rig LAN root pw (same as the sibling e2e scripts)
PAINTER_IP="${PAINTER_IP:-10.77.9.62}"       # cam2 — has /dev/fb0 + the monitor the broadcast cam films
# --- #1135: the SOURCE-camera role, DERIVED off camera_source_box (never hard-pinned to cam1) -----
# The box filling the SOURCE-camera role -- films cam2's monitor, carries the #174 render-time
# capture burn, and is routed onto strih + imag PROGRAM as the camera-under-test -- is DERIVED from
# camera-set.sh's camera_source_box() (the SAME single source of truth recording-e2e.sh's E2E gate
# uses since the issue-1134 change), never a second cam1 literal. Retiring cam1 (dropping it from
# CAMERA_ACTIVE_SET) therefore moves the EVENT-mode fleet sweep, the strih/imag burn-target program
# inputs, the imag PROGRAM routing, and the TEST-mode ACHIEVED prints onto the next strih-routable
# box (cam3 today) with ZERO edits here. CAMERA_SOURCE_BOX (env, honoured by camera_source_box)
# overrides the box for a one-off run, same trust model as recording-e2e.sh's CAM= -- it replaces
# the old CAM1_IP override. Resolved ONCE here (above the source-guard, so tests/rig_mode.rs sees
# the derived facts when it sources this script). camera_resolve / camera_strih_route are FACT
# lookups (resolve cam1/cam3/... regardless of CAMERA_ACTIVE_SET); the values are captured into
# RIG_SOURCE_* immediately so a later camera_resolve/camera_strih_route call cannot clobber them.
RIG_SOURCE_BOX="$(camera_source_box)"        # e.g. cam1 (cam1-first set) / cam3 (cam1 retired)
camera_resolve "$RIG_SOURCE_BOX"             # -> CAMERA_IP for the source box
RIG_SOURCE_IP="$CAMERA_IP"                    # the source camera's device IP (the old CAM1_IP role)
camera_strih_route "$RIG_SOURCE_BOX"         # -> CAMERA_STRIH_SOURCE (also CAMERA_STRIH_SCENE, unused
                                             # here: rig-mode routes the strih NDI *input*, never
                                             # scene-switches strih -- so no RIG_SOURCE_STRIH_SCENE)
RIG_SOURCE_STRIH_SOURCE="$CAMERA_STRIH_SOURCE" # strih NDI input behind the source scene ("NDI camN")
RIG_SOURCE_IMAG_SCENE="$(imag_scene_for_camera "$RIG_SOURCE_BOX")"    # imag scene ("Cam N")
RIG_SOURCE_IMAG_SOURCE="$(imag_source_for_camera "$RIG_SOURCE_BOX")"  # imag NDI input ("NDI CAMN")
# #722: the FULL fleet (targets.md) — used only by the EVENT-mode CONTRACT's fleet-wide
# paint-process/service/stray-unit sweep (event_mode_assert). cam2 already has PAINTER_IP; the
# rest were never previously needed as rig-mode.sh constants (only cam2 is reconfigured here).
# #827 (2026-07-27, binding owner directive): cam5/cam6/cam7's IPs stay declared as FACTS even
# though they are retired from CAMERA_ACTIVE_SET today (grabber cards returned to their owner,
# boxes powered off) — event_mode_assert()'s sweep derives WHICH of these actually get swept
# entirely from camera_active_secondary_set() (camera-set.sh, sourced above), never from a
# second hardcoded list here.
CAM3_IP="${CAM3_IP:-10.77.9.63}"
CAM4_IP="${CAM4_IP:-10.77.9.64}"
CAM5_IP="${CAM5_IP:-10.77.9.65}"
CAM6_IP="${CAM6_IP:-10.77.9.66}"
CAM7_IP="${CAM7_IP:-10.77.9.67}"
# rig_mode_secondary_ip NAME -> the fixed IP for a non-source, non-painter camera name. Mirrors
# recording-e2e.sh's camera_secondary_ip() shape (single lookup site, same facts).
rig_mode_secondary_ip() {
  case "$1" in
    cam3) printf '%s' "$CAM3_IP" ;;
    cam4) printf '%s' "$CAM4_IP" ;;
    cam5) printf '%s' "$CAM5_IP" ;;
    cam6) printf '%s' "$CAM6_IP" ;;
    cam7) printf '%s' "$CAM7_IP" ;;
    *) echo "rig_mode_secondary_ip: unknown secondary camera '$1'" >&2; return 1 ;;
  esac
}
PAINTER_BIN="${PAINTER_BIN:-/usr/local/bin/frame-probe}"
QR_SIZE="${QR_SIZE:-700}"
PAINTER_FPS="${PAINTER_FPS:-60}"             # painter rate — MUST match the 60fps capture (#290)
PAINTER_DURATION_SECS="${PAINTER_DURATION_SECS:-7200}"
PAINTER_PIDFILE="${PAINTER_PIDFILE:-/run/rig-painter.pid}"
PAINTER_EXTRA_FLAGS="${PAINTER_EXTRA_FLAGS:-}"
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
#
# #528 design pivot (2026-07-08): the drop-in used to override ExecStart to run camera-box WITHOUT
# a --display flag (a bare ExecStart previously meant "no display thread at all"). Now that the
# HDMI cameraman preview is UNCONDITIONAL on every cambox (baked into the binary's own
# DEFAULT_DISPLAY_SOURCE default), a bare ExecStart no longer frees /dev/fb0 — the drop-in instead
# sets `Environment=CAMERA_BOX_NO_DISPLAY=1`, the dedicated opt-out src/main.rs checks first (wins
# over everything, including any --display flag). ExecStart itself is never touched any more.

# --- PURE functions (no network, no ssh — unit-tested by sourcing this script) --------------------

# cam2_painter_service_stop_cmds -> the REMOTE bash (#440) that stops the PERMANENT
# `cam2-painter.service` (systemd, always-on dual-QR painter) if it is installed on this box,
# guarded so a box without the unit is unaffected. WHY (#440, live evidence from the #420 A/V-sync
# measurement): `cam2-painter.service` and the TRANSIENT emitter-painter this script launches below
# (`frame-probe --audio-marker`) are SEPARATE processes that BOTH write /dev/fb0 — during a
# measurement the displayed QR alternated between the two painters' run_ids, so the QPSK audio
# marker's frame_id could not reliably match the displayed video QR, breaking --av-sync pairing.
# TEST mode must be the SOLE painter of fb0, so the permanent painter is stopped first.
cam2_painter_service_stop_cmds() {
  cat <<'REMOTE'
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  echo "[#440] cam2-painter.service present -> stopping (avoids racing /dev/fb0 with the TEST-mode emitter-painter)"
  systemctl stop cam2-painter.service 2>/dev/null || true
else
  echo "[#440] cam2-painter.service not installed on this box -> nothing to stop"
fi
REMOTE
}

# cam2_painter_service_disable_cmds -> the REMOTE bash (#892) EVENT mode runs INSTEAD of the old
# #440 "restore" call. The #440 assumption ("EVENT always follows TEST, and TEST is what stopped
# the permanent painter, so EVENT should put it back") is WRONG for EVENT mode: `rig-mode.sh
# event` can be invoked on an already-clean rig with no TEST session preceding it, and (re)starting
# the permanent painter in that case puts a QR straight onto the LIVE BROADCAST (cam2's monitor is
# what the whole cambox fleet films via the splitter) -- live-caught 2026-07-30. So EVENT mode must
# instead STOP (idempotent -- in case something raced it back on) AND DISABLE the unit, so neither
# this call nor a later reboot (the unit's own `enabled` + `Restart=always` state -- the SECOND
# incident on #892, a bare reboot re-armed the QR 3h after a manual stop) can bring the QR back on
# its own. A box without the unit installed is unaffected, same guard shape as #440.
cam2_painter_service_disable_cmds() {
  cat <<'REMOTE'
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  echo "[#892] cam2-painter.service present -> stopping + disabling (EVENT mode must never leave a QR painter that can return on its own, whether via a restart call or a later reboot)"
  systemctl stop cam2-painter.service 2>/dev/null || true
  systemctl disable cam2-painter.service 2>/dev/null || true
else
  echo "[#892] cam2-painter.service not installed on this box -> nothing to stop/disable"
fi
REMOTE
}

# painter_launch_remote BIN DUR QR PIDFILE [EXTRA] [FPS] [DROPIN] [AUDIO_DEV] [AUDIO_CADENCE]
#   [MARKER_LOG] -> the REMOTE bash run on cam2 (over ssh) to enter TEST mode: stop any prior
# painter, free /dev/fb0 WITHOUT killing capture+emit (#291/#528: switch camera-box to a
# CAMERA_BOX_NO_DISPLAY=1 drop-in instead of stopping it), fail loud if the painter binary is
# absent, launch the PINNED dual-QR vernier painter WITH the QPSK A/V-sync audio marker (#420: the
# marker is a thread inside this same process — --audio-marker/--audio-marker-device/
# --audio-marker-cadence-ticks/--marker-log — never a separate launch), recording its PID, then
# verify it is up AND writing /dev/fb0 AND (#420) the marker's ALSA PCM is actually RUNNING — fail
# loud + kill the painter if silent (a run with no audible marker is a wasted, unmeasured run).
# Pure string so a unit test can assert the pinned flags + the safety properties without a live
# cam. Loop vars (\$i, \$!, \$PAINTER_PID) are \$-escaped so they run REMOTELY; the ALSA card/dev
# are parsed from AUDIO_DEV LOCALLY (pure bash parameter expansion) so the self-check below is a
# plain literal path — no remote-side parsing needed inside the already-nested heredoc.
painter_launch_remote() {
  local bin="$1" dur="$2" qr="$3" pidfile="$4" extra="${5:-}"
  # #290: painter rate — positional like the other params, with the PAINTER_FPS pinned constant as
  # the fallback (keeps the builder pure; the call site passes "$PAINTER_FPS"). Paint at the 60fps
  # capture rate so the optical tick advances 60 distinct ids/s.
  local fps="${6:-${PAINTER_FPS:-60}}"
  local dropin="${7:-$RIG_TEST_DROPIN}"   # path single-sourced in lib/rig-test-dropin.sh (#309)
  local dropin_dir; dropin_dir="$(dirname "$dropin")"
  # #420: the QPSK audio-marker params — same positional-with-env-fallback shape as fps/dropin.
  local audio_dev="${8:-${AUDIO_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}}"
  local audio_cadence="${9:-${AUDIO_MARKER_CADENCE_TICKS:-180}}"
  local marker_log="${10:-${AUDIO_MARKER_LOG:-/run/rig-qpsk-markers.csv}}"
  # #420/#421: the ALSA CARD/DEV parsing + the audible RUNNING-poll self-check are DRY-extracted
  # into scripts/lib/audio-marker-check.sh (sourced above) — shared with recording-e2e.sh's
  # AV_RESTART_GATE painter so the two launches can never drift on what "audible" means.
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
# (0.5) #440: stop the PERMANENT cam2-painter.service (a DIFFERENT, always-on dual-QR painter) if
#       present — it would otherwise race /dev/fb0 with the emitter-painter launched below (see the
#       cam2_painter_service_stop_cmds header comment for the full #440 story). Guarded: a box
#       without the unit is unaffected.
$(cam2_painter_service_stop_cmds)
# (1) free /dev/fb0 WITHOUT killing capture+emit (#291). cam2 does THREE independent things: DISPLAY
#     (the HDMI preview -> /dev/fb0), CAPTURE (/dev/video0) and EMIT (NDI to strih). ONLY display
#     grabs fb0; capture+emit do not. The old switch fully STOPPED the whole service, which killed
#     all three and dropped cam2 as a measurable camera. Instead install a TRANSIENT systemd
#     drop-in that sets CAMERA_BOX_NO_DISPLAY=1 (#528: the preview is now unconditional on every
#     cambox, so a bare/plain ExecStart no longer means "no display thread" — this dedicated env
#     var opt-out is what src/main.rs::resolve_display_config checks FIRST, winning over
#     everything else), then reload + restart: display output stops (fb0 freed for the painter)
#     while capture+emit keep running. The drop-in lives in /run (tmpfs) so a reboot auto-reverts
#     to the deployed unconditional-preview unit; EVENT mode removes it explicitly. Because the
#     drop-in IS the active Environment, the unit's Restart=always now respawns the NO-display
#     command — a restart can never re-grab fb0 (the footgun a naive kill+respawn had).
mkdir -p "$dropin_dir"
{
  echo '[Service]'
  echo 'Environment=CAMERA_BOX_NO_DISPLAY=1'
} > "$dropin"
systemctl daemon-reload
systemctl restart camera-box
# (2) wait until /dev/fb0 is actually free (the no-display camera-box released it; teardown is async).
i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done
if fuser -s /dev/fb0 2>/dev/null; then echo "FAIL: /dev/fb0 still held after switching camera-box to no-display mode" >&2; exit 1; fi
echo "ok: /dev/fb0 free (camera-box NOT stopped — only display output dropped; capture+emit keep running)"
# (2b) #291: verify camera-box is STILL ACTIVE (so capture+emit keep running — the whole point) and
#      (#528) now runs WITH CAMERA_BOX_NO_DISPLAY=1 in its effective Environment, so a
#      Restart=always respawn can never re-grab fb0. NOTE: this is a systemd is-active check
#      (Type=simple → 'active' == process forked); it does NOT itself prove the NDI emit reached
#      strih — that optical/network proof is a rig step (see the e2e skill).
i=0; while [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ] && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
if [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ]; then
  echo "FAIL: camera-box not active after switching to no-display mode (capture+emit must keep running)" >&2
  systemctl status camera-box --no-pager >&2 2>/dev/null || true
  exit 1
fi
if ! systemctl show -p Environment --value camera-box 2>/dev/null | grep -q -- 'CAMERA_BOX_NO_DISPLAY=1'; then
  echo "FAIL: camera-box Environment missing CAMERA_BOX_NO_DISPLAY=1 — the unconditional preview would re-grab fb0" >&2
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
# (3b) #440: freshness WARNING (advisory only — never fails the run). Live evidence: cam2's
#      deployed frame-probe was a pre-#431 build, which writes the marker-log CSV only on
#      shutdown, so the #431 emission self-check below FAILED even though the marker was actually
#      running. Auto-deploying the fresh CI artifact is OUT OF SCOPE here (#440) — a clear operator
#      warning, printed BEFORE the #431 check runs, is the deliverable.
BIN_MTIME=\$(stat -c '%y' "$bin" 2>/dev/null || echo unknown)
echo "WARNING: [#440] painter binary $bin build/deploy mtime=\$BIN_MTIME -- if the #431 marker-log-growth check below FAILS, this binary may be a STALE pre-#431 deploy; redeploy the fresh CI artifact: gh run download <latest CI run> -n probe-tools-linux-amd64 && scp frame-probe root@$PAINTER_IP:$bin"
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
# (4b) #723: register this painter in the rig-test LEDGER — the sanctioned registration path, so
#      EVENT mode (or an orphan sweep) can find and kill it BY PID even if the binary is later
#      renamed/copied elsewhere (the #721 incident class). $dur is TEST mode's own intentional
#      measurement-window length (often > the 3600s safety cap) — passed WITH a reason so it is
#      honored verbatim rather than clamped (rig_test_ledger_effective_max_duration).
$(rig_test_ledger_register_remote_cmds "frame-probe --paint-only (rig-mode TEST painter)" '\$PAINTER_PID' cam2 "rig-mode.sh test" "$(rig_test_ledger_effective_max_duration "$dur" "rig-mode TEST measurement window")")
sleep 3
# (5) verify the painter is UP and ACTUALLY PAINTING — presenter-aware (#464). --presenter auto
#     (the default here) may land on the KMS page-flip presenter, which by design NEVER opens
#     /dev/fb0 (see src/presenter_kind.rs::resolve_presenter_kind) — a bare `fuser -s /dev/fb0`
#     reported a healthy, correctly-painting KMS run as FAIL (confirmed live on cam2, #464).
#     scripts/lib/presenter-liveness-check.sh reads the painter's own log to know which presenter
#     actually came up and asserts the matching signal (KMS: the DRM device held + vblank-locked;
#     fbdev: the original /dev/fb0 check, unchanged).
if ! kill -0 "\$PAINTER_PID" 2>/dev/null; then
  echo "FAIL: painter PID \$PAINTER_PID not alive (see /tmp/rig-painter.log on cam2):" >&2
  tail -n 20 /tmp/rig-painter.log >&2 2>/dev/null || true
  exit 1
fi
$(painter_liveness_check_cmds "/tmp/rig-painter.log" "/dev/fb0")
echo "PASS: painter PID \$PAINTER_PID up + painting (dual-QR ${qr}px, ${fps}fps, ${dur}s)"
# (6) #420: verify the QPSK audio marker is ACTUALLY producing audio, not just that the process is
#     alive. The emitter is a CONTINUOUS-FEED writer (silence between markers, tone when due — it
#     never lets the ALSA ring drain), so a healthy run means the PCM backing $audio_dev is OPEN and
#     RUNNING right now. #420's root cause was NOTHING opening the device at all (TEST mode never
#     passed --audio-marker), so this is a REAL kernel-reported signal that catches exactly that
#     failure class — never a stub that always passes. FAIL LOUD + kill the just-verified painter:
#     a silent marker means this whole TEST-mode switch produced an unmeasured, wasted run.
#     (#421: this poll+fail-loud logic now lives in scripts/lib/audio-marker-check.sh, shared with
#     recording-e2e.sh's AV_RESTART_GATE painter — the DRY extraction of this exact block.)
#     (#431: RUNNING alone is satisfied by the continuous-feed silence carrier even if the painter
#     tick stalls and zero markers ever fire — passing $marker_log as the 4th arg below also gates
#     on the marker-log CSV row count actually GROWING, i.e. real emission, not just an open PCM.)
$(audio_marker_check_cmds "$audio_dev" 'kill "$PAINTER_PID" 2>/dev/null || true' "cadence=${audio_cadence} ticks, log=$marker_log" "$marker_log")
REMOTE
}

# painter_stop_remote PIDFILE [DROPIN] -> the REMOTE bash run on cam2 to enter EVENT mode: stop the
# painter cleanly via its PID file (NEVER a 'pkill -f frame-probe' — that matches the remote shell's
# own cmdline and self-kills the cleanup), REMOVE the transient CAMERA_BOX_NO_DISPLAY=1 drop-in TEST
# mode installed (#291/#528), then reload + restart camera-box and verify the service is active AND
# the unconditional preview is restored (camera-box re-grabbed /dev/fb0 to paint the interkom
# return on the monitor).
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
# (2.5) #892: stop + disable the PERMANENT cam2-painter.service if present -- EVENT mode must
#       NEVER (re)start it (the old #440 "restore" call was the live-broadcast QR hazard this
#       ticket fixes), and disabling closes the reboot path too. A box without the unit is
#       unaffected.
$(cam2_painter_service_disable_cmds)
# (2.6) #1075: ALSO disarm the transient cam2-painter-deadman timer. A recording-e2e run that was
#       SIGKILLed leaves that PERIODIC timer armed; it issues a start of the cam2 painter every
#       ~5 min -- and "disable" above does NOT stop such a start -- so without this the QR painter
#       is resurrected ON AIR within ~5 min of switching to EVENT (live 2026-08-15). Reuses the
#       deadman lib's disarm (stop .timer + reset-failed .service). Guarded: a box without the
#       cam2-painter unit is unaffected (same guard shape as the disable above).
$(cam2_painter_deadman_disarm_cmds)
# (3) wait until /dev/fb0 is released by the painter, then RESTORE the unconditional-preview
#     camera-box (#291/#528): remove the transient CAMERA_BOX_NO_DISPLAY=1 drop-in TEST mode
#     installed, reload, and RESTART so the unit's Environment drops the opt-out and camera-box
#     re-grabs /dev/fb0 for the interkom return. (TEST mode no longer STOPS camera-box — it
#     switches it to no-display — so EVENT mode RESTARTS rather than just starts, to drop the
#     override.)
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
# (5) verify the monitor is left CLEAN. WHICH state is correct depends on the box (#868/#892): a
#     box with a dedicated cam2-painter.service carries a PERMANENT no-display drop-in
#     (/etc/systemd/system/camera-box.service.d/cam2-no-display.conf, #863) precisely so camera-box's
#     display thread NEVER grabs /dev/fb0 or DRM master there — the painter service is the sole owner
#     of that monitor, EVEN when it is stopped. Asserting the pre-#863 contract on such a box could
#     only ever FAIL, and the non-zero exit stranded EVENT mode's own burn-clear sweep (a burn left
#     ON then made the next Full-path E2E gate run refuse in its #758 preflight). So branch on the
#     unit's presence — the same condition the #440/#892 stop/disable guard uses, one notion of
#     "painter box" in this script.
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  # #868/#892 painter box: the permanent #863 NO_DISPLAY drop-in is untouched by any of this (a
  # separate, static, provisioning-time config) — camera-box will never re-grab fb0 here regardless.
  # Per #892, cam2-painter must now be INACTIVE (never restored) — so fb0 ends up correctly UNHELD
  # (a blank monitor, not a QR) after EVENT mode. What must hold: the TRANSIENT drop-in is gone
  # (removed above), camera-box is active (checked in (4)), and cam2-painter is confirmed STOPPED.
  i=0; while [ "\$(systemctl is-active cam2-painter 2>/dev/null)" = "active" ] && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
  if [ "\$(systemctl is-active cam2-painter 2>/dev/null)" = "active" ]; then
    echo "FAIL: #892 painter box but cam2-painter is still active — EVENT mode must leave it stopped, never painting the live broadcast" >&2
    systemctl status cam2-painter --no-pager >&2 2>/dev/null || true
    exit 1
  fi
  echo "PASS: #892 transient painter stopped, camera-box active, permanent cam2-painter stopped+disabled (no QR can return, including across a reboot)"
else
  #     The EFFECTIVE Environment no longer carries CAMERA_BOX_NO_DISPLAY=1 (same resolved-check
  #     shape TEST mode uses — 'systemctl show', NOT 'systemctl cat', so a silently-failed drop-in
  #     removal can't false-pass on the base unit) AND camera-box re-grabbed /dev/fb0.
  if systemctl show -p Environment --value camera-box 2>/dev/null | grep -q -- 'CAMERA_BOX_NO_DISPLAY=1'; then
    echo "FAIL: camera-box Environment still carries CAMERA_BOX_NO_DISPLAY=1 — interkom monitor not restored" >&2
    exit 1
  fi
  i=0; while ! fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
  if ! fuser -s /dev/fb0 2>/dev/null; then
    echo "FAIL: camera-box active but /dev/fb0 not held — the unconditional preview is not painting the interkom return" >&2
    exit 1
  fi
  echo "PASS: painter stopped, camera-box active + unconditional preview restored (holding /dev/fb0)"
fi
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
STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-$RIG_SOURCE_STRIH_SOURCE}" # strih program input for the
                                                          # SOURCE camera (#246 burn target). #1135:
                                                          # DERIVED off the resolved source box
                                                          # (RIG_SOURCE_STRIH_SOURCE = 'NDI camN' via
                                                          # camera_strih_route), not the literal
                                                          # 'NDI cam1' — an explicit override wins.
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}" # stream program input (#246 burn target)
# #985: stream's PRODUCTION program scene -- the calibrated A/V-align target TEST mode must be
# PARKED on when it returns (never left on the measurement-only PHASE2-PROBE scene). Same
# convention + default scripts/recording-e2e.sh:1291 already uses for this exact box/scene.
STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-PRO}"
# #462 (EPIC #466 Topology v2): imag-nb — the new 60fps low-latency IMAG cutter. Its scene->camera
# mapping is the Phase 1 1:1 pin (setup-imag.sh, #458): 'NDI CAM1'..'NDI CAM6' -> 'CAMx (usb)'
# 1:1, so cam1 (the SOURCE camera that films cam2's monitor) rides 'NDI CAM1' / scene 'Cam 1'.
#
# #832: IMAG_IP is now DERIVED from scripts/imag-host.sh — the ONE declared imag host (mirrors
# camera-set.sh's CAMERA_ACTIVE_SET design, #827) — instead of an independent literal here.
# Swapping the rig's imag role (incumbent .182 <-> replacement .187) is a one-line change in that
# ONE file (or IMAG_HOST_ACTIVE=incumbent for a one-off run), never a hunt through this script.
# shellcheck source=scripts/imag-host.sh
. "$RIG_MODE_DIR/imag-host.sh"
IMAG_PROG_SOURCE="${IMAG_PROG_SOURCE:-$RIG_SOURCE_IMAG_SOURCE}" # imag input showing the SOURCE camera
                                                          # (#462 burn target). #1135: DERIVED off the
                                                          # resolved source box (imag_source_for_camera
                                                          # -> 'NDI CAMN'), not the literal 'NDI CAM1'.
IMAG_PROG_SCENE="${IMAG_PROG_SCENE:-$RIG_SOURCE_IMAG_SCENE}" # imag scene showing the SOURCE camera —
                                                          # routed to PROGRAM in TEST mode. #1135:
                                                          # DERIVED (imag_scene_for_camera -> 'Cam N'),
                                                          # not the literal 'Cam 1'.
OBS_WS_PASSWORD="${OBS_WS_PASSWORD:-}"

# issue 1171: the per-switch imag offline-leg flag. Defined at top level so every consumer
# (toggle_burn / set_imag_test_program / event_mode_assert) can read it under this file's `set -u`
# even if resolve_imag_offline_leg was never called (e.g. a direct unit-test of one consumer).
# resolve_imag_offline_leg (below) RE-computes both from the SAME issue-1013 offline-ack mechanism
# recording-e2e.sh's [0/8] uses. IMAG_OFFLINE_ACKED=1 means imag is operator-acked offline AND
# genuinely unreachable -> every imag OBS leg SKIPs (loud named note) instead of aborting the switch.
IMAG_OFFLINE_ACKED="${IMAG_OFFLINE_ACKED:-0}"
IMAG_OFFLINE_ACK_REASON="${IMAG_OFFLINE_ACK_REASON:-}"

# #901: whole-chain TEST-mode verification constants (issue 901). STREAM_USER/STREAM_PW mirror
# recording-e2e.sh's own defaults exactly (same box, same creds) — needed here because
# verify_measurement_audio_arrives (below) drives a plain-ssh PowerShell call (win_ssh_run) on
# stream to run ffmpeg volumedetect on a short probe recording, the SAME mechanism recording-
# e2e.sh's [4b2/8] preflight already uses for the identical measurement.
STREAM_USER="${STREAM_USER:-newlevel}"
STREAM_PW="${STREAM_PW:-newlevel}"
MBC_INPUT_NAME="${MBC_INPUT_NAME:-mbc}"                 # stream OBS input carrying the Dante mic
# AUDIO_CHAIN_PROBE_SECS: a SHORT probe (default 5s) — rig-mode.sh test is a quick switch, not a
# full preflight (recording-e2e.sh's own [4b2/8] uses 15s). AUDIO_CHAIN_DEAD_DB/_WARN_DB feed
# audio_preflight_tier (scripts/lib/audio-presence-preflight.sh): below DEAD = hard FAIL (a
# genuinely dead chain — the original 2026-07-31 "sound card off" incident); DEAD..WARN = report
# only, non-blocking (the current issue-976 known ~26dB degradation must never wedge a TEST
# restore); >= WARN = a clean PASS.
AUDIO_CHAIN_PROBE_SECS="${AUDIO_CHAIN_PROBE_SECS:-5}"
AUDIO_CHAIN_DEAD_DB="${AUDIO_CHAIN_DEAD_DB:--80}"
AUDIO_CHAIN_WARN_DB="${AUDIO_CHAIN_WARN_DB:--60}"
# OBS StopRecord returns the recording path BEFORE the MP4's moov atom is finalized (live
# evidence 2026-08-04: the immediate ffmpeg read failed "moov atom not found"; the IDENTICAL
# file parsed fine shortly after). The volumedetect+parse below therefore retries, bounded:
AUDIO_CHAIN_PARSE_RETRIES="${AUDIO_CHAIN_PARSE_RETRIES:-10}"
AUDIO_CHAIN_PARSE_RETRY_DELAY_S="${AUDIO_CHAIN_PARSE_RETRY_DELAY_S:-5}"
# RIG_MODE_AUDIO_CHAIN_ENABLE: an individual opt-out for the measurement-audio-arrival probe
# specifically (the slowest of the #901 chain checks, and the only one needing Windows ssh
# creds) — mirrors recording-e2e.sh's own AUDIO_PREFLIGHT_ENABLE convention for the identical
# class of preflight. The other #901 checks (optical, mbc transport, program-scene assert,
# rendered-input burn) are cheap WS-only calls and are not individually gateable.
RIG_MODE_AUDIO_CHAIN_ENABLE="${RIG_MODE_AUDIO_CHAIN_ENABLE:-1}"

# obs_burn_targets -> the host=ip=source burn triples, one per line "ip|source|box".
obs_burn_targets() {
  printf '%s|%s|%s\n' "$STRIH_IP" "$STRIH_PROG_SOURCE" strih
  printf '%s|%s|%s\n' "$STREAM_IP" "$STREAM_PROG_SOURCE" stream
  printf '%s|%s|%s\n' "$IMAG_IP" "$IMAG_PROG_SOURCE" imag
}

# stray_recording_targets -> the "ip|box" pairs to guard for a stray OBS recording (#524): strih +
# stream ONLY — the two broadcast boxes that have an OBS recording output in this topology. imag-nb
# is excluded (no recording output there, so nothing to guard).
stray_recording_targets() {
  printf '%s|%s\n' "$STRIH_IP" strih
  printf '%s|%s\n' "$STREAM_IP" stream
}

# stop_stray_recordings -> pre-event guard (#524): a stray OBS recording left running fills the
# disk (strih's 265.9 GiB / ~11h-to-full runaway) and can crash OBS mid-broadcast — this happened
# TWICE the same event day (a second 18.57 GiB stray, manually stopped). GetRecordStatus ->
# StopRecord-if-active on BOTH boxes over WebSocket (no relaunch, file KEPT), fail-loud on any box,
# loud WARN naming the box + the stray file (emitted by obs_phase2.py itself).
stop_stray_recordings() {
  local here rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  while IFS='|' read -r ip box; do
    [ -n "$ip" ] || continue
    echo "[obs ${box} ${ip}] #524 pre-event guard: stop any stray recording (WebSocket)"
    python3 "$here/obs_phase2.py" record --action guard --host "$ip" --password "$OBS_WS_PASSWORD" \
      2>&1 | sed "s/^/    [${box} stray-rec] /" || rc=$?
  done < <(stray_recording_targets)
  return $rc
}

# warn_imag_genlock_stale -> #531 pre-event NON-BLOCKING alert: is imag-nb's DEPLOYED genlock build
# BEHIND origin/main? The #530 disaster was imag-nb running a STALE genlock build at a live event
# (-> 45fps) because a merged genlock change had never been deployed there and NOTHING alerted. This
# runs `scripts/drift-guard.sh --check-imag` (#531 made it a DYNAMIC box-vs-origin/main compare) and,
# if it reports the box STALE, prints a LOUD warning banner so the operator sees it BEFORE going live.
# ADVISORY ONLY — it NEVER hard-blocks the switch (blocking a live event on a drift check would be far
# worse than the drift; the operator deploys the current build via setup-imag.sh step-12 at a safe
# off-event moment). Same advisory shape as the #440 painter-freshness WARN. drift-guard is run as a
# SUBPROCESS from the repo root (so its own set -e / exit never affect rig-mode, and its CWD-relative
# vendor/README.md resolves); `|| rc=$?` is belt-and-suspenders. The STALE match is a plain bash glob
# (no pipe -> no grep|head SIGPIPE hazard under rig-mode's set -euo pipefail).
warn_imag_genlock_stale() {
  local here out rc=0
  # #531 review: guard the `cd` itself against this file's `set -e` — this function's whole contract
  # is "never fail rig-mode" (see `return 0` below), so an unguarded assignment that aborts the
  # function (and the calling do_test/do_event, and the whole script) on a `cd` failure would defeat
  # that contract before even reaching drift-guard. `|| here=""` neutralizes errexit; an empty $here
  # just makes the drift-guard subprocess call below fail gracefully (captured into $out, no banner,
  # still returns 0) instead of crashing here.
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[#531] pre-event drift check: is imag-nb's DEPLOYED genlock build current with origin/main?"
  # #832: pass the ACTIVE resolved imag host (scripts/imag-host.sh) — drift-guard.sh already
  # supports a host= override, it just was never given the rig's own resolved value, so this
  # check silently always targeted drift-guard's own hardcoded default regardless of which
  # physical box rig-mode.sh itself was driving.
  out="$( cd "$here/.." && bash scripts/drift-guard.sh --check-imag "host=$IMAG_IP" 2>&1 )" || rc=$?
  printf '%s\n' "$out" | sed 's/^/    [imag drift] /'
  # #531 review: log the actual exit code (comprehensive-logging: values, not just a bare pass/fail)
  # instead of capturing it into `rc` and never reading it — 0=OK, 20=DRIFT, 11=UNKNOWN, anything
  # else is the drift-guard subprocess itself failing to even run (e.g. bash/script not found).
  echo "    [imag drift] drift-guard --check-imag exit=${rc}"
  case "$out" in
    *"genlock STALE"*)
      cat >&2 <<'BANNER'

################################################################################
## WARNING [#531]: imag-nb is running a STALE genlock build (BEHIND origin/main).
## The last event ran at 45fps because of EXACTLY this. Deploy the current build
## to imag-nb via `scripts/setup-imag.sh` step-12 at a safe off-event moment
## BEFORE going live. (Advisory — NOT blocking; see the [imag drift] detail above.)
################################################################################

BANNER
      ;;
  esac
  return 0   # advisory: a drift check must NEVER fail rig-mode / block a live event
}

# imag_genlock_gate_verdict CHECK_OUTPUT CHECK_RC -> the #789 TEST-entry HARD-BLOCK decision (owner
# ROZHODNUTE 2026-08-19, comment 5337986800: HARD-BLOCK, gates maximalne striktne, no WARN-and-
# proceed). Reads ONLY the `genlock_build` facet of `drift-guard.sh --check-imag` output — the
# imag-vs-origin/main PIN check, the early-gate-pin doctrine's PRIMARY signal (it catches even a
# uniformly-stale fleet that bare cross-box parity would pass, and it is genlock-scoped so an
# unrelated facet drift never blocks TEST). Prints:
#   PASS               -> the genlock_build facet reports OK (box current with origin/main HEAD)
#   BLOCK <reason>     -> DRIFT (imag STALE), UNKNOWN (SHA unread / git compare failed), the facet
#                         line ABSENT, or the subprocess itself failed (fail-CLOSED — anything short
#                         of a proven-OK genlock_build line REFUSES).
# Pure (no I/O), ALWAYS returns 0 (verdict is on stdout) so tests/rig_mode.rs can source rig-mode.sh
# and exercise every branch (Tier-0, #477). awk field-splitting ignores leading whitespace, so $1 is
# the exact `genlock_build` label and $2 the STATE; the awk DRAINS the whole input (first-match latch
# + END print, NO early `exit`) so printf never takes SIGPIPE under this file's set -euo pipefail —
# the same drain-safe convention genlock_build_drift_report uses (--check-imag emits this facet once).
imag_genlock_gate_verdict() {
  local out="$1" rc="$2" state
  state="$(printf '%s\n' "$out" | awk '$1=="genlock_build" && s==""{s=$2} END{print s}')"
  if [ "$state" = "OK" ]; then
    printf 'PASS\n'
    return 0
  fi
  if [ -z "$state" ]; then
    printf 'BLOCK (imag genlock build state UNKNOWN — drift-guard --check-imag reported no genlock_build facet [exit=%s]; fail-closed)\n' "$rc"
    return 0
  fi
  printf 'BLOCK (imag genlock build %s — not provably current with origin/main [exit=%s]; fail-closed)\n' "$state" "$rc"
  return 0
}

# imag_genlock_gate_offline_ack_action REASON REACHABLE -> "skip" | "proceed" (issue 1171). The pure
# decision for whether the #789 TEST-entry gate is SKIPPED because imag is legitimately acked offline
# (issue 1013 / rig-fleet.txt). Pure (no I/O), ALWAYS returns 0 (verdict on stdout) so tests/rig_mode.rs
# can source rig-mode.sh and exercise every branch Tier-0 (#477) — same seam pattern as
# imag_genlock_gate_verdict; the reachability PROBE (ping) stays in the caller, only the yes/no is here.
#   REASON empty              -> proceed (imag not acked; run the #789 gate normally)
#   REASON set + REACHABLE=1  -> proceed (acked BUT reachable = STALE ack, NOT exempted -> the real
#                                gate runs against the now-reachable imag; mirrors recording-e2e.sh
#                                [0/8]'s stale-ack protection, just falling through instead of a hard fail)
#   REASON set + REACHABLE=0  -> skip    (acked + genuinely unreachable = issue-1013 legit offline;
#                                skip the gate, TEST mode proceeds without the imag leg)
imag_genlock_gate_offline_ack_action() {
  local reason="$1" reachable="${2:-0}"
  if [ -n "$reason" ] && [ "$reachable" != "1" ]; then
    printf 'skip\n'
  else
    printf 'proceed\n'
  fi
}

# resolve_imag_offline_leg -> compute the per-switch imag offline-leg decision ONCE (issue 1171) and
# publish it as the globals IMAG_OFFLINE_ACKED (0/1) + IMAG_OFFLINE_ACK_REASON. do_test/do_event call
# this at their top; every imag OBS leg (toggle_burn, set_imag_test_program, event_mode_assert) then
# reads the flag instead of each re-probing. It computes the effective ack the SAME way as
# require_imag_genlock_current / recording-e2e.sh (an explicit CAMBOX_OFFLINE_ACK env wins; else
# rig-fleet.txt) and DELEGATES the skip/proceed verdict to the already-tested pure
# imag_genlock_gate_offline_ack_action -- so a legitimately-absent (acked + UNREACHABLE) imag is
# skipped, while an acked-but-REACHABLE (stale ack) or not-acked imag runs the leg fail-closed as
# today. The reachability PROBE (ping) is the only I/O and lives here in the caller; the decision is
# the tested pure function. ONE ping per switch keeps every leg's decision consistent.
resolve_imag_offline_leg() {
  local ack_file eff_ack reachable=0
  ack_file="${RIG_FLEET_ACK_FILE:-$RIG_MODE_DIR/../rig-fleet.txt}"
  eff_ack="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$ack_file")"
  IMAG_OFFLINE_ACK_REASON="$(CAMBOX_OFFLINE_ACK="$eff_ack" cambox_offline_ack_reason imag)"
  if [ -n "$IMAG_OFFLINE_ACK_REASON" ] && ping -c1 -W2 "$IMAG_IP" >/dev/null 2>&1; then reachable=1; fi
  if [ "$(imag_genlock_gate_offline_ack_action "$IMAG_OFFLINE_ACK_REASON" "$reachable")" = skip ]; then
    IMAG_OFFLINE_ACKED=1
    echo "[#1171] imag je operator-acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) a nedosiahnutelny z dev1 (${IMAG_IP}) -- vsetky imag OBS legy (burn toggle/sweep, program routing, event-assert) sa preskocia s hlasnou poznamkou (rovnaka vynimka ako recording-e2e.sh [0/8] preflight)."
  else
    IMAG_OFFLINE_ACKED=0
  fi
}

# require_imag_genlock_current -> #789 TEST-entry HARD-BLOCK gate (owner ROZHODNUTE 2026-08-19). Runs
# `drift-guard.sh --check-imag` against the ACTIVE imag host, feeds its output to
# imag_genlock_gate_verdict, and REFUSES to enter TEST mode (exit 30 — a distinct non-clean exit per
# the early-gate-pin doctrine) with a named Slovak operator message unless the imag genlock build is
# provably current. TEST path ONLY: going LIVE at an event must never be blocked by a drift check, so
# the EVENT path keeps the advisory warn_imag_genlock_stale. Guards its own `cd` under this file's
# set -e (an unread host must fail CLOSED via the verdict, not crash here). Same --check-imag
# mechanism as the advisory warn, opposite (fail-closed) contract.
require_imag_genlock_current() {
  local here out rc=0 verdict ack_file eff_ack ack_reason reachable=0 action
  # issue 1171: before fail-closing, honour the issue-1013 offline-ack. Compute the effective ack
  # the SAME way recording-e2e.sh does (an explicit CAMBOX_OFFLINE_ACK env wins; else rig-fleet.txt),
  # locally (no top-level source-time side effect). If imag is acked, probe reachability — an
  # acked-but-REACHABLE imag is a STALE ack and is NOT exempted (it falls through to the real gate).
  ack_file="${RIG_FLEET_ACK_FILE:-$RIG_MODE_DIR/../rig-fleet.txt}"
  eff_ack="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$ack_file")"
  ack_reason="$(CAMBOX_OFFLINE_ACK="$eff_ack" cambox_offline_ack_reason imag)"
  if [ -n "$ack_reason" ] && ping -c1 -W2 "$IMAG_IP" >/dev/null 2>&1; then reachable=1; fi
  action="$(imag_genlock_gate_offline_ack_action "$ack_reason" "$reachable")"
  if [ "$action" = "skip" ]; then
    echo "[#789/#1171] SKIP imag genlock HARD-BLOCK: imag je operator-acknowledged offline (issue 1013: ${ack_reason}) a nedosiahnutelny z dev1 (${IMAG_IP}) — gate sa PRESKAKUJE, TEST rezim pokracuje bez imag legu (rovnaka vynimka ako recording-e2e.sh [0/8] preflight)."
    return 0
  fi
  # issue 1171 [review]: reaching here with a non-empty ack_reason means acked-BUT-REACHABLE = a
  # STALE ack (imag is back but still listed). We do NOT exit (rig-mode's contract is to still GATE a
  # reachable imag — the real drift-guard below enforces), unlike recording-e2e.sh [0/8] which
  # hard-fails; but surface it LOUDLY so the operator removes the ack instead of it persisting
  # unnoticed across TEST runs (reuses cambox_offline_ack_stale_message, without its exit).
  if [ -n "$ack_reason" ]; then
    cambox_offline_ack_stale_message "imag" >&2
  fi
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[#789] TEST-entry HARD-BLOCK: imag-nb's deployed genlock OBS build MUST be current with origin/main (gates maximalne striktne — owner 2026-08-19):"
  out="$( cd "$here/.." && bash scripts/drift-guard.sh --check-imag "host=$IMAG_IP" 2>&1 )" || rc=$?
  printf '%s\n' "$out" | sed 's/^/    [imag genlock] /'
  echo "    [imag genlock] drift-guard --check-imag exit=${rc}"
  verdict="$(imag_genlock_gate_verdict "$out" "$rc")"
  case "$verdict" in
    PASS*)
      echo "    [imag genlock] OK — imag genlock build je aktualny s origin/main; TEST rezim pokracuje."
      return 0
      ;;
  esac
  cat >&2 <<BANNER

################################################################################
## HARD-BLOCK [#789]: imag-nb NIE JE na kanonickom genlock OBS builde rigu.
## ${verdict#BLOCK }
## TEST rezim sa NESPUSTA — meranie na nekonzistentnom imagu je zakazane
## (owner 2026-08-19: gates maximalne striktne, ziadne WARN-and-proceed).
## NAPRAVA: nasad kanonicky fleet build na VSETKY boxy jedinou cestou:
##   scripts/deploy-genlock-fleet.sh --run-id <CI run> --full
## (alebo na samotny imag: scripts/setup-imag.sh step-12), potom znova:
##   scripts/rig-mode.sh test
## Detail vyssie v riadkoch [imag genlock].
################################################################################

BANNER
  exit 30
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
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  while IFS='|' read -r ip src box; do
    [ -n "$ip" ] || continue
    # issue 1171: an operator-acked, unreachable imag (issue 1013) must not abort the whole switch on
    # 'No route to host' -- SKIP its burn leg loudly, exactly as recording-e2e.sh's [0/8] skips the
    # imag leg. A reachable/unacked imag is NOT skipped (falls through, fail-closed as today).
    if [ "$box" = imag ] && [ "${IMAG_OFFLINE_ACKED:-0}" = 1 ]; then
      echo "    [imag burn] SKIP: imag acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) a nedosiahnutelny -- genlock_burn ${action} sa preskakuje"
      continue
    fi
    echo "[obs ${box} ${ip}] genlock_burn ${action} on '${src}' (WebSocket, no relaunch)"
    python3 "$here/obs_burn_filter.py" "$action" --host "$ip" --input "$src" --password "$OBS_WS_PASSWORD" \
      2>&1 | sed "s/^/    [${box} burn] /" || rc=$?
  done < <(obs_burn_targets)
  # #938/#1011: the pinned obs_burn_targets loop above touches ONLY each box's fixed PROGRAM
  # input. The OFF path (EVENT) must additionally clear EVERY ndi_source input a prior TEST /
  # all-cambox / probe run left genlock_burn ON -- an out-of-set input (strih 'NDI cam2'/'NDI cam4'
  # /'NDI cam3', stream 'phase2-probe-src') is invisible to the pinned list and leaks past the
  # switch (the 2026-08-07 pre-broadcast incident; guard class issue 246/844). Route it through the
  # shared exhaustive enumerator (obs_burn_filter.py sweep-off — GetInputList over WS, never a
  # static/CAMERA_ACTIVE_SET-derived list). ON (TEST) stays pinned-only by design.
  if [ "$mode" = "event" ]; then
    local _sbip _sbbox
    while IFS='|' read -r _sbip _ _sbbox; do
      [ -n "$_sbip" ] || continue
      # issue 1171: skip the exhaustive sweep-off on an acked-offline+unreachable imag (issue 1013).
      if [ "$_sbbox" = imag ] && [ "${IMAG_OFFLINE_ACKED:-0}" = 1 ]; then
        echo "    [imag burn-sweep] SKIP: imag acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) a nedosiahnutelny -- sweep-off sa preskakuje"
        continue
      fi
      python3 "$here/obs_burn_filter.py" sweep-off --host "$_sbip" --password "$OBS_WS_PASSWORD" \
        2>&1 | sed "s/^/    [${_sbbox} burn-sweep] /" || rc=$?
    done < <(obs_burn_targets)
  fi
  return $rc
}

# enforce_strih_ndi_mapping -> set + VERIFY the strih NDI-input→camera mapping (#399) over OBS WS.
# The mapping is fixed + Claude-owned (never a user question): NDI cam1→CAM1, cam2→CAM2, cam3→CAM3,
# cam4→CAM4 (the pins in set-ndi-mapping.py; #753 1:1 pivot). It drifts (recurring bug: two inputs
# both on CAM4 → a camera shows twice, another missing), and a hot WS rebind does not survive a
# force-kill relaunch — so rig activation ENFORCES it every time here instead of the
# operator/agent re-doing it by hand. Fail-loud (non-zero) if it cannot make all pins distinct.
# #827 (2026-07-27, binding owner directive): passes CAMERA_ACTIVE_SET (camera-set.sh, sourced
# above) straight through as set-ndi-mapping.py's own --active filter -- ONE declared list, never
# a second hardcoded enumeration here. Re-enable procedure: cam5 back? add "cam5" to
# CAMERA_ACTIVE_SET in scripts/camera-set.sh, rerun the gate.
enforce_strih_ndi_mapping() {
  local here rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs strih ${STRIH_IP}] #399 enforce NDI-input→camera mapping (distinct) over WebSocket (active: ${CAMERA_ACTIVE_SET}):"
  python3 "$here/set-ndi-mapping.py" --host "$STRIH_IP" --password "$OBS_WS_PASSWORD" \
    --active "$CAMERA_ACTIVE_SET" \
    2>&1 | sed 's/^/    [strih ndi-map] /' || rc=$?
  # #901 gap 4: ALSO run a REPORT-ONLY verify sweep across the FULL 7-camera table (not just the
  # currently active subset) — live evidence: an INACTIVE camera's mangled NDI pin ('NDI cam5'
  # ndi_source_name mangled to 'h') went uncaught because only the active subset was ever
  # checked. Never hard-fails: an unused/offline camera's stale label carries no operational
  # risk, and hard-failing here would wedge every TEST restore over cosmetic drift on hardware
  # nobody is using right now (same "don't gate on something that doesn't matter operationally"
  # principle as the audio-chain tiering below).
  echo "[obs strih ${STRIH_IP}] #901 report-only full-table drift sweep (ALL 7 known cameras, active or not):"
  python3 "$here/set-ndi-mapping.py" --host "$STRIH_IP" --password "$OBS_WS_PASSWORD" \
    --active "cam1 cam2 cam3 cam4 cam5 cam6 cam7" --verify-only \
    2>&1 | sed 's/^/    [strih ndi-map full-table] /' || true
  return $rc
}

# set_imag_test_program -> route imag-nb's PROGRAM to the scene showing the SOURCE camera (#462,
# EPIC #466; #1135: the source is DERIVED via camera_source_box — IMAG_PROG_SCENE/IMAG_PROG_SOURCE
# already carry the resolved box, not a hard-pinned cam1) — the same camera whose feed also proves
# cam→imag zero-loss (the source films cam2's dual-QR monitor).
# TEST-mode ONLY (EVENT mode does not touch imag's scene, mirroring strih/stream — rig-mode never
# scene-switches those either). Reuses obs_phase2.py's `switch` action (SetCurrentProgramScene +
# its shared non-black self-check, #163/#111) — the SAME lightweight mechanism the all-cambox
# sweep uses — so a dead/misconfigured/not-yet-seeded imag scene fails LOUD here (never a silent
# black recording later in recording-e2e.sh).
set_imag_test_program() {
  local here rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  # issue 1171: an operator-acked, unreachable imag (issue 1013) must not abort do_test on 'No route
  # to host' -- SKIP the PROGRAM routing loudly (a reachable/unacked imag runs it, fail-closed as today).
  if [ "${IMAG_OFFLINE_ACKED:-0}" = 1 ]; then
    echo "[obs imag ${IMAG_IP}] SKIP #462 route PROGRAM: imag acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) a nedosiahnutelny -- routing sa preskakuje"
    return 0
  fi
  echo "[obs imag ${IMAG_IP}] #462 route PROGRAM to '${IMAG_PROG_SCENE}' (shows ${RIG_SOURCE_BOX} via '${IMAG_PROG_SOURCE}')"
  python3 "$here/obs_phase2.py" switch --host "$IMAG_IP" --program-scene "$IMAG_PROG_SCENE" \
    --password "$OBS_WS_PASSWORD" 2>&1 | sed 's/^/    [imag program] /' || rc=$?
  return $rc
}

# --- #901: whole-chain verification (issue 901) — TEST mode used to prove only the CAM side
# (painter alive, ALSA RUNNING, marker CSV growing) and report "cam side PASS" as a full-rig
# green. Live evidence closed here: 2026-07-31 the mbc sound card was off and TEST mode still
# PASSED; 2026-08-04 the painter was alive + marker CSV growing while the program was BLACK for a
# whole 150s recording, stream's program scene was left on 'PRO', burns landed on non-rendered
# inputs, and an inactive camera's mangled NDI pin went uncaught. The five functions below close
# each of those gaps; see the design comment on issue 901 for the full root-cause -> approach ->
# rejected-alternative writeup.

# verify_stream_program_phase2 -> #901 gap 2: ASSERT + SET stream's PROGRAM to PHASE2-PROBE (the
# canonical obs_phase2.py probe scene, same SCENE constant it already owns) instead of the old
# printed-but-never-enforced confirm-the-scene prose hint this replaces. Reuses the EXISTING
# `switch` action (SetCurrentProgramScene + its #312 polled non-black self-check) — the identical
# mechanism set_imag_test_program above already uses for imag, applied to stream here.
# #988: every recording-e2e.sh run's cleanup (obs_phase2.py teardown) unbinds the probe input
# between E2E runs -- the NORMAL rest state, not a fault -- so `switch` alone (which assumes the
# input is already wired) guaranteed a false-FAIL here on a perfectly healthy rig. The idempotent
# probe-establishment action now runs first (repairs/creates the probe input, copies the
# certified genlock tuning, self-verifies the baseline, and switches program itself); the
# pre-existing action below then stays as the gap-2 assertion, now a cheap double-check.
verify_stream_program_phase2() {
  local here rc=0 setup_rc=0 switch_rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs stream ${STREAM_IP}] #988 establishing the probe input first (teardown leaves it unbound between E2E runs, otherwise the assert below false-fails a healthy rig)"
  python3 "$here/obs_phase2.py" setup --host "$STREAM_IP" --upstream 'STRIH-SNV (2ME PGM)' \
    --terminal --password "$OBS_WS_PASSWORD" 2>&1 | sed 's/^/    [stream probe setup] /' || setup_rc=$?
  echo "[obs stream ${STREAM_IP}] #901 assert+set PROGRAM = 'PHASE2-PROBE' (was: a printed hint, never enforced)"
  python3 "$here/obs_phase2.py" switch --host "$STREAM_IP" --program-scene "PHASE2-PROBE" \
    --password "$OBS_WS_PASSWORD" 2>&1 | sed 's/^/    [stream program] /' || switch_rc=$?
  # #988 review finding: a bare shared `rc` masked WHICH of the two steps actually failed when
  # both ran (a setup failure almost always makes switch's own non-black assert ALSO fail, so
  # continuing is still correct -- but the final verdict line should name both step outcomes
  # explicitly rather than leaving it to a log-prefix diff).
  if [ "$setup_rc" -ne 0 ] || [ "$switch_rc" -ne 0 ]; then
    echo "[obs stream ${STREAM_IP}] #988 gap-2 FAILED (setup_rc=${setup_rc} switch_rc=${switch_rc}) -- see the [stream probe setup]/[stream program] lines above for which step failed"
    rc=1
  fi
  return $rc
}

# resolve_and_burn_rendered_inputs -> #901 gap 3: for strih + stream, resolve which source is
# ACTUALLY rendered on program right now (obs_phase2.py program-rendered-input) and, if it
# differs from the pinned STRIH_PROG_SOURCE/STREAM_PROG_SOURCE default, ALSO burn it. ADDITIVE
# ONLY — the fixed-target burn toggled earlier in do_test (toggle_burn test, unchanged) stays the
# safety net; every existing burn-state check keyed on the fixed names is untouched. Live
# evidence this closes: strih's program scene rendered 'NDI cam2' while the pinned default burn
# target was 'NDI cam1' — the burn never reached the input actually on screen. Best-effort: a
# resolution failure (an empty/misconfigured scene) WARNS, never aborts the TEST switch.
resolve_and_burn_rendered_inputs() {
  local here ip default_src box resolved rc out
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  while IFS='|' read -r ip default_src box; do
    [ -n "$ip" ] || continue
    rc=0
    resolved="$(python3 "$here/obs_phase2.py" program-rendered-input --host "$ip" \
      --password "$OBS_WS_PASSWORD" 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ]; then
      echo "WARNING: [#901] ${box}: could not resolve the actually-rendered program input (${resolved}) -- the pinned default '${default_src}' burn from earlier still stands." >&2
      continue
    fi
    if [ "$resolved" = "$default_src" ]; then
      echo "[obs ${box} ${ip}] #901 actually-rendered input matches the pinned default ('${resolved}') -- no extra burn needed."
      continue
    fi
    echo "[obs ${box} ${ip}] #901 actually-rendered input ('${resolved}') DIFFERS from the pinned default ('${default_src}') -- burning it too:"
    out="$(python3 "$here/obs_burn_filter.py" add --host "$ip" --input "$resolved" --password "$OBS_WS_PASSWORD" 2>&1)" || {
      echo "WARNING: [#901] ${box}: failed to burn the actually-rendered input '${resolved}': ${out}" >&2
      continue
    }
    echo "$out" | sed "s/^/    [${box} rendered-burn] /"
  done < <(printf '%s|%s|%s\n' "$STRIH_IP" "$STRIH_PROG_SOURCE" strih; printf '%s|%s|%s\n' "$STREAM_IP" "$STREAM_PROG_SOURCE" stream)
  return 0
}

# verify_optical_qr_visible -> #901 gap 1: the optical proof that strih's CURRENT program scene
# is genuinely rendering non-black content — the same polled luma-peak self-check switch()/
# set_imag_test_program above already use, applied WITHOUT switching scenes (read-only, never a
# control op). Live evidence this closes: the cam2 painter PID was alive, its pidfile matched,
# its marker CSV was growing, and ALSA reported RUNNING — yet the actual rendered program was
# BLACK for the whole 150s recording. Process-alive is not QR-on-screen.
verify_optical_qr_visible() {
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs strih ${STRIH_IP}] #901 optical proof: current program scene must render NON-BLACK (camera actually sees content)"
  python3 "$here/obs_phase2.py" assert-program-nonblack --host "$STRIH_IP" \
    --password "$OBS_WS_PASSWORD" --label "#901 chain-verify" 2>&1 | sed 's/^/    [strih optical] /'
}

# verify_mbc_dante_transport -> #901 original item 2: hard-fail loud, BEFORE the measurement-
# audio probe below, when the mbc Dante transport is unambiguously wrong (muted, or bound to
# something other than the Dante Virtual Soundcard device) — a one-call OBS-WS read
# (device_id + mute), no probe recording needed. "A muted/rerouted/renamed input is exactly as
# fatal as a dead card" (issue 901).
verify_mbc_dante_transport() {
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs stream ${STREAM_IP}] #901 mbc Dante-transport check (device bound + unmuted): input='${MBC_INPUT_NAME}'"
  python3 "$here/obs_phase2.py" mbc-input-check --host "$STREAM_IP" --input "$MBC_INPUT_NAME" \
    --password "$OBS_WS_PASSWORD" 2>&1 | sed 's/^/    [stream mbc] /'
}

# verify_measurement_audio_arrives -> #901 original item 1, THE HEADLINE FIX: proves the
# measurement audio genuinely ARRIVES at stream OBS, not just that cam2's emitter is running.
# Reuses the SAME proven mechanism recording-e2e.sh's [4b2/8] preflight uses for the identical
# measurement (a short probe recording + ffmpeg volumedetect, scripts/lib/audio-presence-
# preflight.sh) rather than a live InputVolumeMeters event subscription this code-only pass could
# not calibrate/verify against real hardware (see the issue 901 design comment). Three-tier,
# NON-BLOCKING verdict via audio_preflight_tier (distinct from recording-e2e.sh's own STRICT
# single-threshold gate, which is untouched): "dead" -> hard FAIL (the original 2026-07-31 "sound
# card off" incident this ticket exists to close); "quiet" -> WARN + report, TEST mode proceeds
# (the current, known issue-976 mbc degradation must never wedge a rig-mode restore); "audible"
# -> PASS.
verify_measurement_audio_arrives() {
  if [ "$RIG_MODE_AUDIO_CHAIN_ENABLE" != "1" ]; then
    echo "[#901] measurement-audio-arrival check SKIPPED (RIG_MODE_AUDIO_CHAIN_ENABLE=0)"
    return 0
  fi
  local here path out db tier attempt
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs stream ${STREAM_IP}] #901 measurement-audio ARRIVAL: short (${AUDIO_CHAIN_PROBE_SECS}s) probe recording + ffmpeg volumedetect"
  python3 "$here/obs_phase2.py" record --host "$STREAM_IP" --action start --password "$OBS_WS_PASSWORD" >/dev/null
  sleep "$AUDIO_CHAIN_PROBE_SECS"
  path="$(python3 "$here/obs_phase2.py" record --host "$STREAM_IP" --action stop --password "$OBS_WS_PASSWORD" || true)"
  if [ -z "$path" ]; then
    echo "ERROR: $(audio_preflight_norec_message)" >&2
    exit 1
  fi
  # Bounded retry: the freshly-stopped MP4 may not be finalized yet (moov race, see the
  # AUDIO_CHAIN_PARSE_RETRIES comment in the config block above) -- keep re-reading until the
  # parse yields a number or the attempts run out.
  db=""
  for attempt in $(seq 1 "$AUDIO_CHAIN_PARSE_RETRIES"); do
    out="$(win_ssh_run "$STREAM_USER" "$STREAM_PW" "$STREAM_IP" "$(audio_preflight_volumedetect_ps "$path")" 2>&1 || true)"
    db="$(audio_preflight_parse_max_db "$out" || true)"
    if [ -n "$db" ]; then
      break
    fi
    if [ "$attempt" -lt "$AUDIO_CHAIN_PARSE_RETRIES" ]; then
      echo "    [#901] volumedetect not parseable yet (attempt ${attempt}/${AUDIO_CHAIN_PARSE_RETRIES}; recording likely still finalizing) -- retrying in ${AUDIO_CHAIN_PARSE_RETRY_DELAY_S}s"
      sleep "$AUDIO_CHAIN_PARSE_RETRY_DELAY_S"
    fi
  done
  win_ssh_run "$STREAM_USER" "$STREAM_PW" "$STREAM_IP" "$(audio_preflight_delete_ps "$path")" >/dev/null 2>&1 || true
  if [ -z "$db" ]; then
    echo "ERROR: $(audio_preflight_unreadable_message)" >&2
    exit 1
  fi
  tier="$(audio_preflight_tier "$db" "$AUDIO_CHAIN_DEAD_DB" "$AUDIO_CHAIN_WARN_DB")"
  case "$tier" in
    "dead")
      echo "ERROR: $(audio_preflight_dead_message "$db" "$AUDIO_CHAIN_DEAD_DB")" >&2
      exit 1
      ;;
    "quiet")
      echo "$(audio_preflight_quiet_message "$db" "$AUDIO_CHAIN_WARN_DB")"
      ;;
    "audible")
      echo "    ok: mbc measurement audio AUDIBLE (max_volume ${db} dB >= ${AUDIO_CHAIN_WARN_DB} dB)"
      ;;
    *)
      # Defensive: audio_preflight_tier is a well-tested pure function that only ever returns
      # dead/quiet/audible -- an unmatched value here means the LIB and this script have drifted.
      # Never silently proceed on an unrecognized verdict.
      echo "ERROR: [#901] unexpected audio_preflight_tier result '${tier}' for max_volume ${db} dB -- treating as a hard failure (lib/script drift)." >&2
      exit 1
      ;;
  esac
}

# restore_stream_program_pro -> #985: PARK stream's PROGRAM back on the certified production
# scene ($STREAM_PROG_SCENE, default 'PRO') once the whole-chain checks above are done proving
# the measurement path is alive -- TEST mode is the rig's STANDING state, not a bounded
# measurement window, so leaving PROGRAM on the probe scene indefinitely parks the operator
# monitor on a by-construction A/V-desynced feed (the probe input runs OBS's build-default
# genlock_latency_ms_src while the certified prod input runs its calibrated hold -- live
# evidence: a ~945ms divergence). Reuses the SAME `switch` action (SetCurrentProgramScene + its
# #312 polled non-black self-check) the earlier gap-2 assert already uses -- no new OBS plumbing.
restore_stream_program_pro() {
  local here rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || here=""
  echo "[obs stream ${STREAM_IP}] #985 restore PROGRAM to the production scene '${STREAM_PROG_SCENE}' (never park on the measurement-only probe scene)"
  python3 "$here/obs_phase2.py" switch --host "$STREAM_IP" --program-scene "$STREAM_PROG_SCENE" \
    --password "$OBS_WS_PASSWORD" 2>&1 | sed 's/^/    [stream restore] /' || rc=$?
  return $rc
}

# print_genlock_relaunch_note MODE -> the genlock RELAUNCH step (printed, not run — a GUI OBS
# launch goes via the win-* MCP; #701 proved plain scp/ssh reaches strih/stream, but that doesn't
# drive/verify a GUI app). #257: env-free; the wrapper just verifies the genlock
# render tick is ENABLED (build default). Only needed if OBS is not already running on a box.
print_genlock_relaunch_note() {
  local mode="$1"
  cat <<EOF
# ---- Windows OBS genlock relaunch (only if OBS is not already running; via win-* MCP) ----
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
  scripts/rig-mode.sh event    # stop the QR, restore camera-box's HDMI preview + print the OBS burns-OFF step

The CAM side (cam2 = 10.77.9.62) is applied + verified here over ssh. The OBS burn is toggled DIRECTLY
over OBS WebSocket (scripts/obs_burn_filter.py — no relaunch); the env-free genlock relaunch (no
--mode) is PRINTED to run via the win-* MCP (a GUI relaunch is what the win-* MCP is for; #701
proved plain scp/ssh reaches strih/stream too, but that doesn't drive a GUI app). See the script
header for env overrides.

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

# resolve_marker_device -> stdout: the ALSA device string to use for the QPSK audio marker
# (#725). Fetches a live `aplay -l` from cam2 and resolves via marker_device_resolve_from_aplay
# (scripts/lib/marker-device-resolve.sh); on resolution failure (no device in the live listing
# carries a genuine monitor name) falls back to the pinned AUDIO_MARKER_DEVICE default, loudly
# WARNING that live resolution found nothing — last resort only. A truly dead pin can never end
# in a silent PASS regardless: verify_marker_device_monitor (below) re-checks whichever device
# was actually used, AFTER launch.
resolve_marker_device() {
  local aplay_text resolved
  aplay_text="$(cam_ssh "$(marker_device_aplay_list_cmds)" 2>/dev/null || true)"
  if resolved="$(marker_device_resolve_from_aplay "$aplay_text")"; then
    echo "[#725] resolved marker device from cam2's live aplay -l: $resolved" >&2
    echo "$resolved"
  else
    echo "WARNING: [#725] no HDMI device in cam2's live aplay -l carries a genuine monitor name -- falling back to the pinned default $AUDIO_MARKER_DEVICE (last resort; will still be re-verified after launch)." >&2
    echo "$AUDIO_MARKER_DEVICE"
  fi
}

# verify_marker_device_monitor DEVICE -> exit 0 iff DEVICE still carries a genuine monitor name
# in a FRESH cam2 aplay -l read (#725's post-launch re-check — catches a monitor unplugged in the
# gap between resolution and launch, or a fallback device that has no monitor either). On
# failure: print a FAIL LOUD diagnostic, kill the just-launched painter (a silent dead-pin run
# must never be reported PASS), and return non-zero.
verify_marker_device_monitor() {
  local device="$1" aplay_text
  aplay_text="$(cam_ssh "$(marker_device_aplay_list_cmds)" 2>/dev/null || true)"
  if marker_device_carries_monitor "$aplay_text" "$device"; then
    echo "PASS: [#725] marker device $device confirmed carrying a live monitor (post-launch re-check)"
    return 0
  fi
  echo "FAIL: [#725] marker device $device does NOT carry a live monitor on re-check (dead pin) -- killing the just-launched painter (a silent dead-pin run must never PASS)." >&2
  cam_ssh "PID=\$(cat '$PAINTER_PIDFILE' 2>/dev/null || true); [ -n \"\$PID\" ] && kill \"\$PID\" 2>/dev/null || true" || true
  return 1
}

do_test() {
  require_sshpass
  # #789 (owner 2026-08-19): HARD-BLOCK runs FIRST, before ANY state mutation — a refusal (exit 30)
  # must leave the rig exactly as it was (no heartbeat SET, no burns toggled), so the gate precedes
  # rig_heartbeat_write. It only needs require_sshpass above (its drift-guard --check-imag ssh'es imag).
  echo "[obs] #789 TEST-entry HARD-BLOCK: imag genlock build must be current before any measurement (owner 2026-08-19 — gates maximalne striktne, no WARN-and-proceed):"
  require_imag_genlock_current
  # issue 1171: resolve the imag offline-leg decision ONCE so every imag OBS leg below (toggle_burn,
  # set_imag_test_program) skips a legitimately-absent (acked-offline+unreachable) imag instead of
  # aborting the whole TEST switch on 'No route to host'.
  resolve_imag_offline_leg
  # #281 Fix#3: mark the rig as deliberately in a TEST state so the rig-restore watchdog does not
  # fight an in-progress test (until the marker goes stale — see the lib-source note above).
  rig_heartbeat_write "rig-mode:test" 2>/dev/null \
    && echo "[#281] rig-active heartbeat SET ($(rig_heartbeat_path))" \
    || echo "WARNING: could not set rig-active heartbeat (#281)" >&2
  echo "===== rig-mode TEST (#247/#257/#291) — paint dual-QR vernier on cam2, genlock_burn ON downstream ====="
  echo
  echo "[cam2 ${PAINTER_IP}] #725 resolve the QPSK audio-marker device from cam2's LIVE aplay -l (never trust the hardcoded default):"
  local resolved_marker_device
  resolved_marker_device="$(resolve_marker_device)"
  echo
  echo "[cam2 ${PAINTER_IP}] switch camera-box to no-display (free /dev/fb0, keep capture+emit) -> launch PINNED painter (qr=${QR_SIZE}px)"
  cam_ssh "$(painter_launch_remote "$PAINTER_BIN" "$PAINTER_DURATION_SECS" "$QR_SIZE" "$PAINTER_PIDFILE" "$PAINTER_EXTRA_FLAGS" "$PAINTER_FPS" "$RIG_TEST_DROPIN" "$resolved_marker_device" "$AUDIO_MARKER_CADENCE_TICKS" "$AUDIO_MARKER_LOG")"
  echo
  echo "[cam2 ${PAINTER_IP}] #725 post-launch re-check: does $resolved_marker_device STILL carry a live monitor?"
  verify_marker_device_monitor "$resolved_marker_device"
  echo
  echo "[obs] #257 toggle per-source genlock_burn ON over WebSocket (no relaunch):"
  toggle_burn test
  echo
  echo "[obs] #399 enforce the strih NDI-input→camera mapping (4 distinct):"
  enforce_strih_ndi_mapping
  echo
  echo "[obs] #462 ensure imag-nb's PROGRAM shows the source camera ${RIG_SOURCE_BOX} (EPIC #466 Topology v2 — cam→imag proof):"
  set_imag_test_program
  echo
  echo "[obs] #901 gap 2: assert+set stream's PROGRAM = PHASE2-PROBE (was a printed hint, now enforced):"
  verify_stream_program_phase2
  echo
  echo "[obs] #901 gap 3: resolve + burn the ACTUALLY-RENDERED program inputs (in addition to the pinned defaults):"
  resolve_and_burn_rendered_inputs
  echo
  echo "[obs] #901 gap 1: optical proof — strih's live program scene must render NON-BLACK:"
  verify_optical_qr_visible
  echo
  echo "[obs] #901 item 2: mbc Dante-transport check (device bound + unmuted):"
  verify_mbc_dante_transport
  echo
  echo "[obs] #901 item 1 (the headline fix): measurement-audio ARRIVAL end to end:"
  verify_measurement_audio_arrives
  echo
  echo "[obs] #985: PARK stream's PROGRAM back on the production scene (never leave it on the measurement-only probe scene):"
  restore_stream_program_pro
  echo
  echo "[cam2 ${PAINTER_IP}] #1008/#937 hand STEADY STATE to the PERMANENT supervised cam2-painter.service (durable -- Restart=always, survives crash + reboot -- NOT a disposable 2h nohup):"
  cam_ssh "$(cam2_painter_steady_state_handoff_cmds "$PAINTER_PIDFILE" "$AUDIO_MARKER_LOG")"
  echo
  echo "[cam2 ${PAINTER_IP}] #1075 arm the transient cam2-painter dead-man as a standing 'never dark' net for the handed-off permanent painter (symmetric with EVENT mode's disarm; no-ops on every fire while the painter's own frame-probe is alive):"
  cam_ssh "$(cam2_painter_deadman_arm_cmds)"
  echo
  print_genlock_relaunch_note test
  echo
  echo "ACHIEVED (cam side): cam2 STEADY-STATE painter is now the PERMANENT cam2-painter.service (#1008/#937: durable dual-QR ${QR_SIZE}px + QPSK marker, Restart=always, enabled — survives crash + reboot; the disposable 2h nohup is gone, so TEST mode no longer dies silently)."
  echo "                     cam2 camera-box still ACTIVE in no-display mode (#291: NOT stopped — capture+emit keep running)."
  echo "                     cam2 QPSK audio marker RUNNING+VERIFIED, and the permanent unit's own marker log keeps GROWING (#420/#725/#431: live-resolved device, log ${AUDIO_MARKER_LOG})."
  echo "                     source camera ${RIG_SOURCE_BOX} (${RIG_SOURCE_IP}) left on its DEPLOYED service (already at the 30 fps test rate)."
  echo "ACHIEVED (obs side): genlock_burn=true on strih + stream + imag program inputs (WebSocket, no relaunch)."
  echo "                     imag-nb (${IMAG_IP}) PROGRAM routed to '${IMAG_PROG_SCENE}' (${RIG_SOURCE_BOX}, #462)."
  echo "                     stream PROGRAM briefly proved alive on 'PHASE2-PROBE' (#901), then PARKED back on '${STREAM_PROG_SCENE}' (#985 — the calibrated production hold, never left desynced)."
  echo "ACHIEVED (chain, #901): strih's live program scene confirmed NON-BLACK — the camera genuinely sees content, not just a live process."
  echo "                        mbc Dante transport confirmed bound + unmuted on stream."
  echo "                        measurement-audio arrival checked end to end (verdict printed above — PASS / non-blocking WARN / hard FAIL)."
  echo "RESULT: TEST mode — WHOLE CHAIN verified (cam emission + OBS program/burn/mapping + optical proof + Dante transport + measurement-audio arrival)."
}

# event_mode_ledger_cleanup -> #723: read cam2's rig-test LEDGER and terminate EVERY registered
# entry — SIGTERM, bounded escalation to SIGKILL, and the #660 clean-paint fb0 fallback when a
# KILL was actually needed — then CLEAR the ledger. This is the exhaustive sweep that catches
# anything painter_stop_remote's own name-based `pkill -x frame-probe` might miss (a renamed
# binary — the #721 incident class), and anything ELSE a test/worker registered (a burn, an
# override) that this switch's own steps don't already know to revert. Runs BEFORE the #722
# assert phase so the assert's own paint-process/artifact checks see the CLEANED end state.
# Best-effort per entry (one bad/malformed line must never abort the whole cleanup or do_event
# itself) — logs everything for the operator, never hard-fails.
event_mode_ledger_cleanup() {
  echo "[#723] rig-test ledger cleanup on cam2 (exhaustive sweep, BEFORE the #722 assert phase):"
  local raw
  raw="$(cam_ssh "$(rig_test_ledger_read_remote_cmds)" 2>/dev/null || true)"
  if [ -z "$raw" ]; then
    echo "[#723] ledger empty/absent on cam2 -- nothing to clean."
    return 0
  fi
  local line what pidunit box out
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    what="$(printf '%s' "$line" | jq -r '.what // empty' 2>/dev/null || true)"
    pidunit="$(printf '%s' "$line" | jq -r '.pid_or_unit // empty' 2>/dev/null || true)"
    box="$(printf '%s' "$line" | jq -r '.box // empty' 2>/dev/null || true)"
    if [ -z "$pidunit" ]; then
      echo "WARNING: [#723] skipping malformed ledger line: $line" >&2
      continue
    fi
    echo "[#723] cleaning ledger entry: what=${what:-?} pid_or_unit=$pidunit box=${box:-?}"
    out="$(cam_ssh "$(rig_test_ledger_terminate_entry_cmds "$pidunit" pid)" 2>&1 || true)"
    echo "$out" | sed 's/^/    [ledger cleanup] /'
    if grep -q 'KILL_NEEDED=1' <<<"$out"; then
      echo "[#723] entry required SIGKILL (never got its own graceful teardown) -- running the #660 clean-paint fb0 fallback."
      cam_ssh "$(rig_test_ledger_clean_paint_fallback_cmds)" 2>&1 | sed 's/^/    [ledger cleanup] /' || true
    fi
  done <<< "$raw"
  cam_ssh "$(rig_test_ledger_clear_remote_cmds)" 2>/dev/null || true
  echo "[#723] ledger cleared on cam2."
}

# fleet_ssh HOST CMD -> run CMD on HOST as root, like cam_ssh but for the FULL fleet (cam1-6),
# not just cam2/PAINTER_IP. Used only by the #722 EVENT-mode CONTRACT's fleet-wide sweep.
fleet_ssh() {
  local host="$1" cmd="$2"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 root@"$host" "$cmd"
}

# _bool_or_failclosed VALUE -> "true"/"false" JSON literal text. "True"->true, "False"->false,
# ANYTHING else (unreachable box, RPC error, empty string) -> true — the FAIL-CLOSED default for
# a "is this thing still ON" check (burn/recording/streaming): an unknown state must never read
# as "confirmed off". Mirrors event_assert.py's own fail-closed philosophy for facts this
# function feeds it.
_bool_or_failclosed() {
  case "$1" in
    True) echo true ;;
    False) echo false ;;
    *) echo true ;;
  esac
}

# event_mode_assert -> #722 EVENT-mode CONTRACT: gather all 8 items' facts (the fleet ssh sweep
# above + the existing/new OBS-WS tools: obs_burn_filter.py check, obs_phase2.py
# record/stream-status/latency-check, set-ndi-mapping.py --verify-only,
# qr_screenshot_check.py), hand them to scripts/event_assert.py for the pure decision +
# aggregation, and set two globals for the caller: EVENT_ASSERT_PASS (0=pass/1=fail, matching
# event_assert.py's own exit code) and EVENT_ASSERT_SUMMARY (the printed Slovak summary).
# EVENT_ASSERT_RESULT_JSON is left pointing at the written machine-readable result (consumed by
# the #724 Discord confirmation). Best-effort collection throughout — an unreachable box or a
# failed sub-check is recorded as a FAILING fact (via _bool_or_failclosed / sentinel values),
# never silently omitted, so the aggregate decision always reflects the REAL rig state.
event_mode_assert() {
  # #758 item 5: this used to carry a trailing "/.." -- the ONLY one of this file's `here=`
  # computations that did, resolving one directory too high (the repo root) instead of scripts/,
  # where event_assert.py / obs_phase2.py / obs_burn_filter.py / set-ndi-mapping.py /
  # qr_screenshot_check.py actually live. Matches every OTHER `here=` line in this file (e.g.
  # painter_liveness/OBS-scene helpers above) -- scripts/ itself, no trailing "/..".
  local here; here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local facts_json; facts_json="$(mktemp /tmp/event-assert-facts.XXXXXX.json)"
  EVENT_ASSERT_RESULT_JSON="$(mktemp /tmp/event-assert-result.XXXXXX.json)"
  # #724: the composed Discord confirmation message (Slovak, phone-readable) -- populated by
  # event_assert.py below, sent by the EVENT-mode caller after this function returns (on BOTH
  # outcomes).
  EVENT_ASSERT_DISCORD_MSG_PATH="$(mktemp /tmp/event-assert-discord.XXXXXX.txt)"

  echo "[#722] EVENT-mode CONTRACT -- gathering the 8-item assert-phase facts:"

  # --- item 1 + part of item 5: fleet paint-process / service / stray-unit sweep -------------
  # #827/#1135: the sweep target list is the RESOLVED source box + cam2(painter) + every camera in
  # camera_active_secondary_set() (camera-set.sh) -- the ONE place fleet membership is declared.
  # #1135: the base target is RIG_SOURCE_BOX (=$RIG_SOURCE_IP), not the literal cam1 -- with cam1
  # retired the source is cam3, and camera_active_secondary_set then EXCLUDES it, so the source must
  # be swept via this base entry (the secondary loop no longer carries it). Source + painter +
  # secondaries = exactly the full active set.
  local paint_json="{}" active_json="{}" stray_json="{}"
  local box_ip box ip out pc sa su
  local -a EVENT_ASSERT_TARGETS=("${RIG_SOURCE_BOX}=$RIG_SOURCE_IP" "cam2=$PAINTER_IP")
  for box in $(camera_active_secondary_set); do
    EVENT_ASSERT_TARGETS+=("${box}=$(rig_mode_secondary_ip "$box")")
  done
  for box_ip in "${EVENT_ASSERT_TARGETS[@]}"; do
    box="${box_ip%%=*}"; ip="${box_ip#*=}"
    out="$(fleet_ssh "$ip" "$(event_assert_fleet_check_cmds)" 2>/dev/null || true)"
    pc="$(printf '%s' "$out" | grep -oP 'PAINT_COUNT=\K[0-9]+' || true)"; [ -n "$pc" ] || pc=-1
    sa="$(printf '%s' "$out" | grep -oP 'SERVICE_ACTIVE=\K\S+' || true)"; [ -n "$sa" ] || sa=unreachable
    su="$(printf '%s' "$out" | grep -oP 'STRAY_UNITS=\K\S*' || true)"
    echo "    [$box $ip] paint=$pc service=$sa stray='${su}'"
    paint_json="$(jq --argjson j "$paint_json" --arg k "$box" --argjson v "$pc" -n '$j + {($k): $v}')"
    active_json="$(jq --argjson j "$active_json" --arg k "$box" --argjson v "$([ "$sa" = active ] && echo true || echo false)" -n '$j + {($k): $v}')"
    if [ -n "$su" ]; then
      stray_json="$(jq --argjson j "$stray_json" --arg k "$box" --arg v "$su" -n '$j + {($k): ($v | split(","))}')"
    else
      stray_json="$(jq --argjson j "$stray_json" --arg k "$box" -n '$j + {($k): []}')"
    fi
  done

  # --- item 2: pixel proof (strih's canonical 4 camera scenes, #399) -------------------------
  local qr_json
  qr_json="$(python3 "$here/qr_screenshot_check.py" --host "$STRIH_IP" --password "$OBS_WS_PASSWORD" \
    --scene "Cam 1" --scene "Cam 2" --scene "Cam 3" --scene "Cam 4" 2>/dev/null || true)"
  [ -n "$qr_json" ] || qr_json="{}"
  echo "    [pixel-proof] $qr_json"

  # --- item 3: burns off on every measurement-burn target ------------------------------------
  local burn_json="{}" src label burn_on bv
  while IFS='|' read -r ip src box; do
    [ -n "$ip" ] || continue
    # issue 1171: an acked-offline+unreachable imag (issue 1013) must not FALSELY FAIL the burns-off
    # contract -- a failed check would fail-closed to burn_on=true. SKIP it (not counted); a
    # reachable/unacked imag is still checked + fail-closed as today.
    if [ "$box" = imag ] && [ "${IMAG_OFFLINE_ACKED:-0}" = 1 ]; then
      echo "    [imag burn] SKIP: imag acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) -- not counted in the burns-off contract"
      continue
    fi
    out="$(python3 "$here/obs_burn_filter.py" check --host "$ip" --input "$src" --password "$OBS_WS_PASSWORD" 2>/dev/null || true)"
    burn_on="$(printf '%s' "$out" | grep -oP 'burn_on=\K(True|False)' || true)"
    bv="$(_bool_or_failclosed "$burn_on")"
    label="${box}:${src}"
    burn_json="$(jq --argjson j "$burn_json" --arg k "$label" --argjson v "$bv" -n '$j + {($k): $v}')"
  done < <(obs_burn_targets)
  # #938/#1011: the pinned loop above checks only the 3 program inputs. An out-of-set input
  # (strih 'NDI cam2'/'NDI cam4'/'NDI cam3', stream 'phase2-probe-src') that a prior run left
  # genlock_burn ON passes the pinned check invisibly (the 2026-08-07 pre-broadcast leak; guard
  # class issue 246/844). Merge in the EXHAUSTIVE reality-derived sweep (obs_burn_filter.py
  # sweep-check — GetInputList over WS) so the EVENT contract fails on a burn ANYWHERE, not just on
  # the pinned inputs. Same "box:input" labels — a pinned input already present just re-confirms.
  # FAIL CLOSED: the sweep-check exits SWEEP_ENUM_FAILED(2) when GetInputList itself failed (an
  # un-enumerable OBS) — distinct from finding a burn (1) or clean (0). On enum-failure (or empty/
  # malformed output) inject a FAILING sentinel so the EVENT contract cannot pass on the pinned 3
  # inputs while an un-enumerable box hides an out-of-set burn (the #1011 fail-open the review
  # caught). A successful sweep re-emits the pinned "box:NDI cam1" key too; its raw reading
  # deliberately SUPERSEDES the pinned check's fail-closed value — the sweep genuinely re-read the
  # input, and a real burn still reads true in both, so nothing is masked.
  local _asbip _asbbox sweep_arr sweep_rc
  while IFS='|' read -r _asbip _ _asbbox; do
    [ -n "$_asbip" ] || continue
    # issue 1171: skip the exhaustive sweep-check on an acked-offline+unreachable imag (issue 1013)
    # -- otherwise it fail-closes to the sweep-unreachable sentinel and falsely fails the contract.
    if [ "$_asbbox" = imag ] && [ "${IMAG_OFFLINE_ACKED:-0}" = 1 ]; then
      echo "    [imag burn-sweep] SKIP: imag acknowledged offline (issue 1013: ${IMAG_OFFLINE_ACK_REASON}) -- not swept in the burns-off contract"
      continue
    fi
    if sweep_arr="$(python3 "$here/obs_burn_filter.py" sweep-check --host "$_asbip" --password "$OBS_WS_PASSWORD" 2>/dev/null)"; then sweep_rc=0; else sweep_rc=$?; fi
    if [ "$sweep_rc" -eq 2 ] || [ -z "$sweep_arr" ]; then
      burn_json="$(jq --argjson j "$burn_json" --arg k "${_asbbox}:__sweep_unreachable__" -n '$j + {($k): true}')"
      continue
    fi
    burn_json="$(printf '%s' "$sweep_arr" | jq --argjson j "$burn_json" --arg box "$_asbbox" \
      'reduce .[] as $i ($j; . + {("\($box):\($i.input)"): ($i.burn_on)})' 2>/dev/null \
      || jq --argjson j "$burn_json" --arg k "${_asbbox}:__sweep_parse_error__" -n '$j + {($k): true}')"
  done < <(obs_burn_targets)
  echo "    [burns] $burn_json"

  # --- item 4: no active recordings/streams on strih+stream -----------------------------------
  local rec_json="{}" hb active rv
  for hb in "strih=$STRIH_IP" "stream=$STREAM_IP"; do
    box="${hb%%=*}"; ip="${hb#*=}"
    out="$(python3 "$here/obs_phase2.py" record --action status --host "$ip" --password "$OBS_WS_PASSWORD" 2>/dev/null || true)"
    active="$(printf '%s' "$out" | grep -oP 'active=\K(True|False)' || true)"
    rv="$(_bool_or_failclosed "$active")"
    rec_json="$(jq --argjson j "$rec_json" --arg k "${box}:record" --argjson v "$rv" -n '$j + {($k): $v}')"

    out="$(python3 "$here/obs_phase2.py" stream-status --host "$ip" --password "$OBS_WS_PASSWORD" 2>/dev/null || true)"
    active="$(printf '%s' "$out" | grep -oP 'active=\K(True|False)' || true)"
    rv="$(_bool_or_failclosed "$active")"
    rec_json="$(jq --argjson j "$rec_json" --arg k "${box}:stream" --argjson v "$rv" -n '$j + {($k): $v}')"
  done
  echo "    [recordings] $rec_json"

  # --- item 6: stream PGM latency == calibrated (av-sync-last.json), restore-or-fail ---------
  local calibrated_ms="" current_ms="" latency_out latency_detail=""
  calibrated_ms="$(jq -r '.applied_latency_ms // empty' "$HOME/.camera-box/av-sync-last.json" 2>/dev/null || true)"
  if [ -n "$calibrated_ms" ]; then
    latency_out="$(python3 "$here/obs_phase2.py" latency-check --host "$STREAM_IP" --password "$OBS_WS_PASSWORD" \
      --source "NDI 2ME PGM" --calibrated-ms "$calibrated_ms" 2>/dev/null || true)"
    current_ms="$(printf '%s' "$latency_out" | grep -oP 'final=\K-?[0-9]+' || true)"
    latency_detail="aktualna=${current_ms:-neznama}ms, kalibrovana=${calibrated_ms}ms"
  else
    latency_detail="kalibrovana hodnota nie je znama (av-sync-last.json chyba/necitatelny)"
  fi
  echo "    [latency] $latency_detail"

  # --- item 7: NDI mapping (#399) -------------------------------------------------------------
  local ndi_ok=0 ndi_mismatches="[]"
  python3 "$here/set-ndi-mapping.py" --host "$STRIH_IP" --password "$OBS_WS_PASSWORD" --verify-only >/dev/null 2>&1 || ndi_ok=$?
  [ "$ndi_ok" -eq 0 ] || ndi_mismatches='["drift"]'
  echo "    [ndi-mapping] verify-only exit=$ndi_ok"

  # --- item 8: test artifacts cleared (cam2 pidfile/marker log + dev1 heartbeat) --------------
  local artifacts_remote artifacts_local artifacts_json
  artifacts_remote="$(cam_ssh "$(event_assert_artifacts_check_cmds "$PAINTER_PIDFILE" "$AUDIO_MARKER_LOG")" 2>/dev/null || true)"
  artifacts_local="$(bash -c "$(event_assert_artifacts_check_cmds "$(rig_heartbeat_path)")" 2>/dev/null || true)"
  artifacts_json="$(printf '%s\n%s\n' "$artifacts_remote" "$artifacts_local" | jq -R -s 'split("\n") | map(select(length>0))')"
  echo "    [artifacts] $artifacts_json"

  # --- assemble the facts JSON + decide -------------------------------------------------------
  jq -n \
    --argjson fleet_paint_process_counts "$paint_json" \
    --argjson fleet_service_active "$active_json" \
    --argjson fleet_stray_units "$stray_json" \
    --argjson qr_findings "$qr_json" \
    --argjson burn_states "$burn_json" \
    --argjson recording_states "$rec_json" \
    --arg latency_current_ms "$current_ms" \
    --arg latency_calibrated_ms "$calibrated_ms" \
    --argjson ndi_mismatches "$ndi_mismatches" \
    --argjson artifacts_existing "$artifacts_json" \
    --arg latency_detail "$latency_detail" \
    '{
      fleet_paint_process_counts: $fleet_paint_process_counts,
      fleet_service_active: $fleet_service_active,
      fleet_stray_units: $fleet_stray_units,
      qr_findings: $qr_findings,
      burn_states: $burn_states,
      recording_states: $recording_states,
      latency_current_ms: (if ($latency_current_ms | length) > 0 then ($latency_current_ms | tonumber) else null end),
      latency_calibrated_ms: (if ($latency_calibrated_ms | length) > 0 then ($latency_calibrated_ms | tonumber) else null end),
      ndi_mismatches: $ndi_mismatches,
      artifacts_existing: $artifacts_existing,
      details: {latency_calibrated: $latency_detail}
    }' > "$facts_json"

  echo
  echo "[#722] running the aggregate decision (scripts/event_assert.py):"
  EVENT_ASSERT_PASS=0
  EVENT_ASSERT_SUMMARY="$(python3 "$here/event_assert.py" --facts "$facts_json" \
    --result-out "$EVENT_ASSERT_RESULT_JSON" --discord-out "$EVENT_ASSERT_DISCORD_MSG_PATH")" || EVENT_ASSERT_PASS=$?
  echo "$EVENT_ASSERT_SUMMARY"
  rm -f "$facts_json" 2>/dev/null || true
}

do_event() {
  require_sshpass
  # issue 1171: resolve the imag offline-leg decision ONCE so the EVENT burn-OFF sweep
  # and event_mode_assert's item-3 imag legs skip a legitimately-absent (acked-offline+unreachable)
  # imag instead of aborting the EVENT switch / falsely failing the burns-off contract.
  resolve_imag_offline_leg
  # #281 Fix#3: clear the rig-active heartbeat — we are returning the rig to a clean prod/EVENT
  # state, so the watchdog need no longer treat it as "a test is running".
  rig_heartbeat_clear 2>/dev/null \
    && echo "[#281] rig-active heartbeat CLEARED" \
    || true
  echo "===== rig-mode EVENT (#247/#257/#291) — stop QR, restore clean broadcast, genlock_burn OFF ====="
  echo "[obs] #531 pre-event genlock-staleness check on imag-nb (advisory, never blocks going live):"
  warn_imag_genlock_stale
  echo
  echo "[obs] #524 pre-event guard: stop any stray recording left running on strih/stream (frees disk before going live):"
  stop_stray_recordings
  echo
  echo "[cam2 ${PAINTER_IP}] stop painter (via pidfile) -> remove CAMERA_BOX_NO_DISPLAY drop-in -> restart camera-box -> verify HDMI preview restored"
  # #868: the cam-side restore must NOT be able to strand a measurement burn ON. Under `set -e` a
  # non-zero cam_ssh here aborted do_event outright, so the burn-clear step below never ran and the
  # rig was left with genlock_burn=true on a program input — which then made the NEXT Full-path E2E
  # gate run refuse in its #758 preflight ("genlock_burn is still ON from a prior run"). One stale
  # cam-side assert cost a whole gate cycle. So record the failure, keep going through the OBS
  # burn-clear, and fail loud at the END (folded into the exit status below) — fail-loud is
  # preserved, statelessness of the rig is no longer sacrificed to it.
  _cam_restore_rc=0
  cam_ssh "$(painter_stop_remote "$PAINTER_PIDFILE")" || _cam_restore_rc=$?
  if [ "$_cam_restore_rc" -ne 0 ]; then
    echo "WARNING #868: cam-side restore FAILED (rc=$_cam_restore_rc) — continuing to the OBS burn-clear so no burn is stranded ON; EVENT mode will still exit non-zero." >&2
  fi
  echo
  event_mode_ledger_cleanup
  echo
  # #721 (live 2026-08-16): the painter stop + the ledger sweep above leave every painter dead, but
  # the marker CSV they wrote (/run/rig-qpsk-markers.csv, root-owned on tmpfs) SURVIVES -- and item 8
  # (test artifacts cleared) then FAILED the whole EVENT-mode CONTRACT on that one leftover, rig
  # otherwise clean. Purge the cam2-side item-8 artifacts NOW (both painters are already stopped +
  # disabled, so nothing re-creates the CSV), via the sourced helper -- never editing an anchored
  # line. Best-effort: a purge failure must never abort the EVENT flow; item 8 still flags anything
  # that genuinely survives.
  echo "[#721] purge leftover cam2 test artifacts (marker CSV + pidfile) so the item-8 assert can pass on a clean rig:"
  cam_ssh "$(event_artifact_purge_cmds "$PAINTER_PIDFILE" "$AUDIO_MARKER_LOG")" 2>/dev/null || true
  echo
  echo "[obs] #257 toggle per-source genlock_burn OFF over WebSocket (no relaunch — the #246 guard):"
  toggle_burn event
  echo
  echo "[obs] #399 enforce the strih NDI-input→camera mapping (4 distinct — correct for broadcast too):"
  enforce_strih_ndi_mapping
  echo
  print_genlock_relaunch_note event
  echo
  echo "ACHIEVED (cam side): cam2 painter stopped, camera-box active + unconditional HDMI preview restored."
  echo "ACHIEVED (obs side): genlock_burn=false on strih + stream + imag program inputs (WebSocket, no relaunch)."
  echo
  echo "===== [#722] EVENT-mode CONTRACT — the full machine-checkable assert phase ====="
  event_mode_assert
  echo "=================================================================================="
  echo
  # #724: send the Discord confirmation on BOTH outcomes (pass=confirmation, fail=warning naming
  # what failed) — UNCONDITIONALLY, before the exit below, never gated on EVENT_ASSERT_PASS.
  echo "[#724] posting the EVENT-mode Discord confirmation to the owner's thread:"
  event_mode_discord_confirm_send "$(cat "$EVENT_ASSERT_DISCORD_MSG_PATH" 2>/dev/null || echo "$EVENT_ASSERT_SUMMARY")"
  rm -f "$EVENT_ASSERT_DISCORD_MSG_PATH" "$EVENT_ASSERT_RESULT_JSON" 2>/dev/null || true
  echo
  # #868: fold the recorded cam-side restore failure into the final verdict — deferring it past the
  # burn-clear must never SWALLOW it.
  if [ "$EVENT_ASSERT_PASS" -eq 0 ] && [ "${_cam_restore_rc:-0}" -eq 0 ]; then
    echo "RESULT: EVENT mode — cam side PASS, burns OFF, #722 CONTRACT CONFIRMED clean for broadcast."
    exit 0
  fi
  if [ "${_cam_restore_rc:-0}" -ne 0 ]; then
    echo "RESULT: EVENT mode — cam-side restore FAILED (#868, rc=${_cam_restore_rc}). Burns were still cleared, but the rig is NOT confirmed clean — see the cam-side failure above." >&2
  else
    echo "RESULT: EVENT mode — #722 CONTRACT FAILED. The rig is NOT confirmed clean for broadcast — see the assert summary above." >&2
  fi
  exit 1
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
