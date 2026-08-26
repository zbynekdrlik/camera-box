#!/usr/bin/env bash
# scripts/obs-liveness-watchdog.sh — #391 broadcast-OBS liveness/wedge watchdog.
#
# WHY (#391): stream OBS (10.77.9.204) was hung "(Not Responding)" for ~25 HOURS — obs64 pegged
# ~168% CPU, Responding=False, 16.0% frames-missed-due-to-render-lag — and NOTHING detected it. The
# user found it manually a day later. This watchdog is the Windows-OBS sibling of the #281/#350
# rig-restore-watchdog: it runs on a **dev1 systemd --user timer**, polls BOTH broadcast OBS boxes
# (strih 10.77.9.202, stream 10.77.9.204) over OBS WebSocket `GetStats` (always network-reachable,
# no ssh/MCP needed), runs the strict `obs_watchdog::classify` verdict (via `obs-watchdog-gate`),
# and — once a wedge is CONFIRMED across 2 consecutive passes — fires a Discord alert IMMEDIATELY.
#
# AUTO-RECOVER DESIGN DECISION (documented on #391 — read before changing this): ssh to the Windows
# boxes is DENIED and the win-* MCP is AGENT-ONLY, so a dev1 systemd timer has NO way to force-kill
# or relaunch a wedged obs64.exe process — every Windows action in this repo already goes through
# an agent driving the win-* MCP (launch-obs-genlock.sh itself is a PURE PLANNER that only PRINTS
# the PowerShell program; see scripts/launch-obs-genlock.sh's own header). So this watchdog
# DETECTS + ALERTS autonomously and embeds the exact ready-to-run recovery command in the alert —
# recovery itself is agent-mediated, consistent with 100% of existing Windows-recovery precedent in
# this repo. A Windows-LOCAL unattended self-heal (Task Scheduler) was considered and intentionally
# NOT built here — it would be the first unattended-control mechanism on these boxes and interacts
# with the existing AHK auto-respawn watcher on strih (see .claude/skills/obs-ops "AHK on strih");
# that is a bigger, riskier, precedent-setting change filed separately for the user's decision.
#
# ALL "should we alert?" logic lives in the PURE scripts/lib/obs-watchdog-decision.sh (unit-tested);
# this script only GATHERS the verdict (via obs-liveness-probe.py) and fires the alert.
#
# SHIPS DISABLED — see systemd/obs-liveness-watchdog.README.md. The SUPERVISOR installs + live-
# verifies it (confirm detect -> confirm -> alert with a genuine wedge, and no false positive
# against a healthy box) before turning the timer on. Do NOT enable it as part of merging this PR.
#
# Usage:
#   scripts/obs-liveness-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/obs-liveness-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/obs-liveness-watchdog.sh --help

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "obs-liveness-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
# Topology v2 (#459, EPIC #466, was 60 pre-#459 -- final mixed 60+30 topology, #11): strih is now
# cut-to-stream only at 30fps; the 60fps low-latency IMAG role moved to the new imag-nb box
# (#458/#463). stream is unaffected, still 30fps.
STRIH_TARGET_FPS="${STRIH_TARGET_FPS:-30}"
STREAM_TARGET_FPS="${STREAM_TARGET_FPS:-30}"
WINDOW_S="${OBS_WATCHDOG_WINDOW_S:-4}"
CONFIRM_THRESHOLD="${OBS_WATCHDOG_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${OBS_WATCHDOG_ALERT_THROTTLE_PASSES:-10}"

PROBE_PY="${OBS_LIVENESS_PROBE:-$HERE/obs-liveness-probe.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${OBS_WATCHDOG_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${OBS_WATCHDOG_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${OBS_WATCHDOG_STATE_FILE:-$STATE_DIR/camera-box-obs-watchdog.state}"

log() { printf '%s [obs-liveness-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure (delegates to the python probe + the obs-watchdog-gate binary) ──
# measure_boxes -> sets $VERDICT_LINES to one "<VERDICT> <box>: <reasons>" line per box (the
# obs-watchdog-gate stdout, one line per box regardless of exit code — see its own doc comment).
measure_boxes() {
  VERDICT_LINES="$(python3 "$PROBE_PY" \
    --box "strih=${STRIH_HOST}:${STRIH_TARGET_FPS}" \
    --box "stream=${STREAM_HOST}:${STREAM_TARGET_FPS}" \
    --window-s "$WINDOW_S" 2>>/dev/stderr)" || true
}

# parse_verdict_line <line> -> echoes "box|label|reasons"
parse_verdict_line() {
  local line="$1" label rest box reasons
  label="${line%% *}"
  rest="${line#* }"
  box="${rest%%:*}"
  reasons="${rest#*: }"
  printf '%s|%s|%s\n' "$box" "$label" "$reasons"
}

# ── read / write per-box persisted state ──────────────────────────────────────
# State file format (key=value, one per line), per box <b>:
#   <b>_confirm=<n>       — consecutive-wedge confirmation counter
#   <b>_alert_sig=<str>   — fingerprint of the last-alerted condition (throttle dedup)
#   <b>_alert_passes=<n>  — passes elapsed since the last alert for the same sig
read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  # Read the OTHER keys into memory FIRST, before any file is opened for writing -- so even the
  # mktemp-failure fallback (a direct rewrite of STATE_FILE) can never truncate-before-read and
  # drop them (the previous `tmp=$STATE_FILE` fallback had exactly that latent state-loss bug;
  # fixed here in-line at the issue-732 round integration, mirroring bundle-state-alert-watchdog).
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s
' "$existing"; printf '%s=%s
' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    # mktemp unavailable: `existing` is already captured, so a direct (non-atomic) rewrite is safe.
    { [ -n "$existing" ] && printf '%s
' "$existing"; printf '%s=%s
' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# ── the recovery plan embedded in every alert (agent-driven — see header) ────
# recovery_plan_for BOX LABEL -> the recovery guidance text embedded in the alert. #89: a
# GPU-DEVICE-REMOVED verdict gets DEDICATED, CORRECT guidance — an OBS-only restart
# (launch-obs-genlock.sh --force) typically does NOT clear a DXGI device-removed GPU (a full
# PC reboot is required, see probe::obs_log_audit::ObsLogAudit::diagnosis /
# .claude/skills/obs-ops "GPU wedge on stream box"). Suggesting the generic OBS-restart command
# for THIS cause is actively misleading (wastes an agent's time on a fix that won't work), so it
# is REPLACED for this one verdict; every other verdict keeps the original #391 command.
recovery_plan_for() {
  local box="$1" label="$2"
  if [ "$label" = "GPU-DEVICE-REMOVED" ]; then
    printf '#89: GPU device removed (DXGI TDR/driver-internal-error) on %s — an OBS-only restart typically does NOT clear this; a full PC reboot of the box is required (agent/human-driven, see .claude/skills/obs-ops "GPU wedge on stream box")' "$box"
    return
  fi
  printf 'bash scripts/launch-obs-genlock.sh --box %s --force   # paste the printed PowerShell program into the win-%s MCP Shell' "$box" "$box"
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, threshold=$CONFIRM_THRESHOLD, window=${WINDOW_S}s)"

  measure_boxes
  if [ -z "${VERDICT_LINES:-}" ]; then
    log "ERROR: no verdict lines from obs-liveness-probe.py (probe/binary failure) — nothing to decide this pass"
    return 0
  fi

  local line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    local parsed box label reasons
    parsed="$(parse_verdict_line "$line")"
    box="${parsed%%|*}"
    parsed="${parsed#*|}"
    label="${parsed%%|*}"
    reasons="${parsed#*|}"

    local wedged=0
    [ "$label" != "HEALTHY" ] && wedged=1
    log "$box: verdict=$label reasons='$reasons'"

    local prev_confirm
    prev_confirm="$(read_state_field "${box}_confirm" 0)"
    local decision confirm act
    decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
    confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
    act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
    write_state_field "${box}_confirm" "${confirm:-0}"
    log "$box: confirm=$prev_confirm -> $confirm act=$act"

    [ "${act:-0}" = "1" ] || continue

    local current_sig="${box}:${label}"
    local prior_sig prior_passes throttle_out alert_now new_sig new_passes
    prior_sig="$(read_state_field "${box}_alert_sig" "")"
    prior_passes="$(read_state_field "${box}_alert_passes" 0)"
    throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
    alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
    new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
    new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
    write_state_field "${box}_alert_sig" "$new_sig"
    write_state_field "${box}_alert_passes" "$new_passes"

    if [ "$DRY_RUN" -eq 1 ]; then
      log "[dry-run] WOULD alert: $box CONFIRMED $label (alert_now=$alert_now) — recovery: $(recovery_plan_for "$box" "$label")"
      continue
    fi

    if [ "${alert_now:-0}" = "1" ]; then
      local msg
      msg="🚨 OBS zamrznuté ($REPO_SLUG): OBS na **$box** je **$label**. Dôvod: ${reasons:-none}. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol (nie je to jednorazový výkyv). Rieši Claude automaticky (win-* MCP plán: \`$(recovery_plan_for "$box" "$label")\`), ty nemusíš nič robiť."
      log "ALERT: firing Discord notification for $box ($label)"
      python3 "$NOTIFY" notify --body "$msg" --dedup-key "obs-liveness-$box-$label" >/dev/null 2>&1 \
        || log "ALERT: airuleset.py notify failed (non-fatal)"
    else
      log "ALERT: suppressed by throttle for $box (passes=${prior_passes}/${ALERT_THROTTLE_PASSES} — same condition persists)"
    fi
  done <<EOF
$VERDICT_LINES
EOF

  log "pass end"
}

# Run the pass only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
