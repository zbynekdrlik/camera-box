#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/network-reach-alert-watchdog.sh /
# optical-chain-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/splitter-port-alert-watchdog.sh -- #739: per-cambox HDMI-splitter-port no-signal recurrence
# alert, DEV1-SIDE.
#
# WHY (#739, live 2026-07-13): the rig feeds ONE camera through an HDMI splitter to every cambox, so
# per-cambox capture can only differ by each box's INDIVIDUAL leg (its splitter output port + cable/
# grabber). When 4/6 splitter ports died, the boxes on dead ports saw NO SIGNAL while siblings saw the
# shared camera -- but each grabber renders no-signal differently (Elgato 4K S = purple noise;
# ShadowCast 2 = flat grey), so the failures MASQUERADED as per-camera "colour" bugs and burned two
# days of tint-hunting. Nothing today COMPARES per-cambox capture across the fleet on a periodic,
# dev1-side cadence: optical-chain-alert-watchdog.sh (#860) watches only the cam2 optical-INJECTION leg
# (painter->monitor), and verify-fleet.sh/verify-device.sh are one-shot acceptance gates run by hand.
#
# A DEV1 systemd --user timer ssh-reads each ACTIVE cambox's most recent `capture chroma:` journal line
# (the #299 metric camera-box already logs every ~5s -- zero cambox code change), the shared PURE
# splitter_health_* decisions (scripts/lib/splitter-health.sh) classify the fleet, and it pages via
# `airuleset.py notify` from dev1 (where the checkout + Discord creds live -- a cambox has neither).
#
# THE DISCRIMINATOR (splitter_health_classify): a box pages a SPLITTER-PORT suspicion iff it is
# degraded (not capturing OR grayscale) AND >=1 SIBLING is proven-good (reachable + capturing +
# colour). A proven-good sibling proves the shared camera is delivering AND dev1's path to the rig is
# up, so the only element that can differ for the bad box is its own output port -- this SELF-ANCHORS,
# no separate reference-anchor guard needed. If EVERY reachable box is equally degraded -> SOURCE_WIDE
# (shared camera / AWB / idle rig, NOT a per-port fault) -> report-only, never a false page. The
# confirm-counter + alert throttle are the SAME pure obs_watchdog_confirm / obs_watchdog_alert_throttle
# (scripts/lib/obs-watchdog-decision.sh) #391/#882/#1040/#1001 already use -- no second alert
# mechanism. Per-box state, so each cambox pages independently. A recovery ("colour again") ping fires
# once when a box we paged for returns to OK.
#
# #1079 (Elgato purple-noise): the readable per-box signals liveness + colour/grayscale catch the
# flat-grey no-signal mode (ShadowCast) and any frame-stall mode, but the Elgato purple-noise mode
# (colourful, frames flow) reads as colour=1 = OK. camera-box now logs a per-frame spatial-roughness
# term `rough=` on the `capture chroma:` line (high roughness + colour = the structureless-noise
# signature); this watchdog PARSES + SURFACES it REPORT-ONLY in each box's per-box log line (fleet-wide
# telemetry). It does NOT page/gate on roughness yet -- a data-first follow-up calibrates the threshold
# against the accumulated fleet `rough=` data before flipping it into a live page. See
# .claude/rules/splitter-port-health-watchdog.md.
#
# Usage:
#   scripts/splitter-port-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/splitter-port-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/splitter-port-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/splitter-health.sh
. "$HERE/lib/splitter-health.sh"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# #1290: the SHARED rig TEST/EVENT-mode discriminator (self-sources optical-chain-health.sh for the
# ONE durable painter_expected signal). In provable EVENT mode this fleet's sibling-anchor DEAD_PORT
# verdict is a false page (each cambox has its OWN camera, not one shared via the splitter).
# shellcheck source=scripts/lib/rig-mode-state.sh
. "$HERE/lib/rig-mode-state.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "splitter-port-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The active cambox fleet is derived from CAMERA_ACTIVE_SET (scripts/camera-set.sh) -- NEVER a literal
# cam range (the #827 camera-active-set discipline). camera_resolve maps each name -> CAMERA_IP.
SSH_USER="${SPLITTER_WATCH_SSH_USER:-root}"
# Same well-known dev-cam root credential the sibling fleet scripts default to
# (deploy-fleet.sh / verify-fleet.sh / verify-device.sh) -- override via CAM_PW for a rotated password.
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SPLITTER_WATCH_SSH_TIMEOUT:-8}"        # per-box ssh connect timeout (s)
JOURNAL_WINDOW="${SPLITTER_WATCH_JOURNAL_WINDOW:-120}" # freshness window (s) for the last chroma line;
                                                       # > the ~5s report tick, so a live box always
                                                       # has a fresh line and a stalled one has none.

# 2-pass confirm before paging (matches the sibling watchdogs): a single transient ssh/journal blip
# must never page. A genuinely dead port stays degraded across the 5-min cadence.
CONFIRM_THRESHOLD="${SPLITTER_WATCH_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${SPLITTER_WATCH_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${SPLITTER_WATCH_REPO:-zbynekdrlik/camera-box}"

# #1290: the cam2 painter probe target for the rig EVENT/TEST-mode discriminator. cam2 is the fixed
# painter box; rig-mode.sh event DISABLES cam2-painter.service (#892) + removes the pidfile (-> EVENT)
# and rig-mode.sh test enable-`--now`s it (-> TEST). One ssh to cam2 per pass; an ssh failure -> empty
# snapshot -> UNKNOWN -> today's behaviour (fail-safe).
RIG_MODE_PAINTER_IP="${RIG_MODE_PAINTER_IP:-10.77.9.62}"
RIG_MODE_PAINTER_PIDFILE="${RIG_MODE_PAINTER_PIDFILE:-/run/rig-painter.pid}"
RIG_MODE_PAINTER_SERVICE="${RIG_MODE_PAINTER_SERVICE:-cam2-painter.service}"

STATE_DIR="${SPLITTER_WATCH_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-splitter-port-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-splitter-port-alert-dryrun.state"
STATE_FILE="${SPLITTER_WATCH_STATE_FILE:-$_state_default}"

log() { printf '%s [splitter-port-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- probe (dev1-local; NOT pure -- kept out of the lib) -----------------------------------------
# probe_box <ip> -> stdout: the raw probe output for splitter_health_parse_probe (echoes PROBE_OK on a
# successful ssh connect, then the last `capture chroma:` line within the freshness window). An ssh
# failure -> empty stdout -> reachable=0 = NODATA (never a false signal). The cutoff epoch is computed
# on dev1 and passed as an absolute `@<epoch>` (the rig is dantesync-synced, so box+dev1 clocks agree),
# avoiding any remote `date`/journalctl-relative-syntax dependency.
probe_box() {
  local ip="$1" since_epoch remote_cmd
  since_epoch="$(( $(date +%s) - JOURNAL_WINDOW ))"
  remote_cmd="echo PROBE_OK; journalctl -u camera-box --since \"@${since_epoch}\" --no-pager 2>/dev/null | grep 'capture chroma:' | tail -1"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no "${SSH_USER}@${ip}" "$remote_cmd" 2>/dev/null
}

# rig_mode_probe -> stdout: the cam2 painter snapshot (RIG_MODE_PROBE_OK + the four painter KEY|value
# lines), or empty on an ssh failure. ONE ssh to cam2 per pass. Same ssh shape as probe_box (reuses
# SSH_USER / CAM_PW / SSH_TIMEOUT); an ssh failure -> empty -> rig_mode_from_painter_snapshot UNKNOWN
# -> today's behaviour (fail-safe). Overridden wholesale by the driver tests (like probe_box) with a
# canned snapshot; the probe snippet is the SHARED one rig-mode-state.sh builds.
rig_mode_probe() {
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no "${SSH_USER}@${RIG_MODE_PAINTER_IP}" \
    "$(rig_mode_state_probe_remote_snippet "$RIG_MODE_PAINTER_PIDFILE" "$RIG_MODE_PAINTER_SERVICE")" 2>/dev/null
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

# A non-DEAD_PORT pass (OK / SOURCE_WIDE / NODATA) is not a confirmed per-port fault: clear this box's
# confirm counter AND its throttle sig so a genuinely NEW port fault later pages fresh. Does NOT clear
# the `alerted` recovery latch (handled separately so a box we paged for still emits a "colour again"
# ping when it returns to OK).
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip> <verdict> <capturing> <colour> <u_dev> <v_dev> <rough> -- the verdict + parsed
# fields are computed ONCE in main() (so the fleet healthy-count can be aggregated first) and passed
# in. `rough` (#1079) is SURFACED in the per-box log line REPORT-ONLY (fleet-wide telemetry for a
# data-first noise-threshold calibration follow-up); it does NOT influence the verdict this PR.
handle_box() {
  local box="$1" ip="$2" verdict="$3" capturing="$4" colour="$5" u_dev="$6" v_dev="$7" rough="${8:--}"
  local rig_mode="${9:-UNKNOWN}"
  log "$box ($ip): capturing=$capturing colour=$colour u_dev=$u_dev v_dev=$v_dev rough=$rough -> $verdict"

  # #1290: in provable EVENT/production mode the sibling-anchor DEAD_PORT premise (ONE camera through
  # the HDMI splitter to EVERY cambox) does NOT hold -- each cambox has its OWN camera, so a
  # camera-less cambox is legitimately black. Log this box's would-be verdict report-only and NEVER
  # page. TEST and UNKNOWN (cam2 unreadable) behave exactly as today. Clear the per-box confirm/
  # throttle so a genuine TEST-mode fault later pages fresh (a mode change is not a recovery, so the
  # `alerted` latch is left as-is -- a still-bad box re-confirms from scratch when TEST resumes).
  if [ "$rig_mode" = "EVENT" ]; then
    log "$box $verdict skip: rig in EVENT mode — TEST-premise verdict, no page (#1290)"
    clear_box_throttle "$box"
    return 0
  fi

  if [ "$verdict" = "OK" ]; then
    local was_alerted
    was_alerted="$(read_state_field "alerted_${box}" 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: $box back to colour"
      else
        log "RECOVERY: $box back to colour -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${box}" 0
    fi
    clear_box_throttle "$box"
    return 0
  fi

  if [ "$verdict" = "NODATA" ]; then
    log "$box unreadable (ssh failed / box off) -- nothing to decide for it this pass"
    clear_box_throttle "$box"
    return 0
  fi

  if [ "$verdict" = "NO_CAPTURE" ]; then
    # Reachable but no fresh capture -- an AMBIGUOUS class (camera-box crashed / device-busy / stopped
    # by an E2E run / a genuine grabber stall) that is ROUTINE on this rig. Report-only: paging a
    # splitter-port suspicion here would be a mis-attribution / false page. Surfaced in the log for an
    # operator; reset the per-port confirm/throttle so a later attributable DEAD_PORT episode pages fresh.
    log "$box reachable but NOT capturing -> NO_CAPTURE (camera-box down / device-busy / E2E-stop / grabber stall) -- report-only, not paging as a splitter-port fault"
    clear_box_throttle "$box"
    return 0
  fi

  if [ "$verdict" = "SOURCE_WIDE" ]; then
    # Every reachable box capturing-but-GRAYSCALE => the shared camera/source (AWB desaturation on a
    # B&W pattern, or an idle rig), NOT a per-port fault. Report-only: this watchdog deliberately does
    # NOT page it (that would false-page every time the source content is legitimately monochrome).
    # Reset the per-port confirm/throttle so a later attributable DEAD_PORT episode pages fresh.
    log "$box grayscale but NO proven-good sibling -> SOURCE_WIDE (shared camera/source or idle rig) -- report-only, not paging"
    clear_box_throttle "$box"
    return 0
  fi

  # DEAD_PORT -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box DEAD_PORT confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box DEAD_PORT this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED per-port fault -> latch the recovery flag, then throttle-dedup on the box signature.
  write_state_field "alerted_${box}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="splitterport:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  detail="$(splitter_health_alert_detail "$box" "$capturing" "$colour" "$u_dev" "$v_dev")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED DEAD_PORT ($detail) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box splitter-port fault"
    python3 "$NOTIFY" notify --body \
      "🚨 HDMI splitter ($REPO_SLUG): **$box** ($ip) — ${detail}. Iný cambox na tej istej kamere+splitteri je v poriadku, čiže kamera obraz dodáva: chyba je najskôr v HDMI splitter porte / kábli do $box, nie vo farbe kamery. Potrebný fyzický zásah — skontroluj splitter port a kábel do $box. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol." \
      --dedup-key "splitter-port-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
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

  # #1290: determine the rig mode ONCE per pass from the durable cam2 painter signal. In provable
  # EVENT mode every TEST-premise verdict below is logged report-only (never a phone page); UNKNOWN
  # (cam2 unreadable) and TEST behave exactly as today.
  local rig_mode_snapshot rig_mode
  rig_mode_snapshot="$(rig_mode_probe)"
  rig_mode="$(rig_mode_from_painter_snapshot "$rig_mode_snapshot")"
  log "rig mode (cam2 painter probe @ $RIG_MODE_PAINTER_IP): $rig_mode"

  # -- gather each active box's probe ONCE, parse, and count the proven-good fleet ----------------
  local cam names=() ips=() reaches=() caps=() cols=() us=() vs=() roughs=() healths=()
  local total_healthy=0
  for cam in $active; do
    if ! camera_resolve "$cam"; then
      log "$cam: camera_resolve failed -- skipping"
      continue
    fi
    local ip raw parsed reachable=0 capturing=0 colour=0 u_dev="-" v_dev="-" rough="-" healthy
    ip="$CAMERA_IP"
    raw="$(probe_box "$ip")"
    parsed="$(splitter_health_parse_probe "$raw")"
    # `parsed` is the lib's own `key=value` record (6 fields since #1079, values constrained to
    # 0/1/-/[0-9.]). Consume it in ONE pass -- strip the `key=` prefixes, read the 6 values in field
    # order -- rather than re-parsing each field with a second, independent regex surface that would
    # silently drift from the lib's format (the "a real dead port would not page" failure mode a
    # reviewer flagged). `rough` is the #1079 report-only spatial-roughness telemetry.
    read -r reachable capturing colour u_dev v_dev rough \
      <<< "$(printf '%s' "$parsed" | sed 's/[a-z_]*=//g')"
    healthy="$(splitter_health_is_healthy "${reachable:-0}" "${capturing:-0}" "${colour:-0}")"
    names+=("$cam"); ips+=("$ip"); reaches+=("${reachable:-0}")
    caps+=("${capturing:-0}"); cols+=("${colour:-0}"); us+=("${u_dev:--}"); vs+=("${v_dev:--}")
    roughs+=("${rough:--}")
    healths+=("$healthy")
    [ "$healthy" = "1" ] && total_healthy=$(( total_healthy + 1 ))
  done

  log "fleet proven-good count = $total_healthy of ${#names[@]} active"

  # -- classify each box against the fleet consensus + act ---------------------------------------
  local i
  for i in "${!names[@]}"; do
    local healthy_siblings verdict
    # a box's proven-good SIBLINGS = fleet total minus itself (only if it was itself proven-good --
    # a degraded box contributes 0 to total_healthy, so it already excludes itself: no off-by-one).
    healthy_siblings="$total_healthy"
    [ "${healths[$i]}" = "1" ] && healthy_siblings=$(( total_healthy - 1 ))
    verdict="$(splitter_health_classify "${reaches[$i]}" "${caps[$i]}" "${cols[$i]}" "$healthy_siblings" | sed -n 's/^verdict=//p')"
    handle_box "${names[$i]}" "${ips[$i]}" "$verdict" "${caps[$i]}" "${cols[$i]}" "${us[$i]}" "${vs[$i]}" "${roughs[$i]}" "$rig_mode"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
