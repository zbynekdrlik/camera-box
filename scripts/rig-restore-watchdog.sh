#!/usr/bin/env bash
# scripts/rig-restore-watchdog.sh — #281 Fix#3: the auto-restore rig safety net.
#
# WHY (#281): dispatched workers die mid-rig-task and leave the rig STRANDED in a TEST state — the
# prod camera-box down while a manual /tmp probe holds the capture device, OBS program on a probe
# scene, burns left on. The supervisor used to catch + recover this MANUALLY, repeatedly. This
# watchdog DETECTS a stranded rig, AUTO-RECOVERS prod, and ALWAYS fires a Discord alert so the
# supervisor/user ALWAYS know it acted — even if no Claude session is alive (it runs on dev1 from a
# systemd --user timer, independent of any session).
#
# It runs ONE pass per invocation (the timer schedules repeats ~every 2 min). The 2-consecutive-
# confirmation logic persists its counter in a state file across invocations.
#
# CONSERVATIVE BY DESIGN (the #266 auto-watchdog was removed for false positives):
#   - A FRESH heartbeat (scripts/lib/rig-heartbeat.sh, written by a live E2E) => NEVER act.
#   - Acts ONLY on a CLEAR stranded signal: cam-box down, stale probe running, the #353 E2E MARKER
#     (a harness left it behind on an unclean death => restore every observed OBS box regardless of
#     scene), or — fallback — OBS program on a known TEST scene (default "PHASE2-PROBE";
#     RIG_KNOWN_TEST_SCENES overrides). The marker replaces the fragile scene-name scraping.
#   - Requires RIG_CONFIRM_THRESHOLD (default 2) CONSECUTIVE confirmations before acting.
# ALL "should we act?" logic lives in the PURE scripts/lib/rig-restore-decision.sh (unit-tested);
# this script only GATHERS observations and EXECUTES the decided restores.
#
# SHIPS DISABLED — see systemd/rig-restore-watchdog.README.md. The SUPERVISOR enables + live-
# verifies it (simulate a stranded state, confirm detect->restore->alert with no false positive)
# before turning it on. Do NOT enable it as part of merging this PR.
#
# Usage:
#   scripts/rig-restore-watchdog.sh            # one pass: observe -> decide -> (act + alert)
#   scripts/rig-restore-watchdog.sh --dry-run  # observe + decide + LOG only; never restore/alert
#   scripts/rig-restore-watchdog.sh --help

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$HERE/lib/rig-heartbeat.sh"
# shellcheck source=scripts/lib/rig-restore-decision.sh
. "$HERE/lib/rig-restore-decision.sh"

# ── config (all env-overridable) ─────────────────────────────────────────────
DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "rig-restore-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# Cam boxes (cam1/cam2/cam4 — cam3 excluded, deferred per project state).
CAM1_IP="${CAM1_IP:-10.77.9.61}"
CAM2_IP="${CAM2_IP:-10.77.9.62}"
CAM4_IP="${CAM4_IP:-10.77.9.64}"
CAM_PW="${CAM_PW:-newlevel}"           # dev-rig root pw (same default convention as recording-e2e.sh)
# OBS program boxes.
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_WS_PASSWORD="${OBS_WS_PASSWORD:-}" # strih may require it; stream is no-auth (empty is fine)

SSH_TIMEOUT="${RIG_WATCHDOG_SSH_TIMEOUT:-8}"
OBS_TIMEOUT="${RIG_WATCHDOG_OBS_TIMEOUT:-15}"
# Stale-probe process patterns (the test artifacts that strand a capture device). The FIRST char
# of each alternative is a single-char class ([c], [r], …) — the classic "pgrep can't match its own
# wrapper shell" trick: the ERE still matches the real process cmdlines, but the literal pattern
# STRING (carried in the ssh `sh -c '<pattern>'` wrapper's own argv) does NOT contain the matched
# substring, so pgrep never returns its own wrapper's PID → no false "stale probe" on a healthy box.
PROBE_PATTERNS="${RIG_WATCHDOG_PROBE_PATTERNS:-[c]amera-box-burn-|[r]ecording-verdict|[f]rame-probe|[/]tmp/camera-box-probe}"

# Confirm-counter state file (persists across timer invocations).
STATE_DIR="${RIG_WATCHDOG_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${RIG_WATCHDOG_STATE_FILE:-$STATE_DIR/camera-box-rig-watchdog.state}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${RIG_WATCHDOG_REPO:-zbynekdrlik/camera-box}"

# Export decision tunables consumed by rig_restore_decide.
export RIG_CONFIRM_THRESHOLD="${RIG_CONFIRM_THRESHOLD:-2}"
# #343/#353: REC-STRIH-TMP dropped from the default — the marker (below), not a scene name, detects a
# stranded stream box now (recording-e2e.sh records the already-active prod scene PRO). PHASE2-PROBE
# stays (still a live obs_phase2 probe scene on strih). RIG_KNOWN_TEST_SCENES is now a fallback only.
export RIG_KNOWN_TEST_SCENES="${RIG_KNOWN_TEST_SCENES:-PHASE2-PROBE}"
export RIG_HEARTBEAT_STALE_SEC="${RIG_HEARTBEAT_STALE_SEC:-600}"

# ── logging (verbose per comprehensive-logging.md) ───────────────────────────
log() { printf '%s [rig-restore-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

_ssh_cam() {
  # _ssh_cam <ip> <remote-cmd> -> run a bounded ssh; stdout = remote stdout, rc preserved.
  local ip="$1"; shift
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no root@"$ip" "$@" 2>/dev/null
}

# ── observe: cam boxes ───────────────────────────────────────────────────────
# probe_cam <name> <ip> -> echo an `RIG_OBS` cam record, or nothing if the box is unreachable
# (an unreachable box is NOT treated as stranded — conservative; could be mid-reboot/deploy).
probe_cam() {
  local name="$1" ip="$2"
  local active down=0 probe=0 reachable=1
  active="$(_ssh_cam "$ip" "systemctl is-active camera-box" )" || reachable=0
  if [ "$reachable" -eq 0 ] && [ -z "$active" ]; then
    log "cam $name ($ip): UNREACHABLE — skipping (not treated as stranded)"
    return 0
  fi
  [ "$active" = "active" ] || down=1
  local probes
  probes="$(_ssh_cam "$ip" "pgrep -f '$PROBE_PATTERNS' | head -1" )"
  [ -n "$probes" ] && probe=1
  log "cam $name ($ip): camera-box=${active:-unknown} down=$down stale_probe=$probe"
  printf 'cam %s down=%s probe=%s\n' "$name" "$down" "$probe"
}

# ── observe: OBS program scene ───────────────────────────────────────────────
# probe_obs <name> <host> -> echo an `RIG_OBS` obs record, or nothing if unreachable / unreadable
# (no record => the decision sees no obs signal => no false action).
probe_obs() {
  local name="$1" host="$2" scene
  scene="$(timeout "$OBS_TIMEOUT" python3 "$HERE/obs_phase2.py" program-scene \
            --host "$host" --password "$OBS_WS_PASSWORD" 2>/dev/null)" || scene=""
  scene="$(printf '%s' "$scene" | tr -d '\r' | head -1)"
  if [ -z "$scene" ]; then
    log "obs $name ($host): program scene UNREADABLE — skipping"
    return 0
  fi
  log "obs $name ($host): program scene='$scene'"
  printf 'obs %s scene=%s\n' "$name" "$scene"
}

# ── restore actions ──────────────────────────────────────────────────────────
restore_cam() {
  local name="$1" ip
  case "$name" in
    cam1) ip="$CAM1_IP" ;;
    cam2) ip="$CAM2_IP" ;;
    cam4) ip="$CAM4_IP" ;;
    *) log "restore_cam: unknown cam '$name' — skipping"; return 0 ;;
  esac
  log "RESTORE cam $name ($ip): kill stale probes + restart prod camera-box"
  _ssh_cam "$ip" "pkill -9 -f 'camera-box-burn-' 2>/dev/null; \
                  pkill -f 'recording-verdict|frame-probe' 2>/dev/null; \
                  pkill -x camera-box 2>/dev/null; sleep 1; \
                  rm -f /tmp/camera-box-probe /tmp/camera-box-burn-* 2>/dev/null; \
                  systemctl restart camera-box 2>/dev/null; true" \
    && log "RESTORE cam $name: done" || log "RESTORE cam $name: ssh restore returned non-zero"
}

restore_obs() {
  local name="$1" host
  case "$name" in
    strih) host="$STRIH_HOST" ;;
    stream) host="$STREAM_HOST" ;;
    *) log "restore_obs: unknown obs '$name' — skipping"; return 0 ;;
  esac
  log "RESTORE obs $name ($host): obs_phase2.py teardown (restore prod scene) + burns OFF"
  timeout "$OBS_TIMEOUT" python3 "$HERE/obs_phase2.py" teardown \
    --host "$host" --password "$OBS_WS_PASSWORD" >/dev/null 2>&1 \
    && log "RESTORE obs $name: teardown done" || log "RESTORE obs $name: teardown returned non-zero"
}

# ── read / write the persisted confirm counter ───────────────────────────────
read_prev_confirm() {
  local v=0
  [ -f "$STATE_FILE" ] && v="$(cat "$STATE_FILE" 2>/dev/null)"
  case "$v" in ''|*[!0-9]*) v=0 ;; esac
  printf '%s\n' "$v"
}
write_confirm() {
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  printf '%s\n' "$1" > "$STATE_FILE" 2>/dev/null || true
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, threshold=$RIG_CONFIRM_THRESHOLD, stale=${RIG_HEARTBEAT_STALE_SEC}s, known_test_scenes='$RIG_KNOWN_TEST_SCENES')"

  # Heartbeat gate: a FRESH heartbeat means a legit E2E is running -> never act.
  local hb_active=0 hb_age
  hb_age="$(rig_heartbeat_age_seconds)" || true
  if rig_heartbeat_active; then
    hb_active=1
    log "heartbeat: FRESH (age=${hb_age}s) — a legit E2E is running, will NOT act"
  else
    log "heartbeat: absent/stale (age=${hb_age}s) — eligible to act if a clear stranded signal is confirmed"
  fi
  export RIG_HB_ACTIVE="$hb_active"

  # #353 E2E MARKER: recording-e2e.sh writes it on entry and removes it ONLY on clean exit, so a
  # leftover marker = a harness entered a test state and died WITHOUT cleaning up. Combined with the
  # absent/stale heartbeat above, that is the durable "rig stranded" signal — robust to env-overridden
  # OBS scene names (replaces the fragile scene-name scraping; no scene-list to keep in sync). When
  # present, the decision restores every observed OBS box regardless of scene.
  local marker=0
  if rig_e2e_marker_present; then
    marker=1
    log "e2e-marker: PRESENT ($(rig_e2e_marker_path)) — a harness entered a test state and did NOT clean up"
  else
    log "e2e-marker: absent — no uncleaned test-state harness"
  fi
  export RIG_E2E_MARKER="$marker"

  # Gather observations (each helper logs + emits 0/1 record lines).
  local obs=""
  obs+="$(probe_cam cam1 "$CAM1_IP")"$'\n'
  obs+="$(probe_cam cam2 "$CAM2_IP")"$'\n'
  obs+="$(probe_cam cam4 "$CAM4_IP")"$'\n'
  obs+="$(probe_obs strih "$STRIH_HOST")"$'\n'
  obs+="$(probe_obs stream "$STREAM_HOST")"$'\n'
  export RIG_OBS="$obs"

  local prev; prev="$(read_prev_confirm)"
  export RIG_PREV_CONFIRM="$prev"

  # Decide (PURE).
  local decision confirm act alert actions reason
  decision="$(rig_restore_decide)"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  alert="$(printf '%s\n' "$decision" | sed -n 's/^alert=//p')"
  actions="$(printf '%s\n' "$decision" | sed -n 's/^actions=//p')"
  reason="$(printf '%s\n' "$decision" | sed -n 's/^reason=//p')"
  log "decision: prev_confirm=$prev -> confirm=$confirm act=$act alert=$alert reason='$reason' actions='$actions'"

  # Persist the new counter for the next invocation.
  write_confirm "${confirm:-0}"

  if [ "${act:-0}" != "1" ]; then
    log "pass end — no action this pass"
    return 0
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD execute restores: $actions  AND alert via $NOTIFY"
    log "pass end — dry-run, nothing executed"
    return 0
  fi

  # Execute the decided restores.
  local tok
  for tok in $actions; do
    case "$tok" in
      restore_cam:*) restore_cam "${tok#restore_cam:}" ;;
      restore_obs:*) restore_obs "${tok#restore_obs:}" ;;
      *) log "unknown action token '$tok' — skipping" ;;
    esac
  done

  # #353: prod is now restored — CLEAR the E2E marker (left behind by the dead harness) so the next
  # pass does not re-detect the same stale marker and re-act/re-alert every ~2 min. Idempotent: a
  # no-op when no marker was present (e.g. the act was driven purely by a cam down/probe signal).
  if [ "$marker" = "1" ]; then
    log "e2e-marker: clearing $(rig_e2e_marker_path) now that prod is restored"
    rig_e2e_marker_clear 2>/dev/null || true
  fi

  # ALWAYS alert when we act (so the supervisor/user ALWAYS know).
  if [ "${alert:-0}" = "1" ]; then
    local msg
    msg="🛟 #281 rig-restore-watchdog AUTO-RECOVERED a stranded rig ($REPO_SLUG). Reason: $reason. Restored: $actions. (No active E2E heartbeat; ${RIG_CONFIRM_THRESHOLD} consecutive confirmations.)"
    log "ALERT: firing Discord notification"
    python3 "$NOTIFY" notify --body "$msg" >/dev/null 2>&1 \
      || log "ALERT: airuleset.py notify failed (non-fatal) — restore already executed"
  fi

  log "pass end — acted on: $actions"
}

main
