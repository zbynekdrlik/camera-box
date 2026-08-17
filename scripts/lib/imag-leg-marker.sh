#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) —
# matches the sibling scripts/lib/*.sh convention (camera-box-restart-verify.sh, cbox-burn-log-
# persist.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it
# in the CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
# scripts/recording-e2e.sh (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/imag-leg-marker.sh — issue 798: emit ONE distinct, greppable run-log marker at the
# [8/8c] imag extract declaring whether the imag leg was ACTUALLY verified this run (an imag
# partial reached dev1) or SILENTLY skipped, and — when skipped — WHY.
#
# WHY (#675 pattern): recording-e2e.sh's [8/8c] imag extract degrades gracefully on any imag-side
# failure (StopRecord returned no path / imag unreachable / stale binary / ssh hiccup / decode
# error), printing a WARNING to stderr and letting the [8/8d] merge silently omit
# `--merge-partials imag=...`. Mining the rig showed the imag partial reaches the merge in 0 of 76+
# recent runs — so a green run does NOT prove the imag leg, it silently skips it (a HIDDEN partial:
# the "ONE full test, no partials" doctrine's banned outcome). recording-verdict.rs now records
# `full_chain.imag_leg_verified` in the verdict JSON (the durable, mineable signal) at [8/8d]; THIS
# helper is its run-log twin at [8/8c] — a single unmistakable line, emitted the moment the outcome
# is known, that also names the skip REASON the verdict JSON cannot (no-recording-path vs
# extract-failed). It gates NOTHING — purely observability (report-only, path A).
#
# Added via a NEW call line (the #675 sourced-helper discipline), never by editing an anchored line
# in recording-e2e.sh — the static-anchor test suite reads only recording-e2e.sh's own text, so
# this function body is invisible to it while its emitted line still lands in the run log.

# imag_leg_run_marker <imag_partial_path> <imag_host_path>
# Prints EXACTLY one marker line to stdout. Distinct greppable tokens:
#   IMAG-LEG-VERIFIED       — the partial JSON exists on dev1 (the cam→imag leg flowed + is proven).
#   IMAG-LEG-NOT-VERIFIED   — no partial reached dev1; the message names the reason.
# Pure/read-only (a single `[ -f ]` stat + printf) — no network, no mutation; safe to unit-test by
# sourcing + calling with a temp path.
imag_leg_run_marker() {
  local partial="${1:-}" host_path="${2:-}"
  if [ -n "$partial" ] && [ -f "$partial" ]; then
    printf 'IMAG-LEG-VERIFIED: imag partial produced (%s) — the cam->imag leg is proven this run (report-only, issue 798).\n' "$partial"
  elif [ -z "$host_path" ]; then
    printf 'IMAG-LEG-NOT-VERIFIED: no imag recording path (StopRecord returned none) — cam->imag proof SKIPPED this run. A green run that skips the imag leg is a hidden partial (issue 798; ONE full test, no partials). Report-only.\n'
  else
    printf 'IMAG-LEG-NOT-VERIFIED: imag extract failed (recording-verdict-on-imag unreachable / stale binary / decode error) — cam->imag proof SKIPPED this run. A green run that skips the imag leg is a hidden partial (issue 798). Report-only.\n'
  fi
}
