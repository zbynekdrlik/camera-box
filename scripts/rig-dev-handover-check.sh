#!/usr/bin/env bash
# scripts/rig-dev-handover-check.sh -- the "development" handover check (extended header below).
set -euo pipefail
#
# #1312: ONE dev1 command the supervisor runs the moment the owner hands the rig back after a
# production ("development"). It verifies the whole rig by RUNNING EACH EXISTING read-only probe
# (never a new probe), bounded + drain-safe, and prints ONE Slovak checklist of what the owner
# forgot to switch back into development state -- ending with a single `zabudol si: …` line the
# supervisor pastes verbatim to the owner. Exits non-zero when any item is FORGOT or UNKNOWN.
#
# WHY (owner directive 14.9.2026): after every production some operator-side state stays in
# production shape (muted measurement mic on `mbc`, painter/burns off, EVENT scenes, drifted NDI
# mapping/pins) and nobody notices until the release E2E burns a cycle (13.9.: muted mic ->
# `[4b2/8]` abort). The check must be MY pre-flight, not the owner's memory.
#
# ARCHITECTURE (thin orchestrator + pure engine): this bash script is I/O ONLY. For each item it
# runs the existing probe in its read-only mode (the dev1 alert-watchdog `--dry-run`s;
# obs_burn_filter sweep-check; set-ndi-mapping --verify-only; latency_pins_verify;
# dantesync-version-gate / camera-box-version-gate; rig-mode-state), captures the probe's merged
# stdout+stderr into $WORKDIR/<name>.out and its exit code into <name>.rc, then hands the whole
# capture set to the PURE decision engine `rig_dev_handover_decision.py`, which parses the verdict
# lines / exit codes, maps every item to OK / FORGOT-BY-OWNER / UNKNOWN + a Slovak line, and
# decides the exit code. All logic + all tests live there (pytest Tier-0); this side is orchestration.
#
# The check REPORTS, it NEVER mutates the rig (no `--fix` -- that is a followup). Every probe is
# bounded with `timeout` so a DOWN box (e.g. cam2) fails safe to UNKNOWN, never a hang and never a
# false OK. Each probe COMMAND + TIMEOUT is env-overridable (RDH_<ITEM>_PROBE / RDH_<ITEM>_TIMEOUT)
# so a stubbed run needs no live box.
#
# Usage:
#   scripts/rig-dev-handover-check.sh            # run every probe, print the Slovak checklist
#   scripts/rig-dev-handover-check.sh --json     # machine-readable JSON
#   scripts/rig-dev-handover-check.sh --keep      # keep the capture WORKDIR (debug)
#   scripts/rig-dev-handover-check.sh --help
#
# Env:
#   OBS_PASSWORD   OBS-WebSocket password for the strih/stream/imag OBS reads (burns/mapping/pins).
#   CAM_PW         cam2 ssh password for the rig-mode / painter probe (default: newlevel).
#   STRIH/STREAM/IMAG_HOST, CAM2_HOST   box addresses (defaults below).
#   RDH_TIMEOUT    default per-probe timeout seconds (default 30); RDH_<ITEM>_TIMEOUT overrides one.
#   RDH_<ITEM>_PROBE   override a single probe's script path (Tier-0 stub seam).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

JSON=0
KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --json) JSON=1 ;;
    --keep) KEEP=1 ;;
    --help | -h)
      sed -n '5,41p' "${BASH_SOURCE[0]}"  # description + Usage + Env (skip shebang/summary/set line)
      exit 0
      ;;
    *)
      echo "rig-dev-handover-check: unknown arg '$1' (try --help)" >&2
      exit 2
      ;;
  esac
  shift
done

# --- box addresses (env-overridable) ------------------------------------------------------------
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
IMAG_HOST="${IMAG_HOST:-10.77.9.182}"
CAM2_HOST="${CAM2_HOST:-10.77.9.62}"
CAM_PW="${CAM_PW:-newlevel}"
IMAG_USER="${IMAG_USER:-newlevel}"
WIN_SSH_USER="${WIN_SSH_USER:-newlevel}"
CAMSET_LIB="${RDH_CAMSET_LIB:-$HERE/camera-set.sh}"
export OBS_PASSWORD="${OBS_PASSWORD:-}"

# --- default per-probe timeouts + script paths (RDH_* stub seams) -------------------------------
RDH_TIMEOUT="${RDH_TIMEOUT:-30}"
# the two version items ssh the WHOLE fleet (many nodes) -> a more generous default timeout
RDH_DANTESYNC_TIMEOUT="${RDH_DANTESYNC_TIMEOUT:-150}"
RDH_CAMBOX_TIMEOUT="${RDH_CAMBOX_TIMEOUT:-120}"
# #1312: avlatency samples the stream mbc meter ~32 s + ssh-reads the cam2 marker log -> generous bound
RDH_AVLATENCY_TIMEOUT="${RDH_AVLATENCY_TIMEOUT:-100}"
RC_MISSING=127

MIC_PROBE="${RDH_MIC_PROBE:-$HERE/measurement-audio-alert-watchdog.sh}"
PAINTER_PROBE="${RDH_PAINTER_PROBE:-$HERE/optical-chain-alert-watchdog.sh}"
CLOCK_PROBE="${RDH_CLOCK_PROBE:-$HERE/dantesync-clock-alert-watchdog.sh}"
OBS_PROBE="${RDH_OBS_PROBE:-$HERE/obs-liveness-watchdog.sh}"
NET_PROBE="${RDH_NET_PROBE:-$HERE/network-reach-alert-watchdog.sh}"
AUDIOLAG_PROBE="${RDH_AUDIOLAG_PROBE:-$HERE/audio-lag-alert-watchdog.sh}"
GENLOCK_PROBE="${RDH_GENLOCK_PROBE:-$HERE/genlock-lock-alert-watchdog.sh}"
BURN_PROBE="${RDH_BURN_PROBE:-$HERE/obs_burn_filter.py}"
MAPPING_PROBE="${RDH_MAPPING_PROBE:-$HERE/set-ndi-mapping.py}"
PINS_PROBE="${RDH_PINS_PROBE:-$HERE/latency_pins_verify.py}"
DANTESYNC_PROBE="${RDH_DANTESYNC_PROBE:-$HERE/dantesync-version-gate.sh}"
CAMBOX_PROBE="${RDH_CAMBOX_PROBE:-$HERE/camera-box-version-gate.sh}"
AVLATENCY_PROBE="${RDH_AVLATENCY_PROBE:-$HERE/measurement-chain-latency.sh}"
RIGMODE_LIB="${RDH_RIGMODE_LIB:-$HERE/lib/rig-mode-state.sh}"
DECIDE="${RDH_DECIDE:-$HERE/rig_dev_handover_decision.py}"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/rig-dev-handover.XXXXXX")"
cleanup() { [ "$KEEP" -eq 1 ] || rm -rf "$WORKDIR"; }
trap cleanup EXIT

_probe_timeout() {
  # per-item override RDH_<ITEM_UPPER>_TIMEOUT, else the global default
  local upper var
  upper="$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]')"
  var="RDH_${upper}_TIMEOUT"
  printf '%s' "${!var:-$RDH_TIMEOUT}"
}

# run_probe <name> <interp> <script> [args...] -- bounded, drain-safe capture. NEVER aborts the run
# on a probe's non-zero exit (a bad verdict is data, not a script failure); a missing probe script
# is recorded as the RC_MISSING sentinel -> UNKNOWN downstream.
run_probe() {
  local name="$1" interp="$2" script="$3"
  shift 3
  local out="$WORKDIR/$name.out" rcf="$WORKDIR/$name.rc" rc=0 t
  t="$(_probe_timeout "$name")"
  if [ ! -e "$script" ]; then
    printf 'MISSING: %s\n' "$script" >"$out"
    printf '%s\n' "$RC_MISSING" >"$rcf"
    return 0
  fi
  timeout "$t" "$interp" "$script" "$@" >"$out" 2>&1 || rc=$?
  printf '%s\n' "$rc" >"$rcf"
  return 0
}

# --- item 1: mic (measurement-audio --dry-run; ssh cam2 + stream OBS-WS) ------------------------
run_probe mic bash "$MIC_PROBE" --dry-run

# --- item 2: mode (rig-mode-state cam2 painter probe -> TEST/EVENT/UNKNOWN) ----------------------
# Reuse the shared lib's remote snippet + pure classifier. A down/unreachable cam2 -> empty
# snapshot -> UNKNOWN (fail-safe). No mutation.
{
  if [ -e "$RIGMODE_LIB" ] && command -v sshpass >/dev/null 2>&1; then
    # shellcheck source=scripts/lib/rig-mode-state.sh
    . "$RIGMODE_LIB"
    _snap=""
    _snip="$(rig_mode_state_probe_remote_snippet)"
    _mt="$(_probe_timeout mode)"
    _snap="$(timeout "$_mt" sshpass -p "$CAM_PW" ssh \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=5 -o BatchMode=no \
      "root@$CAM2_HOST" "$_snip" 2>/dev/null || true)"
    rig_mode_from_painter_snapshot "$_snap" >"$WORKDIR/mode.out" 2>/dev/null || echo UNKNOWN >"$WORKDIR/mode.out"
  else
    echo UNKNOWN >"$WORKDIR/mode.out"
  fi
  echo 0 >"$WORKDIR/mode.rc"
} || { echo UNKNOWN >"$WORKDIR/mode.out"; echo 0 >"$WORKDIR/mode.rc"; }

# --- item 3: painter (optical-chain --dry-run) --------------------------------------------------
run_probe painter bash "$PAINTER_PROBE" --dry-run

# --- item 4: burns (obs_burn_filter sweep-check per box; exit 1=burns ON=dev OK, 0=none) --------
run_probe burns_strih  python3 "$BURN_PROBE" sweep-check --host "$STRIH_HOST"  --password "$OBS_PASSWORD"
run_probe burns_stream python3 "$BURN_PROBE" sweep-check --host "$STREAM_HOST" --password "$OBS_PASSWORD"

# --- item 5: mapping (set-ndi-mapping --verify-only on strih) -----------------------------------
run_probe mapping_strih python3 "$MAPPING_PROBE" --host "$STRIH_HOST" --password "$OBS_PASSWORD" --verify-only

# --- item 6: pins (latency_pins_verify per box) -------------------------------------------------
run_probe pins_strih  python3 "$PINS_PROBE" --box strih  --host "$STRIH_HOST"  --password "$OBS_PASSWORD"
run_probe pins_stream python3 "$PINS_PROBE" --box stream --host "$STREAM_HOST" --password "$OBS_PASSWORD"
run_probe pins_imag   python3 "$PINS_PROBE" --box imag   --host "$IMAG_HOST"   --password "$OBS_PASSWORD"

# --- item 7: clock (dantesync-clock --dry-run) --------------------------------------------------
run_probe clock bash "$CLOCK_PROBE" --dry-run

# --- item 8: obs liveness (obs-liveness --dry-run) ----------------------------------------------
run_probe obs bash "$OBS_PROBE" --dry-run

# --- item 9: net (network-reach --dry-run) ------------------------------------------------------
run_probe net bash "$NET_PROBE" --dry-run

# --- item 10: audio lag (audio-lag --dry-run) ---------------------------------------------------
run_probe audiolag bash "$AUDIOLAG_PROBE" --dry-run

# --- item 11: genlock lock (genlock-lock --dry-run) ---------------------------------------------
run_probe genlock bash "$GENLOCK_PROBE" --dry-run

# --- version items: build the fleet node specs the gates REQUIRE ---------------------------------
# The dantesync/camera-box version gates REFUSE (exit 1) with no `--linux/--win` nodes. Build the
# active-cam `name=root@ip` spec by sourcing the roster lib (camera-set.sh) in a $() SUBSHELL so it
# never pollutes this shell; mirror recording-e2e.sh's [0/8] enumeration. An acked-offline / down
# cam is handled by the gate itself (CAMBOX_OFFLINE_ACK / rig-fleet.txt), never by us.
build_cam_linux_spec() {
  local lib="$1" spec="" cam
  [ -e "$lib" ] || return 0
  # shellcheck source=scripts/camera-set.sh
  . "$lib" 2>/dev/null || return 0
  for cam in ${CAMERA_ACTIVE_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}; do
    if camera_resolve "$cam" 2>/dev/null; then
      spec="$spec $cam=root@${CAMERA_IP}"
    fi
  done
  printf '%s' "${spec# }"
}
CAM_LINUX_SPEC="$(build_cam_linux_spec "$CAMSET_LIB")"

# --- item 12: dantesync version pin (read-only gate; cams + imag-nb + dev1 + OBS boxes) ----------
if [ -n "$CAM_LINUX_SPEC" ]; then
  run_probe dantesync bash "$DANTESYNC_PROBE" \
    --linux "$CAM_LINUX_SPEC imag-nb=${IMAG_USER}@${IMAG_HOST}" \
    --local dev1 \
    --win "strih=${WIN_SSH_USER}@${STRIH_HOST} stream=${WIN_SSH_USER}@${STREAM_HOST}"
else
  printf 'roster lib %s unreadable -- no nodes to gate\n' "$CAMSET_LIB" >"$WORKDIR/dantesync.out"
  printf '%s\n' "$RC_MISSING" >"$WORKDIR/dantesync.rc"
fi

# --- item 13: camera-box uniform build (read-only gate; active cam fleet, relative peer parity) --
if [ -n "$CAM_LINUX_SPEC" ]; then
  run_probe cambox bash "$CAMBOX_PROBE" --linux "$CAM_LINUX_SPEC" --no-main-pin
else
  printf 'roster lib %s unreadable -- no nodes to gate\n' "$CAMSET_LIB" >"$WORKDIR/cambox.out"
  printf '%s\n' "$RC_MISSING" >"$WORKDIR/cambox.rc"
fi

# --- item 14: avlatency (mbc measurement-chain latency vs baseline; read-only paired measurement) --
# measurement-chain-latency.sh reads STREAM_HOST/CAM2_HOST/CAM_PW + OBS_PASSWORD from the env, so export
# them for the child (OBS_PASSWORD is already exported above). It compares the median chain latency to
# ~/.camera-box/measurement-chain-latency-baseline.json (seeded by the supervisor with --baseline after
# a green E2E) and reads UNKNOWN when cam2 is down / no baseline / the painter emit_ts is not wall-clock.
export STREAM_HOST CAM2_HOST CAM_PW
run_probe avlatency bash "$AVLATENCY_PROBE"

# --- item 15: shading (per-cambox bkshading-relay enabled/active + camera online, #1309) ----------
# Read-only per box: systemctl is-enabled/is-active bkshading-relay + curl :8771/api/state for the
# camera-online flag. Emits ONE `verdict=SHADING-*` line per box for the pure decider:
#   SHADING-ON   relay enabled+active AND camera online   (good)
#   SHADING-DEAD relay enabled but NOT active             (forgot: should run, crashed -- #1309)
#   SHADING-OFF  relay disabled/masked (the TEST-mode default per bkshading.md)   (neutral)
#   SHADING-NO-CAMERA relay active but online:false       (neutral)
#   SHADING-UNREACHABLE ssh/curl failed / no /api/state    (neutral -> UNKNOWN, never a false page)
# The per-box detail lives in the capture; a DOWN box fails safe to SHADING-UNREACHABLE (never OK).
probe_shading() {
  local spec="$1" pw="$2" tok label ip en act online verdict body port="${SHADING_RELAY_PORT:-8771}"
  # Bounds are PER BOX (timeout 8 ssh / 6 curl below), not the whole-item RDH_*_TIMEOUT budget --
  # a per-box bound keeps the worst case linear in the small cambox count and never hangs the run.
  [ -n "$spec" ] || { printf 'roster empty -- no camboxes to probe\n'; return 0; }
  command -v sshpass >/dev/null 2>&1 || { printf 'sshpass missing -- cannot probe shading\n'; return 0; }
  for tok in $spec; do
    label="${tok%%=*}"; ip="${tok#*=root@}"
    en=""; act=""; online=""
    en="$(timeout 8 sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=5 "root@$ip" \
      'systemctl is-enabled bkshading-relay 2>/dev/null; echo ---; systemctl is-active bkshading-relay 2>/dev/null' \
      2>/dev/null || true)"
    act="$(printf '%s\n' "$en" | sed -n '/^---$/,$p' | sed '1d' | head -1)"
    en="$(printf '%s\n' "$en" | sed -n '1p')"
    body="$(timeout 6 curl -fsS "http://$ip:$port/api/state" 2>/dev/null || true)"
    online="$(printf '%s' "$body" | grep -o '"online"[[:space:]]*:[[:space:]]*[a-z]*' | grep -o '[a-z]*$' | head -1)"
    case "$en" in
      enabled|static|indirect)
        if [ "$act" = active ]; then
          if [ "$online" = true ]; then verdict=SHADING-ON; else verdict=SHADING-NO-CAMERA; fi
        else
          verdict=SHADING-DEAD
        fi ;;
      disabled|masked) verdict=SHADING-OFF ;;
      *) verdict=SHADING-UNREACHABLE ;;
    esac
    printf '%s (%s): enabled=%s active=%s online=%s -> verdict=%s\n' \
      "$label" "$ip" "${en:-?}" "${act:-?}" "${online:-?}" "$verdict"
  done
  return 0
}
probe_shading "$CAM_LINUX_SPEC" "$CAM_PW" >"$WORKDIR/shading.out" 2>&1 || true
echo 0 >"$WORKDIR/shading.rc"

# --- decide + print ------------------------------------------------------------------------------
json_flag=()
[ "$JSON" -eq 1 ] && json_flag=(--json)
rc=0
python3 "$DECIDE" --work-dir "$WORKDIR" "${json_flag[@]}" || rc=$?
exit "$rc"
