#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/frozen-input-alert-watchdog.sh /
# scripts/network-reach-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/cadence-alert-watchdog.sh -- #794: NON-60 SOURCE-CADENCE alert, DEV1-SIDE. A fourth
# sibling of the dev1 alert-watchdog family (network-reach issue 1001 / frozen-input issue 1052 /
# bundle-state issue 732).
#
# WHY (#794, live 2026-07-17): a camera set to 50 fps feeding the 60 fps chain went undetected for
# weeks (no frame loss, but 5:6 pulldown judder + a misleading input for the blend). Every fps
# counter reads the CANVAS rate (60/30), and a grabber that upconverts 50->60 by DUPLICATION delivers
# a genuine 60 NDI frames/s -- so the canonical figures are all clean. The ONE per-source signal that
# reflects the true NDI DELIVERY rate is the genlock-fifo `received=` cumulative counter on the
# RECEIVER (strih, where the camera NDI inputs arrive): a camera genuinely delivering 50/43 fps
# advances `received=` at 50/43 per second. (BLIND SPOT: a duplication-masked 50->60 delivers a
# padded 60 and reads a clean 60 here -- that "hard layer" (per-frame content hashing) is a separate
# follow-up; this watchdog covers the genuinely-non-60-DELIVERED case.)
#
# TAP: the strih OBS log prints `HH:MM:SS.mmm: genlock-fifo audit '<src>': received=N ...` ~every 5 s
# (GENLOCK_AUDIT_LOG_INTERVAL_NS, obs-source.c) -- inherent genlock telemetry, so it works in BOTH
# test and event mode (no burns, no warm-up). This watchdog samples the newest line's `received=`
# PLUS the line's OWN timestamp per watched source each pass, PERSISTS both, and computes a measured
# fps across two consecutive passes via the PURE scripts/lib/cadence-health.sh seam.
#
# THE LOAD-BEARING DECISION -- issue-797 "phantom 50.1 fps" avoidance: the rate is `received-delta /
# (the two lines' OWN elapsed seconds)`, NEVER `received-delta / a wall-clock sleep`. A 6 s wall
# window spans ~one 5.017 s tick (+301 frames at a true 60) and prints 301/6 = 50.17 -- a phantom
# "50" at EVERY true-60 source. Dividing by the lines' own 5.017 s reads ~60 instead. Over the 5-min
# pass window the two sampled lines are ~60 ticks apart, so the rate is both exact and well-averaged.
#
# The confirm-counter + alert throttle are the SAME shared obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use -- no
# second alert mechanism. A recovery ("cadence back to ~60") ping fires once when a source we paged
# for returns to an OK cadence.
#
# NO-DOUBLE-PAGE GUARD: before deciding, read issue-1001's OWN on-disk state (never re-probe) -- if
# the strih box (which hosts the camera inputs) is CONFIRMED unreachable there, issue-1001 already
# owns the page and a bad cadence reading is out of scope -> SKIP.
#
# FAIL-LOUD TOOL PREFLIGHT: a missing `awk`/`sshpass`/`ssh`/`timeout` on dev1 would otherwise make
# every probe read empty forever (a permanently-blind watchdog that still looks green) -- exactly the
# "a missing tool must fail LOUD by name, never read as a measured zero" class
# (.claude/rules/imag-ssh-remote-tool-preflight.md, #833). require_tools aborts the pass loudly.
#
# BEST-EFFORT PROBE: ONE flat `ssh ... powershell` OBS-log tail per PASS -- NOT per source. The tail
# carries every watched source's audit line, so one fetch + N local greps is identical to N fetches;
# and N * SSH_TIMEOUT must stay under the unit's TimeoutStartSec (7 sources * 20 s would exceed a 90 s
# oneshot on a reachable-but-slow box). A session-agnostic file read, allowed for a headless dev1
# watchdog per win-ssh-vs-mcp; NEVER nested PowerShell. A failed/absent read yields an empty sample ->
# the pure seam returns UNKNOWN -> never a false page. Override the whole read with CADENCE_PROBE_CMD
# (run with <box_ip>, stdout = the RAW log text of the newest OBS log) for a --dry-run smoke test or
# an alternate tap.
#
# Usage:
#   scripts/cadence-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/cadence-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/cadence-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/cadence-health.sh
. "$HERE/lib/cadence-health.sh"
# shellcheck source=scripts/lib/ps-encoded.sh
. "$HERE/lib/ps-encoded.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,58p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "cadence-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The box whose OBS log carries the per-camera `received=` counters, as "name|ip". strih receives the
# RAW camera NDI at its native delivery rate; stream only sees 2ME PGM (30 fps) + interkom.
CADENCE_BOX="${CADENCE_BOX:-strih|10.77.9.202}"
CADENCE_NAME="${CADENCE_BOX%%|*}"; CADENCE_IP="${CADENCE_BOX##*|}"
# Watched source names on the box, ';'-separated (names contain spaces). SET THIS to the strih
# labels of the currently-active cameras at enable time (like FROZEN_INPUT_SOURCES) -- the default is
# a placeholder of the 7-camera table. Listing a source here IS its "expected 60 fps" scope.
CADENCE_SOURCES="${CADENCE_SOURCES:-NDI cam1;NDI cam2;NDI cam3;NDI cam4;NDI cam5;NDI cam6;NDI cam7}"

CADENCE_EXPECTED_FPS="${CADENCE_EXPECTED_FPS:-60}"
CADENCE_TOLERANCE_FPS="${CADENCE_TOLERANCE_FPS:-3}"     # WRONG outside [57,63]; 59.94 NTSC is in-band
CADENCE_MIN_WINDOW_S="${CADENCE_MIN_WINDOW_S:-60}"      # windows shorter than this -> UNKNOWN (noisy)
# CADENCE_MAX_WINDOW_S (default 21600 in the lib) caps a stale-prev window -> reseed, not a rate.

SSH_USER="${CADENCE_SSH_USER:-newlevel}"
SSH_PW="${CADENCE_SSH_PW:-newlevel}"
SSH_TIMEOUT="${CADENCE_SSH_TIMEOUT:-20}"
SSH_OPTS="${CADENCE_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"
OBS_LOG_TAIL="${CADENCE_OBS_LOG_TAIL:-800}"

# 2-pass confirm before paging: one noisy window / transient must never page. A genuinely
# mis-configured camera stays wrong across the cadence.
CONFIRM_THRESHOLD="${CADENCE_ALERT_CONFIRM_THRESHOLD:-2}"
# ~30 min re-alert at the 5-min cadence (the ticket asks for a periodic repeat every ~30 min).
ALERT_THROTTLE_PASSES="${CADENCE_ALERT_THROTTLE_PASSES:-6}"
# A source whose audit line is ABSENT every pass (a broken tap: a renamed / dropped source, or a
# CADENCE_SOURCES drift) stays UNKNOWN forever and never pages while the watchdog looks green --
# exactly the "silent unknown" the standing rig-degradation rule forbids. Fire ONE "tap broken" WARN
# past this many CONSECUTIVE blind passes. Default ~2h at the 5-min cadence.
TAP_BROKEN_THRESHOLD="${CADENCE_TAP_BROKEN_THRESHOLD:-24}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${CADENCE_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${CADENCE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-cadence-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-cadence-alert-dryrun.state"
STATE_FILE="${CADENCE_ALERT_STATE_FILE:-$_state_default}"
# Issue-1001's OWN state file -- read (never written) for the no-double-page guard.
NETREACH_STATE_FILE="${CADENCE_NETREACH_STATE_FILE:-$STATE_DIR/camera-box-network-reach-alert.state}"

# EXPECTED_LIVE is 1 for every configured source (the list is the scope). Kept as a seam input.
EXPECTED_LIVE=1

log() { printf '%s [cadence-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# require_tools -> non-zero (loud) if a REQUIRED external tool is missing on dev1. `awk` is always
# required (the pure lib computes the rate with it). `sshpass`/`ssh`/`timeout` are required only for
# the real probe -- a CADENCE_PROBE_CMD override (--dry-run / stub) needs none of them.
require_tools() {
  local missing=() t need=("awk")
  [ -z "${CADENCE_PROBE_CMD:-}" ] && need+=("sshpass" "ssh" "timeout")
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
# fetch_box_log <box_ip> -> stdout: the RAW newest-OBS-log tail (ALL sources' audit lines), or EMPTY
# on a failed/absent read. Called ONCE per pass (not per source) -- the tail carries every watched
# source's line, so extract_sample greps each locally. Overridable via CADENCE_PROBE_CMD (run with
# <box_ip>, stdout = raw log text) for a --dry-run smoke test or an alternate tap.
fetch_box_log() {
  local ip="$1"
  if [ -n "${CADENCE_PROBE_CMD:-}" ]; then
    $CADENCE_PROBE_CMD "$ip" 2>/dev/null || true
    return 0
  fi
  # #1259: -EncodedCommand (base64 UTF-16LE), NEVER the naive -Command "…| sort …| select …".
  # Win32-OpenSSH's default cmd.exe shell leaks the unescaped `|` pipes -> a mangled/blind read (the
  # issue-1258 root cause). ps_encoded_command (scripts/lib/ps-encoded.sh) encodes the tail command
  # to a pure-ASCII blob cmd.exe cannot touch; an empty encode -> empty read -> UNKNOWN, never an abort.
  local _enc _tail
  _tail="$(ps_clamp_numeric "$OBS_LOG_TAIL" 800)" # #1259: guard the env count before the payload
  _enc="$(ps_encoded_command "gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $_tail")"
  # shellcheck disable=SC2086
  timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" \
    "powershell -NoProfile -NonInteractive -EncodedCommand $_enc" \
    2>/dev/null || true
}

# extract_sample <raw_log> <source> -> stdout: `<received> <timestamp>` from the source's NEWEST
# audit line, or EMPTY when the source's audit line is absent / the received= counter is unreadable
# (the seam then returns UNKNOWN -- never a false page). Pure text (no I/O), fed the once-fetched log.
extract_sample() {
  local raw="$1" source="$2" line recv ts
  # #1258 layer 2: LC_ALL=C + grep -a -- the stdin here is the raw PS-fetched OBS-log tail
  # (fetch_box_log, ps_encoded_command), which can carry invalid-UTF-8 bytes (PowerShell 5.1's
  # `gc` re-encoding ANSI on a non-ASCII glyph elsewhere in the SAME tail); a UTF-8-locale grep
  # then flags the WHOLE stdin binary (empty stdout) and sed's trailing `.*` refuses to consume
  # the invalid byte (line-tail garbage after the digits) -- byte-safe end to end.
  line="$(printf '%s\n' "$raw" | LC_ALL=C grep -aF "genlock-fifo audit '$source':" | tail -1)"
  [ -n "$line" ] || return 0
  recv="$(printf '%s\n' "$line" | LC_ALL=C sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p' | tail -1)"
  # The OBS log prefix: `HH:MM:SS.mmm: <message>`. Extract the leading clock time (its trailing colon
  # is stripped by cadence_ts_to_seconds). An absent prefix -> empty ts -> a NOT-usable sample below.
  ts="$(printf '%s\n' "$line" | LC_ALL=C sed -n 's/^\([0-9][0-9]:[0-9][0-9]:[0-9][0-9][.0-9]*\).*/\1/p' | tail -1)"
  [ -n "$recv" ] || return 0
  printf '%s %s\n' "$recv" "$ts"
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

# Extract a `key=value` token from a cadence_measure_fps line.
measure_field() {
  local line="$1" key="$2" tok
  for tok in $line; do
    case "$tok" in "${key}="*) printf '%s' "${tok#*=}"; return 0 ;; esac
  done
}

# An OK cadence is not an incident: clear its confirm counter + throttle sig so a genuinely NEW wrong
# cadence later pages fresh. Does NOT clear the `alerted` latch (the recovery-ping latch).
clear_source_throttle() {
  local k="$1"
  write_state_field "confirm_${k}" 0
  write_state_field "alert_sig_${k}" ""
  write_state_field "alert_passes_${k}" 0
}

# -- per-source decision ------------------------------------------------------------------------
handle_source() {
  local source="$1" box_reachable="$2" raw_log="$3"
  local k prev_recv prev_ts sample curr_recv curr_ts usable measure fps window advanced verdict unk tap_alerted
  k="$(source_key "$source")"
  prev_recv="$(read_state_field "recv_${k}" "")"
  prev_ts="$(read_state_field "recv_ts_${k}" "")"

  # Extract this source's newest sample from the ONCE-fetched box log. When the no-double-page guard
  # forced SKIP (box down per issue-1001) the raw log is empty, so the sample is empty and the verdict
  # is predetermined SKIP.
  curr_recv=""; curr_ts=""
  if [ "$box_reachable" = "1" ]; then
    sample="$(extract_sample "$raw_log" "$source")"
    [ -n "$sample" ] && read -r curr_recv curr_ts <<<"$sample"
  fi

  # A USABLE sample needs BOTH a real received= integer AND a parsed line timestamp -- the rate is
  # unmeasurable without either. Keying persist + tap-liveness on this PAIR (not just curr_recv)
  # closes the invariant hole where a valid received= with an EMPTY timestamp would reset the
  # blind-tap counter yet leave the source UNKNOWN forever (never a page, never a "tap broken" WARN).
  usable=0
  case "$curr_recv" in '' | *[!0-9]*) : ;; *) [ -n "$curr_ts" ] && usable=1 ;; esac

  measure="$(cadence_measure_fps "$prev_ts" "$prev_recv" "$curr_ts" "$curr_recv")"
  fps="$(measure_field "$measure" fps)"
  window="$(measure_field "$measure" window_s)"
  advanced="$(measure_field "$measure" advanced)"
  verdict="$(cadence_classify "$fps" "$window" "$advanced" \
    "$CADENCE_EXPECTED_FPS" "$CADENCE_TOLERANCE_FPS" "$CADENCE_MIN_WINDOW_S" "$EXPECTED_LIVE" "$box_reachable")"
  log "'$source' on $CADENCE_NAME: prev=${prev_recv:-<none>}@${prev_ts:-<none>} curr=${curr_recv:-<none>}@${curr_ts:-<none>} fps=${fps:-<n/a>} win=${window:-<n/a>} adv=${advanced:-<n/a>} reachable=$box_reachable -> $verdict"

  # Persist the new baseline ONLY when the sample is USABLE (recv integer + ts present) -- a
  # failed/absent/timestampless read must not clobber the last good baseline (next pass then still
  # has a value to compare against).
  [ "$usable" = 1 ] && { write_state_field "recv_${k}" "$curr_recv"; write_state_field "recv_ts_${k}" "$curr_ts"; }

  # Tap-liveness -- only meaningful when we actually PROBED (box_reachable=1). A probe that yields no
  # USABLE (recv + ts) sample is a BLIND tap: it stays UNKNOWN forever and never pages while the
  # watchdog looks green. Track consecutive blind passes and fire ONE "tap broken" WARN past the
  # threshold. A usable sample proves the tap works (even when the verdict is UNKNOWN from a
  # first-sample / short-window / reset) -> reset. A SKIP pass (box down) leaves the counter untouched.
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
          log "[dry-run] WOULD alert: '$source' tap BROKEN (no usable audit sample for $unk consecutive passes)"
        else
          log "ALERT: firing Discord notification for '$source' tap broken"
          python3 "$NOTIFY" notify --body \
            "⚠️ kadencia kamery ($REPO_SLUG): meranie kadencie zdroja '$source' na $CADENCE_NAME je SLEPÉ už $unk kontrol po sebe (premenovaný/chýbajúci zdroj, kamera preč alebo nečitateľný log) — kontrola 60 fps pre tento zdroj je vypnutá, kým sa to neopraví. Rieši Claude automaticky, ty nemusíš nič robiť." \
            --dedup-key "cadence-tap-$source" \
            >/dev/null 2>&1 || log "tap-broken: airuleset.py notify failed (non-fatal)"
        fi
        write_state_field "tap_broken_${k}" 1
      fi
    fi
  fi

  # OK cadence -> recovery path (fire one recovery ping if we had paged), clear throttle.
  if [ "$verdict" = "OK" ]; then
    local was_alerted recover
    was_alerted="$(read_state_field "alerted_${k}" 0)"
    recover="$(cadence_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
    if [ "$recover" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: '$source' cadence back to ~$CADENCE_EXPECTED_FPS"
      else
        log "RECOVERY: '$source' cadence OK again (~${CADENCE_EXPECTED_FPS} fps) -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${k}" 0
    fi
    clear_source_throttle "$k"
    return 0
  fi

  # Only WRONG feeds the confirm counter; UNKNOWN / SKIP reset it (a broken streak must restart).
  local is_wrong=0
  [ "$verdict" = "WRONG" ] && is_wrong=1
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${k}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$is_wrong" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${k}" "${confirm:-0}"

  if [ "$verdict" != "WRONG" ]; then
    log "'$source' $verdict this pass -- not a wrong cadence, holding"
    return 0
  fi
  log "'$source' confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "'$source' WRONG cadence this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED wrong -> latch the recovery flag, then throttle-dedup on a STABLE signature (the source
  # + the rounded fps, so a held wrong rate throttles to ~30 min while a shift to a DIFFERENT wrong
  # rate re-alerts fresh).
  write_state_field "alerted_${k}" 1
  local sig_fps show_fps current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  sig_fps="$(LC_ALL=C awk -v f="$fps" 'BEGIN{printf "%d", f + 0.5}')"
  show_fps="$(LC_ALL=C awk -v f="$fps" 'BEGIN{printf "%.1f", f + 0}')"
  current_sig="wrong:${k}:${sig_fps}"
  prior_sig="$(read_state_field "alert_sig_${k}" "")"
  prior_passes="$(read_state_field "alert_passes_${k}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${k}" "$new_sig"
  write_state_field "alert_passes_${k}" "$new_passes"

  detail="$(cadence_alert_detail "$source" "$show_fps" "$CADENCE_EXPECTED_FPS")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: '$source' CONFIRMED wrong cadence ($detail) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for '$source' wrong cadence"
    python3 "$NOTIFY" notify --body \
      "🚨 kadencia kamery ($REPO_SLUG): **$detail**. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol, $CADENCE_NAME je dostupný — kamera je zrejme prepnutá mimo 60 fps. Potrebný zásah — skontroluj a prepni kameru späť na 60 fps." \
      --dedup-key "cadence-$source" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, box='$CADENCE_BOX', expected=${CADENCE_EXPECTED_FPS}±${CADENCE_TOLERANCE_FPS} fps, sources='$CADENCE_SOURCES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  # No-double-page guard: the box hosting the camera inputs must be reachable per issue-1001's OWN
  # on-disk state, else issue-1001 already owns the page -> box_reachable=0 -> every source SKIPs.
  local box_down box_reachable raw_log=""
  box_down="$(netreach_box_alerted "$CADENCE_NAME")"
  if [ "$box_down" = "1" ]; then
    box_reachable=0
    log "issue-1001 state: $CADENCE_NAME down=$box_down -> #1001 owns the page, SKIP all sources this pass"
  else
    box_reachable=1
    # Fetch the newest OBS-log tail ONCE for the whole pass -- it carries every watched source's
    # audit line, so each handle_source greps its own from this one text (one ssh, not one per source).
    raw_log="$(fetch_box_log "$CADENCE_IP")"
  fi

  # Iterate ';'-separated source names (they contain spaces, so split on ';' not whitespace).
  # `set -f` disables globbing so a metachar in a future source name never pathname-expands.
  local old_ifs="$IFS" source
  set -f
  IFS=';'
  for source in $CADENCE_SOURCES; do
    IFS="$old_ifs"
    source="${source#"${source%%[![:space:]]*}"}"   # ltrim
    source="${source%"${source##*[![:space:]]}"}"    # rtrim
    [ -n "$source" ] && handle_source "$source" "$box_reachable" "$raw_log"
    IFS=';'
  done
  IFS="$old_ifs"
  set +f
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
