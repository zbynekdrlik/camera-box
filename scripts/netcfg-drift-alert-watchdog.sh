#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next timer
# tick -- same convention as scripts/network-reach-alert-watchdog.sh / optical-chain-alert-watchdog.sh
# (set -uo pipefail, not -e).
#
# scripts/netcfg-drift-alert-watchdog.sh -- #797: DEV1-SIDE, REPORT-ONLY alert for venue-switch config
# drift. Runs `scripts/netcfg-audit.sh --check` (read-only ssh to the MikroTik chain), and once a
# DRIFT is CONFIRMED across N consecutive passes fires ONE Discord alert -- exactly the dev1-side
# alert-watchdog topology + confirm/throttle framework the network-reach / imag-obs / cadence
# watchdogs already use (scripts/lib/obs-watchdog-decision.sh). It NEVER writes switch config; a
# legitimate re-config is recorded by updating scripts/netcfg-baseline.json in a PR (the check then
# goes CLEAN again).
#
# WHY (#797): the KEPT microburst fix (`shared-buffers` 40->80%) has no guard against a silent revert
# (a factory reset / config restore / firmware reflash drops it back to the 40% default that CAUSED
# the 2026-07 burst-gap egress drops), and a port silently negotiating a degraded link (duplex/speed
# regression) or a fresh drop-rate storm is nobody's job until someone hand-ssh-es in mid-incident.
# A slow dev1 timer surfaces all three BEFORE the next event, on the same rig-degradation-alert
# discipline as its sibling watchdogs.
#
# Cadence is HOURLY (config drift is a low-frequency event, not a 5-min liveness poll). Ships DISABLED
# like its siblings -- see .claude/rules/netcfg-audit.md for the one-time enable + credential setup.
#
# Usage:
#   scripts/netcfg-drift-alert-watchdog.sh            # one check -> decide -> alert pass
#   scripts/netcfg-drift-alert-watchdog.sh --dry-run  # check + decide + LOG only; never alert
#   scripts/netcfg-drift-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h) sed -n '5,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") : ;;
  *) echo "netcfg-drift-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

AUDIT="${NETCFG_DRIFT_AUDIT_CMD:-$HERE/netcfg-audit.sh}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${NETCFG_DRIFT_ALERT_REPO:-zbynekdrlik/camera-box}"
# 2-pass confirm before paging (a single transient ssh blip / mid-reconfig read must never page); a
# genuine revert stays drifted across the hourly cadence. ~12 throttle passes = a reminder ~ every
# 12h while the SAME drift persists (not every pass).
CONFIRM_THRESHOLD="${NETCFG_DRIFT_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${NETCFG_DRIFT_ALERT_THROTTLE_PASSES:-12}"

STATE_DIR="${NETCFG_DRIFT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-netcfg-drift-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-netcfg-drift-alert-dryrun.state"
STATE_FILE="${NETCFG_DRIFT_STATE_FILE:-$_state_default}"

log() { printf '%s [netcfg-drift-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

read_state_field() {
  local key="$1" default="$2" v
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } > "$STATE_FILE" 2>/dev/null || true
  fi
}
clear_throttle() {
  write_state_field confirm 0
  write_state_field alert_sig ""
  write_state_field alert_passes 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, audit=$AUDIT)"

  # Run the read-only audit. stdout carries the one-line summary (NETCFG-CLEAN / NETCFG-DRIFT ...);
  # exit 0 = CLEAN, 3 = DRIFT, anything else = gather/usage error (reachability is #1001's job, not
  # ours -- an error is "nothing to decide", never a config-drift page).
  local summary rc
  summary="$("$AUDIT" --check 2>/dev/null)"; rc=$?
  log "audit rc=$rc summary='${summary:-}'"

  if [ "$rc" -eq 0 ]; then
    local was_alerted
    was_alerted="$(read_state_field alerted 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: venue switch chain matches baseline again"
      else
        log "RECOVERY: chain matches baseline again -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field alerted 0
    fi
    clear_throttle
    return 0
  fi

  if [ "$rc" -ne 3 ]; then
    log "audit error (rc=$rc) -- nothing to decide this pass (reachability/baseline is not this watchdog's job)"
    return 0
  fi

  # DRIFT -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field confirm 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field confirm "${confirm:-0}"
  log "confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "DRIFT this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED drift -> latch recovery flag, throttle-dedup on the summary signature (a CHANGED set of
  # findings re-alerts; the same drift re-alerts only every ALERT_THROTTLE_PASSES passes).
  write_state_field alerted 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${summary:-netcfg-drift}"
  prior_sig="$(read_state_field alert_sig "")"
  prior_passes="$(read_state_field alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field alert_sig "$new_sig"
  write_state_field alert_passes "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $current_sig (alert_now=$alert_now)"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for netcfg drift"
    python3 "$NOTIFY" notify --body \
      "🚨 #797 netcfg-drift: the venue MikroTik switch chain drifted from its checked-in baseline ($REPO_SLUG). ${summary}. Run \`scripts/netcfg-audit.sh --check\` for the full report; if the change is intentional, update scripts/netcfg-baseline.json in a PR." \
      --dedup-key "netcfg-drift" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main
