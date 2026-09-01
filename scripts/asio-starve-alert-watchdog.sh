#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/cadence-alert-watchdog.sh /
# scripts/frozen-input-alert-watchdog.sh / scripts/network-reach-alert-watchdog.sh (set -uo pipefail,
# not -e).
#
# scripts/asio-starve-alert-watchdog.sh -- #1023: ASIO-SOURCE-STARVED alert, DEV1-SIDE. A sibling of
# the dev1 alert-watchdog family (network-reach issue 1001 / frozen-input issue 1052 / bundle-state
# issue 732 / cadence issue 794).
#
# WHY (#1023): when stream OBS starts BEFORE its ASIO device/matrix is ready, an ASIO source connects
# but its audio callback perpetually STARVES (no samples) -> the source is silent and only an OBS
# reset fixes it. Reproduced LIVE 2026-08-17: 'ASIO Input Capture' read starved_blocks≈2946 EVERY
# 60 s interval for 11.5 h while 'mbc' read 0 -- exactly this defect, live on the stream box. The
# closed VB-Matrix/Dante ASIO plugin is UPSTREAM of the vendored genlock build (no obs-asio plugin
# in vendor/), so this is not fixable in our code; this watchdog DETECTS the signature and pages with
# the OBS-reset cure (alert-only, like obs-liveness #391 / obs-session #979).
#
# TAP: the stream OBS log prints `HH:MM:SS.mmm: asrc: source '<name>' … starved_blocks=N (#803/#806/#960)`
# once per ASRC_LOG_INTERVAL_S (=60 s, vendor/obs-studio/libobs/obs-source.c). `starved_blocks=N` is
# PER-INTERVAL (reset-on-read, asrc-compensator.c) -- so the NEWEST line's value is a self-contained
# 60 s measurement (no prev/curr delta needed, unlike the #794/#1052 `received=` counter).
#
# THE DISCRIMINATOR -- per-source, symmetric: a watched source pages STARVED only when its
# starved_blocks sits >= threshold AND at least one OTHER watched source is proven HEALTHY (~0). A
# healthy sibling proves the box's clock/OBS/audio subsystem is fine and the starvation is
# source-specific (the exact "started before the matrix" defect), not a box-wide outage (which
# obs-liveness #391 / audio-presence own -- never double-paged here). Any listed source can be the
# starved one; its siblings are the reference. The 2-pass confirm handles a single transient.
#
# The confirm-counter + alert throttle are the SAME shared obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use -- no
# second alert mechanism. A recovery ("receiving audio again") ping fires once when a source we paged
# for reads OK again.
#
# NO-DOUBLE-PAGE GUARD: before deciding, read issue-1001's OWN on-disk state (never re-probe) -- if
# the stream box is CONFIRMED unreachable there, issue-1001 already owns the page and a starved read
# is out of scope (the OBS log can't be read anyway) -> SKIP every source.
#
# FAIL-LOUD TOOL PREFLIGHT: a missing `sshpass`/`ssh`/`timeout` on dev1 would otherwise make every
# probe read empty forever (a permanently-blind watchdog that still looks green) -- the "a missing
# tool must fail LOUD by name, never read as a measured zero" class
# (.claude/rules/imag-ssh-remote-tool-preflight.md, #833). require_tools aborts the pass loudly.
#
# BEST-EFFORT PROBE: ONE flat `ssh ... powershell` OBS-log tail per PASS -- NOT per source. The tail
# carries every watched source's asrc line, so one fetch + N local greps is identical to N fetches; a
# session-agnostic file read, allowed for a headless dev1 watchdog per win-ssh-vs-mcp; NEVER nested
# PowerShell (`$env:APPDATA` has no spaces -> no inner double-quotes). A failed/absent read yields an
# empty sample -> the pure seam returns UNKNOWN -> never a false page. Override the whole read with
# ASIO_STARVE_PROBE_CMD (run with <box_ip>, stdout = RAW log text) for a --dry-run smoke test.
#
# Usage:
#   scripts/asio-starve-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/asio-starve-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/asio-starve-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/asio-starve-health.sh
. "$HERE/lib/asio-starve-health.sh"
# shellcheck source=scripts/lib/ps-encoded.sh
. "$HERE/lib/ps-encoded.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,55p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "asio-starve-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The box whose OBS log carries the per-source `asrc: … starved_blocks=` lines, as "name|ip". The
# ASIO inputs ('mbc' on Dante VSC, 'ASIO Input Capture' on VB-Matrix VASIO-8) live on the STREAM box.
ASIO_STARVE_BOX="${ASIO_STARVE_BOX:-stream|10.77.9.204}"
ASIO_STARVE_NAME="${ASIO_STARVE_BOX%%|*}"; ASIO_STARVE_IP="${ASIO_STARVE_BOX##*|}"
# Watched REAL ASIO source names, ';'-separated (names contain spaces). Default = the two real inputs;
# the synthetic asrc repro sources ('test-audio' / 'fallback repro', which also starve by design) are
# EXCLUDED simply by not being listed. >=2 sources must be listed for the healthy-sibling
# discriminator to work (a lone source can never prove its own starvation is source-specific).
ASIO_STARVE_SOURCES="${ASIO_STARVE_SOURCES:-ASIO Input Capture;mbc}"
# starved_blocks >= this (per 60 s interval) counts as STARVED. Observed live: healthy = 0, defect ≈
# 2946 (≈100 % of ~2900 callbacks/60 s), so 1000 (≈34 %) cleanly separates a badly-broken source from
# incidental noise with huge margin both ways. Env-tunable for later live calibration.
ASIO_STARVE_THRESHOLD="${ASIO_STARVE_THRESHOLD:-1000}"

SSH_USER="${ASIO_STARVE_SSH_USER:-newlevel}"
SSH_PW="${ASIO_STARVE_SSH_PW:-newlevel}"
SSH_TIMEOUT="${ASIO_STARVE_SSH_TIMEOUT:-20}"
SSH_OPTS="${ASIO_STARVE_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"
# asrc lines emit once/60 s/source amid ~100 other audit lines/min; 1200 lines spans several asrc
# intervals so the newest asrc line per source is captured (and is <=60 s old).
OBS_LOG_TAIL="${ASIO_STARVE_OBS_LOG_TAIL:-1200}"

# 2-pass confirm before paging: a single transient high read must never page. A genuinely starved
# source stays starved across the cadence.
CONFIRM_THRESHOLD="${ASIO_STARVE_ALERT_CONFIRM_THRESHOLD:-2}"
# ~30 min re-alert at the 5-min cadence (a reminder, not a ping every pass while it persists).
ALERT_THROTTLE_PASSES="${ASIO_STARVE_ALERT_THROTTLE_PASSES:-6}"
# A source whose asrc line is ABSENT every pass (renamed / dropped from the scene, or an
# ASIO_STARVE_SOURCES drift) stays UNKNOWN forever and never pages while the watchdog looks green --
# the "silent unknown" the standing rig-degradation rule forbids. Fire ONE "tap broken" WARN past
# this many CONSECUTIVE blind passes. Default ~2 h at the 5-min cadence.
TAP_BROKEN_THRESHOLD="${ASIO_STARVE_TAP_BROKEN_THRESHOLD:-24}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${ASIO_STARVE_ALERT_REPO:-zbynekdrlik/camera-box}"
# The OBS-reset cure the ticket confirms ("only reset OBS helps"). Alert-only: a dev1 timer has no
# session-aware win-* MCP to restart the live stream OBS, and an unattended restart of the broadcast
# output is not done -- the alert embeds the ready-to-run recovery plan instead (obs-liveness #391).
RECOVERY_PLAN="${ASIO_STARVE_RECOVERY_PLAN:-scripts/launch-obs-genlock.sh --box $ASIO_STARVE_NAME --force}"

STATE_DIR="${ASIO_STARVE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-asio-starve-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-asio-starve-alert-dryrun.state"
STATE_FILE="${ASIO_STARVE_ALERT_STATE_FILE:-$_state_default}"
# Issue-1001's OWN state file -- read (never written) for the no-double-page guard.
NETREACH_STATE_FILE="${ASIO_STARVE_NETREACH_STATE_FILE:-$STATE_DIR/camera-box-network-reach-alert.state}"

# EXPECTED_LIVE is 1 for every configured source (the list is the scope). Kept as a seam input.
EXPECTED_LIVE=1

log() { printf '%s [asio-starve-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# require_tools -> non-zero (loud) if a REQUIRED external tool is missing on dev1. `sshpass`/`ssh`/
# `timeout` are required only for the real probe -- an ASIO_STARVE_PROBE_CMD override (--dry-run /
# stub) needs none of them.
require_tools() {
  local missing=() t need=()
  [ -z "${ASIO_STARVE_PROBE_CMD:-}" ] && need+=("sshpass" "ssh" "timeout")
  for t in "${need[@]}"; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing tool would read every probe as empty forever, a permanently-blind watchdog)"
    return 1
  fi
  return 0
}

# -- I/O probe (dev1-local; NOT pure -- kept out of the lib) -------------------------------------
# fetch_box_log <box_ip> -> stdout: the RAW newest-OBS-log tail (ALL sources' asrc lines), or EMPTY
# on a failed/absent read. Called ONCE per pass. Overridable via ASIO_STARVE_PROBE_CMD (run with
# <box_ip>, stdout = raw log text) for a --dry-run smoke test or an alternate tap.
fetch_box_log() {
  local ip="$1"
  if [ -n "${ASIO_STARVE_PROBE_CMD:-}" ]; then
    $ASIO_STARVE_PROBE_CMD "$ip" 2>/dev/null || true
    return 0
  fi
  # #1259: invoke PowerShell via -EncodedCommand (base64 UTF-16LE), NEVER the naive
  # -Command "…| sort …| select …". Win32-OpenSSH's default cmd.exe shell leaks the unescaped `|`
  # pipes -> a mangled/blind read (the issue-1258 root cause). ps_encoded_command
  # (scripts/lib/ps-encoded.sh) encodes the tail command to a pure-ASCII blob cmd.exe cannot touch;
  # an empty encode -> empty read -> UNKNOWN, never an abort. Sourcing the whole tail; grep on dev1.
  local _enc _tail
  _tail="$(ps_clamp_numeric "$OBS_LOG_TAIL" 1200)" # #1259: guard the env count before the payload
  _enc="$(ps_encoded_command "gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $_tail")"
  # shellcheck disable=SC2086
  timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" \
    "powershell -NoProfile -NonInteractive -EncodedCommand $_enc" \
    2>/dev/null || true
}

# -- issue-1001 reachability read (never re-probed) ---------------------------------------------
netreach_box_alerted() {
  local box="$1"
  [ -f "$NETREACH_STATE_FILE" ] || { printf '0'; return 0; }
  local v
  v="$(sed -n "s/^alerted_${box}=//p" "$NETREACH_STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-0}"
}

# -- persisted per-source state (key=value lines) -----------------------------------------------
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

# A source key safe for state-field names (source names carry spaces / punctuation). A short cksum of
# the RAW name is appended so two distinct names that sanitize identically never share state.
source_key() {
  local san sum
  san="$(printf '%s' "$1" | tr -c 'A-Za-z0-9' '_')"
  sum="$(printf '%s' "$1" | cksum | cut -d' ' -f1)"
  printf '%s_%s' "$san" "$sum"
}

# An OK sample is not an incident: clear its confirm counter + throttle sig so a genuinely NEW
# starvation later pages fresh. Does NOT clear the `alerted` latch (the recovery-ping latch).
clear_source_throttle() {
  local k="$1"
  write_state_field "confirm_${k}" 0
  write_state_field "alert_sig_${k}" ""
  write_state_field "alert_passes_${k}" 0
}

# -- per-source decision ------------------------------------------------------------------------
# handle_source <source> <blocks> <healthy_sibling 0|1> <box_reachable 0|1>
#   <blocks> is this source's newest starved_blocks (already parsed from the once-fetched log), or
#   empty when its asrc line was absent. <healthy_sibling> = does ANY OTHER watched source read
#   healthy this pass.
handle_source() {
  local source="$1" blocks="$2" healthy_sibling="$3" box_reachable="$4"
  local k verdict usable unk tap_alerted
  k="$(source_key "$source")"

  verdict="$(asio_starve_classify "$blocks" "$ASIO_STARVE_THRESHOLD" "$healthy_sibling" \
    "$([ "$box_reachable" = "1" ] && printf '%s' "$EXPECTED_LIVE" || printf '0')")"
  log "'$source' on $ASIO_STARVE_NAME: blocks=${blocks:-<none>} thr=$ASIO_STARVE_THRESHOLD healthy_sibling=$healthy_sibling reachable=$box_reachable -> $verdict"

  # A USABLE sample needs a real starved_blocks integer (proves the tap works this pass, even when
  # the verdict is UNKNOWN from an all-starving/no-healthy-sibling pass). Only meaningful when we
  # actually PROBED (box_reachable=1); a SKIP pass (box down) leaves the tap counter untouched.
  usable=0
  case "$blocks" in '' | *[!0-9]*) : ;; *) usable=1 ;; esac

  if [ "$box_reachable" = "1" ]; then
    if [ "$usable" = 1 ]; then
      write_state_field "unknown_${k}" 0
      write_state_field "tap_broken_${k}" 0
    else
      unk="$(read_state_field "unknown_${k}" 0)"
      case "$unk" in '' | *[!0-9]*) unk=0 ;; esac
      unk=$((unk + 1))
      write_state_field "unknown_${k}" "$unk"
      tap_alerted="$(read_state_field "tap_broken_${k}" 0)"
      if [ "$unk" -ge "$TAP_BROKEN_THRESHOLD" ] && [ "$tap_alerted" != "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD alert: '$source' tap BROKEN (no asrc sample for $unk consecutive passes)"
        else
          log "ALERT: firing Discord notification for '$source' tap broken"
          python3 "$NOTIFY" notify --body \
            "⚠️ #1023 asio-starve: no \`asrc: source '$source'\` sample on $ASIO_STARVE_NAME for $unk consecutive passes ($REPO_SLUG). The ASIO-starve TAP for this source is BLIND (source renamed / dropped from the scene, or ASIO_STARVE_SOURCES drifted) -- silent-audio coverage for '$source' is OFF until fixed." \
            --dedup-key "asio-starve-tap-$source" \
            >/dev/null 2>&1 || log "tap-broken: airuleset.py notify failed (non-fatal)"
        fi
        write_state_field "tap_broken_${k}" 1
      fi
    fi
  fi

  # OK sample -> recovery path (fire one recovery ping if we had paged), clear throttle.
  if [ "$verdict" = "OK" ]; then
    local was_alerted recover
    was_alerted="$(read_state_field "alerted_${k}" 0)"
    recover="$(asio_starve_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
    if [ "$recover" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: '$source' receiving audio again (starved_blocks=${blocks:-0})"
      else
        log "RECOVERY: '$source' OK again (receiving audio, starved_blocks=${blocks:-0}/60s) -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${k}" 0
    fi
    clear_source_throttle "$k"
    return 0
  fi

  # Only STARVED feeds the confirm counter; UNKNOWN / SKIP reset it (a broken streak must restart).
  local is_starved=0
  [ "$verdict" = "STARVED" ] && is_starved=1
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${k}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$is_starved" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${k}" "${confirm:-0}"

  if [ "$verdict" != "STARVED" ]; then
    log "'$source' $verdict this pass -- not a confirmed starvation, holding"
    return 0
  fi
  log "'$source' confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "'$source' STARVED this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED starved -> latch the recovery flag, then throttle-dedup on a STABLE signature (the
  # source name; the value is noisy per-interval so it is not part of the signature).
  write_state_field "alerted_${k}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="starved:${k}"
  prior_sig="$(read_state_field "alert_sig_${k}" "")"
  prior_passes="$(read_state_field "alert_passes_${k}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${k}" "$new_sig"
  write_state_field "alert_passes_${k}" "$new_passes"

  detail="$(asio_starve_alert_detail "$source" "$blocks" "$ASIO_STARVE_THRESHOLD")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: '$source' CONFIRMED starved ($detail) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for '$source' starved"
    python3 "$NOTIFY" notify --body \
      "🚨 #1023 asio-starve: **$detail** ($REPO_SLUG). Confirmed over ${CONFIRM_THRESHOLD} consecutive passes -- the ASIO source is silent (OBS likely started before the ASIO device/matrix was ready). LIEK: resetni (reštartni) OBS na $ASIO_STARVE_NAME: \`$RECOVERY_PLAN\`" \
      --dedup-key "asio-starve-$source" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, box='$ASIO_STARVE_BOX', threshold=$ASIO_STARVE_THRESHOLD, sources='$ASIO_STARVE_SOURCES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  # No-double-page guard: the stream box must be reachable per issue-1001's OWN on-disk state, else
  # issue-1001 already owns the page (and the OBS log can't be read anyway) -> every source SKIPs.
  local box_down box_reachable raw_log=""
  box_down="$(netreach_box_alerted "$ASIO_STARVE_NAME")"
  if [ "$box_down" = "1" ]; then
    box_reachable=0
    log "issue-1001 state: $ASIO_STARVE_NAME down=$box_down -> #1001 owns the page, SKIP all sources this pass"
  else
    box_reachable=1
    raw_log="$(fetch_box_log "$ASIO_STARVE_IP")"
  fi

  # Parse each watched source's newest starved_blocks ONCE from the shared log, then derive per-source
  # health so each source's healthy-sibling flag = "is ANY OTHER watched source healthy this pass".
  declare -A SRC_BLOCKS SRC_HEALTHY
  local old_ifs="$IFS" source k n_healthy=0
  local -a ORDER=()
  set -f
  IFS=';'
  for source in $ASIO_STARVE_SOURCES; do
    IFS="$old_ifs"
    source="${source#"${source%%[![:space:]]*}"}"   # ltrim
    source="${source%"${source##*[![:space:]]}"}"    # rtrim
    if [ -n "$source" ]; then
      k="$(source_key "$source")"
      ORDER+=("$source")
      if [ "$box_reachable" = "1" ]; then
        SRC_BLOCKS["$k"]="$(printf '%s\n' "$raw_log" | asio_starve_parse_blocks "$source")"
      else
        SRC_BLOCKS["$k"]=""
      fi
      SRC_HEALTHY["$k"]="$(asio_starve_is_healthy "${SRC_BLOCKS["$k"]}" "$ASIO_STARVE_THRESHOLD")"
      [ "${SRC_HEALTHY["$k"]}" = "1" ] && n_healthy=$((n_healthy + 1))
    fi
    IFS=';'
  done
  IFS="$old_ifs"
  set +f

  # Per source, healthy_sibling = 1 iff ANY OTHER watched source is healthy (subtract this source's
  # own health from the total healthy count).
  for source in "${ORDER[@]}"; do
    k="$(source_key "$source")"
    local others_healthy=$(( n_healthy - ( SRC_HEALTHY["$k"] == 1 ? 1 : 0 ) ))
    local healthy_sibling=0
    [ "$others_healthy" -gt 0 ] && healthy_sibling=1
    handle_source "$source" "${SRC_BLOCKS["$k"]}" "$healthy_sibling" "$box_reachable"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
