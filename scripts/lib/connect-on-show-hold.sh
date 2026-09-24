#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes this
# in the CALLER's shell (recording-e2e.sh, which sets its own strict mode). Every function is written
# to be safe under the caller's set -euo pipefail (drain-safe pipes, bounded loops).
#
# scripts/lib/connect-on-show-hold.sh -- issue 1242: the E2E connect-on-show HOLD.
#
# On strih-lx a program-path camera input (`NDI camN`, genlock_connect_on_show) PARKS -- releases its
# NDI receiver -- while nothing shows it (owner ruling 24.9.2026: full bandwidth only for shown
# cameras). The E2E measurement needs EVERY program-path input connected full-bandwidth for the whole
# run (the [4c/8] received= gate, the mv-reverify escalation, the per-camera recordings), so the harness
# HOLDS the flag off right after its cleanup trap arms (behind its own rig-busy guard) and cleanup()
# restores it. Added through this sourced helper (the #675 pattern) so recording-e2e.sh gains plain
# call lines and no new anchor text.
#
# Two layers, because a strih OBS relaunch DURING the run (the #1093 wedge escalation) re-runs the
# launch-time role apply (strih_scenes.py --apply-roles), which would otherwise re-park the held mains:
#   1. a strih-side MARKER (~/.camera-box/connect-on-show-e2e-hold on strih-lx, touched over ssh at the
#      hold, removed first thing at the restore): while it is fresh, the role apply keeps the mains
#      connected (strih_bandwidth_roles.e2e_hold_active, 4 h TTL so a SIGKILLed run cannot pin it);
#   2. the live WS flip (obs_phase2.py connect-on-show --hold/--restore, read back), recorded in a
#      STABLE dev1 state file so a killed run's list is unioned + restored by the next run.
#
#   connect_on_show_e2e_hold      HERE STRIH STATE_FILE -> 0 held (read back) | non-zero: the run
#                                                          must abort (a hidden input would be measured
#                                                          cold)
#   connect_on_show_e2e_wait_live HERE STRIH STATE_FILE -> ALWAYS 0 (bounded wait, fail-open WARNING)
#   connect_on_show_e2e_restore   HERE STRIH STATE_FILE -> ALWAYS 0 (cleanup-safe; a failed restore is
#                                                          fail-SAFE: the inputs just stay connected
#                                                          until the next strih OBS launch re-applies
#                                                          the roles)

# connect_on_show_strih_marker set|clear STRIH -> ALWAYS 0. The strih-side hold marker, over plain ssh
# on the Linux strih only (a Windows strih never runs the role apply). CONNECT_ON_SHOW_MARKER_CMD
# overrides the whole operation (tests: run with "<set|clear> <strih>"). A failed write is a WARNING:
# the live WS hold below still holds unless OBS is relaunched mid-run.
connect_on_show_strih_marker() {
  local op="$1" strih="$2" cmd
  if [ -n "${CONNECT_ON_SHOW_MARKER_CMD:-}" ]; then
    $CONNECT_ON_SHOW_MARKER_CMD "$op" "$strih" || true
    return 0
  fi
  if ! declare -F strih_platform >/dev/null || [ "$(strih_platform "$strih")" != linux ]; then
    return 0
  fi
  if [ "$op" = set ]; then
    cmd='mkdir -p "$HOME/.camera-box" && touch "$HOME/.camera-box/connect-on-show-e2e-hold"'
  else
    cmd='rm -f "$HOME/.camera-box/connect-on-show-e2e-hold"'
  fi
  if ! timeout "${CONNECT_ON_SHOW_MARKER_SSH_TIMEOUT:-20}" sshpass -p "${STRIH_PW:-newlevel}" ssh \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 \
      "${STRIH_USER:-newlevel}@${strih}" "$cmd" >/dev/null 2>&1; then
    echo "WARNING: issue 1242 connect-on-show: could not ${op} the strih-side hold marker on ${strih} (a strih OBS relaunch during this run would re-park the held inputs)" >&2
  fi
  return 0
}

connect_on_show_e2e_hold() {
  local here="$1" strih="$2" state="$3"
  echo "    issue 1242 connect-on-show hold: every strih program-path camera input stays connected for this run (state $state)"
  connect_on_show_strih_marker set "$strih"
  if ! timeout "${CONNECT_ON_SHOW_HOLD_TIMEOUT_S:-60}" python3 "$here/obs_phase2.py" connect-on-show \
      --host "$strih" --password "${OBS_PASSWORD:-}" --hold "$state"; then
    echo "ERROR: issue 1242 connect-on-show hold FAILED on $strih -- a hidden program-path input would be measured cold/parked; aborting the run" >&2
    return 1
  fi
  return 0
}

# connect_on_show_e2e_wait_live HERE STRIH STATE_FILE -> ALWAYS 0. After the hold flips the flags, each
# held input still has to UNPARK (its receiver thread polls every 5 ms), run the issue-1096 fresh
# finder (up to ~2 s), connect and relock its genlock FIFO. Wait, BOUNDED by WALL time, until every
# held input's last park line is not `state=parked` and its strih `genlock-fifo audit received=` has
# ADVANCED past its first sample (or, when the first read had no audit line for it, is present at
# all) -- so the next liveness checks ([1/8] pixel liveness, [2/8] reverify) never race a cold
# reconnect. Fail-OPEN on the budget (a WARNING naming the laggards; the downstream gates own the real
# liveness verdict). Needs genlock_park_state_of (scripts/lib/genlock-park.sh) and, unless
# CONNECT_ON_SHOW_LOG_READ_CMD overrides the read (tests), mv_reverify_probe_raw (the shared strih
# OBS-log reader, scripts/lib/mv-reverify-escalate.sh) -- both sourced by recording-e2e.sh.
connect_on_show_e2e_wait_live() {
  local here="$1" strih="$2" state="$3" names raw n recv st budget poll lag start now
  names="$(python3 -c 'import json, sys; print("\n".join(json.load(open(sys.argv[1]))))' "$state" 2>/dev/null || true)"
  if [ -z "$names" ]; then
    return 0
  fi
  budget="${CONNECT_ON_SHOW_LIVE_WAIT_S:-30}"
  poll="${CONNECT_ON_SHOW_LIVE_POLL_S:-2}"
  start="$(date +%s)"
  local -A base=()
  raw="$(connect_on_show_read_log "$strih")"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    base["$n"]="$(printf '%s\n' "$raw" | connect_on_show_received_of "$n")"
  done <<<"$names"
  while :; do
    lag=""
    while IFS= read -r n; do
      [ -n "$n" ] || continue
      recv="$(printf '%s\n' "$raw" | connect_on_show_received_of "$n")"
      st="$(printf '%s\n' "$raw" | genlock_park_state_of "$n")"
      if [ "$st" = parked ] || [ -z "$recv" ]; then
        lag="${lag:+$lag, }$n"
      elif [ -n "${base[$n]}" ] && [ "$recv" -le "${base[$n]}" ]; then
        lag="${lag:+$lag, }$n"
      fi
    done <<<"$names"
    now="$(date +%s)"
    if [ -z "$lag" ]; then
      echo "    issue 1242 connect-on-show hold: every held input is delivering again ($((now - start))s)"
      return 0
    fi
    if [ $((now - start)) -ge "$budget" ]; then
      echo "WARNING: issue 1242 connect-on-show hold: not yet delivering after ${budget}s: $lag -- continuing (the liveness gates below own the verdict)" >&2
      return 0
    fi
    sleep "$poll" || true
    raw="$(connect_on_show_read_log "$strih")"
  done
}

# connect_on_show_read_log STRIH -> stdout: the strih OBS-log tail (empty on a failed read).
connect_on_show_read_log() {
  if [ -n "${CONNECT_ON_SHOW_LOG_READ_CMD:-}" ]; then
    $CONNECT_ON_SHOW_LOG_READ_CMD "$1" 2>/dev/null || true
    return 0
  fi
  mv_reverify_probe_raw "$1" "" 2>/dev/null || true
}

# connect_on_show_received_of SRC -> stdout: the newest `received=` of SRC's audit line on STDIN, or
# empty. Quote-anchored, byte-safe, drain-safe (the frozen-input-alert-watchdog extraction).
connect_on_show_received_of() {
  { LC_ALL=C grep -aF "genlock-fifo audit '$1':" || true; } \
    | { tail -n 1 || true; } \
    | { LC_ALL=C sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p' || true; } \
    | { tail -n 1 || true; }
}

connect_on_show_e2e_restore() {
  local here="$1" strih="$2" state="$3"
  # the marker FIRST: a strih OBS launch racing this restore must never keep the mains connected
  connect_on_show_strih_marker clear "$strih"
  if [ -z "$state" ] || [ ! -f "$state" ]; then
    return 0
  fi
  if timeout "${CONNECT_ON_SHOW_HOLD_TIMEOUT_S:-60}" python3 "$here/obs_phase2.py" connect-on-show \
      --host "$strih" --password "${OBS_PASSWORD:-}" --restore "$state"; then
    return 0
  fi
  echo "WARNING: issue 1242 connect-on-show restore FAILED on $strih (state kept at $state) -- the held inputs stay connected (full bandwidth) until the next strih OBS launch re-applies the roles or a manual: python3 scripts/obs_phase2.py connect-on-show --host $strih --restore $state" >&2
  return 0
}
