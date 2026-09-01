#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/network-reach-alert-watchdog.sh /
# scripts/optical-chain-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/frozen-input-alert-watchdog.sh -- #1052: STREAM FROZEN-INPUT alert, DEV1-SIDE
# (network-reach watchdog PHASE 2).
#
# WHY (#1052, phase-2 of #1001): reachability (#1001) only classifies a box REACHABLE/UNREACHABLE --
# it is blind to a box that is fully REACHABLE while its NDI input is silently frozen on the last
# frame (the #767 receiver-rebind class: the cambox is fine but stream's DistroAV receiver froze).
# Emit-freeze (#944) is CAMBOX-side and never fires for a receiver-side freeze. So a stream input
# frozen-on-last-frame while the box stays up is undetected during a live event.
#
# TAP: the stream OBS log prints `genlock-fifo audit '<src>': received=N ...` ~every 5 s
# (GENLOCK_AUDIT_LOG_INTERVAL_NS, obs-source.c). `received=` is the cumulative count of frames the
# FIFO RECEIVED from the source, so a frozen input STOPS advancing it -- immune to the static-content
# false-positive a screenshot-hash tap suffers, and it works in BOTH test and event mode (no burns,
# no scene warm-up). This watchdog samples the newest `received=` per watched source once per pass,
# PERSISTS it, and compares to the prior pass via the PURE scripts/lib/frozen-input-health.sh seam.
# The confirm-counter + alert throttle are the SAME shared obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) #391/#1001 already use -- no
# second alert mechanism. A recovery ("advancing again") ping fires once when a source we paged for
# resumes.
#
# NO-DOUBLE-PAGE GUARD: before deciding, read issue-1001's OWN on-disk state (never re-probe) -- if
# either the SENDER box (strih, which sends `NDI 2ME PGM`) or the RECEIVER box (stream) is CONFIRMED
# unreachable there, #1001 already owns the page and a frozen input is a downstream symptom -> SKIP.
# Only "both boxes reachable BUT the input frozen" pages, which is exactly this ticket's class.
#
# BEST-EFFORT PROBE: the received counter is read with one flat `ssh ... powershell` OBS-log tail (a
# session-agnostic file read, allowed for a headless dev1 watchdog per win-ssh-vs-mcp; NEVER nested
# PowerShell). A failed read yields an empty sample -> the pure seam returns UNKNOWN -> never a false
# page (verified in tests/harness_frozen_input_health_1052.rs). Override the whole read with
# FROZEN_INPUT_PROBE_CMD (a command run with <receiver_ip> <source>, stdout = the RAW log text, same
# shape the ssh tail returns -- the `genlock-fifo audit '<src>': received=N` extraction is always
# applied on top) for a --dry-run smoke test or an alternate tap (e.g. a future :8899 field).
#
# Usage:
#   scripts/frozen-input-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/frozen-input-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/frozen-input-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/frozen-input-health.sh
. "$HERE/lib/frozen-input-health.sh"
# shellcheck source=scripts/lib/mv-reverify-escalate.sh
# #1069: the STRIH-instance enumeration read reuses mv_reverify_probe_raw (the round-24 #1093 flat
# session-agnostic strih OBS-log tail) rather than a THIRD ssh-tail implementation. Source-only lib,
# no top-level statements, and its own $HERE-relative sourcing is call-time only (never triggered by
# the raw read), so sourcing it here is side-effect-free.
. "$HERE/lib/mv-reverify-escalate.sh"
# shellcheck source=scripts/lib/ps-encoded.sh
. "$HERE/lib/ps-encoded.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "frozen-input-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The RECEIVER box whose OBS log carries the `received=` counters, as "name|ip".
RECEIVER="${FROZEN_INPUT_RECEIVER:-stream|10.77.9.204}"
RECEIVER_NAME="${RECEIVER%%|*}"; RECEIVER_IP="${RECEIVER##*|}"
# The SENDER box whose #1001 reachability gates the no-double-page guard (the `NDI 2ME PGM` program
# feed is produced by strih). Just the box NAME as #1001 keys it in its state file.
SENDER_NAME="${FROZEN_INPUT_SENDER:-strih}"
# Watched source names on the receiver, ';'-separated (names contain spaces). Default: the program
# feed only -- the always-live input whose #767 receiver freeze this ticket targets. Listing a source
# here IS the "expected live" scope (an idle input that may legitimately stop is simply not listed).
SOURCES="${FROZEN_INPUT_SOURCES:-NDI 2ME PGM}"

# -- #1069 dynamic-enumeration mode (default OFF -> the #1052 stream instance behaves identically) --
# When ENUMERATE=1 the watched source LIST is derived from the receiver's live OBS log each pass
# (never a static cam-number list) instead of the static SOURCES above -- for the STRIH instance that
# watches the changing cambox camera branch (`NDI cam1`..`NDI camN`). The cambox filter is the pure
# frozen_input_cambox_sources seam (include/exclude regexes, defaults match the live rig labels).
ENUMERATE="${FROZEN_INPUT_ENUMERATE:-0}"
ENUM_INCLUDE="${FROZEN_INPUT_ENUMERATE_INCLUDE:-cam}"
ENUM_EXCLUDE="${FROZEN_INPUT_ENUMERATE_EXCLUDE:-2me|pgm|pvw|multiview|preview|program}"
# A failed enumeration must NEVER read as a silent green (the standing rig-degradation rule / the
# burn-target FAIL-CLOSED discipline): after this many CONSECUTIVE passes that enumerate ZERO cambox
# sources while the receiver is reachable, fire ONE fail-loud "enumeration blind" WARN. Default ~2h.
ENUM_BLIND_THRESHOLD="${FROZEN_INPUT_ENUM_BLIND_THRESHOLD:-24}"

# The Discord ticket tag on every alert body. Default #1052 (the stream instance); the strih instance
# sets #1069. The human "cure" line + the reachable-boxes phrase are env-overridable too so each
# instance names its own remedy without the decision core knowing which box it watches.
ALERT_TAG="${FROZEN_INPUT_ALERT_TAG:-#1052}"
CURE_HINT="${FROZEN_INPUT_CURE_HINT:-Restart the OBS receiver for the frozen source on $RECEIVER_NAME.}"
if [ "$SENDER_NAME" = "$RECEIVER_NAME" ]; then
  REACHABLE_PHRASE="$RECEIVER_NAME is reachable"
else
  REACHABLE_PHRASE="both $SENDER_NAME and $RECEIVER_NAME are reachable"
fi

SSH_USER="${FROZEN_INPUT_SSH_USER:-newlevel}"
SSH_PW="${FROZEN_INPUT_SSH_PW:-newlevel}"
SSH_TIMEOUT="${FROZEN_INPUT_SSH_TIMEOUT:-20}"
SSH_OPTS="${FROZEN_INPUT_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"
OBS_LOG_TAIL="${FROZEN_INPUT_OBS_LOG_TAIL:-800}"

# 2-pass confirm before paging (matches the sibling watchdogs): one missed log read / transient must
# never page. A genuinely frozen input stays frozen across the 5-min cadence.
CONFIRM_THRESHOLD="${FROZEN_INPUT_ALERT_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${FROZEN_INPUT_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence
# A source whose audit line is ABSENT every pass (a broken tap: a config label drift, a renamed /
# re-created DistroAV source, a source that stopped ticking) stays UNKNOWN forever and never pages,
# while the watchdog itself looks green -- exactly the "silent unknown" the standing rig-degradation
# rule forbids. After this many CONSECUTIVE UNKNOWN passes, fire ONE "tap broken" WARN ping so a
# permanently-blind tap becomes visible. Default ~2h at the 5-min cadence (well past a legitimate
# transient / OBS-restart reseed).
TAP_BROKEN_THRESHOLD="${FROZEN_INPUT_TAP_BROKEN_THRESHOLD:-24}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${FROZEN_INPUT_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${FROZEN_INPUT_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-frozen-input-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-frozen-input-alert-dryrun.state"
STATE_FILE="${FROZEN_INPUT_ALERT_STATE_FILE:-$_state_default}"
# Issue-1001's OWN state file -- read (never written) for the no-double-page guard.
NETREACH_STATE_FILE="${FROZEN_INPUT_NETREACH_STATE_FILE:-$STATE_DIR/camera-box-network-reach-alert.state}"

# EXPECTED_LIVE is 1 for every configured source (the list is the scope). Kept as a seam input so a
# future caller can gate a source on live "is it in program" state without touching the decision core.
EXPECTED_LIVE=1

log() { printf '%s [frozen-input-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure -- kept out of the lib) -------------------------------------
# probe_received <receiver_ip> <source> -> stdout: the newest cumulative `received=` value for the
# source (a bare integer), or EMPTY when the read failed / the source's audit line was absent this
# pass (the seam then returns UNKNOWN -- never a false page). Overridable via FROZEN_INPUT_PROBE_CMD.
probe_received() {
  local ip="$1" source="$2" raw
  if [ -n "${FROZEN_INPUT_PROBE_CMD:-}" ]; then
    raw="$($FROZEN_INPUT_PROBE_CMD "$ip" "$source" 2>/dev/null || true)"
  else
    # #1259: -EncodedCommand (base64 UTF-16LE), NEVER the naive -Command "…| sort …| select …".
    # Win32-OpenSSH's default cmd.exe shell leaks the unescaped `|` pipes -> a mangled/blind read (the
    # issue-1258 root cause). ps_encoded_command (scripts/lib/ps-encoded.sh) encodes the tail command
    # to a pure-ASCII blob cmd.exe cannot touch; an empty encode -> empty read -> UNKNOWN, never an abort.
    local _enc _tail
    _tail="$(ps_clamp_numeric "$OBS_LOG_TAIL" 800)" # #1259: guard the env count before the payload
    _enc="$(ps_encoded_command "gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $_tail")"
    # shellcheck disable=SC2086
    raw="$(timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" \
      "powershell -NoProfile -NonInteractive -EncodedCommand $_enc" \
      2>/dev/null || true)"
  fi
  # Newest audit line for THIS source -> the received= integer. Empty if none found.
  # #1258 layer 2: LC_ALL=C + grep -a -- PowerShell 5.1 `gc` (no -Encoding) reads the UTF-8
  # strih OBS log as ANSI and re-encodes on output, so an audit line's non-ASCII glyphs come
  # back as invalid-UTF-8 bytes; in a UTF-8 locale GNU grep then flags stdin BINARY (empty
  # stdout) and sed's trailing `.*` refuses to consume the invalid byte (line-tail garbage
  # after the digits) -- byte-safe end to end regardless of what bytes the raw tail carries.
  printf '%s\n' "$raw" \
    | LC_ALL=C grep -aF "genlock-fifo audit '$source':" \
    | tail -1 \
    | LC_ALL=C sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p' \
    | tail -1
}

# -- #1069 dynamic enumeration probe (dev1-local; NOT pure) -------------------------------------
# probe_enumerate <receiver_ip> -> stdout: newline-separated cambox source names to watch this pass
# (dynamic, derived from the receiver's live OBS log). The RAW OBS-log read reuses the round-24
# mv_reverify_probe_raw (scripts/lib/mv-reverify-escalate.sh, #1093) -- the SAME flat session-agnostic
# `sshpass … powershell … -Tail` strih OBS-log tail -- rather than a third ssh-tail; the cambox filter
# is the pure frozen_input_cambox_sources seam. Override the whole RAW read with FROZEN_INPUT_ENUMERATE_CMD
# (run with <receiver_ip>, stdout = raw log text) for a --dry-run smoke test / offline tests. An empty
# read yields no sources -> the caller's enum-blind guard fires a fail-loud WARN (never a silent green).
probe_enumerate() {
  local ip="$1" raw
  if [ -n "${FROZEN_INPUT_ENUMERATE_CMD:-}" ]; then
    raw="$($FROZEN_INPUT_ENUMERATE_CMD "$ip" 2>/dev/null || true)"
  else
    # Reuse the #1093 strih OBS-log reader (source name unused for the raw read -> ""). Thread the
    # watchdog's OWN FROZEN_INPUT_SSH_* env into mv_reverify_probe_raw's STRIH_*/tail/timeout knobs so
    # the enumeration read and the per-source probe_received read share ONE credential + tail namespace
    # -- otherwise changing only FROZEN_INPUT_SSH_PW would fix per-source reads while the enumeration
    # read (on STRIH_PW) starts failing -> a spurious enumeration-BLIND WARN (review #1069).
    raw="$(STRIH_USER="$SSH_USER" STRIH_PW="$SSH_PW" \
      MV_REVERIFY_RECEIVED_SSH_TIMEOUT="$SSH_TIMEOUT" MV_REVERIFY_RECEIVED_TAIL="$OBS_LOG_TAIL" \
      mv_reverify_probe_raw "$ip" "" 2>/dev/null || true)"
  fi
  printf '%s\n' "$raw" | frozen_input_cambox_sources "$ENUM_INCLUDE" "$ENUM_EXCLUDE"
}

# -- issue-1001 reachability read (never re-probed) ---------------------------------------------
# netreach_box_alerted <box> -> "1" iff #1001 has that box CONFIRMED unreachable (paged). Absent
# state file / field -> "0" (either #1001 has not run yet, or the box is reachable) -- the probe
# itself still fails-safe to UNKNOWN if the box is genuinely down.
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
# the RAW name is appended so two distinct names that sanitize identically (e.g. "NDI 2ME PGM" vs
# "NDI-2ME-PGM") never silently share `recv_/confirm_/alerted_` state.
source_key() {
  local san sum
  san="$(printf '%s' "$1" | tr -c 'A-Za-z0-9' '_')"
  sum="$(printf '%s' "$1" | cksum | cut -d' ' -f1)"
  printf '%s_%s' "$san" "$sum"
}

# An ADVANCING source is not an incident: clear its confirm counter + throttle sig so a genuinely NEW
# freeze later pages fresh. Does NOT clear the `alerted` latch (the recovery-ping latch).
clear_source_throttle() {
  local k="$1"
  write_state_field "confirm_${k}" 0
  write_state_field "alert_sig_${k}" ""
  write_state_field "alert_passes_${k}" 0
}

# -- per-source decision ------------------------------------------------------------------------
handle_source() {
  local source="$1" sender_reachable="$2"
  local k prev curr verdict unk tap_alerted
  k="$(source_key "$source")"
  prev="$(read_state_field "recv_${k}" "")"
  # Skip the ssh probe entirely when the no-double-page guard already forces SKIP (a box is down per
  # issue-1001): the verdict is predetermined, and probing a down receiver would block up to
  # SSH_TIMEOUT per source for nothing.
  if [ "$sender_reachable" = "1" ]; then
    curr="$(probe_received "$RECEIVER_IP" "$source")"
  else
    curr=""
  fi

  verdict="$(frozen_input_classify "$prev" "$curr" "$EXPECTED_LIVE" "$sender_reachable")"
  log "'$source' on $RECEIVER_NAME: prev=${prev:-<none>} curr=${curr:-<none>} live=$EXPECTED_LIVE sender_reachable=$sender_reachable -> $verdict"

  # Persist the new baseline ONLY when the current sample is a real integer (a failed/absent read
  # must not clobber the last good baseline -- next pass then still has a value to compare against).
  case "$curr" in '' | *[!0-9]*) : ;; *) write_state_field "recv_${k}" "$curr" ;; esac

  # Tap-liveness -- only meaningful when we actually PROBED (sender_reachable=1). A probe that returns
  # NO usable value (no `genlock-fifo audit '<src>'` line found) is a BLIND tap: it stays UNKNOWN
  # forever and never pages while the watchdog looks green -- the "silent unknown" the standing
  # rig-degradation rule forbids. Track consecutive blind passes and fire ONE "tap broken" WARN past
  # the threshold. A probe that returns a REAL integer proves the tap works (even when the verdict is
  # UNKNOWN from a first-sample / counter-reset) -> reset. A SKIP pass (box down per issue-1001, no
  # probe) leaves the counter untouched -- that is not a tap fault.
  if [ "$sender_reachable" = "1" ]; then
    case "$curr" in
      '' | *[!0-9]*)
        unk="$(read_state_field "unknown_${k}" 0)"
        case "$unk" in '' | *[!0-9]*) unk=0 ;; esac
        unk=$((unk + 1))
        write_state_field "unknown_${k}" "$unk"
        tap_alerted="$(read_state_field "tap_broken_${k}" 0)"
        if [ "$unk" -ge "$TAP_BROKEN_THRESHOLD" ] && [ "$tap_alerted" != "1" ]; then
          if [ "$DRY_RUN" -eq 1 ]; then
            log "[dry-run] WOULD alert: '$source' tap BROKEN (no audit line for $unk consecutive passes)"
          else
            log "ALERT: firing Discord notification for '$source' tap broken"
            python3 "$NOTIFY" notify --body \
              "⚠️ $ALERT_TAG frozen-input: no \`genlock-fifo audit '$source'\` line on $RECEIVER_NAME for $unk consecutive passes ($REPO_SLUG). The freeze TAP for this source is BLIND (source renamed / re-created / dropped from the scene, or FROZEN_INPUT_SOURCES drifted from the live label) -- frozen-input coverage for '$source' is OFF until fixed." \
              --dedup-key "frozen-input-tap-$RECEIVER_NAME-$source" \
              >/dev/null 2>&1 || log "tap-broken: airuleset.py notify failed (non-fatal)"
          fi
          write_state_field "tap_broken_${k}" 1
        fi
        ;;
      *)
        write_state_field "unknown_${k}" 0
        write_state_field "tap_broken_${k}" 0
        ;;
    esac
  fi

  if [ "$verdict" = "ADVANCING" ]; then
    local was_alerted recover
    was_alerted="$(read_state_field "alerted_${k}" 0)"
    recover="$(frozen_input_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
    if [ "$recover" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: '$source' advancing again"
      else
        log "RECOVERY: '$source' advancing again on $RECEIVER_NAME -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${k}" 0
    fi
    clear_source_throttle "$k"
    return 0
  fi

  # Only FROZEN feeds the confirm counter; UNKNOWN / SKIP reset it (a broken streak must restart).
  local is_frozen=0
  [ "$verdict" = "FROZEN" ] && is_frozen=1
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${k}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$is_frozen" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${k}" "${confirm:-0}"

  if [ "$verdict" != "FROZEN" ]; then
    log "'$source' $verdict this pass -- not frozen, holding"
    return 0
  fi
  log "'$source' confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "'$source' FROZEN this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED frozen -> latch the recovery flag, then throttle-dedup on the source signature.
  write_state_field "alerted_${k}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="frozen:${k}:${curr}"
  prior_sig="$(read_state_field "alert_sig_${k}" "")"
  prior_passes="$(read_state_field "alert_passes_${k}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${k}" "$new_sig"
  write_state_field "alert_passes_${k}" "$new_passes"

  detail="$(frozen_input_alert_detail "$source" "$curr")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: '$source' CONFIRMED frozen ($detail) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for '$source' frozen"
    python3 "$NOTIFY" notify --body \
      "🚨 $ALERT_TAG frozen-input: **$detail** ($REPO_SLUG). Confirmed over ${CONFIRM_THRESHOLD} consecutive passes while $REACHABLE_PHRASE -- the input is silently frozen on its last frame (a DistroAV receiver-rebind / upstream freeze). $CURE_HINT" \
      --dedup-key "frozen-input-$RECEIVER_NAME-$source" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# -- #1069 enumeration + fail-loud blind guard --------------------------------------------------
# enumerate_and_guard <receiver_ip> -> stdout: ';'-joined cambox source list to watch this pass.
# Derives the watched set DYNAMICALLY from the receiver's live OBS log (never a static cam-number
# list). A failed / empty enumeration must NEVER read as a silent green: track consecutive-empty
# passes and fire ONE fail-loud "enumeration blind" WARN past ENUM_BLIND_THRESHOLD (the burn-target
# FAIL-CLOSED discipline + the standing rig-degradation rule). Only called when the receiver is
# reachable (a box-down pass SKIPs before here, leaving the counter untouched).
enumerate_and_guard() {
  local ip="$1" names joined blind blind_alerted
  names="$(probe_enumerate "$ip")"
  if [ -n "$names" ]; then
    # A real enumeration proves the tap works -> reset the blind streak + latch.
    write_state_field "enum_blind" 0
    write_state_field "enum_blind_alerted" 0
    joined="$(printf '%s' "$names" | paste -sd';' -)"
    log "enumerated $(printf '%s\n' "$names" | grep -c .) cambox source(s) on $RECEIVER_NAME: $joined"
    printf '%s' "$joined"
    return 0
  fi
  # Zero cambox sources this pass -> a blind tap. Count it; WARN once past the threshold.
  blind="$(read_state_field "enum_blind" 0)"
  case "$blind" in '' | *[!0-9]*) blind=0 ;; esac
  blind=$((blind + 1))
  write_state_field "enum_blind" "$blind"
  log "enumeration returned ZERO cambox sources on $RECEIVER_NAME (blind pass $blind/$ENUM_BLIND_THRESHOLD)"
  blind_alerted="$(read_state_field "enum_blind_alerted" 0)"
  if [ "$blind" -ge "$ENUM_BLIND_THRESHOLD" ] && [ "$blind_alerted" != "1" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      log "[dry-run] WOULD alert: enumeration BLIND on $RECEIVER_NAME ($blind consecutive passes)"
    else
      log "ALERT: firing Discord notification for enumeration blind on $RECEIVER_NAME"
      python3 "$NOTIFY" notify --body \
        "⚠️ $ALERT_TAG frozen-input: enumeration BLIND on $RECEIVER_NAME -- zero cambox inputs matched (\`genlock-fifo audit '<src>':\` include=/$ENUM_INCLUDE/ exclude=/$ENUM_EXCLUDE/) for $blind consecutive passes ($REPO_SLUG). Frozen-input coverage for the $RECEIVER_NAME cambox branch is OFF (the OBS-log read is failing, or the cambox source labels drifted) -- no frozen-input page can fire until this is fixed." \
        --dedup-key "frozen-input-enum-$RECEIVER_NAME" \
        >/dev/null 2>&1 || log "enum-blind: airuleset.py notify failed (non-fatal)"
    fi
    write_state_field "enum_blind_alerted" 1
  fi
  printf '%s' ""
}

main() {
  log "pass start (dry_run=$DRY_RUN, receiver='$RECEIVER', sender='$SENDER_NAME', enumerate=$ENUMERATE, sources='$SOURCES')"

  # No-double-page guard: both the sender and the receiver box must be reachable per #1001's OWN
  # on-disk state, else #1001 already owns the page -> sender_reachable=0 -> every source SKIPs.
  local sender_down receiver_down sender_reachable
  sender_down="$(netreach_box_alerted "$SENDER_NAME")"
  receiver_down="$(netreach_box_alerted "$RECEIVER_NAME")"
  if [ "$sender_down" = "1" ] || [ "$receiver_down" = "1" ]; then
    sender_reachable=0
    log "issue-1001 state: $SENDER_NAME down=$sender_down $RECEIVER_NAME down=$receiver_down -> #1001 owns the page, SKIP all sources this pass"
  else
    sender_reachable=1
  fi

  # #1069 enumeration mode: derive the watched cambox set from the receiver's live OBS log each pass
  # (never a static list). Only when the receiver is reachable -- a box-down pass has nothing to read
  # and must not count as an enumeration-blind streak. A box-down pass leaves SOURCES empty -> the
  # loop below simply iterates nothing (every source would SKIP anyway).
  if [ "$ENUMERATE" = "1" ]; then
    if [ "$sender_reachable" = "1" ]; then
      SOURCES="$(enumerate_and_guard "$RECEIVER_IP")"
    else
      SOURCES=""
      log "enumerate mode: receiver not reachable per #1001 -> no enumeration this pass (SKIP)"
    fi
  fi

  # Iterate ';'-separated source names (they contain spaces, so split on ';' not whitespace).
  # `set -f` disables globbing so a `*`/`?`/`[` metachar in a future source name never pathname-expands.
  local old_ifs="$IFS" source
  set -f
  IFS=';'
  for source in $SOURCES; do
    IFS="$old_ifs"
    source="${source#"${source%%[![:space:]]*}"}"   # ltrim
    source="${source%"${source##*[![:space:]]}"}"    # rtrim
    [ -n "$source" ] && handle_source "$source" "$sender_reachable"
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
