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
#   - Acts ONLY on a CLEAR stranded signal: the #353 E2E MARKER (a harness left it behind on an
#     unclean death => restore every observed OBS box regardless of scene), or — fallback — OBS
#     program on a known TEST scene (default "PHASE2-PROBE"; RIG_KNOWN_TEST_SCENES overrides). The
#     marker replaces the fragile scene-name scraping. Cam-box down / stale-probe signals drive the
#     cam restores, but (#396) ONLY alongside one of those test-state signals: an absent/stale
#     heartbeat just means "no E2E ran recently" (normal idle), and idle cams flap down/stale-probe
#     for unrelated reasons — a bare cam signal is observe/log only, never acted on.
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
  # #396: the pkill patterns (and the rm glob) use the same first-char-class trick as
  # PROBE_PATTERNS above — the remote `sh -c '<cmd>'` wrapper's own argv CONTAINS this whole
  # command string, so the previously-undisguised burn-painter pattern made the FIRST pkill -9 -f
  # SIGKILL the wrapper itself before anything ran: ssh returned non-zero on every pass and prod
  # camera-box was never restarted (the "restore always fails" root cause). No literal pkill'd
  # substring may appear ANYWHERE in this function (comments included — the lock test scans the
  # whole block so a literal can never migrate back into the command). The restore's REAL exit
  # status (the systemctl restart) is returned so the caller counts a failed cam restore into the
  # #370 classification (kind=partial, throttled) instead of alerting a positive "AUTO-RECOVERED"
  # for a restore that failed.
  local rc=0
  _ssh_cam "$ip" "pkill -9 -f '[c]amera-box-burn-' 2>/dev/null; \
                  pkill -f '[r]ecording-verdict|[f]rame-probe' 2>/dev/null; \
                  pkill -x camera-box 2>/dev/null; sleep 1; \
                  rm -f /tmp/camera-box-probe /tmp/camera-box-[b]urn-* 2>/dev/null; \
                  systemctl restart camera-box 2>/dev/null" || rc=$?
  if [ "$rc" -eq 0 ]; then
    log "RESTORE cam $name: done"
  else
    log "RESTORE cam $name: ssh restore returned non-zero ($rc)"
  fi
  return "$rc"
}

restore_obs() {
  local name="$1" host
  case "$name" in
    strih) host="$STRIH_HOST" ;;
    stream) host="$STREAM_HOST" ;;
    *) log "restore_obs: unknown obs '$name' — skipping"; return 0 ;;
  esac
  log "RESTORE obs $name ($host): obs_phase2.py teardown (restore prod scene) + burns OFF"
  # #353 (review): preserve the teardown's REAL exit status (return it) so the caller can tell whether
  # the box was actually restored — the marker is cleared only after EVERY OBS restore succeeded.
  local rc=0
  timeout "$OBS_TIMEOUT" python3 "$HERE/obs_phase2.py" teardown \
    --host "$host" --password "$OBS_WS_PASSWORD" >/dev/null 2>&1 || rc=$?
  if [ "$rc" -eq 0 ]; then
    log "RESTORE obs $name: teardown done"
  else
    log "RESTORE obs $name: teardown returned non-zero ($rc)"
  fi
  return "$rc"
}

# ── observe: collect all records in the MAIN shell (#394 — journald-reliable) ─
# Sets RIG_OBS to the newline-separated records of all five probes. The probes MUST run in this
# long-lived shell with their record lines redirected to a temp file — NOT via per-call $(…)
# command substitution: each $(…) forks a short-lived subshell, and journald intermittently LOSES
# the log() stderr lines of those fast-exiting children under the systemd unit (0/5 strih, 2/5
# stream across 5 timer-style passes). A redirection does not fork, so the observation log is
# emitted by the main process and reliably reaches the journal.
collect_observations() {
  local obs_file
  obs_file="$(mktemp "${TMPDIR:-/tmp}/camera-box-rig-watchdog-obs.XXXXXX")"
  probe_cam cam1 "$CAM1_IP" >>"$obs_file"
  probe_cam cam2 "$CAM2_IP" >>"$obs_file"
  probe_cam cam4 "$CAM4_IP" >>"$obs_file"
  probe_obs strih "$STRIH_HOST" >>"$obs_file"
  probe_obs stream "$STREAM_HOST" >>"$obs_file"
  RIG_OBS="$(cat "$obs_file")"
  rm -f "$obs_file"
}

# ── read / write the persisted state (confirm counter + #370 alert throttle) ──
# State file format (key=value, one per line — backward-compat: a bare number is legacy confirm):
#   confirm=<n>       — consecutive-stranded confirmation counter (the #281 2-sample gate)
#   alert_sig=<str>   — fingerprint of the last-alerted condition (for throttle dedup)
#   alert_passes=<n>  — passes elapsed since the last alert for the same sig (throttle counter)
read_state() {
  local confirm=0 alert_sig="" alert_passes=0
  if [ -f "$STATE_FILE" ]; then
    local raw; raw="$(cat "$STATE_FILE" 2>/dev/null)"
    if grep -q '^confirm=' <<<"$raw"; then
      # Current key=value format
      confirm="$(printf '%s\n' "$raw" | sed -n 's/^confirm=//p')"
      alert_sig="$(printf '%s\n' "$raw" | sed -n 's/^alert_sig=//p')"
      alert_passes="$(printf '%s\n' "$raw" | sed -n 's/^alert_passes=//p')"
    else
      # Legacy bare-number format (confirm counter only — upgrade on next write)
      case "$raw" in ''|*[!0-9]*) confirm=0 ;; *) confirm="$raw" ;; esac
    fi
  fi
  case "$confirm" in ''|*[!0-9]*) confirm=0 ;; esac
  case "$alert_passes" in ''|*[!0-9]*) alert_passes=0 ;; esac
  printf 'confirm=%s\nalert_sig=%s\nalert_passes=%s\n' "$confirm" "$alert_sig" "$alert_passes"
}
write_state() {
  # write_state <confirm> <alert_sig> <alert_passes>
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  printf 'confirm=%s\nalert_sig=%s\nalert_passes=%s\n' "$1" "${2:-}" "${3:-0}" \
    > "$STATE_FILE" 2>/dev/null || true
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

  # Gather observations (each helper logs + emits 0/1 record lines) — in THIS shell, never via
  # $(…) subshells (#394, see collect_observations).
  collect_observations
  export RIG_OBS

  # #353 (review): how many of the 2 OBS boxes (strih, stream) were UNREADABLE this pass. probe_obs
  # emits an `obs ...` record only for a readable box, so unreadable = 2 - seen. An unreadable box may
  # hide a stranded scene, so the marker must NOT be cleared while any OBS probe failed (below).
  local obs_seen obs_unreadable
  obs_seen="$(printf '%s\n' "$RIG_OBS" | grep -c '^obs ')"
  obs_unreadable=$(( 2 - obs_seen ))
  [ "$obs_unreadable" -lt 0 ] && obs_unreadable=0

  # Read ALL persisted state: confirm counter + #370 alert throttle state.
  local state_out prev prior_alert_sig prior_alert_passes
  state_out="$(read_state)"
  prev="$(printf '%s\n' "$state_out" | sed -n 's/^confirm=//p')"
  prior_alert_sig="$(printf '%s\n' "$state_out" | sed -n 's/^alert_sig=//p')"
  prior_alert_passes="$(printf '%s\n' "$state_out" | sed -n 's/^alert_passes=//p')"
  export RIG_PREV_CONFIRM="${prev:-0}"

  # Decide (PURE).
  local decision confirm act alert actions reason
  decision="$(rig_restore_decide)"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  alert="$(printf '%s\n' "$decision" | sed -n 's/^alert=//p')"
  actions="$(printf '%s\n' "$decision" | sed -n 's/^actions=//p')"
  reason="$(printf '%s\n' "$decision" | sed -n 's/^reason=//p')"
  log "decision: prev_confirm=$prev -> confirm=$confirm act=$act alert=$alert reason='$reason' actions='$actions'"

  # Persist the new confirm counter early (before executing restores so a crash mid-restore
  # doesn't lose the counter — the next pass will continue from the confirmed count).
  # Alert throttle state is unchanged at this point; it is updated after the restores below.
  write_state "${confirm:-0}" "$prior_alert_sig" "${prior_alert_passes:-0}"

  if [ "${act:-0}" != "1" ]; then
    log "pass end — no action this pass"
    return 0
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD execute restores: $actions  AND alert via $NOTIFY"
    log "pass end — dry-run, nothing executed"
    return 0
  fi

  # Execute the decided restores. Count any OBS teardown that FAILED so the marker is kept (below)
  # until every OBS box is positively restored — and (#396) any CAM restore that FAILED so the
  # classification can never call a failed pass a positive "AUTO-RECOVERED".
  local tok obs_failed=0 cam_failed=0
  for tok in $actions; do
    case "$tok" in
      restore_cam:*) restore_cam "${tok#restore_cam:}" || cam_failed=$(( cam_failed + 1 )) ;;
      restore_obs:*) restore_obs "${tok#restore_obs:}" || obs_failed=$(( obs_failed + 1 )) ;;
      *) log "unknown action token '$tok' — skipping" ;;
    esac
  done

  # #353 (review): clear the E2E marker (left behind by the dead harness) ONLY after a POSITIVE full
  # OBS restore — marker present, we acted, NO OBS box was unreadable this pass, and EVERY OBS
  # teardown succeeded. Otherwise KEEP it so a later pass retries; clearing while an OBS box was
  # unreadable/failed would drop the durable signal and a box on a custom/env scene (outside the
  # fallback list) would never be detected again (the masking bug).
  if rig_marker_should_clear "$marker" "$act" "$obs_unreadable" "$obs_failed"; then
    log "e2e-marker: clearing $(rig_e2e_marker_path) — prod fully restored (obs_unreadable=$obs_unreadable obs_failed=$obs_failed)"
    rig_e2e_marker_clear 2>/dev/null || true
  elif [ "$marker" = "1" ]; then
    log "e2e-marker: KEPT ($(rig_e2e_marker_path)) — not a positive full restore (obs_unreadable=$obs_unreadable obs_failed=$obs_failed); a later pass will retry"
  fi

  # #370: Classify this restore (positive full / partial-KEPT) and apply the alert rate-limit.
  # Names of OBS boxes that were unreadable this pass (missing from obs records → probe failed).
  local obs_unreadable_names=""
  for _box_name in strih stream; do
    if ! grep -q "^obs $_box_name " <<<"$RIG_OBS"; then
      obs_unreadable_names="${obs_unreadable_names:+$obs_unreadable_names }$_box_name"
    fi
  done

  # Pure classification: positive (full restore) or partial (KEPT — some OBS box unreadable/failed,
  # or (#396) a cam restore failed — a failed restore is never a positive "AUTO-RECOVERED").
  local classify_out restore_kind
  classify_out="$(rig_classify_restore "${act:-0}" "$obs_unreadable" "$obs_failed" "$cam_failed")"
  restore_kind="$(printf '%s\n' "$classify_out" | sed -n 's/^kind=//p')"

  # Fingerprint the current alert condition for throttle dedup (kind + unreadable names + failed counts).
  local current_alert_sig="${restore_kind}:${obs_unreadable_names}:${obs_failed}:${cam_failed}"

  # Throttle decision: positive restores always alert; partial/KEPT condition only alerts on first
  # occurrence or signature change, then once per RIG_ALERT_THROTTLE_PASSES passes (default 5).
  local throttle_out alert_now new_alert_sig new_alert_passes
  throttle_out="$(rig_alert_throttle "$restore_kind" "$current_alert_sig" \
                   "$prior_alert_sig" "${prior_alert_passes:-0}")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_alert_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_alert_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  log "alert#370: kind=$restore_kind sig='$current_alert_sig' prior_sig='$prior_alert_sig' prior_passes=${prior_alert_passes:-0} alert_now=$alert_now new_passes=$new_alert_passes"

  # Persist updated state (confirm counter is already persisted early; update alert throttle now).
  write_state "${confirm:-0}" "$new_alert_sig" "${new_alert_passes:-0}"

  # Fire the Discord alert when the decision says to act AND the throttle permits it.
  if [ "${alert:-0}" = "1" ] && [ "${alert_now:-0}" = "1" ]; then
    local msg
    if [ "$restore_kind" = "positive" ]; then
      # Full positive restore — use the existing AUTO-RECOVERED body (every occurrence pings).
      msg="🛟 #281 rig-restore-watchdog AUTO-RECOVERED a stranded rig ($REPO_SLUG). Reason: $reason. Restored: $actions. (No active E2E heartbeat; ${RIG_CONFIRM_THRESHOLD} consecutive confirmations.)"
    else
      # PARTIAL restore — an OBS box unreadable, a teardown failed, or (#396) a cam restore failed.
      # Lower-urgency body; rate-limited to avoid repeated pings while the condition persists. The
      # body states what actually happened — it must never assert a recovery that failed.
      local unreadable_desc="${obs_unreadable_names:-none}"
      msg="⚠️ #281 rig-restore-watchdog: PARTIAL restore — prod NOT fully recovered ($REPO_SLUG). OBS box(es) unreadable: ${unreadable_desc}. Failed teardowns: ${obs_failed}. Failed cam restores: ${cam_failed}. Will retry. Reason: $reason."
    fi
    log "ALERT: firing Discord notification (kind=$restore_kind)"
    python3 "$NOTIFY" notify --body "$msg" >/dev/null 2>&1 \
      || log "ALERT: airuleset.py notify failed (non-fatal) — restore already executed"
  elif [ "${alert:-0}" = "1" ] && [ "${alert_now:-0}" = "0" ]; then
    log "ALERT: suppressed by throttle (kind=$restore_kind passes=${prior_alert_passes:-0}/${RIG_ALERT_THROTTLE_PASSES:-5} — same partial condition persists)"
  fi

  log "pass end — acted on: $actions"
}

# #394: run the pass only when EXECUTED (systemd/CLI). Sourcing the script (tests) only defines
# the functions/config so collect_observations can be unit-tested with stubbed probes.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
