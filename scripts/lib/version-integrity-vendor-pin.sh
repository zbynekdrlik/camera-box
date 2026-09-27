#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions only, no top-level statements) -- the sourcing gate owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/version-integrity-vendor-pin.sh -- the genlock vendor-pin report-only ALARM family of
# scripts/version-integrity-gate.sh (issues 1137 + 1292), moved out of the gate VERBATIM (issue 1377:
# the gate was over the repo's 1000-line file budget). No behaviour change: the gate sources this lib
# BEFORE its source-guard, exactly where these functions used to sit, so sourcing the gate (the unit
# tests in tests/version_integrity_gate.rs) still defines every function. The three git range helpers
# mirror drift-guard.sh's imag_genlock_range_log / imag_genlock_ahead_log / imag_genlock_on_dev;
# genlock_vendor_pin_verdict is the pure verdict the gate's main() prints (report-only, never folded
# into the gate's exit code). The moved comments were written inside the gate: "this file's own
# pre-existing `--format=` style elsewhere" and "main()" mean scripts/version-integrity-gate.sh.

# vendor_pin_range_log REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: prints `git log
# --format='%h %s' $(git merge-base DEPLOYED_SHA origin/main)..origin/main -- vendor/` (one
# vendor-touching commit per line origin/main carries that DEPLOYED_SHA's own lineage never
# received -- i.e. DEPLOYED_SHA is genuinely LAGGING relative to it); exit status mirrors the FIRST
# failing git call (the merge-base resolve, then the log). Mirrors drift-guard.sh's
# imag_genlock_range_log's LOGIC exactly (same #1292 merge-base fix, same `--end-of-options` defense
# against an unvalidated SHA value shaped like a git flag) -- two deliberate differences: `--format=
# '%h %s'` instead of `--oneline` (identical output shape, consistent with this file's own
# pre-existing `--format=` style elsewhere), and scoped to the WHOLE `vendor/` tree instead of
# just `vendor/obs-studio vendor/distroav`, because this facet covers every deployed box (strih,
# stream, imag), not only imag's own consumed paths (see genlock_parity_consumed_paths for that
# per-box distinction, which this facet deliberately does NOT apply -- it PINS every box's deployed
# SHA against the single newest vendor/** commit on origin/main, regardless of which sub-paths that
# box's own build actually consumes).
#
# #1292 root cause this exists to fix: the caller used to compute PENDING_LIST via a PLAIN ancestry
# range (`DEPLOYED_SHA..origin/main`), which reads LAGGING for a deployed SHA that is genuinely AHEAD
# of main on the dev candidate line -- this repo's two-branch workflow never merges main's own merge
# commits back into dev (top-level CLAUDE.md GOTCHA), so a deployed SHA's dev-side lineage is never a
# git-ancestor of main's merge commits even when it is a CONTENT superset of them. Scoping the range
# to the common ancestor (git merge-base) removes ONLY that false positive -- see
# vendor_pin_ahead_log/vendor_pin_on_dev immediately below for the AHEAD-direction classification,
# and genlock_vendor_pin_verdict for how the three combine into the report-only verdict. Isolated so
# it is independently testable against a throwaway synthetic repo (tests/version_integrity_gate.rs)
# -- no live git fetch needed.
vendor_pin_range_log() {
  local repo_root="$1" deployed="$2" base
  base="$(git -C "$repo_root" merge-base --end-of-options "$deployed" origin/main 2>/dev/null)" \
    || return $?
  git -C "$repo_root" log --format='%h %s' --end-of-options "${base}..origin/main" \
    -- vendor/ 2>/dev/null
}

# vendor_pin_ahead_log REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: the AHEAD-direction
# counterpart to vendor_pin_range_log -- prints `git log --format='%h %s'
# origin/main..DEPLOYED_SHA -- vendor/` (one vendor-touching commit per line DEPLOYED_SHA carries
# that origin/main does not). Mirrors drift-guard.sh's imag_genlock_ahead_log's logic (same
# `--end-of-options` defense, same explicit `-n` empty-SHA guard so an empty DEPLOYED_SHA fails LOUD
# instead of silently resolving `origin/main..` as `origin/main..HEAD`; same `--format=` vs
# `--oneline` style difference as vendor_pin_range_log above).
vendor_pin_ahead_log() {
  local repo_root="$1" deployed="$2"
  [ -n "$deployed" ] || return 128
  git -C "$repo_root" log --format='%h %s' --end-of-options "origin/main..${deployed}" \
    -- vendor/ 2>/dev/null
}

# vendor_pin_on_dev REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: exit 0 when DEPLOYED_SHA is
# reachable from origin/dev (a recognized release-candidate bundle deployed ahead of main),
# non-zero otherwise (unreachable, or DEPLOYED_SHA itself unresolvable) -- fail CLOSED, never a
# silent "yes" on an unresolvable check. Mirrors drift-guard.sh's imag_genlock_on_dev exactly. A
# deployed SHA that carries vendor commits reachable from NEITHER origin/main NOR origin/dev is an
# unrecognized/orphan build (early-gate-pin doctrine: "an orphan release must SCREAM"), never a
# quiet OK just because it happens to be a content superset of main.
vendor_pin_on_dev() {
  local repo_root="$1" deployed="$2"
  [ -n "$deployed" ] || return 128
  git -C "$repo_root" merge-base --is-ancestor --end-of-options "$deployed" origin/dev 2>/dev/null
}

# genlock_vendor_pin_verdict DEPLOYED_SHA NEWEST_VENDOR_SHA PENDING_LIST [AHEAD_LIST] [ON_DEV] ->
# #1137 REPORT-ONLY vendor-pin ALARM. The gate's only genlock check is CROSS-BOX PARITY
# (genlock_build_parity_report, #756/#949) -- it PASSES a UNIFORMLY-stale fleet where every box
# agrees on an OLD build (live: both boxes 03cd9c073 with 2 undeployed #1097 vendor commits). This
# layer PINS the fleet-deployed genlock_build_sha to the NEWEST origin/main commit touching
# vendor/** -- the missing PIN the .claude/rules/early-gate-pin-doctrine.md orphan class names
# ("peer parity is a SUPPLEMENT, never a substitute"):
#   DEPLOYED_SHA empty                             -> UNKNOWN (31): deployed SHA unread, fail-closed
#   NEWEST_VENDOR_SHA empty                        -> UNKNOWN (31): origin/main newest vendor/**
#                                                      commit unresolved, fail-closed
#   PENDING_LIST non-empty                         -> ALARM   (30): deployed bundle LAGS -- names
#                                                      every pending vendor commit
#   PENDING_LIST empty + AHEAD_LIST non-empty
#     + ON_DEV="1"                                 -> OK      (0):  deployed bundle is AHEAD of
#                                                      origin/main on the dev candidate line -- a
#                                                      recognized release-candidate build (#1292,
#                                                      mirrors drift-guard.sh's
#                                                      genlock_build_drift_report AHEAD branch)
#   PENDING_LIST empty + AHEAD_LIST non-empty
#     + ON_DEV!="1"                                -> ALARM   (30): ORPHAN -- vendor commits
#                                                      reachable from NEITHER origin/main NOR
#                                                      origin/dev
#   else (both PENDING_LIST and AHEAD_LIST empty)  -> OK      (0):  deployed bundle is at the
#                                                      newest vendored HEAD
# PENDING_LIST/AHEAD_LIST = newline-separated "<sha> <subject>" (main() computes them via
# vendor_pin_range_log/vendor_pin_ahead_log; empty = none). AHEAD_LIST/ON_DEV are #1292 additions,
# OPTIONAL (default "" / "0") so every pre-#1292 3-arg call site keeps its exact prior behavior for
# the LAGS/OK branches -- only the NEW ahead-but-empty-pending branch needs them.
#
# REPORT-ONLY by design, unchanged by #1292 (rc 30 for BOTH the LAGS and the ORPHAN reason): the
# vendored OBS bundle deploys via COORDINATED OBS restarts (not a hot swap), so a merged-but-not-yet-
# redeployed vendor commit is a normal transient during dev -- a hard block on every E2E would be
# "too blunt" (the doctrine's own word), so this component gets an ALARM, not a hard-gate, exactly
# like the dantesync canary lag (#1139). But it SCREAMS on every run and NAMES the pending/ahead
# commits, so an orphan can never sit silently "discovered by eye weeks later" (#1136 owner
# directive) -- and, since #1292, a box that is legitimately ahead on the dev candidate line no
# longer false-ALARMs at all. It prints its verdict to STDOUT (tests capture it); main() adds a
# stderr SCREAM banner on ALARM/UNKNOWN and NEVER folds it into the gate's bad/unknown counters
# (that is what keeps it report-only). The documented two-step upgrade to a hard-gate is: once the
# vendored bundle is folded into an auto-deploy that advances with origin/main (the camera-box
# orphan-PROOF shape), flip the ALARM rows into the gate's bad/unknown roll-up.
genlock_vendor_pin_verdict() {
  local deployed="$1" newest="$2" pending="$3" ahead="${4:-}" on_dev="${5:-0}"
  if [ -z "$deployed" ]; then
    printf '  %-22s UNKNOWN  (deployed genlock_build_sha unread -- vendor pin unverifiable, fail-closed)\n' "vendor_pin"
    return 31
  fi
  if [ -z "$newest" ]; then
    printf '  %-22s UNKNOWN  (origin/main newest vendor/** commit unresolved for %s -- vendor pin unverifiable, fail-closed)\n' "vendor_pin" "$deployed"
    return 31
  fi
  local cleaned n
  cleaned="$(printf '%s\n' "$pending" | sed '/^[[:space:]]*$/d')"
  if [ -n "$cleaned" ]; then
    n="$(printf '%s\n' "$cleaned" | wc -l | tr -d ' ')"
    printf '  %-22s ALARM    (deployed bundle %s LAGS origin/main vendor HEAD %s -- %s undeployed vendor commit(s), redeploy the fleet):\n' \
      "vendor_pin" "$deployed" "$newest" "$n"
    printf '%s\n' "$cleaned" | sed 's/^/                           - /'
    return 30
  fi
  local cleaned_ahead n_ahead
  cleaned_ahead="$(printf '%s\n' "$ahead" | sed '/^[[:space:]]*$/d')"
  if [ -n "$cleaned_ahead" ]; then
    n_ahead="$(printf '%s\n' "$cleaned_ahead" | wc -l | tr -d ' ')"
    if [ "$on_dev" = "1" ]; then
      printf '  %-22s OK       (deployed bundle %s is %s vendored vendor/** commit(s) AHEAD of origin/main on the dev candidate line -- a recognized release-candidate build, #1292)\n' \
        "vendor_pin" "$deployed" "$n_ahead"
      return 0
    fi
    printf '  %-22s ALARM    (deployed bundle %s genlock ORPHAN -- reachable from NEITHER origin/main NOR origin/dev; it carries %s vendored vendor/** commit(s) beyond origin/main, redeploy the fleet -- if this is unexpected, confirm origin/dev is fetched in this checkout):\n' \
      "vendor_pin" "$deployed" "$n_ahead"
    printf '%s\n' "$cleaned_ahead" | sed 's/^/                           - /'
    return 30
  fi
  printf '  %-22s OK       (deployed bundle %s is at the newest origin/main vendor HEAD)\n' "vendor_pin" "$deployed"
  return 0
}
