#!/usr/bin/env bash
# scripts/measurement-audio-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so it runs
# with `-e` OFF -- the sibling convention (dantesync-clock / audio-lag / genlock-lock / splitter-port
# alert watchdogs all use `set -uo pipefail`, NOT -e). The `set -euo pipefail` above satisfies the
# new-.sh script-failure-policy check; this turns -e back off so one fetch/decision hiccup never
# aborts the whole pass.
set +e
set -uo pipefail
#
# scripts/measurement-audio-alert-watchdog.sh -- #1310: dev1-side ALERT watchdog for the mbc
# MEASUREMENT-AUDIO chain reading DIGITAL SILENCE between E2E runs.
#
# WHY (#1310, owner directive 2026-09-13 „stale nevidim qrkod a zvuk z cam2" + the #1308 class „veci
# bez ktorych nevie produkcia bezat spravne musi notifikovat ... byt o tom dokolecka notifikovany"):
# the mbc chain (cam2 HDMI monitor speaker plays the QPSK marker -> measurement mic -> mbc Ableton on
# 10.77.7.232 -> Dante Virtual Soundcard -> stream OBS ASIO input `mbc`) is the instrument the whole
# A/V-sync leg reads. After a production it can go DIGITAL-SILENT (mic off, Ableton mbc channel muted,
# Dante route dropped) and NOTHING pages it -- the only mbc-silence check today is the in-RUN #748
# preflight (`recording-e2e.sh [4b2/8]`), so a silent chain is invisible until the next full ~300 s
# E2E burns a cycle discovering it (release E2E 34764817477 aborted at `max_volume -91.0 dB`, `n=120`
# samples flowing but all zeros; the 2026-07-12 „mutnutý mikrofón prežil týždeň" incident). This
# watchdog closes that between-run gap.
#
# HOW: every 60 s (dev1 systemd timer) read the `mbc` input's peak LEVEL off stream OBS via the
# obs-websocket InputVolumeMeters event (scripts/measurement_audio_meter_probe.py -- NO recording, no
# disk, no rig mutation), classify SILENT vs PRESENT at the SAME -60 dB bar as the #748 preflight
# (sourced from scripts/lib/audio-presence-preflight.sh, NEVER retyped), and page
# MEASUREMENT_AUDIO_SILENT after a 2-pass confirm. The VERDICT is decided by the PURE
# scripts/measurement_audio_decision.py (pytest Tier-0, the #1199 python-mirror pattern).
#
# TEST-PREMISE (gated on rig-mode-state.sh like splitter-port #1290): the QPSK marker only sounds in
# TEST mode, so in EVENT/production a silent mbc is EXPECTED -> SKIP the whole check (no page). TEST or
# UNKNOWN -> proceed (fail-safe: an unreadable mode never silences a real TEST-mode fault).
#
# PRODUCTION-CRITICAL RE-PING (#1308, owner ruling): a persisting SILENT fault re-pings „dokolecka"
# via a TIME-BUCKETED airuleset --dedup-key (measurement-audio-stream-<floor(now/REPING_INTERVAL_S)>,
# 600 s default, floor 60), built by the ONE shared watchdog_notify_key helper. Within a bucket an
# identical state EDITS the card (no flood); every new bucket is a fresh ping. Recovery is ONE
# machine-channel log line, never a phone ping (#1206).
#
# DETECTION ONLY -- no auto-action (the cure is a rig-ops decision: unmute the mic/Ableton channel,
# fix the Dante route). SAME dev1 topology as audio-lag (#1226) / dantesync-clock (#1307).
#
# Usage:
#   scripts/measurement-audio-alert-watchdog.sh            # one pass: probe -> decide -> alert
#   scripts/measurement-audio-alert-watchdog.sh --dry-run  # probe + decide + LOG only; never alert
#   scripts/measurement-audio-alert-watchdog.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/rig-mode-state.sh
. "$HERE/lib/rig-mode-state.sh"
# shellcheck source=scripts/lib/audio-presence-preflight.sh
. "$HERE/lib/audio-presence-preflight.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '12,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "measurement-audio-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The mbc chain lives ONLY on the stream box; obs_fleet_host is the single IP source of truth.
WS_HOST="${MEASUREMENT_AUDIO_WS_HOST:-$(obs_fleet_host stream 2>/dev/null || echo 10.77.9.204)}"
WS_PORT="${MEASUREMENT_AUDIO_WS_PORT:-4455}"
INPUT_NAME="${MEASUREMENT_AUDIO_INPUT:-mbc}"

# 2-pass confirm before the FIRST page (matches the siblings): a single blipped reading (a meter
# hiccup, a one-window renegotiation) must never fire. A genuine silence persists across the cadence.
CONFIRM_THRESHOLD="${MEASUREMENT_AUDIO_CONFIRM_THRESHOLD:-2}"
# Production-critical re-ping bucket (owner ruling): while the fault persists, re-ping every
# REPING_INTERVAL_S. The shared helper floors this at 60 s.
REPING_INTERVAL_S="${MEASUREMENT_AUDIO_REPING_INTERVAL_S:-600}"

DECIDE="${MEASUREMENT_AUDIO_DECIDE:-$HERE/measurement_audio_decision.py}"
PROBE="${MEASUREMENT_AUDIO_PROBE:-$HERE/measurement_audio_meter_probe.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${MEASUREMENT_AUDIO_ALERT_REPO:-zbynekdrlik/camera-box}"

# cam2 rig-mode (EVENT/TEST) probe -- the SAME ssh shape + credential the splitter-port sibling uses.
SSH_USER="${MEASUREMENT_AUDIO_SSH_USER:-root}"
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${MEASUREMENT_AUDIO_SSH_TIMEOUT:-8}"
RIG_MODE_PAINTER_IP="${RIG_MODE_PAINTER_IP:-10.77.9.62}"
RIG_MODE_PAINTER_PIDFILE="${RIG_MODE_PAINTER_PIDFILE:-/run/rig-painter.pid}"
RIG_MODE_PAINTER_SERVICE="${RIG_MODE_PAINTER_SERVICE:-cam2-painter.service}"
RIG_MODE_PROBE_TIMEOUT="${RIG_MODE_PROBE_TIMEOUT:-20}"

STATE_DIR="${MEASUREMENT_AUDIO_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-measurement-audio-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-measurement-audio-alert-dryrun.state"
STATE_FILE="${MEASUREMENT_AUDIO_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [measurement-audio-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_meter <host> <port> -> prints the probe's key=value stdout, returns 0 iff the WS connected +
# handshook (box reachable). A connect failure returns 1 (box_reachable=0 -> SKIP, defer #1001/#732).
# MEASUREMENT_AUDIO_FETCH_CMD (Tier-0 seam, mirrors DANTE_CLOCK_FETCH_CMD): when set it is invoked as
# `<cmd> <host> <port>` and its stdout+exit REPLACE the real probe -- so a --dry-run against a stub
# needs no live box.
fetch_meter() {
  local host="$1" port="$2"
  if [ -n "${MEASUREMENT_AUDIO_FETCH_CMD:-}" ]; then
    "$MEASUREMENT_AUDIO_FETCH_CMD" "$host" "$port"
    return $?
  fi
  python3 "$PROBE" "$host" "$port" --input "$INPUT_NAME"
}

# rig_mode_probe -> stdout: the cam2 painter snapshot (RIG_MODE_PROBE_OK + the four painter KEY|value
# lines), or empty on an ssh failure/timeout -> rig_mode_from_painter_snapshot UNKNOWN (fail-safe:
# proceed as TEST). ONE ssh to cam2 per pass. `timeout` sits INSIDE sshpass so the driver tests'
# `sshpass()` FUNCTION stub still intercepts the whole call (a `timeout sshpass …` would bypass the
# stub and reach the live rig -- .claude/rules/rig-mode-event-gate.md / win-ssh-vs-mcp #1290).
# Overridden wholesale by the driver tests.
rig_mode_probe() {
  sshpass -p "$CAM_PW" timeout "$RIG_MODE_PROBE_TIMEOUT" \
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no "${SSH_USER}@${RIG_MODE_PAINTER_IP}" \
    "$(rig_mode_state_probe_remote_snippet "$RIG_MODE_PAINTER_PIDFILE" "$RIG_MODE_PAINTER_SERVICE")" 2>/dev/null || true
}

# -- persisted per-key state (key=value lines) -- verbatim shape from the sibling watchdogs ---------
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
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# now_epoch -> seconds since the epoch (injectable for tests via MEASUREMENT_AUDIO_NOW).
now_epoch() { printf '%s' "${MEASUREMENT_AUDIO_NOW:-$(date +%s)}"; }

# fire_alert <dedup-base> <body...> -- the ONE ALERT emit seam. The --dedup-key is TIME-BUCKETED via
# the shared watchdog_notify_key helper (owner ruling #1308: re-ping while it persists; airuleset
# edits the card within a bucket). watchdog_notify_key MUST stay on this notify line (the #1206 sweep
# `test_production_critical_watchdogs_actually_bucket_their_inline_key` requires it).
fire_alert() {
  local base="$1"; shift
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert (dedup base=$base, bucket=${REPING_INTERVAL_S}s): $*"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$*" \
    --dedup-key "$(watchdog_notify_key "$base" "$(now_epoch)" "$REPING_INTERVAL_S")" \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# confirm_then_alert <state-key-suffix> <fault 0|1> <dedup-base> <body...>
#   2-pass confirm on the fault; latches `alerted_<suffix>` for recovery; once confirmed, calls
#   fire_alert EVERY pass (the time-bucket, not a per-pass throttle, controls the re-ping cadence). A
#   cleared fault logs a machine-channel recovery once (#1206), never a phone ping.
confirm_then_alert() {
  local suffix="$1" fault="$2" base="$3"; shift 3
  local prev decision confirm act
  prev="$(read_state_field "confirm_${suffix}" 0)"
  decision="$(obs_watchdog_confirm "$prev" "$fault" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${suffix}" "${confirm:-0}"

  if [ "$fault" != "1" ]; then
    local was_alerted
    was_alerted="$(read_state_field "alerted_${suffix}" 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: ${suffix} measurement audio present again"
      else
        log "RECOVERY: ${suffix} measurement audio PRESENT again -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${suffix}" 0
    fi
    return 0
  fi

  if [ "${act:-0}" != "1" ]; then
    log "${suffix} SILENT this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "alerted_${suffix}" 1
  fire_alert "$base" "$@"
}

# require_tools -> loud exit if a REQUIRED tool / module is missing (a missing python3 -> the probe
# never runs -> every pass SKIP -> SILENT FOREVER; a missing threshold getter -> analyze can't grade;
# exactly the "a missing dependency must fail LOUD by name" class, #833).
require_tools() {
  local missing=() t
  for t in python3 sshpass; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (would SKIP every pass and never page a real silent mbc chain)"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (analyze would emit nothing -> 'holding' forever; fix MEASUREMENT_AUDIO_DECIDE)"
    return 1
  fi
  if ! declare -F audio_preflight_default_threshold_db >/dev/null 2>&1; then
    log "FATAL: audio_preflight_default_threshold_db not sourced -- refusing to run (cannot resolve the -60 dB silence bar; check scripts/lib/audio-presence-preflight.sh)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, confirm=$CONFIRM_THRESHOLD, reping=${REPING_INTERVAL_S}s, ws=${WS_HOST}:${WS_PORT}, input='$INPUT_NAME')"
  require_tools || { log "pass end (aborted: missing required tools/modules)"; return 3; }

  # -- EVENT gate (TEST-premise, #1290): the QPSK marker only sounds in TEST mode. -----------------
  local rig_mode_snapshot rig_mode threshold_db
  rig_mode_snapshot="$(rig_mode_probe)"
  rig_mode="$(rig_mode_from_painter_snapshot "$rig_mode_snapshot")"
  log "rig mode (cam2 painter probe @ $RIG_MODE_PAINTER_IP): $rig_mode"
  if [ "$rig_mode" = "EVENT" ]; then
    log "rig in EVENT mode -- the QPSK marker is silent by design; TEST-premise check skipped, no page (#1290). Clearing any latch."
    confirm_then_alert "stream" 0 "measurement-audio-stream"
    log "pass end (EVENT skip)"
    return 0
  fi
  # TEST or UNKNOWN -> proceed (fail-safe: an unreadable mode never silences a real TEST-mode fault).

  # -- the -60 dB silence bar, SOURCED from the #748 lib (never retyped here) ----------------------
  threshold_db="$(audio_preflight_default_threshold_db)"

  # -- probe the mbc peak level off stream OBS (no recording) --------------------------------------
  local body reachable out verdict peak present
  if body="$(fetch_meter "$WS_HOST" "$WS_PORT")"; then
    reachable=1
  else
    reachable=0; body=""
  fi
  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" --threshold-db "$threshold_db" 2>/dev/null)"
  verdict="$(printf '%s\n' "$out" | sed -n 's/^verdict=//p')"
  peak="$(printf '%s\n' "$out" | sed -n 's/^peak_db=//p')"
  present="$(printf '%s\n' "$out" | sed -n 's/^meter_present=//p')"
  log "stream ($WS_HOST:$WS_PORT): reachable=$reachable verdict=${verdict:-<none>} peak_db=${peak:-} meter_present=${present:-}"

  case "$verdict" in
    SKIP)
      log "stream OBS :$WS_PORT not reachable this pass -- box/WS-down is #1001/#732 territory; holding, no page"
      return 0
      ;;
    UNKNOWN)
      log "stream reachable but input '$INPUT_NAME' carried no meter reading (renamed/removed input, or InputVolumeMeters unavailable) -- no reading, holding, no page"
      return 0
      ;;
    PRESENT)
      confirm_then_alert "stream" 0 "measurement-audio-stream"
      ;;
    SILENT)
      confirm_then_alert "stream" 1 "measurement-audio-stream" \
        "🚨 Meracia audio ($REPO_SLUG): stream OBS vstup **$INPUT_NAME** číta DIGITÁLNE TICHO (peak ${peak} dB < ${threshold_db} dB; ticho je ~-91 dB, živý QPSK marker ~-5 dB). Meracia mbc cesta je mŕtva -- A/V-sync gate na nej stojí, takže ďalšia produkcia sa neoverí. Skontroluj v poradí: (1) je meracie mikrofón zapnutý pri reproduktore cam2 monitora? (2) je mbc kanál v Ableton Live na 10.77.7.232 ODMUTOVANÝ? (3) je Dante routing z mbc do DVS -> stream OBS v poriadku? (targets.md mbc riadok má checklist). Potvrdené počas ${CONFIRM_THRESHOLD} kontrol; re-ping každých ~$((REPING_INTERVAL_S/60)) min kým to trvá."
      ;;
    *)
      log "stream: unexpected verdict '${verdict:-<empty>}' from measurement_audio_decision.py (analyze failed?) -- holding, no page"
      ;;
  esac
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
