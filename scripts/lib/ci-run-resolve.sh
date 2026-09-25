#!/usr/bin/env bash
# scripts/lib/ci-run-resolve.sh -- the ONE "newest successful CI run" resolver (issue 808).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# WHY: scripts/deploy-fleet.sh and scripts/bkshading-deploy-relay.sh each carried their own inline
# `gh run list --status success --limit 1` query. On 25.9.2026 the relay deploy took a STALE main run
# (33857572305 from 4.9.) instead of the newest one. Re-running that same query later that day
# returned the newest run again, so why it answered stale once is not known. The query trusted two
# server-side behaviours at once (the `status` filter and the result order) and never said which
# run it picked. This lib replaces both call sites with ONE resolver that decides on the client side:
#   - list the recent runs of the workflow on the branch (NO server-side status filter);
#   - keep conclusion == success, newest first by createdAt;
#   - take the first one that actually CARRIES the wanted artifact (an expired or missing artifact
#     would otherwise only fail at download time, after the choice was made).
# The chosen run is logged to stderr with its date and sha, so a wrong pick is visible.
#
# Every JSON step runs through gh's BUILT-IN --jq (never a standalone `jq`): a cambox that runs
# setup-device.sh's relay fetch has gh but no jq package.
#
# Source-only: function definitions, no side effects at source time. The gh binary is overridable
# (CI_RUN_RESOLVE_GH) so Tier-0 tests can inject a fake.

# ci_run_newest_success_filter -> the jq program (one source of truth) that turns a
#   `gh run list --json databaseId,createdAt,conclusion,headSha` array into `<id> <createdAt> <sha>`
#   lines for the SUCCESSFUL runs, newest first by createdAt.
ci_run_newest_success_filter() {
  printf '%s\n' '[.[] | select(.conclusion == "success")] | sort_by(.createdAt) | reverse | .[] | "\(.databaseId) \(.createdAt) \(.headSha)"'
}

# ci_run_has_artifact REPO RUN_ID ARTIFACT -> 0 = the run lists a non-expired ARTIFACT, 1 = it does
#   not, 2 = the artifact list could NOT be read (a gh/api failure). The caller must stop on 2: moving
#   on to an older run there would be exactly the stale pick this lib exists to prevent.
ci_run_has_artifact() {
  local repo="$1" run="$2" art="$3" gh="${CI_RUN_RESOLVE_GH:-gh}" names
  names="$("$gh" api "repos/$repo/actions/runs/$run/artifacts?per_page=100" \
    --jq '.artifacts[] | select(.expired | not) | .name' </dev/null 2>/dev/null)" || return 2
  printf '%s\n' "$names" | grep -qxF -- "$art" && return 0
  return 1
}

# ci_run_latest_success REPO BRANCH WORKFLOW ARTIFACT [LIMIT]
#   -> stdout: the id of the newest successful WORKFLOW run on BRANCH that carries ARTIFACT;
#      stderr: one line naming the chosen run (id, date, sha). Returns 1 with empty stdout when no
#      run qualifies (or gh fails) -- the caller fails loud. LIMIT (default 100) bounds how far back
#      it looks; the list is not status-filtered, so it must cover a streak of failed runs.
ci_run_latest_success() {
  local repo="$1" branch="$2" workflow="$3" art="$4" limit="${5:-100}" gh="${CI_RUN_RESOLVE_GH:-gh}"
  local lines id when sha rc
  lines="$("$gh" run list --repo "$repo" --branch "$branch" --workflow "$workflow" \
    --limit "$limit" --json databaseId,createdAt,conclusion,headSha \
    --jq "$(ci_run_newest_success_filter)" 2>/dev/null)" || {
    echo "ci-run-resolve: gh run list failed for $workflow on $branch ($repo)" >&2
    return 1
  }
  while read -r id when sha; do
    case "$id" in '' | *[!0-9]*) continue ;; esac
    rc=0
    ci_run_has_artifact "$repo" "$id" "$art" || rc=$?
    case "$rc" in
      0)
        echo "ci-run-resolve: newest successful $workflow run on $branch carrying $art = $id (created ${when:-?}, sha ${sha:0:9})" >&2
        printf '%s\n' "$id"
        return 0
        ;;
      1) echo "ci-run-resolve: run $id has no non-expired $art artifact -- trying the next older successful run" >&2 ;;
      *)
        echo "ci-run-resolve: the artifact list of run $id is UNREADABLE (gh/api failure) -- refusing to fall back to an older run" >&2
        return 1
        ;;
    esac
  done <<<"$lines"
  echo "ci-run-resolve: no successful $workflow run on $branch among the last $limit carries $art" >&2
  return 1
}
