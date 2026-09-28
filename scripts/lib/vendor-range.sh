#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure git range helpers, no top-level statements) -- the sourcing script owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/vendor-range.sh -- the ONE merge-base-scoped vendored-genlock git range implementation
# (issue 1384). Two early gates pin a DEPLOYED genlock build SHA against the vendored tree on
# origin/main and used to carry line-for-line copies of the same three helpers:
#   - scripts/drift-guard.sh (imag_genlock_range_log / _ahead_log / _on_dev -- the issue-531 moving
#     pin of the genlock_build facet, `--oneline` over vendor/obs-studio + vendor/distroav);
#   - scripts/lib/version-integrity-vendor-pin.sh (vendor_pin_range_log / _ahead_log / _on_dev --
#     the issue-1137 report-only vendor-pin alarm, `--format='%h %s'` over the whole vendor/ tree).
# Both now call the functions below with their own git-log format and pathspec, so each gate's
# output is byte-identical to its former copy and a fix to the range logic lands in both at once.
#
# The three helpers answer, for SHA against origin/main (+ origin/dev):
#   vendor_range_lag_log    -- which vendored commits origin/main has that SHA's lineage lacks (STALE)
#   vendor_range_ahead_log  -- which vendored commits SHA has that origin/main lacks (AHEAD)
#   vendor_range_on_dev     -- is SHA reachable from origin/dev (a recognized release candidate)
# How each gate COMBINES them into a verdict stays in that gate (genlock_build_drift_report in
# drift-guard.sh, genlock_vendor_pin_verdict in the vendor-pin lib); this lib only reads git.
#
# Why each piece is there (moved here from the two copies' headers):
#   - `--end-of-options` (git >= 2.24): SHA is used UNVALIDATED -- drift-guard reads it from a file
#     over ssh (GENLOCK_BUILD_SHA.txt), the gate from a bundle-state JSON. A truncated/corrupted value
#     shaped like a git long option (e.g. `--grep=x`) would otherwise be CONSUMED as a flag, exit 0
#     with EMPTY output, and read as "box is current" -- a FALSE OK (issue 531). With the marker,
#     any value is always a revision, so a malformed one fails LOUD (non-zero from merge-base/log).
#   - The merge-base scoping of the STALE range (issue 1292): a plain ancestry range
#     `SHA..origin/main` reads FALSELY STALE for a box that is genuinely AHEAD of main on the dev
#     candidate line. This repo's two-branch workflow never merges main's own PR-merge commits back
#     into dev (top-level CLAUDE.md GOTCHA), so those merge commits are never git-ancestors of a dev
#     build even when the build is a CONTENT superset of them (live: box=3ffe2fbc5 read "2
#     genlock-commit(s) behind origin/main [cfdbdb003,e5b46ab60]"). Starting the range at
#     `git merge-base SHA origin/main` reads it correctly empty for a superset, while a genuinely
#     stale SHA (merge-base far back) still lists every missing vendor commit.
#   - The MECHANISM behind that empty range (issue-1292 review finding S1) is git's default HISTORY
#     SIMPLIFICATION for a `log -- <pathspec>` walk (git 2.43 revision.c: a merge TREESAME to one
#     parent for the given paths collapses onto that parent's line). `--first-parent` on the same
#     range DOES list the "missing" merge commits. NEVER add `--first-parent` or `--full-history` to
#     the lag log below: either silently reintroduces the false-STALE bug (and turns the
#     merge-base-scoped tests in tests/drift_guard.rs + tests/version_integrity_gate.rs red).
#   - The explicit `-n` guard on the AHEAD / on-dev helpers (issue-1292 review finding S3): an EMPTY
#     SHA would otherwise resolve `origin/main..` as `origin/main..HEAD` (git's empty-right-side =
#     HEAD convention) instead of failing, so it returns 128 LOUD -- matching the lag log, whose own
#     `git merge-base` already rejects an empty SHA.
#   - vendor_range_on_dev fails CLOSED: exit 0 only when SHA is provably reachable from origin/dev,
#     non-zero when it is not OR cannot be resolved. A build carrying vendored commits reachable
#     from NEITHER origin/main NOR origin/dev is an unrecognized ORPHAN, which the early-gate-pin
#     doctrine says must SCREAM (.claude/rules/early-gate-pin-doctrine.md), never a quiet OK.
#
# Testability: pure git I/O against REPO_ROOT, no ssh / box. Each gate's own tests drive the thin
# wrappers against a throwaway synthetic two-branch repo (tests/drift_guard.rs,
# tests/version_integrity_gate.rs) -- never against this repo's ever-advancing origin/main.
#
# LOG_FORMAT is ONE git-log formatting argument (`--oneline`, or `--format=%h %s` passed as a single
# word); PATH... is the pathspec (at least one path -- an empty pathspec would silently widen the
# range to the whole tree, so it is refused with rc 2).

# vendor_range_lag_log REPO_ROOT SHA LOG_FORMAT PATH... -> prints `git log LOG_FORMAT
# $(git merge-base SHA origin/main)..origin/main -- PATH...` (one vendored commit per line that
# origin/main carries and SHA's own lineage never received -- SHA is genuinely STALE relative to it).
# Exit status mirrors the FIRST failing git call (the merge-base resolve, then the log).
vendor_range_lag_log() {
  [ "$#" -ge 4 ] || return 2
  local repo_root="$1" sha="$2" log_format="$3" base
  shift 3
  base="$(git -C "$repo_root" merge-base --end-of-options "$sha" origin/main 2>/dev/null)" \
    || return $?
  git -C "$repo_root" log "$log_format" --end-of-options "${base}..origin/main" \
    -- "$@" 2>/dev/null
}

# vendor_range_ahead_log REPO_ROOT SHA LOG_FORMAT PATH... -> prints `git log LOG_FORMAT
# origin/main..SHA -- PATH...` (one vendored commit per line SHA carries that origin/main does not);
# exit status mirrors that `git log` call; an empty SHA returns 128.
vendor_range_ahead_log() {
  [ "$#" -ge 4 ] || return 2
  local repo_root="$1" sha="$2" log_format="$3"
  shift 3
  [ -n "$sha" ] || return 128
  git -C "$repo_root" log "$log_format" --end-of-options "origin/main..${sha}" \
    -- "$@" 2>/dev/null
}

# vendor_range_on_dev REPO_ROOT SHA -> exit 0 when SHA is reachable from origin/dev (a build on the
# dev candidate line, deployed ahead of main), non-zero otherwise (unreachable, or SHA itself
# unresolvable); an empty SHA returns 128.
vendor_range_on_dev() {
  local repo_root="$1" sha="$2"
  [ -n "$sha" ] || return 128
  git -C "$repo_root" merge-base --is-ancestor --end-of-options "$sha" origin/dev 2>/dev/null
}
