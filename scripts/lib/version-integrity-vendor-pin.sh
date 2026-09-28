#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions + one lazy source of scripts/lib/vendor-range.sh, no other top-level statements) -- the sourcing gate owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/version-integrity-vendor-pin.sh -- the genlock vendor-pin report-only ALARM family of
# scripts/version-integrity-gate.sh (issues 1137 + 1292), moved out of the gate VERBATIM (issue 1377:
# the gate was over the repo's 1000-line file budget). No behaviour change: the gate sources this lib
# BEFORE its source-guard, exactly where these functions used to sit, so sourcing the gate (the unit
# tests in tests/version_integrity_gate.rs) still defines every function.
#
# Issue 1384 (no behaviour change either):
#   - The three git range helpers are thin wrappers over the ONE shared implementation in
#     scripts/lib/vendor-range.sh, which scripts/drift-guard.sh's imag_genlock_* helpers use too.
#     This family keeps its own two parameters: `--format='%h %s'` and the WHOLE `vendor/` tree.
#   - The alarm's ROW block moved out of the gate's main() into vig_row_vendor_pin (end of file).
# genlock_vendor_pin_verdict is the pure verdict vig_row_vendor_pin prints (report-only, never folded
# into the gate's exit code). The moved comments were written inside the gate: "main()" there means
# scripts/version-integrity-gate.sh's main(), whose vendor-pin block is now vig_row_vendor_pin.

if ! command -v vendor_range_lag_log >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/vendor-range.sh
  . "${BASH_SOURCE[0]%/*}/vendor-range.sh"
fi

# vendor_pin_range_log REPO_ROOT DEPLOYED_SHA -> prints one `<short-sha> <subject>` line per commit
# touching vendor/ that origin/main carries and DEPLOYED_SHA's own lineage never received -- the
# merge-base-scoped LAG range (issue-1292 review follow-up), i.e. `git log --format='%h %s'
# $(git merge-base DEPLOYED_SHA origin/main)..origin/main -- vendor/`; exit status mirrors the FIRST
# failing git call (the merge-base resolve, then the log). A wrapper over vendor_range_lag_log
# (scripts/lib/vendor-range.sh holds why the range is merge-base-scoped, why `--end-of-options`,
# and why no --first-parent). Two deliberate differences from drift-guard.sh's
# imag_genlock_range_log: `--format='%h %s'` instead of `--oneline` (the same shape, this family's
# own style), and the WHOLE `vendor/` tree instead of just `vendor/obs-studio vendor/distroav`,
# because this facet covers every deployed box (strih, stream, imag), not only imag's own consumed
# paths (see genlock_parity_consumed_paths for that per-box distinction, which this facet
# deliberately does NOT apply -- it PINS every box's deployed SHA against the single newest
# vendor/** commit on origin/main, regardless of which sub-paths that box's own build consumes).
vendor_pin_range_log() {
  vendor_range_lag_log "$1" "$2" '--format=%h %s' vendor/
}

# vendor_pin_ahead_log REPO_ROOT DEPLOYED_SHA -> the AHEAD-direction counterpart (issue-1292 review
# follow-up): one `<short-sha> <subject>` line per vendor/ commit DEPLOYED_SHA carries that
# origin/main does not (`git log --format='%h %s' origin/main..DEPLOYED_SHA -- vendor/`); an empty
# DEPLOYED_SHA fails LOUD (rc 128). A wrapper over vendor_range_ahead_log.
vendor_pin_ahead_log() {
  vendor_range_ahead_log "$1" "$2" '--format=%h %s' vendor/
}

# vendor_pin_on_dev REPO_ROOT DEPLOYED_SHA -> exit 0 when DEPLOYED_SHA is reachable from origin/dev
# (a recognized release-candidate bundle deployed ahead of main), non-zero otherwise -- fail CLOSED
# (issue-1292 review follow-up). A wrapper over vendor_range_on_dev.
vendor_pin_on_dev() {
  vendor_range_on_dev "$1" "$2"
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
# PENDING_LIST/AHEAD_LIST = newline-separated "<sha> <subject>" (vig_row_vendor_pin computes them
# via vendor_pin_range_log/vendor_pin_ahead_log; empty = none). AHEAD_LIST/ON_DEV are #1292 additions,
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
# longer false-ALARMs at all. It prints its verdict to STDOUT (tests capture it); vig_row_vendor_pin
# adds a stderr SCREAM banner on ALARM/UNKNOWN and NEVER folds it into the gate's bad/unknown
# counters (that is what keeps it report-only). The documented two-step upgrade to a hard-gate is:
# once the
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

# vig_row_vendor_pin LABEL=SHA... -> the issue-1137 REPORT-ONLY vendor-pin alarm rows, moved
# verbatim out of the gate's main() (issue 1384). The arguments are the LABEL=SHA readings
# vig_row_genlock_parity gathered (main() passes its parity_args), so the alarm needs no new
# read. It never touches main()'s ok / bad / unknown counters (that is what keeps it
# report-only), prints its stderr SCREAM on ALARM / UNKNOWN, reads the gate's HERE for the
# repo root, and always returns 0.
vig_row_vendor_pin() {
  local -a parity_args=("$@")
  # #1137 -- REPORT-ONLY vendor-pin ALARM. The cross-box parity row passes a UNIFORMLY-stale fleet
  # (every box agrees on an OLD genlock build); this PINS the fleet-deployed genlock_build_sha to the
  # NEWEST origin/main commit touching vendor/** and SCREAMS when it lags. It NEVER touches the gate's
  # bad/unknown counters (report-only) -- the coordinated-restart bundle deploy makes a hard block on
  # every E2E too blunt, so #1136's doctrine assigns this component an ALARM (see
  # genlock_vendor_pin_verdict's header in scripts/lib/version-integrity-vendor-pin.sh for the
  # two-step upgrade to a hard-gate). Reuses the deployed
  # SHAs already gathered in parity_args (no new read). Fail-closed-LOUD on an unreadable pin. Fixture
  # seams for the flow test: VERSION_INTEGRITY_GATE_VENDOR_NEWEST (override the newest vendor HEAD),
  # VERSION_INTEGRITY_GATE_VENDOR_PENDING (override the pending list; set-but-empty = "current"), and
  # (#1292) VERSION_INTEGRITY_GATE_VENDOR_AHEAD / VERSION_INTEGRITY_GATE_VENDOR_ON_DEV (override the
  # ahead-list / on-dev-line facts -- read only once VENDOR_PENDING is set, same activation as the
  # pending seam).
  echo "  -- vendor-pin alarm (#1137, report-only) --"
  local repo_root_vp=""
  repo_root_vp="$(cd "$HERE/.." 2>/dev/null && pwd)" || repo_root_vp=""
  local -a vp_shas=()
  local vp_seen=" " pe psha
  for pe in "${parity_args[@]}"; do
    psha="${pe#*=}"
    [ -z "$psha" ] && continue
    case "$vp_seen" in *" $psha "*) continue ;; esac
    vp_seen="${vp_seen}${psha} "
    vp_shas+=("$psha")
  done
  local vp_newest=""
  if [ -n "${VERSION_INTEGRITY_GATE_VENDOR_NEWEST:-}" ]; then
    vp_newest="$VERSION_INTEGRITY_GATE_VENDOR_NEWEST"
  elif [ -n "$repo_root_vp" ]; then
    timeout 15 git -C "$repo_root_vp" fetch origin --quiet 2>/dev/null || true
    vp_newest="$(git -C "$repo_root_vp" log -1 --format='%H' origin/main -- vendor/ 2>/dev/null || true)"
  fi
  if [ "${#vp_shas[@]}" -eq 0 ]; then
    local vp_out=""; vp_out="$(genlock_vendor_pin_verdict "" "$vp_newest" "")" || true
    printf '%s\n' "$vp_out" | sed 's/^/    /'
    echo "!! VENDOR-PIN ALARM: no deployed genlock_build_sha to pin -- vendor currency UNVERIFIED (report-only, does NOT block this run)." >&2
  else
    local vp_sha vp_newest_eff vp_pending vp_ahead vp_on_dev vp_rc vp_out vprc
    for vp_sha in "${vp_shas[@]}"; do
      vp_newest_eff="$vp_newest"
      vp_pending=""
      vp_ahead=""
      vp_on_dev=0
      vp_rc=0
      if [ -n "${VERSION_INTEGRITY_GATE_VENDOR_PENDING+x}" ]; then
        vp_pending="$VERSION_INTEGRITY_GATE_VENDOR_PENDING"
        vp_ahead="${VERSION_INTEGRITY_GATE_VENDOR_AHEAD:-}"
        vp_on_dev="${VERSION_INTEGRITY_GATE_VENDOR_ON_DEV:-0}"
      elif [ -n "$repo_root_vp" ] && [ -n "$vp_newest_eff" ]; then
        if git -C "$repo_root_vp" cat-file -e "${vp_sha}^{commit}" 2>/dev/null; then
          # #1292: merge-base-scoped LAG range (vendor_pin_range_log), never a plain ancestry range
          # -- see its own header for why a plain range falsely reads a deployed SHA that is
          # genuinely AHEAD of main on the dev candidate line as LAGGING. `|| vp_rc=$?` is
          # load-bearing (mirrored from drift-guard.sh's own #1292 review finding W1): a failing
          # ahead-log call must land on the SAME UNKNOWN path as a failing range-log call, never
          # silently swallow into an empty vp_ahead that genlock_vendor_pin_verdict would read as
          # "OK, current".
          vp_pending="$(vendor_pin_range_log "$repo_root_vp" "$vp_sha")" || vp_rc=$?
          if [ "$vp_rc" = "0" ] && [ -z "$vp_pending" ]; then
            vp_ahead="$(vendor_pin_ahead_log "$repo_root_vp" "$vp_sha")" || vp_rc=$?
            if [ "$vp_rc" = "0" ] && [ -n "$vp_ahead" ]; then
              vendor_pin_on_dev "$repo_root_vp" "$vp_sha" && vp_on_dev=1 || vp_on_dev=0
            fi
          fi
          if [ "$vp_rc" != "0" ]; then
            # A merge-base/log git error -> fail-closed UNKNOWN (never a false "no pending" from an
            # empty/partial read).
            vp_newest_eff=""
          fi
        else
          # The deployed SHA is unknown to local git -> rev-range would silently return "" and read
          # as "none pending" (a FALSE OK). Force UNKNOWN (fail-closed) instead.
          vp_newest_eff=""
        fi
      fi
      vprc=0
      vp_out="$(genlock_vendor_pin_verdict "$vp_sha" "$vp_newest_eff" "$vp_pending" "$vp_ahead" "$vp_on_dev")" || vprc=$?
      printf '%s\n' "$vp_out" | sed 's/^/    /'
      case "$vprc" in
        30) echo "!! VENDOR-PIN ALARM: deployed genlock bundle ${vp_sha} is DRIFTED from origin/main vendor HEAD (LAGS, or an unrecognized ORPHAN build reachable from neither origin/main nor origin/dev) -- see the vendor_pin detail line above for the exact reason; redeploy the fleet (report-only, does NOT block this run)." >&2 ;;
        31) echo "!! VENDOR-PIN ALARM: could not verify deployed genlock bundle ${vp_sha} against origin/main vendor HEAD (report-only)." >&2 ;;
      esac
    done
  fi
  return 0
}
