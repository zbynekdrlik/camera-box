#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (frozen-input-health.sh, cadence-health.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so strict mode here would leak into whichever caller sources it. Every function below is
# written to be SAFE under a caller's set -euo pipefail (drain-safe pipelines, `|| true` tails).
#
# scripts/lib/genlock-park.sh -- issue 1242: the bash twin of scripts/genlock_park.py.
#
# strih-lx pulls FULL bandwidth only for the cameras that are SHOWN. The vendored DistroAV receiver
# PARKS a genlocked program-path input (`genlock_connect_on_show`) while nothing shows it and logs
#   genlock-park '<src>': state=parked parked_s=N (...)     (every 5 s while parked)
#   genlock-park '<src>': state=unparked parked_s=N (...)   (once, on show)
# A parked input's `genlock-fifo audit received=` stops advancing BY DESIGN, so every received=
# consumer (the dev1 frozen-input / cadence / ndi-halving watchdogs) classifies it as HIDDEN BY
# DESIGN -> SKIP, and the strih frozen-input enumeration reads the always-connected `MV <input>`
# monitor twin in its place (never both -> never a double page for one camera).
#
# Byte-for-byte twin of scripts/genlock_park.py (park_state_of / watch_set), pinned to it over the
# same fixtures by tests/python/test_genlock_park_1242.py. Drain-safe: every pipeline stage ends
# `|| true` (a no-match grep / an early-closing tail must never kill a caller running under
# set -euo pipefail -- the drift-guard-log-parsers class). Byte-safe: LC_ALL=C grep -a (a PS-fetched
# or odd-glyph OBS-log tail must never read as binary -- the issue-1258 class).

# genlock_park_state_of <source> -> stdout: parked | unparked | (empty). Reads the raw OBS-log tail on
# STDIN; the state of the source's LAST quote-anchored park line wins.
genlock_park_state_of() {
  local src="${1:-}"
  if [ -z "$src" ]; then
    cat >/dev/null || true
    return 0
  fi
  { LC_ALL=C grep -aF "genlock-park '$src': state=" || true; } \
    | { tail -n 1 || true; } \
    | { LC_ALL=C sed -n 's/.*: state=\(parked\|unparked\)\b.*/\1/p' || true; } \
    | { tail -n 1 || true; }
}

# genlock_park_is_monitor_twin <name> -> exit 0 iff an always-connected multiview twin ('MV ...').
genlock_park_is_monitor_twin() {
  case "${1:-}" in "MV "*) return 0 ;; *) return 1 ;; esac
}

# genlock_park_watch_set <newline-separated names> -> stdout: the names to WATCH, one per line, order
# kept. Reads the raw OBS-log tail on STDIN. Exactly ONE live receiver per camera: a live main is
# watched (its twin dropped); a PARKED main is dropped and its twin watched; a twin whose main is not
# enumerated is kept; a parked main with no twin is dropped (hidden by design, nothing to observe).
genlock_park_watch_set() {
  local names="${1:-}" raw n main state
  raw="$(cat || true)"
  local -A present=()
  while IFS= read -r n; do
    [ -n "$n" ] && present["$n"]=1
  done <<<"$names"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    if genlock_park_is_monitor_twin "$n"; then
      main="${n#MV }"
      if [ -n "${present[$main]:-}" ]; then
        state="$(printf '%s\n' "$raw" | genlock_park_state_of "$main")"
        [ "$state" = parked ] || continue
      fi
      printf '%s\n' "$n"
    else
      state="$(printf '%s\n' "$raw" | genlock_park_state_of "$n")"
      [ "$state" = parked ] && continue
      printf '%s\n' "$n"
    fi
  done <<<"$names"
  return 0
}
