#!/usr/bin/env bash
# fleet-parity-alert-watchdog.sh -- issue 1426: ONE fleet-wide version-parity pass (see the extended header below).
# airuleset:script-ok set -euo pipefail below, then set +e after the sources (the watchdog-common convention)
set -euo pipefail
#
# WHY (owner 13.10.2026, verbatim): "... ci ze sa ti znova rozisli verzie dantesync a obs a okrem toho
# ze si to vobec dovolil a ze to nic nezachytilo ...". The PP cameras ran an older dantesync after a
# fleet roll that ran while PP was powered off, and the OBS genlock builds differed between the venues;
# nothing compared the two venues, so nothing paged.
#
# ONE pass (a dev1 timer, every 30 min; dev1 is the only machine that reaches both venues):
#   1. OBS genlock build: every box of the obs-fleet `build-parity` facet (strih + stream of SNV and
#      PP; the cg OBS resolume boxes are out, owner 16.10.2026). An away traveling box (PP powered
#      off) is AWAY -- normal, never a page. A box is home when its OBS-WS :4455 OR its :8899
#      answers (an idle box with OBS stopped still serves its installed build). A home box:
#      `:8899` `genlock_build_sha`. The pure decision
#      scripts/fleet_build_parity_decision.py grades the set (OK / DRIFT / INCOMPLETE / UNKNOWN).
#   2. dantesync: scripts/dantesync-version-gate.sh --fleet (it skips an away reach / traveling node
#      by itself). Exit 20 = a present node is not on DANTESYNC_VERSION_PIN.
#   3. dantesync CATCH-UP (FLEET_PARITY_DANTESYNC_CATCHUP, default 1): on a dantesync drift of at most
#      FLEET_PARITY_CATCHUP_MAX nodes (a larger drift is a pin bump = a deliberate supervisor roll),
#      run scripts/dantesync-fleet-upgrade.sh --fleet (canary-first, at-pin nodes untouched, away
#      nodes skipped) -- ONLY while the rig lease is free and no home OBS box of either venue streams
#      or records (an unreadable home venue = no roll this pass, fail closed). This is how a box that
#      was off during a roll catches up when it comes back.
#   4. A DRIFT (OBS or dantesync) confirmed over FLEET_PARITY_CONFIRM_THRESHOLD passes pages ONE
#      Slovak card (stable dedup key `fleet-parity`, throttled), naming every outlier and the catch-up
#      result. Recovery is a log line only.
#
# Usage: fleet-parity-alert-watchdog.sh [--dry-run]   (--dry-run: read + decide + log; no roll, no page)
# Seams (tests): FLEET_PARITY_BOXES, FLEET_PARITY_FETCH_CMD (fetch_bundle_json replay),
#   FLEET_PARITY_DSYNC_GATE_CMD / FLEET_PARITY_DSYNC_UPGRADE_CMD (replace the two dantesync scripts),
#   FLEET_PARITY_BUSY_CMD (replace obs_phase2.py rig-busy-check), OBS_FLEET_HOME, RIG_LEASE_DIR.
# Ships DISABLED (systemd/fleet-parity-alert-watchdog.timer); the supervisor enables it on dev1.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/lib/obs-watchdog-decision.sh"
. "$HERE/lib/watchdog-common.sh"
. "$HERE/lib/obs-fleet.sh"
. "$HERE/lib/rig-lease.sh"
set +e
set -uo pipefail

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "fleet-parity-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

BOXES="${FLEET_PARITY_BOXES:-$(obs_fleet_boxes build-parity)}"
# shellcheck disable=SC2034 # read by scripts/lib/watchdog-common.sh (fetch_bundle_json)
BUNDLE_PORT="${FLEET_PARITY_BUNDLE_PORT:-8899}"
# shellcheck disable=SC2034 # read by scripts/lib/watchdog-common.sh (fetch_bundle_json)
BUNDLE_PATH="${FLEET_PARITY_BUNDLE_PATH:-/bundle-state.json}"
# shellcheck disable=SC2034 # read by scripts/lib/watchdog-common.sh (fetch_bundle_json)
CURL_TIMEOUT="${FLEET_PARITY_CURL_TIMEOUT:-10}"
CONFIRM_THRESHOLD="${FLEET_PARITY_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${FLEET_PARITY_ALERT_THROTTLE_PASSES:-12}"   # ~6 h at the 30-min cadence
CATCHUP="${FLEET_PARITY_DANTESYNC_CATCHUP:-1}"
CATCHUP_MAX="${FLEET_PARITY_CATCHUP_MAX:-8}"
DECIDE="${FLEET_PARITY_DECIDE:-$HERE/fleet_build_parity_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
STATE_DIR="${FLEET_PARITY_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-fleet-parity.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-fleet-parity-dryrun.state"
# shellcheck disable=SC2034 # read by scripts/lib/watchdog-common.sh (state helpers)
STATE_FILE="${FLEET_PARITY_STATE_FILE:-$_state_default}"

log() { printf '%s [fleet-parity-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# box_entry NAME HOST -> one JSON object for the decision module (stdout).
box_entry() {
  local box="$1" host="$2" body sha rc=0
  # The build is a fact of the installed bundle, not of a running OBS: an idle box whose OBS is
  # stopped (OBS-WS :4455 refused) is still home when its BundleStateServer :8899 answers.
  obs_fleet_poll_now "$box" || rc=$?
  if [ "$rc" -eq 1 ]; then
    rc=0
    OBS_FLEET_HOME_PORT=8899 obs_fleet_poll_now "$box" || rc=$?
  fi
  if [ "$rc" -ne 0 ]; then
    printf '{"box": "%s", "state": "away"}' "$box"
    return 0
  fi
  if ! body="$(fetch_bundle_json "$host" FLEET_PARITY_FETCH_CMD)"; then
    printf '{"box": "%s", "state": "unreadable"}' "$box"
    return 0
  fi
  sha="$(printf '%s' "$body" | python3 -c 'import json,sys
try:
    v = json.load(sys.stdin).get("genlock_build_sha") or ""
except Exception:
    v = ""
print(v if isinstance(v, str) else "")' 2>/dev/null)"
  if [[ "$sha" =~ ^[0-9a-f]{7,40}$ ]]; then
    printf '{"box": "%s", "state": "present", "sha": "%s"}' "$box" "$sha"
  else
    printf '{"box": "%s", "state": "nofacet"}' "$box"
  fi
}

# venue_busy STRIH_NAME STREAM_NAME -> 0 = busy or unreadable (no roll), 1 = idle or both away.
venue_busy() {
  local a="$1" b="$2" ha="" hb="" out rc
  obs_fleet_poll_now "$a" && ha="$(obs_fleet_host "$a")"
  obs_fleet_poll_now "$b" && hb="$(obs_fleet_host "$b")"
  if [ -z "$ha" ] && [ -z "$hb" ]; then
    return 1
  fi
  [ -n "$ha" ] || ha="$hb"
  [ -n "$hb" ] || hb="$ha"
  if [ -n "${FLEET_PARITY_BUSY_CMD:-}" ]; then
    out="$($FLEET_PARITY_BUSY_CMD "$ha" "$hb" 2>/dev/null)"
    rc=$?
  else
    out="$(python3 "$HERE/obs_phase2.py" rig-busy-check --strih-host "$ha" --stream-host "$hb" \
      --password "${OBS_PASSWORD:-}" 2>/dev/null)"
    rc=$?
  fi
  [ "$rc" -eq 0 ] || return 0
  [ "$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("busy"))' 2>/dev/null)" = False ] && return 1
  return 0
}

lease_held_by_someone() {
  [ -d "$(rig_lease_dir)" ] || return 1
  rig_lease_is_fresh "$(rig_lease_heartbeat_age_seconds)" "${RIG_LEASE_STALE_SECS:-5400}"
}

main() {
  log "pass start (dry_run=$DRY_RUN, boxes='$BOXES')"
  command -v python3 >/dev/null 2>&1 || { log "FATAL: python3 missing -- refusing to run blind"; return 3; }
  [ -r "$DECIDE" ] || { log "FATAL: decision module not readable: $DECIDE"; return 3; }

  # 1. OBS genlock build
  local pair box host entries="" analyze verdict modal outliers blind away
  for pair in $BOXES; do
    box="${pair%%|*}"; host="${pair##*|}"
    entries="${entries:+$entries, }$(box_entry "$box" "$host")"
  done
  analyze="$(printf '[%s]' "$entries" | python3 "$DECIDE" analyze)"
  verdict="$(printf '%s\n' "$analyze" | sed -n 's/^verdict=//p')"
  modal="$(printf '%s\n' "$analyze" | sed -n 's/^modal=//p')"
  outliers="$(printf '%s\n' "$analyze" | sed -n 's/^outliers=//p')"
  blind="$(printf '%s\n' "$analyze" | sed -n 's/^blind=//p')"
  away="$(printf '%s\n' "$analyze" | sed -n 's/^away=//p')"
  log "obs build: verdict=${verdict:-<none>} modal=${modal:0:12} outliers=${outliers:-none} blind=${blind:-none} away=${away:-none}"

  # 2. dantesync
  local dout drc ddrift="" ndrift=0 catchup_note=""
  if [ -n "${FLEET_PARITY_DSYNC_GATE_CMD:-}" ]; then
    dout="$($FLEET_PARITY_DSYNC_GATE_CMD 2>&1)"
  else
    dout="$(DANTESYNC_FLEET_SCAN_CMD=true timeout 900 bash "$HERE/dantesync-version-gate.sh" --fleet 2>&1)"
  fi
  drc=$?
  ddrift="$(printf '%s\n' "$dout" | awk '$3 == "DRIFT" {printf "%s%s=%s", sep, $1, $2; sep=","}')"
  [ -n "$ddrift" ] && ndrift="$(printf '%s' "$ddrift" | tr ',' '\n' | grep -c .)"
  log "dantesync: gate rc=$drc drift=${ddrift:-none}"

  # 3. dantesync catch-up
  if [ "$drc" -eq 20 ] && [ "$ndrift" -gt 0 ]; then
    if [ "$CATCHUP" != 1 ]; then
      catchup_note="catch-up vypnutý"
    elif [ "$ndrift" -gt "$CATCHUP_MAX" ]; then
      catchup_note="neposunuté: ${ndrift} uzlov je viac ako ${CATCHUP_MAX} (posun pinu = riadený roll)"
    elif lease_held_by_someone; then
      catchup_note="odložené: rig lease drží iný beh"
    elif venue_busy strih-snv stream-snv || venue_busy strih-pp stream-pp; then
      catchup_note="odložené: niektorý OBS box vysiela/nahráva alebo sa nedá prečítať"
    elif [ "$DRY_RUN" -eq 1 ]; then
      catchup_note="[dry-run] by spustil dantesync-fleet-upgrade.sh --fleet"
    else
      local urc
      log "catch-up: dantesync-fleet-upgrade.sh --fleet for ${ddrift}"
      if [ -n "${FLEET_PARITY_DSYNC_UPGRADE_CMD:-}" ]; then
        $FLEET_PARITY_DSYNC_UPGRADE_CMD >&2
      else
        timeout 3600 bash "$HERE/dantesync-fleet-upgrade.sh" --fleet >&2
      fi
      urc=$?
      if [ "$urc" -eq 0 ]; then catchup_note="dorovnané (upgrade rc 0)"; else catchup_note="upgrade ZLYHAL (rc $urc)"; fi
    fi
    log "catch-up: ${catchup_note}"
  fi

  # 4. confirm + page
  local drifting=0 sig
  [ "$verdict" = DRIFT ] && drifting=1
  if [ -n "$ddrift" ] && [ "$catchup_note" != "dorovnané (upgrade rc 0)" ]; then drifting=1; fi
  if [ "$drifting" -eq 0 ]; then
    if [ "$(read_state_field alerted 0)" = 1 ]; then
      log "RECOVERY: fleet versions agree again -- log only"
      write_state_field alerted 0
    fi
    write_state_field confirm 0
    clear_throttle
    log "pass end"
    return 0
  fi
  local prev decision confirm act
  prev="$(read_state_field confirm 0)"
  decision="$(obs_watchdog_confirm "$prev" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field confirm "${confirm:-0}"
  if [ "${act:-0}" != 1 ]; then
    log "drift seen, not yet confirmed (confirm=$confirm/$CONFIRM_THRESHOLD) -- holding"
    log "pass end"
    return 0
  fi
  write_state_field alerted 1
  sig="fleetparity:${outliers}:${ddrift}"
  local tout alert_now
  tout="$(obs_watchdog_alert_throttle "$sig" "$(read_state_field alert_sig "")" "$(read_state_field alert_passes 0)" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$tout" | sed -n 's/^alert_now=//p')"
  write_state_field alert_sig "$(printf '%s\n' "$tout" | sed -n 's/^new_sig=//p')"
  write_state_field alert_passes "$(printf '%s\n' "$tout" | sed -n 's/^new_passes=//p')"
  local body="⚠️ Verzie na rigu sa rozišli (camera-box issue 1426)."
  [ -n "$outliers" ] && body="$body OBS build: väčšina ${modal:0:12}, inak: ${outliers}."
  [ -n "$ddrift" ] && body="$body dantesync mimo pinu: ${ddrift} (${catchup_note:-bez catch-upu})."
  [ -n "$away" ] && body="$body Vypnuté/preč (nekontrolované): ${away}."
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD page (alert_now=$alert_now): $body"
  elif [ "${alert_now:-0}" = 1 ]; then
    log "ALERT: $body"
    python3 "$NOTIFY" notify --body "$body" --dedup-key "fleet-parity" >/dev/null 2>&1 \
      || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT suppressed by throttle -- still drifting"
  fi
  log "pass end"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
