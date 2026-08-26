#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/obs-liveness-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/imag-obs-alert-watchdog.sh — #882 imag-nb OBS liveness alert, DEV1-SIDE.
#
# WHY: imag-nb's own imag-wallpaper-refresh.sh ALREADY detects "obs not running" every 5 minutes
# (it logs "obs not running — keeping existing fallback") but has structurally NO WAY to alert
# anyone from there — imag-nb is a remote appliance box with no ~/devel/airuleset checkout and no
# Discord credentials. Confirmed LIVE (2026-07-30): a first attempt to fire `airuleset.py notify`
# directly FROM imag-nb failed every time ("airuleset.py notify failed"), because the tool simply
# isn't there. The #391 broadcast-OBS liveness watchdog (scripts/obs-liveness-watchdog.sh) already
# established the correct topology for this fleet: a DEV1 systemd --user timer (which DOES have
# the airuleset checkout + Discord credentials) polls the REMOTE box's live state and fires the
# alert from HERE. This script is that same topology applied to imag-nb's much simpler "is OBS
# running at all" liveness question (no render-wedge classification needed, unlike strih/stream),
# via the #882 SSH reachability probe (scripts/lib/imag-obs-reachability.sh) reused unchanged.
#
# ALL "should we alert?" logic is the SAME pure scripts/lib/obs-watchdog-decision.sh #391 already
# uses (obs_watchdog_confirm / obs_watchdog_alert_throttle) — no second decision mechanism.
#
# Confirm threshold defaults to 1, NOT #391's 2: this timer's own cadence is 5 minutes (matching
# imag-wallpaper-refresh.timer's own cadence so the two detections stay in lockstep), not #391's
# 4-second tight-poll window — a single miss here already means ~5 real minutes of downtime, not
# a transient blip needing debounce.
#
# Usage:
#   scripts/imag-obs-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/imag-obs-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/imag-obs-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/imag-obs-reachability.sh
. "$HERE/lib/imag-obs-reachability.sh"
# shellcheck source=scripts/lib/imag-obs-restart-storm.sh
. "$HERE/lib/imag-obs-restart-storm.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '5,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "imag-obs-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
IMAG_IP="${IMAG_IP:-10.77.9.182}"
IMAG_USER_SSH="${IMAG_USER:-newlevel}"
IMAG_PW_SSH="${IMAG_PW:-newlevel}"
CONFIRM_THRESHOLD="${IMAG_OBS_ALERT_CONFIRM_THRESHOLD:-1}"
ALERT_THROTTLE_PASSES="${IMAG_OBS_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

# #788: operator-quit discriminator. When OBS is down because the operator DELIBERATELY quit it
# (a clean imag-obs.service exit, or a fresh operator pause file), never fire a false 'crashed'
# alarm. PAUSE_FILE is a time-bounded operator override on the imag box (`touch` it before quitting
# OBS to test latency; a FORGOTTEN pause can never mask a real crash forever -- it expires after
# PAUSE_WINDOW_S from its mtime).
PAUSE_FILE="${IMAG_WATCHDOG_PAUSE_FILE:-/tmp/imag-watchdog-pause}"
PAUSE_WINDOW_S="${IMAG_WATCHDOG_PAUSE_WINDOW_S:-3600}"           # 60 min from the pause file's mtime

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${IMAG_OBS_ALERT_REPO:-zbynekdrlik/camera-box}"

# #1156: restart-STORM detection config. imag-obs.service's NRestarts counter is read each pass (a
# 2nd small ssh) and a STORM (>= THRESHOLD restarts within WINDOW_S) fires ONE deduped page, dedup'd
# through the SAME #391 obs_watchdog_alert_throttle the down-alert already uses -- never a second
# mechanism. Ships DISABLED: runs only when IMAG_OBS_RESTART_STORM_ENABLE=1 (a supervisor
# Environment= edit to imag-obs-alert-watchdog.service enables it), per the dev1-watchdog
# "ships disabled" convention. NRestarts climbs on the #1156 seed-import-fail Restart loop too, so
# this is exactly the alert the 1737-restart / 8.5h incident lacked.
RESTART_STORM_THRESHOLD="${IMAG_OBS_RESTART_STORM_THRESHOLD:-10}"
RESTART_STORM_WINDOW_S="${IMAG_OBS_RESTART_STORM_WINDOW_S:-600}"
RESTART_STORM_THROTTLE_PASSES="${IMAG_OBS_RESTART_STORM_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

# #1070: the REPORT-ONLY latency-pin verify-at-start for imag -- the "outside a gate run" residual
# of 1061 (during a gate run the issue-1061 self-healer script restores the floor + e2e_discord_report.py
# reports drift; between runs an OBS restart that reloads a reverted genlock_latency_ms_src is
# reported by nothing). Run here on a HEALTHY pass, REPORT-ONLY -- NEVER overwritten. Own throttle
# window, independent of the down-alert throttle above. imag's OBS WS password matches the ssh pw.
LATENCY_VERIFY="${LATENCY_PINS_VERIFY:-$HERE/latency_pins_verify.py}"
IMAG_OBS_WS_PASSWORD="${IMAG_OBS_WS_PASSWORD:-${IMAG_PW:-newlevel}}"
LATENCY_ALERT_THROTTLE_PASSES="${IMAG_LATENCY_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

STATE_DIR="${IMAG_OBS_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${IMAG_OBS_ALERT_STATE_FILE:-$STATE_DIR/camera-box-imag-obs-alert.state}"

log() { printf '%s [imag-obs-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure (SSH + the #882 reachability probe) ─────────────────────────────
# measure -> sets $PROBE_OUT to the remote probe's stdout. Empty means an ssh/connectivity
# failure (the fleet's own [0/8] preflight is the authority for THAT condition, not this
# watchdog) -- treated this pass as "nothing to decide", never as a false "OBS is down".
measure() {
  PROBE_OUT="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_obs_reachability_probe_cmd)" 2>/dev/null || true)"
}

# ── read / write persisted state ────────────────────────────────────────────
# State file format (key=value, one per line):
#   confirm=<n>       — consecutive-down confirmation counter
#   alert_sig=<str>   — fingerprint of the last-alerted condition (throttle dedup)
#   alert_passes=<n>  — passes elapsed since the last alert for the same sig
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

# ── #1070 latency-pin verify-at-start (REPORT-ONLY) ─────────────────────────
# Run ONLY on a HEALTHY (OBS-up) pass. Reads imag's live per-source genlock_latency_ms_src over WS
# (read-only) and diffs against the committed baseline (scripts/latency-pins-baseline.json) via
# scripts/latency_pins_verify.py --box imag. On drift (exit 1) fires a THROTTLED Discord report,
# signature + state distinct from the OBS-down alert. exit 2 (WS not ready / read failure) is NOT a
# drift -- the down-alert path owns a genuinely-down OBS, so a transient WS read failure just skips
# this pass. NEVER overwrites: per-source latency is the operator's A/V-align domain (imag is always
# the 3ms floor, but a genuine re-tune is recorded by editing the baseline in a PR, never forced).
latency_drift_check() {
  local out rc
  # NB: the `|| rc=$?` capture is load-bearing under this script's `set -e` -- a bare
  # `out="$(cmd)"; rc=$?` is itself a failing statement when cmd exits non-zero (drift = rc 1),
  # which killed the whole pass with exit 1 before any report could fire (caught by CI's first
  # execution of the issue-1070 harness; Tier-0 forbids running these tests locally).
  rc=0
  out="$(python3 "$LATENCY_VERIFY" --box imag --host "$IMAG_IP" --password "$IMAG_OBS_WS_PASSWORD" 2>&1)" || rc=$?
  if [ "$rc" -eq 0 ]; then
    write_state_field latency_alert_sig ""
    write_state_field latency_alert_passes 0
    log "latency-pin verify: imag on the agreed baseline (report-only)"
    return 0
  fi
  if [ "$rc" -ne 1 ]; then
    log "latency-pin verify: could not read imag pins (rc=$rc) -- skipping this pass (not a drift)"
    return 0
  fi
  local drift_lines sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  drift_lines="$(printf '%s\n' "$out" | grep 'LATENCY-PIN DRIFT' | sort | tr '\n' ';')"
  sig="imag-latency:${drift_lines}"
  prior_sig="$(read_state_field latency_alert_sig "")"
  prior_passes="$(read_state_field latency_alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$sig" "$prior_sig" "$prior_passes" "$LATENCY_ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field latency_alert_sig "$new_sig"
  write_state_field latency_alert_passes "$new_passes"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD report imag latency-pin drift (report-only): $out"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord report for imag latency-pin DRIFT (REPORT-ONLY, not overwritten)"
    python3 "$NOTIFY" notify --body \
      "⚠️ imag latencia pinov ($REPO_SLUG) — len INFO, piny sa neprepisujú (toto je doména operátora pre A/V zarovnanie): $out" \
      --dedup-key "imag-obs-latency-drift" \
      >/dev/null 2>&1 || log "latency report: airuleset.py notify failed (non-fatal)"
  else
    log "latency drift report suppressed by throttle (pass ${prior_passes}/${LATENCY_ALERT_THROTTLE_PASSES})"
  fi
}

# ── #1156 restart-STORM check (a 2nd small ssh for imag-obs.service's NRestarts) ─────────────
# Ships DISABLED (IMAG_OBS_RESTART_STORM_ENABLE!=1 -> immediate no-op, no ssh). When enabled: read
# the NRestarts counter, classify against the persisted baseline+ts (time-windowed "N per window_s",
# fail-safe = never false-page), and on a STORM fire ONE throttled Discord page (sig imag-restart-storm,
# ~1h reminder while it persists, cleared on any non-storm pass so a NEW storm pages fresh). The ssh
# probe is overridable via IMAG_OBS_RESTART_PROBE_CMD and the clock via IMAG_OBS_RESTART_NOW (offline
# tests). Runs its own ssh, independent of the reachability measure() above -- a storm can flap OBS
# up/down, so it must be caught regardless of this instant's reachability verdict.
restart_storm_check() {
  if [ "${IMAG_OBS_RESTART_STORM_ENABLE:-0}" != "1" ]; then
    log "restart-storm check disabled (IMAG_OBS_RESTART_STORM_ENABLE!=1) -- skipping"
    return 0
  fi

  local now probe
  now="${IMAG_OBS_RESTART_NOW:-$(date +%s)}"
  if [ -n "${IMAG_OBS_RESTART_PROBE_CMD:-}" ]; then
    probe="$(eval "$IMAG_OBS_RESTART_PROBE_CMD" 2>/dev/null || true)"
  else
    probe="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
      "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_obs_restart_counter_probe_cmd)" 2>/dev/null || true)"
  fi

  case "$probe" in
    *NRestarts=*) : ;;
    *)
      log "restart-storm: could not read imag-obs NRestarts (probe='$probe') -- nothing to decide this pass"
      return 0
      ;;
  esac

  local prev_baseline prev_ts cls storm baseline baseline_ts reason
  prev_baseline="$(read_state_field restart_baseline "")"
  prev_ts="$(read_state_field restart_baseline_ts "")"
  cls="$(imag_obs_restart_storm_classify "$prev_baseline" "$prev_ts" "$probe" "$now" "$RESTART_STORM_THRESHOLD" "$RESTART_STORM_WINDOW_S")"
  storm="$(printf '%s\n' "$cls" | sed -n 's/^storm=//p')"
  baseline="$(printf '%s\n' "$cls" | sed -n 's/^baseline=//p')"
  baseline_ts="$(printf '%s\n' "$cls" | sed -n 's/^baseline_ts=//p')"
  reason="$(printf '%s\n' "$cls" | sed -n 's/^reason=//p')"
  write_state_field restart_baseline "${baseline:-}"
  write_state_field restart_baseline_ts "${baseline_ts:-}"
  log "restart-storm: ${probe} storm=${storm:-0} (${reason})"

  if [ "${storm:-0}" != "1" ]; then
    # not storming -> clear the throttle signature so a LATER genuine storm always pages fresh.
    write_state_field restart_alert_sig ""
    write_state_field restart_alert_passes 0
    return 0
  fi

  local sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  sig="imag-restart-storm"
  prior_sig="$(read_state_field restart_alert_sig "")"
  prior_passes="$(read_state_field restart_alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$sig" "$prior_sig" "$prior_passes" "$RESTART_STORM_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field restart_alert_sig "$new_sig"
  write_state_field restart_alert_passes "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: imag-obs RESTART STORM (${reason}) alert_now=${alert_now}"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for imag-obs restart storm"
    python3 "$NOTIFY" notify --body \
      "🚨 imag OBS reštart-storm ($REPO_SLUG): imag-obs.service sa reštartuje v slučke (${reason}). Pravdepodobne chýbajúci python sibling po deployi. Rieši Claude automaticky, ty nemusíš nič robiť." \
      --dedup-key "imag-obs-restart-storm" \
      >/dev/null 2>&1 || log "restart-storm ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "restart-storm alert suppressed by throttle (pass ${prior_passes}/${RESTART_STORM_THROTTLE_PASSES})"
  fi
  return 0
}
# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, threshold=$CONFIRM_THRESHOLD)"
  restart_storm_check

  measure
  if [ -z "${PROBE_OUT:-}" ]; then
    log "ERROR: no probe output from imag-nb (ssh/connectivity failure) -- nothing to decide this pass"
    return 0
  fi

  local msg wedged prev_confirm decision confirm act
  msg="$(imag_obs_reachability_message "$PROBE_OUT")"
  wedged=0
  [ -n "$msg" ] && wedged=1
  log "imag-nb: probe='$PROBE_OUT' wedged=$wedged"

  prev_confirm="$(read_state_field confirm 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field confirm "${confirm:-0}"
  log "confirm=$prev_confirm -> $confirm act=$act"

  # A genuinely HEALTHY pass (obs reachable) also clears the throttle signature -- otherwise a
  # NEW outage that happens to produce the SAME message text as a PREVIOUS, already-recovered
  # episode would be silently throttled by the stale signature instead of alerting fresh.
  if [ "$wedged" -eq 0 ]; then
    write_state_field alert_sig ""
    write_state_field alert_passes 0
    # #1070: OBS is up -> also run the REPORT-ONLY latency-pin verify-at-start for imag (the
    # "outside a gate run" residual of 1061). Never overwrites; a drift fires its own throttled
    # Discord report. Only meaningful when OBS is up (it reads pins over WS), hence here.
    latency_drift_check
    log "pass end"
    return 0
  fi

  # #788: OBS is down -- distinguish a DELIBERATE operator quit from a real crash before alerting.
  # Only meaningful when the OBS PROCESS is ABSENT (OBS_PORT_NOT_LISTENING = process up, not a
  # quit). The authoritative signal is systemd's own Restart=on-failure verdict on imag-obs.service
  # (issue 882): a clean exit(0) / `systemctl --user stop` -> loaded/inactive/success; a crash-loop
  # -> failed. A fresh operator pause file is an explicit override. Fail-safe: anything we cannot
  # PROVE deliberate (ssh failure, unreadable unit, `failed`, `activating`, ...) falls through to
  # the existing alarm ("bez clean markera = pád -> alarm ako doteraz"). The `|| dd_rc=$?` capture
  # keeps the second ssh from aborting this pass under the sourced libs' `set -e`.
  case "$PROBE_OUT" in
    *OBS_PROCESS_ABSENT*)
      local dd_probe dd_rc dd_verdict dd_deliberate dd_reason
      dd_rc=0
      dd_probe="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
        "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_obs_deliberate_down_probe_cmd "$PAUSE_FILE" "$PAUSE_WINDOW_S")" 2>/dev/null)" || dd_rc=$?
      dd_verdict="$(imag_obs_down_is_deliberate "$dd_probe")"
      dd_deliberate="$(printf '%s\n' "$dd_verdict" | sed -n 's/^deliberate=//p')"
      dd_reason="$(printf '%s\n' "$dd_verdict" | sed -n 's/^reason=//p')"
      if [ "${dd_deliberate:-0}" = "1" ]; then
        log "imag-nb OBS is down but this is a DELIBERATE operator quit ($dd_reason) -- NOT alerting (issue 788: never false-'crashed' on an operator quit)"
        # A deliberate quit is a clean, intentional state -- treat it exactly like a HEALTHY pass
        # for alert bookkeeping: clear the confirm counter AND the throttle signature, so a LATER
        # genuine crash always alerts fresh (never masked by a stale signature from a prior episode).
        write_state_field confirm 0
        write_state_field alert_sig ""
        write_state_field alert_passes 0
        log "pass end"
        return 0
      fi
      log "imag-nb OBS is down and NOT a deliberate quit ($dd_reason; probe2 ssh rc=$dd_rc, out='$(printf '%s' "$dd_probe" | tr '\n' ' ')') -- proceeding to alert"
      ;;
  esac

  if [ "${act:-0}" != "1" ]; then
    log "pass end"
    return 0
  fi

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="imag:${PROBE_OUT}"
  prior_sig="$(read_state_field alert_sig "")"
  prior_passes="$(read_state_field alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field alert_sig "$new_sig"
  write_state_field alert_passes "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: imag-nb CONFIRMED down ($msg) alert_now=$alert_now"
    log "pass end"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for imag-nb"
    python3 "$NOTIFY" notify --body \
      "🚨 imag OBS ($REPO_SLUG): OBS na imag-nb je DOLE. ${msg} Rieši Claude automaticky, ty nemusíš nič robiť." \
      --dedup-key "imag-obs-down" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi

  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
