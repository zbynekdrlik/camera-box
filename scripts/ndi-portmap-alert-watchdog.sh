#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next timer
# tick -- same convention as scripts/netcfg-drift-alert-watchdog.sh / network-reach-alert-watchdog.sh
# (set -uo pipefail, not -e).
#
# scripts/ndi-portmap-alert-watchdog.sh -- #1181: DEV1-SIDE, REPORT-ONLY alert for a STRIH-SNV OBS
# NDI SENDER port-map change. Runs `scripts/ndi-portmap-audit.sh --check` (read-only avahi mDNS read,
# no rig writes), and once a CHANGED map is CONFIRMED across N consecutive passes fires ONE Slovak
# Discord alert -- exactly the dev1-side alert-watchdog topology + confirm/throttle framework the
# netcfg-drift / network-reach / cadence / cg-bridge watchdogs already use
# (scripts/lib/obs-watchdog-decision.sh). It NEVER writes to any rig box; a deliberate re-capture is
# recorded by re-running `--capture` and committing scripts/ndi-portmap-baseline.json in a PR (the
# check then goes STABLE again).
#
# WHY (#1181): libndi assigns sender TCP ports sequentially from 5961 in creation order in one OBS
# process; adding/removing a dedicated NDI output live changes the next restart's creation order, so
# the sender port map RESHUFFLES and a stock NDI Studio Monitor / building TV reconnecting by a CACHED
# port silently shows whichever sender inherited it (NDI connect-by-URL never verifies the name). We
# cannot patch stock receivers -- the protection there is a STABLE sender set + LOUD detection of any
# change so the operator re-opens the affected receivers and re-captures the baseline.
#
# Cadence 5-min: a reshuffled port is an ACTIVE on-air fault (a receiver shows the wrong source RIGHT
# NOW), so detection is prompter than netcfg's hourly config-drift poll, while the 2-pass confirm still
# swallows a transient OBS-reload read (during a reload the map is briefly EMPTY -> the audit returns a
# gather error, exit 2, never CHANGED). Ships DISABLED like its siblings -- see
# .claude/rules/distroav-receiver-lifecycle.md for the one-time enable.
#
# Usage:
#   scripts/ndi-portmap-alert-watchdog.sh            # one check -> decide -> alert pass
#   scripts/ndi-portmap-alert-watchdog.sh --dry-run  # check + decide + LOG only; never alert
#   scripts/ndi-portmap-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h) sed -n '5,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") : ;;
  *) echo "ndi-portmap-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

AUDIT="${NDI_PORTMAP_ALERT_AUDIT_CMD:-$HERE/ndi-portmap-audit.sh}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${NDI_PORTMAP_ALERT_REPO:-zbynekdrlik/camera-box}"
# 2-pass confirm before paging (a single transient read must never page); a genuine reshuffle stays
# CHANGED across passes. ~12 throttle passes * 5min = a reminder ~ every hour while the SAME change
# persists (not every pass while the operator is already re-opening receivers).
CONFIRM_THRESHOLD="${NDI_PORTMAP_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${NDI_PORTMAP_ALERT_THROTTLE_PASSES:-12}"

STATE_DIR="${NDI_PORTMAP_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-ndi-portmap-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-ndi-portmap-alert-dryrun.state"
STATE_FILE="${NDI_PORTMAP_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [ndi-portmap-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

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

  # Run the read-only audit. stdout carries the one-line summary (NDI-PORTMAP-STABLE / -CHANGED ...);
  # exit 0 = STABLE, 3 = CHANGED, anything else = gather/usage error (OBS down / avahi unreachable /
  # anchor absent -> "nothing to decide", never a change page -- box reachability is #1001's job).
  local summary rc
  summary="$("$AUDIT" --check 2>/dev/null)"; rc=$?
  log "audit rc=$rc summary='${summary:-}'"

  if [ "$rc" -eq 0 ]; then
    local was_alerted
    was_alerted="$(read_state_field alerted 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: OBS sender port map matches baseline again"
      else
        log "RECOVERY: map matches baseline again -- firing recovery notification"
        python3 "$NOTIFY" notify --body \
          "✅ #1181 NDI port-map: sender-porty STRIH-SNV OBS opäť zodpovedajú baseline ($REPO_SLUG)." \
          >/dev/null 2>&1 || log "RECOVERY: airuleset.py notify failed (non-fatal)"
      fi
      write_state_field alerted 0
    fi
    clear_throttle
    return 0
  fi

  if [ "$rc" -ne 3 ]; then
    log "audit error (rc=$rc) -- nothing to decide this pass (OBS down / avahi unreachable is not this watchdog's job)"
    return 0
  fi

  # CHANGED -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field confirm 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field confirm "${confirm:-0}"
  log "confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "CHANGED this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED change -> latch recovery flag, throttle-dedup on the summary signature (a CHANGED set of
  # moved senders re-alerts; the same change re-alerts only every ALERT_THROTTLE_PASSES passes).
  write_state_field alerted 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${summary:-ndi-portmap-changed}"
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
    log "ALERT: firing Discord notification for NDI port-map change"
    python3 "$NOTIFY" notify --body \
      "🚨 #1181 NDI port-map: sender-porty STRIH-SNV OBS sa ZMENILI oproti baseline ($REPO_SLUG) — stock NDI prijímače (TV / NDI Studio Monitor) môžu teraz ukazovať NESPRÁVNY zdroj pod pôvodným menom (pripojené na zapamätaný port). ${summary}. Akcia: na TV/Studio Monitor prijímačoch znovu otvoriť zdroj; ak je zmena zámerná (pridaný/odobraný výstup + reštart OBS), obnoviť baseline \`scripts/ndi-portmap-audit.sh --capture\` a commitnúť v PR." \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main
