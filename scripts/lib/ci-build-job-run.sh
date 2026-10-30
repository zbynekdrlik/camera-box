#!/usr/bin/env bash
# airuleset:script-ok source-only lib (one function, no top-level statements): strict mode belongs to the caller.
#
# scripts/lib/ci-build-job-run.sh -- issue 1387 (30.10.2026): the ci.yml run that published a commit's
# Linux build artifacts (camera-box-linux-amd64, probe-tools-linux-amd64), taken while that run may still
# be running. ci.yml's `Build` job uploads them a few minutes in; the whole run takes 30+ minutes (the
# Python harness tests). The E2E reaches its [0/8] aligns long before the run concludes, so asking for a
# run with `--status success` found nothing (E2E run 41001425945). A red ci.yml still blocks the merge on
# its own; the aligns only need the artifacts, and those exist once the Build job succeeded.

# ci_build_job_run_id REPO SHA -> the id of the commit's ci.yml run whose `Build` job completed with
# success, else nothing. Always rc 0: the caller's "not published yet" path handles the empty answer.
ci_build_job_run_id() {
  local repo="$1" sha="$2" run_id state
  run_id="$(gh run list --repo "$repo" --commit "$sha" --workflow ci.yml --limit 1 \
    --json databaseId -q '.[0].databaseId' 2>/dev/null || true)"
  [ -n "$run_id" ] || return 0
  state="$(gh api "repos/$repo/actions/runs/$run_id/jobs?per_page=100" \
    --jq '.jobs[] | select(.name == "Build") | "\(.status) \(.conclusion)"' 2>/dev/null | head -1 || true)"
  if [ "$state" = "completed success" ]; then
    printf '%s' "$run_id"
  fi
  return 0
}
