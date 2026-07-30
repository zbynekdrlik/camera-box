#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (rig-test-dropin.sh, audio-marker-check.sh,
# camera-box-restart-verify.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing
# this file executes it in the CALLER's shell, so imposing strict mode here would leak into
# whichever caller sources it. recording-e2e.sh (the only caller today) already sets
# -euo pipefail itself.
#
# scripts/lib/stale-artifact-guard.sh -- warns loudly when a `dante-*.json` file is already
# sitting in the harness's own run directory ($OUTDIR) that the harness itself did NOT write.
#
# WHY (#835): `.claude/skills/e2e/SKILL.md` used to instruct an operator to hand-pre-fetch each
# Windows box's DanteSync status into `$OUTDIR/dante-{strih,stream}.json` (via the win-* MCP,
# reading `\\.\pipe\dantesync`) BEFORE launching the harness. That flow was replaced by a LIVE
# --win-http fetch in #648 (`e2cfeb3d7`, 2026-07-10) -- `recording-e2e.sh` has not written a
# `dante-*.json` file since. Anyone still following the stale runbook (or reusing a fixed RUN_ID
# whose $OUTDIR was never cleaned between runs) drops an artifact into the run directory that
# nothing reads today but that COULD be silently misread by a future caller as a real (and
# possibly ancient) clock reading -- exactly the false-GREEN hazard #835 traces (a live incident:
# a 21-day-old `dante-stream.json` sat in a run dir with a fresh mtime, indistinguishable from a
# current reading by file metadata alone). This check makes that artifact announce itself the
# moment the harness sees it, instead of lurking silently.
#
# Source-only: defines a PURE, directly-callable local function -- no ssh, no top-level
# statements (unlike scripts/lib/camera-box-restart-verify.sh's REMOTE-command-string builders,
# $OUTDIR lives on dev1 itself, so this runs straight in the caller's own shell). Safe to source
# from recording-e2e.sh and from unit tests.

# stale_dante_artifact_warn OUTDIR -> if OUTDIR already contains one or more `dante-*.json`
# files, print a loud (non-fatal) WARNING to stderr naming EACH one. Prints nothing when OUTDIR
# has no such file (the expected, healthy case since #648) or does not exist yet. Never aborts
# the run and never fails the caller's shell: the value in a stray file is inert today (nothing
# in the current gate reads it) -- this is advisory self-announcement, not a gate.
stale_dante_artifact_warn() {
  local outdir="$1" f
  [ -d "$outdir" ] || return 0
  for f in "$outdir"/dante-*.json; do
    [ -e "$f" ] || continue   # glob matched nothing -- the literal pattern stays unexpanded
    echo "WARNING #835: stray artifact $f found in this run's directory." >&2
    echo "  This harness has NOT written a dante-*.json file since #648 (it fetches DanteSync" >&2
    echo "  status LIVE over HTTP now, via --win-http -- no manual pre-fetch, no win-* MCP)." >&2
    echo "  This is a leftover from a stale manual pre-fetch runbook, or a reused RUN_ID whose" >&2
    echo "  run directory was never cleaned. Nothing in the current gate reads it, but delete" >&2
    echo "  it if you don't know its origin -- a stale clock reading must never be mistaken" >&2
    echo "  for a fresh one." >&2
  done
  return 0
}
