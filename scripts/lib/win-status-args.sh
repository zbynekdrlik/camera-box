#!/usr/bin/env bash
# scripts/lib/win-status-args.sh — shared "--win-status NAME=FILE" parse + missing-file guard
# (#622, extracted from scripts/w32time-gate.sh + scripts/dantesync-gate.sh). Both gates read a
# Windows box's pre-fetched status text (ssh to Windows is denied on this rig, so the caller — the
# autopilot worker / operator, who HAS the win-* MCP — pre-fetches each box's status to a local
# file) via a repeatable `--win-status NAME=FILE` CLI arg. #598's own review flagged this exact
# block as character-for-character duplicated between the two gates: the same
# name="${entry%%=*}"; file="${entry#*=}" split, the same
# [ -z "$file" ] || [ ! -s "$file" ] guard, the same printf width specifiers and wording for the
# "no status file -> UNKNOWN" diagnostic, and the same `cat "$file" 2>/dev/null || true` read. This
# is the same "second, driftable copy" class scripts/lib/timesync-authority.sh was extracted for
# (#596) — filed as #622.
#
# Source-only: this file defines one function and performs no side effects on its own.

# win_status_parse_entry ENTRY -> parses a "--win-status NAME=FILE" entry (ENTRY is one array
# element of the CLI's repeatable --win-status flag).
#
#   * FILE present and non-empty: sets the globals WIN_STATUS_NAME (the box name) and
#     WIN_STATUS_TEXT (the file's content, via the SAME `cat "$file" 2>/dev/null || true` both
#     gates used inline) and returns 0. Prints nothing.
#   * FILE missing or empty: prints the gates' standard per-box "UNKNOWN (no status file ...)"
#     diagnostic table row to stdout (unchanged wording/width from both original inline blocks)
#     and returns 1. WIN_STATUS_NAME/WIN_STATUS_TEXT are left unset/stale on this path — callers
#     must not read them after a non-zero return (the `continue` in the loop pattern below already
#     ensures that).
#
# Callers loop like:
#   for entry in "${win_status[@]}"; do
#     if ! win_status_parse_entry "$entry"; then
#       unknown=$((unknown + 1)); continue
#     fi
#     name="$WIN_STATUS_NAME"; text="$WIN_STATUS_TEXT"
#     ...use name/text...
#   done
win_status_parse_entry() {
  local entry="$1" name file
  name="${entry%%=*}"; file="${entry#*=}"
  if [ -z "$file" ] || [ ! -s "$file" ]; then
    printf '  %-14s UNKNOWN      (no status file %s — win-* MCP fetch missing)\n' "$name" "${file:-<none>}"
    return 1
  fi
  # shellcheck disable=SC2034  # intentional out-params: read by the sourcing gate script's loop
  WIN_STATUS_NAME="$name"
  # shellcheck disable=SC2034  # intentional out-params: read by the sourcing gate script's loop
  WIN_STATUS_TEXT="$(cat "$file" 2>/dev/null || true)"
  return 0
}
