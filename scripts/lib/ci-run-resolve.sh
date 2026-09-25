#!/usr/bin/env bash
# scripts/lib/ci-run-resolve.sh -- the ONE "newest successful CI run" resolver (issue 808).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# WHY: scripts/deploy-fleet.sh and scripts/bkshading-deploy-relay.sh each carried their own inline
# `gh run list --status success --limit 1` query. On 25.9.2026 the relay deploy took a STALE main run
# (33857572305 from 4.9.) instead of the newest one, so the relay fleet would have gone back three
# weeks. That query trusts two server-side behaviours at once: the `status` filter and the result
# order, and it never says which run it picked. This lib replaces both call sites with ONE resolver
# that decides on the client side:
#   - list the recent runs of the workflow on the branch (NO server-side status filter);
#   - keep conclusion == success, newest first by createdAt;
#   - take the first one that actually CARRIES the wanted artifact (an expired or missing artifact
#     would otherwise only fail at download time, after the choice was made).
# The chosen run is logged to stderr with its date and sha, so a wrong pick is visible.
#
# Source-only: function definitions, no side effects at source time. The gh binary is overridable
# (CI_RUN_RESOLVE_GH) so Tier-0 tests can inject a fake.

# ci_run_newest_success_ids  (stdin: a `gh run list --json databaseId,createdAt,conclusion` array)
#   -> the databaseIds of the SUCCESSFUL runs, newest first by createdAt, one per line. Pure (jq).
ci_run_newest_success_ids() {
  jq -r '[.[] | select(.conclusion == "success")] | sort_by(.createdAt) | reverse | .[].databaseId'
}

# ci_run_has_artifact REPO RUN_ID ARTIFACT -> 0 iff that run lists a non-expired ARTIFACT.
ci_run_has_artifact() {
  local repo="$1" run="$2" art="$3" gh="${CI_RUN_RESOLVE_GH:-gh}" names
  names="$("$gh" api "repos/$repo/actions/runs/$run/artifacts?per_page=100" \
    --jq '.artifacts[] | select(.expired | not) | .name' 2>/dev/null)" || return 1
  printf '%s\n' "$names" | grep -qxF "$art"
}

# ci_run_latest_success REPO BRANCH WORKFLOW ARTIFACT [LIMIT]
#   -> stdout: the id of the newest successful WORKFLOW run on BRANCH that carries ARTIFACT;
#      stderr: one line naming the chosen run (id, date, sha). Returns 1 with empty stdout when no
#      run qualifies (or gh fails) -- the caller fails loud.
ci_run_latest_success() {
  local repo="$1" branch="$2" workflow="$3" art="$4" limit="${5:-30}" gh="${CI_RUN_RESOLVE_GH:-gh}"
  local runs ids id when sha
  runs="$("$gh" run list --repo "$repo" --branch "$branch" --workflow "$workflow" \
    --limit "$limit" --json databaseId,createdAt,conclusion,headSha 2>/dev/null)" || return 1
  [ -n "$runs" ] || return 1
  ids="$(printf '%s' "$runs" | ci_run_newest_success_ids 2>/dev/null)" || return 1
  for id in $ids; do
    case "$id" in '' | *[!0-9]*) continue ;; esac
    if ci_run_has_artifact "$repo" "$id" "$art"; then
      when="$(printf '%s' "$runs" | jq -r --argjson i "$id" '.[] | select(.databaseId == $i) | .createdAt' 2>/dev/null | head -n 1)"
      sha="$(printf '%s' "$runs" | jq -r --argjson i "$id" '.[] | select(.databaseId == $i) | .headSha' 2>/dev/null | head -n 1)"
      echo "ci-run-resolve: newest successful $workflow run on $branch carrying $art = $id (created ${when:-?}, sha ${sha:0:9})" >&2
      printf '%s\n' "$id"
      return 0
    fi
    echo "ci-run-resolve: run $id has no non-expired $art artifact -- trying the next older successful run" >&2
  done
  echo "ci-run-resolve: no successful $workflow run on $branch among the last $limit carries $art" >&2
  return 1
}
