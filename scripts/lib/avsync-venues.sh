#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the per-venue lipsync table + pure accessors, no
# top-level statements besides constants and two lib sources) -- the sibling scripts/lib/obs-fleet.sh /
# strih-box-facts.sh convention: NO `set -euo pipefail` here, because sourcing runs in the CALLER's
# shell (the dev2 measurer and both dev1 watchdogs set their own mode). Every function is safe under a
# caller's `set -euo pipefail`.
#
# scripts/lib/avsync-venues.sh -- issue 1441 (owner 15.10.2026: one unified PP/SNV solution, Linux is
# the standard): the ONE table of the venues whose stream program the SyncNet lipsync measurement
# watches. One row per venue, `venue|box|host|key`:
#   * venue -- the venue name (`snv`, `pp`; lower-case letters/digits), the key of every per-venue
#              file on dev2 (heartbeat, day log) and of every per-venue state/dedup key on dev1.
#   * box   -- the venue's stream box (its obs-fleet row name).
#   * host  -- how dev1/dev2 dial that box: its obs-fleet row host (scripts/lib/obs-fleet.sh), never a
#              hand-typed address. The PP boxes carry 10.77.8.x on the box but dev1/dev2 reach them as
#              stream-pp.lan (10.76.8.x through companion-pp) -- measured on issue 1441.
#   * key   -- the RTMP stream key the box's local restreamer serves at rtmp://<host>:1234/live/<key>.
#
# Where the rows come from (derived, never typed):
#   1. every fact file `scripts/strih-boxes/<box>.env` with STRIH_ROLE=stream (the Linux stream role,
#      setup-strih --box <box>): venue = STRIH_VENUE, key = the box name (the issue-1436 convention:
#      stream-pp publishes rtmp://127.0.0.1:1234/live/stream-pp). Parsed with the loader's own parser
#      (strih_box_parse_file -- never sourced or executed).
#   2. the declared Windows stream box(es) below that have no fact file yet: today only stream-snv
#      (hand-built, key obs-e2e-test). Owner ROZHODNUTÉ 15.10.2026 on issue 1441: stream-snv moves to
#      the Linux stream role once its program audio runs over Dante; its fact file then wins over this
#      row (a stale declared row is named on stderr) and the row is deleted.
# A new venue's stream box is therefore a fact file + an obs-fleet row -- no edit here.
#
# AVSYNC_VENUE_TABLE (whole-table override, `venue|box|host|key` rows) replaces the derivation -- the
# test / ops seam, like OBS_FLEET. Every row, derived or overridden, is validated; a bad row is dropped
# with one stderr line naming it (never a guessed URL).
#
# A caller that reads the table many times per pass (the dev1 watchdogs, the measurer) runs
# avsync_venue_table_load ONCE in its own shell: the derived rows are cached for the inputs they came
# from, so every later accessor (also inside `$(...)`) reuses them and the warnings print once.

_avsync_venues_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$_avsync_venues_dir/obs-fleet.sh"
# shellcheck source=scripts/lib/strih-box-facts.sh
. "$_avsync_venues_dir/strih-box-facts.sh"

# The restreamer RTMP endpoint every stream box serves (the Windows restreamer xiu on stream-snv and the
# Linux restreamer on stream-pp both listen on :1234, app `live`).
AVSYNC_RTMP_PORT="${AVSYNC_RTMP_PORT:-1234}"
AVSYNC_RTMP_APP="${AVSYNC_RTMP_APP:-live}"

# The declared Windows stream boxes with no fact file (`venue|box|key`, one per line).
AVSYNC_DECLARED_STREAM_VENUES="snv|stream-snv|obs-e2e-test"

# The venue whose OLD single dev2 heartbeat path (~/.camera-box/avsync-watchdog-heartbeat.txt) the
# measurer keeps as a compatibility symlink to its per-venue file, so a dev1 watchdog that predates
# issue 1441 keeps reading SNV until it is redeployed. Remove with the symlink once dev1 runs the
# venue-aware watchdogs.
# shellcheck disable=SC2034  # read by scripts/avsync-measure-dev2.sh
AVSYNC_LEGACY_HEARTBEAT_VENUE=snv

# avsync_venue_name_ok NAME -> 0 iff NAME is a plain venue name (ASCII lower-case letters and digits;
# it names files on dev2 and state/dedup keys on dev1). The ASCII sets are spelled out: a bracket RANGE
# follows the locale (.claude/rules/ci-testing-gotchas.md, issue 1302).
avsync_venue_name_ok() {
  local LC_ALL=C lower='abcdefghijklmnopqrstuvwxyz' digit='0123456789'
  [[ "${1-}" =~ ^[${lower}${digit}]+$ ]]
}

# _avsync_venue_row_ok VENUE BOX HOST KEY -> 0 iff the row is well-formed (same spelled-out sets).
_avsync_venue_row_ok() {
  local LC_ALL=C lower='abcdefghijklmnopqrstuvwxyz' upper='ABCDEFGHIJKLMNOPQRSTUVWXYZ' digit='0123456789'
  avsync_venue_name_ok "${1-}" || return 1
  [[ "${2-}" =~ ^[${lower}${digit}][${lower}${digit}-]*$ ]] || return 1
  _strih_box_host "${3-}" || return 1
  [[ "${4-}" =~ ^[${lower}${upper}${digit}][${lower}${upper}${digit}._-]*$ ]] || return 1
}

# _avsync_venue_emit VENUE BOX HOST KEY SOURCE -> print the row, or a named stderr line when it is
# malformed (rc 0 either way: one bad row never ends the table).
_avsync_venue_emit() {
  if _avsync_venue_row_ok "$1" "$2" "$3" "$4"; then
    printf '%s|%s|%s|%s\n' "$1" "$2" "$3" "$4"
  else
    echo "avsync-venues: dropping malformed row '${1}|${2}|${3}|${4}' (${5})" >&2
  fi
  return 0
}

# _avsync_venue_fact_rows -> `venue|box|key` for every stream-role fact file (the loader's box order).
_avsync_venue_fact_rows() {
  local f box parsed line role venue boxes
  boxes="$(strih_box_known)"
  while IFS= read -r box; do
    [ -n "$box" ] || continue
    f="$(strih_box_dir)/${box}.env"
    if ! parsed="$(strih_box_parse_file "$f" 2>/dev/null)"; then
      echo "avsync-venues: skipping box ${box}: its fact file ${f} does not parse (scripts/lib/strih-box-facts.sh)" >&2
      continue
    fi
    role=""; venue=""
    while IFS= read -r line; do
      case "$line" in
        STRIH_ROLE=*) role="${line#*=}" ;;
        STRIH_VENUE=*) venue="${line#*=}" ;;
      esac
    done <<<"$parsed"
    [ "$role" = stream ] || continue
    printf '%s|%s|%s\n' "$venue" "$box" "$box"
  done <<<"$boxes"
  return 0
}

# _avsync_first_row_of VENUE ROWS -> the first `venue|...` line of ROWS whose venue field is VENUE
# (an exact field compare, never a regex); rc 1 when none.
_avsync_first_row_of() {
  local want="$1" line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if [ "${line%%|*}" = "$want" ]; then printf '%s' "$line"; return 0; fi
  done <<<"${2-}"
  return 1
}

# _avsync_venue_cache_key -> the inputs the derived table depends on (a cached table is reused only
# while they are unchanged).
_avsync_venue_cache_key() {
  printf '%s\037%s\037%s\037%s' "${AVSYNC_VENUE_TABLE:-}" "${STRIH_BOXES_DIR:-}" "${OBS_FLEET:-}" \
    "${AVSYNC_DECLARED_STREAM_VENUES:-}"
}

# avsync_venue_table_load -> derive the table once and cache it in this shell (run it in the caller's
# own shell, never inside `$(...)`, or the cache dies with the subshell). rc 0 always.
avsync_venue_table_load() {
  _AVSYNC_VENUE_ROWS="$(_avsync_venue_table_compute)"
  _AVSYNC_VENUE_ROWS_KEY="$(_avsync_venue_cache_key)"
  return 0
}

# avsync_venue_table -> every venue row `venue|box|host|key`, one per line: the cached rows when
# avsync_venue_table_load ran for the current inputs, else derived now.
avsync_venue_table() {
  if [ -n "${_AVSYNC_VENUE_ROWS_KEY+set}" ] && [ "${_AVSYNC_VENUE_ROWS_KEY}" = "$(_avsync_venue_cache_key)" ]; then
    [ -n "${_AVSYNC_VENUE_ROWS:-}" ] && printf '%s\n' "$_AVSYNC_VENUE_ROWS"
    return 0
  fi
  _avsync_venue_table_compute
}

# _avsync_venue_table_compute -> the override when AVSYNC_VENUE_TABLE is set, else the fact-file
# stream boxes' venues after the declared Windows rows (a fact-file venue replaces a declared row of the
# same venue).
_avsync_venue_table_compute() {
  local line venue box host key seen=" " facts declared fact_line
  if [ -n "${AVSYNC_VENUE_TABLE:-}" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      IFS='|' read -r venue box host key <<<"$line"
      case "$seen" in *" ${venue} "*) echo "avsync-venues: duplicate venue '${venue}' in AVSYNC_VENUE_TABLE -- first row kept" >&2; continue ;; esac
      seen="${seen}${venue} "
      _avsync_venue_emit "$venue" "$box" "$host" "$key" "AVSYNC_VENUE_TABLE"
    done <<<"$AVSYNC_VENUE_TABLE"
    return 0
  fi
  facts="$(_avsync_venue_fact_rows)"
  declared=""
  # the declared rows first (a fact-file row of the same venue takes the declared row's place)
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    IFS='|' read -r venue box key <<<"$line"
    if fact_line="$(_avsync_first_row_of "$venue" "$facts")"; then
      echo "avsync-venues: venue '${venue}' has a stream-role fact file now -- the declared row '${line}' is stale (delete it from AVSYNC_DECLARED_STREAM_VENUES)" >&2
      line="$fact_line"
    fi
    declared="${declared}${line}"$'\n'
  done <<<"$AVSYNC_DECLARED_STREAM_VENUES"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    IFS='|' read -r venue box key <<<"$line"
    case "$seen" in *" ${venue} "*) continue ;; esac
    if ! host="$(obs_fleet_host "$box")"; then
      echo "avsync-venues: skipping venue '${venue}': box ${box} has no fleet row in scripts/lib/obs-fleet.sh (no address to dial)" >&2
      continue
    fi
    seen="${seen}${venue} "
    _avsync_venue_emit "$venue" "$box" "$host" "$key" "box ${box}"
  done <<<"${declared}${facts}"
  return 0
}

# _avsync_venue_field VENUE N -> field N (2 box, 3 host, 4 key) of VENUE's row; rc 1 + a stderr line
# naming the known venues when VENUE has no row.
_avsync_venue_field() {
  local want="${1-}" n="$2" line venue box host key known=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    IFS='|' read -r venue box host key <<<"$line"
    known="${known:+$known }${venue}"
    if [ "$venue" = "$want" ]; then
      case "$n" in 2) printf '%s' "$box" ;; 3) printf '%s' "$host" ;; 4) printf '%s' "$key" ;; esac
      return 0
    fi
  done <<<"$(avsync_venue_table)"
  echo "avsync-venues: unknown venue '${want}' (known: ${known:-none})" >&2
  return 1
}

# avsync_venue_names -> the venue names in table order, one per line.
avsync_venue_names() {
  local line
  while IFS= read -r line; do
    [ -n "$line" ] && printf '%s\n' "${line%%|*}"
  done <<<"$(avsync_venue_table)"
  return 0
}

avsync_venue_box() { _avsync_venue_field "${1-}" 2; }
avsync_venue_host() { _avsync_venue_field "${1-}" 3; }
avsync_venue_key() { _avsync_venue_field "${1-}" 4; }
# the stream box's OBS-WebSocket host (outputActive) is the box itself, dialled the same way.
avsync_venue_ws_host() { _avsync_venue_field "${1-}" 3; }

# avsync_venue_rtmp_url VENUE -> the RTMP URL dev2 pulls the venue's encoded stream from.
avsync_venue_rtmp_url() {
  local host key
  host="$(avsync_venue_host "${1-}")" || return 1
  key="$(avsync_venue_key "${1-}")" || return 1
  printf 'rtmp://%s:%s/%s/%s' "$host" "$AVSYNC_RTMP_PORT" "$AVSYNC_RTMP_APP" "$key"
}

# avsync_venue_label VENUE -> the venue as a message shows it (`SNV`, `PP`).
avsync_venue_label() {
  printf '%s' "${1-}" | tr '[:lower:]' '[:upper:]'
}

# avsync_venue_heartbeat_file VENUE -> the venue's dev2 heartbeat file NAME (under ~/.camera-box/).
avsync_venue_heartbeat_file() {
  printf 'avsync-watchdog-heartbeat-%s.txt' "${1-}"
}

# avsync_venue_dev2_heartbeat_remote_path VENUE -> the venue's heartbeat path as a dev1 watchdog
# hands it to dev2's shell: `$HOME` stays LITERAL so dev2 expands it (the same file
# avsync_measure_heartbeat_path writes there).
avsync_venue_dev2_heartbeat_remote_path() {
  printf '$HOME/.camera-box/%s' "$(avsync_venue_heartbeat_file "${1-}")"
}

# avsync_venue_dev2_legacy_heartbeat_remote_path VENUE -> for the legacy venue only, the OLD single
# heartbeat path a dev1 reader falls back to when the per-venue file is missing (dev2 still runs the
# pre-issue-1441 single unit); empty for every other venue (PP never reads SNV's old file).
avsync_venue_dev2_legacy_heartbeat_remote_path() {
  [ "${1-}" = "$AVSYNC_LEGACY_HEARTBEAT_VENUE" ] || return 0
  printf '$HOME/.camera-box/avsync-watchdog-heartbeat.txt'
}

# avsync_venue_table_problem HOST -> one line naming why the dev1 watchdogs would watch less than they
# must (nothing selected at all, or -- with no AVSYNC_VENUES narrowing -- the table lost the legacy
# venue); empty when the table is whole. A production-critical alarm must never fail open by silently
# watching nothing.
avsync_venue_table_problem() {
  local selected
  selected="$(avsync_watchdog_venues "${1-}")"
  if [ -z "$selected" ]; then
    printf 'the venue table selects no venue -- no lipsync measurement is watched at all'
    return 0
  fi
  if [ -z "${AVSYNC_VENUES:-}" ]; then
    case $'\n'"$(avsync_venue_names)"$'\n' in
      *$'\n'"${AVSYNC_LEGACY_HEARTBEAT_VENUE}"$'\n'*) return 0 ;;
    esac
    printf "the venue table has no '%s' row -- that venue's stream goes unwatched (scripts/lib/avsync-venues.sh)" \
      "$AVSYNC_LEGACY_HEARTBEAT_VENUE"
  fi
  return 0
}

# avsync_watchdog_venues HOST -> the venues a dev1 watchdog pass reads from heartbeat HOST, one per
# line: every selected venue (avsync_selected_venues) for the dev2 measurer; only the legacy venue for
# the retired Windows "stream" host (it only ever measured SNV, on SNV's own stream box).
avsync_watchdog_venues() {
  local v
  for v in $(avsync_selected_venues); do
    if [ "${1-}" = stream ] && [ "$v" != "$AVSYNC_LEGACY_HEARTBEAT_VENUE" ]; then
      continue
    fi
    printf '%s\n' "$v"
  done
  return 0
}

# avsync_selected_venues -> the venues a dev1 watchdog pass handles, in table order: every table venue,
# or only those AVSYNC_VENUES (space-separated) names; a name with no row is named on stderr.
avsync_selected_venues() {
  local names v w found
  local -a want=()
  names="$(avsync_venue_names)"
  if [ -z "${AVSYNC_VENUES:-}" ]; then
    [ -n "$names" ] && printf '%s\n' "$names"
    return 0
  fi
  # split on whitespace WITHOUT glob expansion (a `*` names no venue, never the files in the cwd)
  read -r -a want <<<"$AVSYNC_VENUES"
  for w in "${want[@]}"; do
    found=0
    for v in $names; do [ "$v" = "$w" ] && found=1; done
    [ "$found" = 1 ] || echo "avsync-venues: AVSYNC_VENUES names '${w}', which is no venue (known: $(printf '%s' "$names" | tr '\n' ' '))" >&2
  done
  for v in $names; do
    for w in "${want[@]}"; do
      if [ "$v" = "$w" ]; then printf '%s\n' "$v"; break; fi
    done
  done
  return 0
}
