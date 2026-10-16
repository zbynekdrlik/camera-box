#!/usr/bin/env bash
# scripts/lib/unit-env.sh -- read a dev1 --user unit's own Environment= (sourced, see below).
set -euo pipefail
#
# The dev1 alert watchdogs run from systemd --user timers, and some of their configuration lives
# only in the unit (or a dev1-local drop-in), e.g. OBS_WATCHDOG_GATE_BIN for obs-liveness. A tool
# that runs the SAME watchdog by hand (the development handover check) must grade it with that SAME
# configuration, or it reads a healthy box as UNKNOWN (15.10.2026: obs-liveness printed "binary not
# found" from the handover check while the timer read HEALTHY on every box).
#
# unit_env_assignments UNIT [SYSTEMCTL]
#   Prints one `NAME=value` line per variable in the unit's resolved Environment= (systemd expands
#   the %h specifiers before printing), skipping every NAME the caller's environment already sets
#   (an explicit caller value always wins). Read-only. A unit that does not exist, an unreadable
#   systemctl, or a malformed entry prints nothing for it and returns 0 -- the probe then runs
#   exactly as before.

unit_env_assignments() {
  local unit="$1" sc="${2:-systemctl}" raw item name
  raw="$("$sc" --user show "$unit" -p Environment --value 2>/dev/null || true)"
  [ -n "$raw" ] || return 0
  # systemd quotes a value that contains spaces; xargs undoes exactly that quoting.
  while IFS= read -r item; do
    case "$item" in
      [A-Za-z_]*=*) ;;
      *) continue ;;
    esac
    name="${item%%=*}"
    case "$name" in
      *[!A-Za-z0-9_]*) continue ;;
    esac
    [ -n "${!name+x}" ] && continue
    printf '%s\n' "$item"
  done < <(printf '%s' "$raw" | xargs -n1 printf '%s\n' 2>/dev/null || true)
  return 0
}
