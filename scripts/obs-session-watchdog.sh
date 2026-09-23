#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/obs-liveness-watchdog.sh / scripts/imag-obs-alert-
# watchdog.sh (set -uo pipefail, not -e).
#
# scripts/obs-session-watchdog.sh — #979 dev1-side CONTINUOUS obs64/AHK session-visibility
# watchdog for the WINDOWS genlock OBS boxes, the always-on sibling of #977's per-PR E2E gate
# preflight.
#
# ROSTER (issue 1317 part 2, M4 cut-over): the boxes come from the ONE fleet list
# (scripts/lib/obs-fleet.sh, facet `obs-session` = stream + resolume), overridable byte-compatibly via
# OBS_SESSION_WATCHDOG_BOXES="name|host ...". The session-0 / AHK premise is Windows-only, so each
# box is ALSO class-gated here (windows-genlock only): the Linux production strih-lx at .202 is never
# handed the PowerShell probe (the old literal `strih` arm failed there every pass with "no probe
# output"). The traveling resolume is probed only while home (obs_fleet_poll_now).
#
# WHY (issue 958 + #977): #977's E2E gate only runs on a push -- the rig can degrade BETWEEN CI
# runs (the real incident: obs64 sat in Windows session 0, invisible to the operator, for ~3.5h
# before the user found it manually). This watchdog is the #391/#882 dev1-timer topology applied
# to the SAME session-visibility probe #977/#978 use (scripts/lib/obs-session-visibility.sh,
# reused VERBATIM -- never a second detector) -- polls each Windows OBS box over win_ssh_run
# (scripts/lib/win-ssh-exec.sh, #703) every few minutes and fires ONE deduped Discord alert the
# moment any box goes invisible, embedding the exact win-* MCP recovery command.
#
# Detection-and-alert only, NO auto-recovery -- same "agent-mediated recovery" precedent as
# #391/#882 (a dev1 timer has no win-* MCP session to drive a GUI relaunch itself).
#
# ALL "should we alert?" logic is the SAME pure scripts/lib/obs-watchdog-decision.sh #391 already
# uses (obs_watchdog_confirm / obs_watchdog_alert_throttle) -- no second decision mechanism.
#
# An EMPTY ssh probe (connectivity failure) is treated as "nothing to decide this pass" -- never
# calls the message parser -- mirroring #882's own explicit precedent ("the fleet's own [0/8]
# preflight is the authority for connectivity, not this watchdog"). This is DELIBERATELY the
# OPPOSITE of #977's E2E gate, which fails loud on the identical empty-probe case: a CI gate and a
# best-effort timer have opposite correct defaults for a transient ssh hiccup.
#
# SHIPS DISABLED -- see systemd/obs-session-watchdog.README.md. The SUPERVISOR installs +
# live-verifies it (confirm detect -> confirm -> alert with a genuine invisibility, and no false
# positive against a healthy box) before turning the timer on. Do NOT enable it as part of
# merging this PR.
#
# Usage:
#   scripts/obs-session-watchdog.sh            # one pass: measure -> decide -> alert (each Windows OBS box)
#   scripts/obs-session-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/obs-session-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-session-visibility.sh
. "$HERE/lib/obs-session-visibility.sh"
# shellcheck source=scripts/lib/win-ssh-exec.sh
. "$HERE/lib/win-ssh-exec.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '5,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "obs-session-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
# issue 1317 part 2: the roster is the fleet `obs-session` facet (stream + resolume), `name|host`
# pairs; OBS_SESSION_WATCHDOG_BOXES overrides it byte-compatibly (the fleet <X>_BOXES convention).
BOXES="${OBS_SESSION_WATCHDOG_BOXES:-$(obs_fleet_boxes obs-session)}"
# Per-box host + ssh credential overrides (host default = the roster's host). targets.md: both boxes
# take newlevel/newlevel over Win32-OpenSSH.
STREAM_USER="${STREAM_USER:-newlevel}"
STREAM_PW="${STREAM_PW:-newlevel}"
RESOLUME_USER="${RESOLUME_USER:-newlevel}"
RESOLUME_PW="${RESOLUME_PW:-newlevel}"
CONFIRM_THRESHOLD="${OBS_SESSION_WATCHDOG_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${OBS_SESSION_WATCHDOG_ALERT_THROTTLE_PASSES:-10}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${OBS_SESSION_WATCHDOG_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${OBS_SESSION_WATCHDOG_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A DIFFERENT default state file than #391's own obs-liveness-watchdog.sh -- both scripts key
# per-box state on the SAME box names ("stream", "resolume", ...), so sharing one file would corrupt each
# other's confirm/throttle counters.
STATE_FILE="${OBS_SESSION_WATCHDOG_STATE_FILE:-$STATE_DIR/camera-box-obs-session-watchdog.state}"

log() { printf '%s [obs-session-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── read / write per-box persisted state (SAME key shape as obs-liveness-watchdog.sh) ───────────
# State file format (key=value, one per line), per box <b>:
#   <b>_confirm=<n>       — consecutive-invisibility confirmation counter
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
  local key="$1" val="$2" tmp
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || echo "$STATE_FILE")"
  { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

# ── process ONE box: measure -> decide -> (maybe) alert ─────────────────────────────────────────
process_box() {
  local box="$1" user="$2" pw="$3" host="$4" has_ahk="$5"
  local probe_out
  probe_out="$(win_ssh_run "$user" "$pw" "$host" "$(obs_session_visibility_probe_ps "$has_ahk")" 2>/dev/null || true)"
  if [ -z "$probe_out" ]; then
    log "$box: ERROR: no probe output (ssh/connectivity failure) -- nothing to decide this pass (the fleet's own reachability preflight is the authority for connectivity, not this watchdog)"
    return 0
  fi

  local msg wedged znote
  msg="$(obs_session_visibility_message "$probe_out" "$has_ahk")"
  wedged=0
  [ -n "$msg" ] && wedged=1
  log "$box: probe='$probe_out' wedged=$wedged msg='$msg'"
  # #1295: report-only -- surface any DEAD obs64 handle the probe ignored so an operator can reap
  # it, WITHOUT ever treating it as a wedge (a stale zombie handle is not an operator-visibility
  # fault; the one live instance is visible). Never folds into $wedged / the alert path.
  znote="$(obs_session_visibility_zombie_note "$probe_out")"
  [ -n "$znote" ] && log "$box: $znote"

  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "${box}_confirm" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${box}_confirm" "${confirm:-0}"
  log "$box: confirm=$prev_confirm -> $confirm act=$act"

  # A genuinely VISIBLE pass also clears the throttle signature -- a NEW invisibility episode that
  # happens to reproduce the SAME message text as a previous, already-recovered one must alert
  # fresh, not stay silently throttled by the stale signature (same discipline as #882).
  if [ "$wedged" -eq 0 ]; then
    write_state_field "${box}_alert_sig" ""
    write_state_field "${box}_alert_passes" 0
    return 0
  fi

  [ "${act:-0}" = "1" ] || return 0

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${box}:${msg}"
  prior_sig="$(read_state_field "${box}_alert_sig" "")"
  prior_passes="$(read_state_field "${box}_alert_passes" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "${box}_alert_sig" "$new_sig"
  write_state_field "${box}_alert_passes" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED INVISIBLE ($msg) alert_now=$alert_now"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box"
    python3 "$NOTIFY" notify --body \
      "🚨 #979 obs-session-watchdog: **$box** obs64/AHK is INVISIBLE to the operator ($REPO_SLUG). ${msg}. Confirmed over ${CONFIRM_THRESHOLD} consecutive passes. Recovery (agent-driven, win-* MCP Shell): \`bash scripts/launch-obs-genlock.sh --box $box --force\`" \
      --dedup-key "obs-session-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle for $box (passes=${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# ── roster (issue 1317 part 2) ───────────────────────────────────────────────
# obs_session_box_has_ahk <name> -> 1 when the box runs an NL_STARTUP.ahk AutoHotkey auto-respawn
# watcher whose session the probe must also check (resolume's AHK v2 safe-loop, issue 1295 -- the
# same fact deploy-genlock-fleet.sh's fleet_box_has_ahk / launch-obs-genlock.sh carry), else 0.
obs_session_box_has_ahk() {
  case "${1:-}" in
    resolume) printf '1' ;;
    *) printf '0' ;;
  esac
}

# obs_session_targets -> one `name host has_ahk` line per box to probe THIS pass, in roster order.
# A box is probed only when BOTH hold:
#   * its fleet class is windows-genlock -- the session-0/AHK probe is PowerShell over win_ssh_run,
#     so a Linux box (strih-lx) or a name the fleet list does not know is SKIPPED (logged), even when
#     an OBS_SESSION_WATCHDOG_BOXES override names it (defense in depth beyond the facet policy);
#   * obs_fleet_poll_now says so -- a traveling box (resolume) only while home, never a false
#     "invisible" verdict on a box that is simply away.
# STREAM_HOST / RESOLUME_HOST repoint a box; otherwise the roster's host is dialled.
obs_session_targets() {
  local pair name host class
  for pair in $BOXES; do
    name="${pair%%|*}"
    host="${pair##*|}"
    class="$(obs_fleet_class "$name" 2>/dev/null || true)"
    if [ "$class" != "windows-genlock" ]; then
      log "$name: skipped -- fleet class '${class:-unknown}' is not windows-genlock (the session-0/AHK probe is Windows-only; a Linux box is never handed PowerShell)"
      continue
    fi
    if ! obs_fleet_poll_now "$name"; then
      log "$name: skipped -- traveling box is away (obs_fleet_poll_now), nothing to probe this pass"
      continue
    fi
    case "$name" in
      stream) host="${STREAM_HOST:-$host}" ;;
      resolume) host="${RESOLUME_HOST:-$host}" ;;
    esac
    printf '%s %s %s\n' "$name" "$host" "$(obs_session_box_has_ahk "$name")"
  done
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, threshold=$CONFIRM_THRESHOLD, roster='$BOXES')"
  # The target list is written by a plain redirect (no $(...) fork), so its skip log lines come from
  # this long-lived shell and reliably reach the journal (the #394 subshell-log-loss lesson).
  local targets_file name host has_ahk user pw
  targets_file="$(mktemp "${TMPDIR:-/tmp}/camera-box-obs-session-targets.XXXXXX")" || {
    log "ERROR: mktemp failed -- nothing probed this pass"
    return 0
  }
  obs_session_targets >"$targets_file"
  while read -r name host has_ahk; do
    [ -n "$name" ] || continue
    case "$name" in
      stream) user="$STREAM_USER"; pw="$STREAM_PW" ;;
      resolume) user="$RESOLUME_USER"; pw="$RESOLUME_PW" ;;
      *)
        log "$name: skipped -- no ssh credentials configured for this Windows box (add a <BOX>_USER/_PW pair here)"
        continue
        ;;
    esac
    # stdin from /dev/null: ssh inside win_ssh_run would otherwise swallow the rest of the list.
    process_box "$name" "$user" "$pw" "$host" "$has_ahk" </dev/null
  done <"$targets_file"
  rm -f "$targets_file"
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
