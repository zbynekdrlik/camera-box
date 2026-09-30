#!/usr/bin/env bash
# scripts/lib/ci-run-resolve.sh -- the ONE "newest successful CI run" resolver (issue 808, #1394).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# WHY: scripts/deploy-fleet.sh and scripts/bkshading-deploy-relay.sh each carried their own inline
# `gh run list --status success --limit 1` query. On 25.9.2026 the relay deploy took a STALE main run
# (33857572305 from 4.9.) instead of the newest one. Re-running that same query later that day
# returned the newest run again, so why it answered stale once is not known. The query trusted two
# server-side behaviours at once (the `status` filter and the result order) and never said which
# run it picked. This lib replaced both call sites with ONE resolver that decides on the client side:
#   - list the recent runs of the workflow on the branch (NO server-side status filter);
#   - keep conclusion == success, newest first by createdAt;
#   - take the first one that actually CARRIES the wanted artifact (an expired or missing artifact
#     would otherwise only fail at download time, after the choice was made).
# The chosen run is logged to stderr with its date and sha, so a wrong pick is visible.
#
# RECURRENCE (30.9.2026, #1394): a relay deploy to cam4-cam7 again resolved 33857572305 (4.9.)
# while the main head 1f7e6569b already had its own successful run 36687519583. The filtered runs
# listing itself was STALE -- its newest success was 34999288119 (15.9., artifact expired) -- and
# five reads of the same query a minute later all returned the newest run first. A client-side
# sort cannot fix that: the newest run is simply ABSENT from the payload. So the resolver now
# anchors on the branch HEAD, which it reads from the branch REF (a git ref read, strongly
# consistent -- not the runs listing):
#   1. read the head sha: `gh api repos/R/branches/B --jq .commit.sha`;
#   2. look up the head commit's OWN runs (`gh run list --commit <head>`); a successful one that
#      carries the artifact is the pick, logged `head-anchored`;
#   3. neither that lookup NOR the branch listing holds ANY run for the head -> the listing is
#      STALE: re-read both, bounded (CI_RUN_RESOLVE_RETRIES re-reads, default 3, with
#      CI_RUN_RESOLVE_RETRY_S seconds between them, default 10), then FAIL LOUD naming the head
#      and `pass --run <id>` -- never a silent older run (ci.yml runs on every push to dev and
#      main, so a head with no run anywhere is a stale listing or a run seconds from being created);
#   4. the head's run exists but is queued / in progress / failed / without the artifact -> the
#      older-success walk above, with ONE loud line naming the head's state and the fallback run
#      id, date and sha;
#   5. a gh error reading the head or its runs -> fail loud, never a fallback.
#
# Every JSON step runs through gh's BUILT-IN --jq (never a standalone `jq`): a cambox that runs
# setup-device.sh's relay and camera-box fetch has gh but no jq package.
#
# Source-only: function definitions, no side effects at source time. The gh binary is overridable
# (CI_RUN_RESOLVE_GH) and so is the sleep between re-reads (CI_RUN_RESOLVE_SLEEP), so Tier-0 tests
# can inject fakes.

# ci_run_newest_success_filter -> the jq program (one source of truth) that turns a
#   `gh run list --json databaseId,createdAt,conclusion,headSha` array into `<id> <createdAt> <sha>`
#   lines for the SUCCESSFUL runs, newest first by createdAt.
ci_run_newest_success_filter() {
  printf '%s\n' '[.[] | select(.conclusion == "success")] | sort_by(.createdAt) | reverse | .[] | "\(.databaseId) \(.createdAt) \(.headSha)"'
}

# ci_run_sha_ok SHA -> 0 when SHA is a full git commit sha (40 lowercase hex) -- what a branch ref
#   read returns. The head sha is interpolated into a jq program, so nothing else may pass.
ci_run_sha_ok() {
  case "$1" in '' | *[!0-9a-f]*) return 1 ;; esac
  [ "${#1}" -eq 40 ]
}

# ci_run_rows_of_sha_filter SHA -> the jq program that turns a runs array into
#   `<id> <createdAt> <status> <conclusion> <sha>` lines for the runs of commit SHA, newest first;
#   an empty status/conclusion (a run still in flight) reads `none`, so the columns never shift.
ci_run_rows_of_sha_filter() {
  local prog='[.[] | select(.headSha == "__SHA__")] | sort_by(.createdAt) | reverse | .[] | "\(.databaseId) \(.createdAt) \((.status // "") | if . == "" then "none" else . end) \((.conclusion // "") | if . == "" then "none" else . end) \(.headSha)"'
  printf '%s\n' "${prog//__SHA__/$1}"
}

# ci_run_listing_filter SHA -> the jq program for ONE read of the branch listing: `H <row>` lines
#   (ci_run_rows_of_sha_filter) for the runs of the head SHA, then `S <id> <createdAt> <sha>` lines
#   (ci_run_newest_success_filter) for every successful run.
ci_run_listing_filter() {
  printf '((%s) | "H \\(.)"), ((%s) | "S \\(.)")\n' "$(ci_run_rows_of_sha_filter "$1")" "$(ci_run_newest_success_filter)"
}

# ci_run_has_artifact REPO RUN_ID ARTIFACT -> 0 = the run lists a non-expired ARTIFACT, 1 = it does
#   not, 2 = the artifact list could NOT be read (a gh/api failure). The caller must stop on 2: moving
#   on to an older run there would be exactly the stale pick this lib exists to prevent.
ci_run_has_artifact() {
  local repo="$1" run="$2" art="$3" gh="${CI_RUN_RESOLVE_GH:-gh}" names errf rc=0
  errf="$(mktemp)" || return 2
  names="$("$gh" api "repos/$repo/actions/runs/$run/artifacts?per_page=100" \
    --jq '.artifacts[] | select(.expired | not) | .name' </dev/null 2>"$errf")" || rc=2
  if [ "$rc" -ne 0 ]; then
    echo "ci-run-resolve: gh api (artifacts of run $run): $(tail -n 1 "$errf" 2>/dev/null)" >&2
    rm -f "$errf"
    return 2
  fi
  rm -f "$errf"
  printf '%s\n' "$names" | grep -qxF -- "$art" && return 0
  return 1
}

# ci_run_branch_head REPO BRANCH -> stdout: the sha the BRANCH ref points at. rc 1 with a named
#   stderr line when gh fails or the answer is not a commit sha -- the caller fails loud, never
#   falls back.
ci_run_branch_head() {
  local repo="$1" branch="$2" gh="${CI_RUN_RESOLVE_GH:-gh}" head errf
  errf="$(mktemp)" || return 1
  head="$("$gh" api "repos/$repo/branches/$branch" --jq .commit.sha </dev/null 2>"$errf")" || {
    echo "ci-run-resolve: gh api (head of branch $branch in $repo) failed: $(tail -n 1 "$errf" 2>/dev/null) -- refusing to resolve without the head; pass --run <id>" >&2
    rm -f "$errf"
    return 1
  }
  rm -f "$errf"
  if ! ci_run_sha_ok "$head"; then
    echo "ci-run-resolve: the head of branch $branch in $repo read back as '$head', not a commit sha -- refusing to resolve without the head; pass --run <id>" >&2
    return 1
  fi
  printf '%s\n' "$head"
}

# ci_run_gh_lines LABEL CMD... -> stdout: CMD's stdout; rc 1 with a named stderr line (LABEL + gh's
#   last stderr line) when CMD fails.
ci_run_gh_lines() {
  local label="$1" out errf
  shift
  errf="$(mktemp)" || return 1
  out="$("$@" </dev/null 2>"$errf")" || {
    echo "ci-run-resolve: $label failed: $(tail -n 1 "$errf" 2>/dev/null) -- refusing to fall back to an older run; pass --run <id>" >&2
    rm -f "$errf"
    return 1
  }
  rm -f "$errf"
  printf '%s\n' "$out"
}

# ci_run_head_rows SHA TEXT... -> stdout: every well-formed `<id> <createdAt> <status> <conclusion>
#   <sha>` row of commit SHA found in the TEXT arguments, each run once, newest first. Rows of any
#   other sha, or with a non-numeric id, are dropped.
ci_run_head_rows() {
  local sha="$1"
  shift
  printf '%s\n' "$@" | awk -v h="$sha" 'NF == 5 && $1 ~ /^[0-9]+$/ && $5 == h && !seen[$1]++' |
    sort -s -t ' ' -k2,2r
}

# ci_run_pick_head_run ROWS -> 0 = a successful head run carrying the artifact was picked (logged
#   `head-anchored`, its id left in `picked`), 1 = none of ROWS qualifies, 2 = an artifact list was
#   UNREADABLE (the caller stops). ROWS are ci_run_head_rows lines. Called by ci_run_latest_success
#   ONLY: it reads repo/branch/workflow/art and updates tried/head_state/picked in the caller's scope
#   (bash dynamic scope), so it must never run in a subshell.
ci_run_pick_head_run() {
  local id when status concl sha rc
  while read -r id when status concl sha; do
    [ "$concl" = success ] || continue
    case "$tried" in *" $id "*) continue ;; esac
    tried="$tried$id "
    rc=0
    ci_run_has_artifact "$repo" "$id" "$art" || rc=$?
    case "$rc" in
      0)
        echo "ci-run-resolve: head-anchored: $workflow run $id of the $branch head carries $art (created ${when:-?}, sha ${sha:0:9})" >&2
        picked="$id"
        return 0
        ;;
      1) [ -n "$head_state" ] || head_state="run $id succeeded but carries no non-expired $art artifact" ;;
      *)
        echo "ci-run-resolve: the artifact list of the head's run $id is UNREADABLE (gh/api failure) -- refusing to fall back to an older run" >&2
        return 2
        ;;
    esac
  done <<<"$1"
  return 1
}

# ci_run_latest_success REPO BRANCH WORKFLOW ARTIFACT [LIMIT]
#   -> stdout: the id of the WORKFLOW run to deploy from BRANCH -- the branch head's own successful
#      run carrying ARTIFACT, else (the head's run in flight, failed or without the artifact) the
#      newest older successful run carrying it; stderr: one line naming the chosen run (id, date,
#      sha) and, on a fallback, the head's state. Returns 1 with empty stdout when no run qualifies,
#      the listing stays stale, or gh fails -- the caller fails loud. LIMIT (default 100) bounds how
#      far back the branch listing looks; it is not status-filtered, so it must cover a streak of
#      failed runs. The branch listing is read only when the head lookup has no run for the head
#      (the stale check) or the head's run is not usable (the fallback), so a listing hiccup never
#      refuses a head run that is already known good.
ci_run_latest_success() {
  local repo="$1" branch="$2" workflow="$3" art="$4" limit="${5:-100}" gh="${CI_RUN_RESOLVE_GH:-gh}"
  local retries="${CI_RUN_RESOLVE_RETRIES:-3}" wait_s="${CI_RUN_RESOLVE_RETRY_S:-10}"
  local sleeper="${CI_RUN_RESOLVE_SLEEP:-sleep}"
  local head head_rows="" listing="" listing_read=0 head_all="" reads=1 rc
  local kind id when status concl sha head_state="" tried=" " picked=""
  case "$retries" in '' | *[!0-9]*) retries=3 ;; esac
  case "$wait_s" in '' | *[!0-9]*) wait_s=10 ;; esac
  head="$(ci_run_branch_head "$repo" "$branch")" || return 1

  # Find at least one run of the head: its own lookup first, the branch listing only when that is
  # empty. Neither has one = a STALE listing -> bounded re-reads, then a loud refusal.
  while :; do
    head_rows="$(ci_run_gh_lines "gh run list (the $workflow runs of the $branch head ${head:0:9})" \
      "$gh" run list --repo "$repo" --commit "$head" --workflow "$workflow" \
      --json databaseId,createdAt,conclusion,status,headSha \
      --jq "$(ci_run_rows_of_sha_filter "$head")")" || return 1
    head_all="$(ci_run_head_rows "$head" "$head_rows")"
    [ -n "$head_all" ] && break
    listing="$(ci_run_gh_lines "gh run list ($workflow on $branch in $repo)" \
      "$gh" run list --repo "$repo" --branch "$branch" --workflow "$workflow" \
      --limit "$limit" --json databaseId,createdAt,conclusion,status,headSha \
      --jq "$(ci_run_listing_filter "$head")")" || return 1
    listing_read=1
    head_all="$(ci_run_head_rows "$head" "$(printf '%s\n' "$listing" | sed -n 's/^H //p')")"
    [ -n "$head_all" ] && break
    if [ "$reads" -gt "$retries" ]; then
      echo "ci-run-resolve: no $workflow run for the $branch head $head in the runs listing after $reads reads -- stale GitHub listing, refusing to pick an older run; pass --run <id>" >&2
      return 1
    fi
    echo "ci-run-resolve: no $workflow run for the $branch head ${head:0:9} in the runs listing (read $reads of $((retries + 1))) -- re-reading in ${wait_s}s" >&2
    "$sleeper" "$wait_s"
    reads=$((reads + 1))
  done

  # The head's own successful run carrying the artifact is THE pick.
  rc=0
  ci_run_pick_head_run "$head_all" || rc=$?
  case "$rc" in
    0) printf '%s\n' "$picked" && return 0 ;;
    2) return 1 ;;
  esac
  # Not usable: the fallback needs the branch listing, which may also know more runs of the head.
  if [ "$listing_read" -eq 0 ]; then
    listing="$(ci_run_gh_lines "gh run list ($workflow on $branch in $repo)" \
      "$gh" run list --repo "$repo" --branch "$branch" --workflow "$workflow" \
      --limit "$limit" --json databaseId,createdAt,conclusion,status,headSha \
      --jq "$(ci_run_listing_filter "$head")")" || return 1
    head_all="$(ci_run_head_rows "$head" "$head_all" "$(printf '%s\n' "$listing" | sed -n 's/^H //p')")"
    rc=0
    ci_run_pick_head_run "$head_all" || rc=$?
    case "$rc" in
      0) printf '%s\n' "$picked" && return 0 ;;
      2) return 1 ;;
    esac
  fi
  if [ -z "$head_state" ]; then
    read -r id when status concl sha <<<"$head_all"
    head_state="run $id is $status"
    [ "$concl" = none ] || head_state="$head_state/$concl"
  fi

  # The head's run is in flight, failed or has no artifact: the newest OLDER successful run.
  while read -r kind id when sha; do
    [ "$kind" = S ] || continue
    case "$id" in '' | *[!0-9]*) continue ;; esac
    [ "$sha" != "$head" ] || continue
    rc=0
    ci_run_has_artifact "$repo" "$id" "$art" || rc=$?
    case "$rc" in
      0)
        echo "ci-run-resolve: FALLING BACK: the $branch head ${head:0:9} has no usable $workflow run ($head_state) -- using the newest older successful run carrying $art = $id (created ${when:-?}, sha ${sha:0:9})" >&2
        printf '%s\n' "$id"
        return 0
        ;;
      1) echo "ci-run-resolve: run $id has no non-expired $art artifact -- trying the next older successful run" >&2 ;;
      *)
        echo "ci-run-resolve: the artifact list of run $id is UNREADABLE (gh/api failure) -- refusing to fall back to an older run" >&2
        return 1
        ;;
    esac
  done <<<"$listing"
  echo "ci-run-resolve: no successful $workflow run on $branch among the last $limit carries $art (the head ${head:0:9}: $head_state)" >&2
  return 1
}
