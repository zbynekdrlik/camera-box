#!/usr/bin/env bash
#
# w32time-gate.sh — Windows W32Time verify-gate for strih + stream (#598).
#
# WHY THIS GATE EXISTS: dantesync is the SOLE clock authority on the whole rig. #591/#596/#597
# made a 2nd timesync daemon a hard FAIL on the LINUX cam appliances (scripts/lib/
# timesync-authority.sh). The exact same desync class exists on the WINDOWS OBS boxes that do the
# genlock (strih 10.77.9.202, stream 10.77.9.204): the built-in Windows Time service (W32Time) can
# run as an NTP/domain client, competing with dantesync on the very boxes doing the genlock. Both
# boxes were fixed live 2026-07-07 (W32Time Stopped + Disabled) but until this gate that was a
# manual, unverified invariant — nothing prevented drift back (see .claude/skills/ops/SKILL.md).
#
# NODE ACCESS: ssh to the Windows OBS boxes is DENIED (see scripts/dantesync-gate.sh's own header
# for the identical constraint on its Windows half), so this script cannot gather W32Time state
# itself. The caller (the autopilot worker / operator, who HAS the win-* MCP) runs
# w32time_gather_remote_snippet()'s read-only command block on each box and writes the combined
# output to a local file, then passes it via --win-status NAME=FILE — the SAME pattern
# dantesync-gate.sh already uses for strih/stream. A box with NO status file is UNKNOWN -> the
# gate fails (never a silent pass).
#
# Usage:
#   w32time-gate.sh --win-status strih=/tmp/w32time-strih.txt --win-status stream=/tmp/w32time-stream.txt
#   w32time-gate.sh --help
#
# Exit codes: 0 = every gated box confirmed W32Time is NOT a (current or latent) 2nd clock
# authority, 20 = at least one box FAILED (W32Time is running as, or configured to resurrect as, a
# competing authority), 11 = at least one box UNREACHABLE / status UNKNOWN (incomplete — NOT
# clean), 1 = usage error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/w32time-authority.sh
. "$HERE/lib/w32time-authority.sh"

usage() {
  cat <<EOF
w32time-gate.sh — Windows W32Time verify-gate for strih+stream (#598).

FAILS unless EVERY gated box confirms W32Time is not acting as (or configured to come back as) a
2nd clock authority. dantesync must be the SOLE timesync authority on strih+stream, the same
mandate #591 already enforces on the Linux cam appliances.

Usage:
  w32time-gate.sh --win-status NAME=FILE [--win-status NAME=FILE ...]

Options:
  --win-status N=FILE  a Windows box N whose combined W32Time status text (sc query + sc qc +
                       reg query Type + w32tm /query /status) the caller wrote to FILE (ssh to
                       Windows is denied; the win-* MCP holder pre-fetches it via
                       w32time_gather_remote_snippet()). Repeatable.

Exit: 0 = all boxes OK, 20 = a box FAILED (active or latent 2nd authority), 11 = a box
UNREACHABLE/UNKNOWN, 1 = usage error.
EOF
}

main() {
  local -a win_status=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --win-status) shift; win_status+=("${1:-}") ;;
      -h | --help) usage; exit 0 ;;
      --*) echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *) echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if [ "${#win_status[@]}" -eq 0 ]; then
    echo "ERROR: no boxes to gate (--win-status is empty)." >&2
    echo "The W32Time gate cannot certify strih/stream with zero boxes — refusing to pass." >&2
    exit 1
  fi

  echo "== w32time-gate (#598): W32Time must NOT be a (current or latent) 2nd clock authority =="
  echo "   dantesync is the sole timesync authority on strih+stream (the same mandate #591 enforces on Linux)"

  local bad=0 unknown=0 ok=0
  local entry name file text state start_type reg_type source verdict class
  for entry in "${win_status[@]}"; do
    name="${entry%%=*}"; file="${entry#*=}"
    if [ -z "$file" ] || [ ! -s "$file" ]; then
      printf '  %-14s UNKNOWN      (no status file %s — win-* MCP fetch missing)\n' "$name" "${file:-<none>}"
      unknown=$((unknown + 1)); continue
    fi
    text="$(cat "$file" 2>/dev/null || true)"
    state="$(w32time_state_from_text "$text")"
    start_type="$(w32time_start_type_from_text "$text")"
    reg_type="$(w32time_reg_type_from_text "$text")"
    source="$(w32time_source_from_text "$text")"
    verdict="$(w32time_daemon_verdict "$state" "$start_type" "$reg_type" "$source")"
    class="$(w32time_verdict_class "$verdict")"
    case "$class" in
      OK)
        printf '  %-14s OK           (state=%s start_type=%s type=%s — not a 2nd authority)\n' \
          "$name" "${state:-?}" "${start_type:-?}" "${reg_type:-?}"
        ok=$((ok + 1)) ;;
      BAD)
        printf '  %-14s FAIL         (%s)\n' "$name" "${verdict#FAIL: }"
        bad=$((bad + 1)) ;;
      *)
        printf '  %-14s UNKNOWN      (%s)\n' "$name" "${verdict#UNKNOWN: }"
        unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} box(es) have W32Time acting as (or configured to resurrect as) a 2nd clock authority." >&2
    echo "!! Stop + Disable W32Time on the failed box(es) — dantesync must be the sole timesync authority." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further box(es) UNKNOWN — also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNREACHABLE or status UNKNOWN — NOT clean." >&2
    echo "!! Every gated box must report a readable W32Time state before certifying. (${ok} box(es) were OK.)" >&2
    exit 11
  fi
  echo "GATE PASS — ${ok} box(es) confirmed W32Time is not a (current or latent) 2nd clock authority."
  exit 0
}

# Source-guard: when sourced by the unit tests, expose the functions and stop (do not run main).
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

main "$@"
