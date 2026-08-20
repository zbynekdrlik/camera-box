#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/splitter-port-alert-watchdog.sh /
# network-reach-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/grabber-stuck-alert-watchdog.sh -- #1128: fast-capture grabber STUCK alert, DEV1-SIDE.
#
# WHY (#1128, live 2026-08-19 on CAM1): the GENKI ShadowCast 2 grabber can free-run at ~62.5 fps
# AND deliver persistent corrupted frames -- a state `systemctl restart camera-box` does NOT clear
# (only a USB re-enumeration does). The camera-box appliance's crate-root detector
# (src/grabber_stuck.rs) decides this and logs the report-only marker `#1128 grabber STUCK` every
# 5s to its journal REGARDLESS of whether the in-process re-auth is enabled -- so this state is
# visible even when self-heal is off. This watchdog is the ALERT half: a dev1 --user timer
# ssh-reads each ACTIVE cambox's journal for that marker within a freshness window, the shared PURE
# grabber_stuck_* decisions (scripts/lib/grabber-stuck-health.sh) classify it, and it pages via
# `airuleset.py notify` from dev1 (where the checkout + Discord creds live). ONE source of truth
# for the verdict: the Rust detector decides, this only relays -- the same "self-heal emits,
# watchdog pages" pattern as the #663 self-heal marker.
#
# DISCORD VOLUME (discord-volume-near-zero): a chronic stuck grabber must NOT re-ping the phone
# every pass. This fires exactly ONE alert per STUCK episode (a very large throttle) + ONE recovery
# ping when the box returns to OK -- never a repeated alert of a chronic state.
#
# The confirm-counter + alert throttle are the SAME pure obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use --
# no second alert mechanism. Per-box state, so each cambox pages independently. NODATA (unreachable
# box / ssh blip) is never a page and never a false recovery.
#
# Usage:
#   scripts/grabber-stuck-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/grabber-stuck-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/grabber-stuck-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/grabber-stuck-health.sh
. "$HERE/lib/grabber-stuck-health.sh"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "grabber-stuck-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The active cambox fleet is derived from CAMERA_ACTIVE_SET (scripts/camera-set.sh) -- NEVER a
# literal cam range (the #827 camera-active-set discipline). camera_resolve maps each name -> IP.
SSH_USER="${GRABBER_STUCK_WATCH_SSH_USER:-root}"
# Same well-known dev-cam root credential the sibling fleet scripts default to (override via CAM_PW).
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${GRABBER_STUCK_WATCH_SSH_TIMEOUT:-8}"          # per-box ssh connect timeout (s)
# Freshness window (s) for the newest `#1128 grabber STUCK` marker. The appliance logs the marker
# every ~5s while stuck, so a genuinely stuck box always has a fresh line and a recovered one has
# none. 120s > the ~5s cadence with margin.
JOURNAL_WINDOW="${GRABBER_STUCK_WATCH_JOURNAL_WINDOW:-120}"

# 2-pass confirm before paging (matches the sibling watchdogs): one transient ssh/journal blip must
# never page. A genuinely stuck grabber keeps logging the marker across the 5-min cadence.
CONFIRM_THRESHOLD="${GRABBER_STUCK_WATCH_CONFIRM_THRESHOLD:-2}"
# ONE alert per episode: a huge throttle means the same-signature stuck state never re-pings while
# it persists (discord-volume-near-zero). The recovery ping on return-to-OK is the only follow-up.
ALERT_THROTTLE_PASSES="${GRABBER_STUCK_WATCH_THROTTLE_PASSES:-1000000}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${GRABBER_STUCK_WATCH_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${GRABBER_STUCK_WATCH_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-grabber-stuck-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-grabber-stuck-alert-dryrun.state"
STATE_FILE="${GRABBER_STUCK_WATCH_STATE_FILE:-$_state_default}"

log() { printf '%s [grabber-stuck-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- probe (dev1-local; NOT pure -- kept out of the lib) -----------------------------------------
# probe_box <ip> -> stdout: PROBE_OK on a successful ssh connect, then the newest `#1128 grabber
# STUCK` marker line within the freshness window (or nothing if none). An ssh failure -> empty
# stdout -> reachable=0 = NODATA (never a false signal). The cutoff epoch is computed on dev1 and
# passed as an absolute `@<epoch>` (the rig is dantesync-synced, so box+dev1 clocks agree).
probe_box() {
  local ip="$1" since_epoch remote_cmd
  since_epoch="$(( $(date +%s) - JOURNAL_WINDOW ))"
  remote_cmd="echo PROBE_OK; journalctl -u camera-box --since \"@${since_epoch}\" --no-pager 2>/dev/null | grep -F '#1128 grabber STUCK' | tail -1"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no "${SSH_USER}@${ip}" "$remote_cmd" 2>/dev/null
}

# -- persisted per-box state (key=value lines) --------------------------------------------------
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

# OK (genuine recovery) — clear this box's confirm counter AND its throttle sig so a genuinely NEW
# stuck episode later pages fresh. Does NOT clear the `alerted` recovery latch (handled in
# handle_box so a box we paged for still emits a recovery ping on OK).
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# NODATA (a transient ssh/journal blip, NOT a recovery) — clear ONLY the confirm counter, and LEAVE
# the throttle sig/passes intact. Otherwise a single blip mid-episode would reset the
# one-ping-per-episode latch, and the next two STUCK passes would page a SECOND time for the SAME
# ongoing episode (discord-volume-near-zero: a chronic stuck box must page exactly once). A genuine
# recovery is signalled by an OK pass (full clear above), never by an unreadable one.
clear_box_confirm_only() {
  local box="$1"
  write_state_field "confirm_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip> <verdict> <fps> -- verdict + parsed fps computed once in main().
handle_box() {
  local box="$1" ip="$2" verdict="$3" fps="${4:-?}"
  log "$box ($ip): verdict=$verdict fps=$fps"

  if [ "$verdict" = "OK" ]; then
    local was_alerted
    was_alerted="$(read_state_field "alerted_${box}" 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: $box grabber back to normal"
      else
        log "RECOVERY: $box grabber back to normal -- firing recovery notification"
        python3 "$NOTIFY" notify --body \
          "✅ grabber ($REPO_SLUG): **$box** ($ip) opäť sníma normálne — STUCK stav zmizol (kadencia ~60 fps, žiadne poškodené snímky)." \
          >/dev/null 2>&1 || log "RECOVERY: airuleset.py notify failed (non-fatal)"
      fi
      write_state_field "alerted_${box}" 0
    fi
    clear_box_throttle "$box"
    return 0
  fi

  if [ "$verdict" = "NODATA" ]; then
    log "$box unreadable (ssh failed / box off) -- nothing to decide for it this pass"
    clear_box_confirm_only "$box"
    return 0
  fi

  # STUCK -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box STUCK confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box STUCK this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED stuck -> latch the recovery flag, then throttle-dedup on the box signature (ONE ping
  # per episode: the huge throttle means it never re-pings while the SAME box stays stuck).
  write_state_field "alerted_${box}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="grabberstuck:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  # ONE ping per episode: the huge throttle means alert_now is 1 only on the FIRST confirmed pass
  # of an episode; every subsequent pass while the SAME box stays stuck is suppressed (this gate
  # applies to BOTH the dry-run and the real path, so the one-ping-per-episode behavior is visible
  # in a dry-run too).
  if [ "${alert_now:-0}" != "1" ]; then
    log "ALERT: suppressed by throttle (one-ping-per-episode; pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED grabber STUCK (~${fps} fps)"
    return 0
  fi
  log "ALERT: firing Discord notification for $box grabber STUCK"
  python3 "$NOTIFY" notify --body \
    "🚨 grabber STUCK ($REPO_SLUG): **$box** ($ip) — USB grabber uviazol (~${fps} fps + trvalé poškodené snímky). \`systemctl restart\` to NEopraví; treba USB re-enumeráciu grabbera. Ak je self-heal zapnutý (CAMERA_BOX_GRABBER_STUCK_SELFHEAL), appliance to skúsi sám; inak treba fyzický zásah / výmenu grabbera. Jednorázový alert — potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol." \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

main() {
  # -- fail-loud tool preflight (issue 833: a missing tool must fail by NAME, never read as a
  # measured zero -- an absent sshpass would make every box look unreachable = a silent watchdog).
  if ! command -v sshpass >/dev/null 2>&1; then
    log "FATAL: sshpass not found on dev1 -- cannot probe the cambox fleet (apt-get install sshpass)"
    exit 3
  fi

  local active
  active="$CAMERA_ACTIVE_SET"
  log "pass start (dry_run=$DRY_RUN, active='$active', window=${JOURNAL_WINDOW}s)"

  local cam
  for cam in $active; do
    if ! camera_resolve "$cam"; then
      log "$cam: camera_resolve failed -- skipping"
      continue
    fi
    local ip raw parsed reachable=0 stuck=0 verdict fps
    ip="$CAMERA_IP"
    raw="$(probe_box "$ip")"
    parsed="$(grabber_stuck_parse_probe "$raw")"
    read -r reachable stuck <<< "$(printf '%s' "$parsed" | sed 's/[a-z_]*=//g')"
    verdict="$(grabber_stuck_classify "${reachable:-0}" "${stuck:-0}" | sed -n 's/^verdict=//p')"
    fps="$(grabber_stuck_marker_fps "$raw")"
    handle_box "$cam" "$ip" "$verdict" "$fps"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
