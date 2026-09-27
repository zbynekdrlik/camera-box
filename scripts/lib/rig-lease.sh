#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no unconditional side effects at source
# time), mirrors the sibling scripts/lib/v4l2-neutral.sh / rig-test-dropin.sh convention which are
# also `set -euo pipefail`-free for the same reason (sourcing this file must never mutate the
# CALLING script's own shell options -- see .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/rig-lease.sh -- the CAMERA-BOX half of the #830 shared cross-repo rig lease.
#
# WHY: camera-box's full-path-e2e.yml and restreamer's Rust CI both drive the SAME physical rig
# (strih 10.77.9.202 / stream 10.77.9.204 OBS) from the SAME self-hosted dev1 runner, with no
# mutual exclusion. Live collision (2026-07-27, gh issue #830): our gate burnt its full 30-minute
# rig-busy budget and died OUTCOME=RIG_BUSY while restreamer's soak held stream OBS -- and the
# REVERSE direction was entirely unguarded (our harness could reroute program scenes underneath a
# live restreamer E2E and corrupt it). Design settled in the issue's own comment (owner, 2026-07-27):
# a lockdir lease on dev1 -- no service, no network dependency, since both runners are literally the
# same machine.
#
#   /var/tmp/rig-lease/            lockdir; atomic `mkdir` = acquire
#   /var/tmp/rig-lease/holder.json {"repo","run_id","run_url","job","acquired_at","expected_release_at"}
#   /var/tmp/rig-lease/heartbeat   mtime bumped by the holder while it works
#
# ROLLING KEEP-ALIVE (issue 1383): while the holder is alive, rig_lease_refresh_if_mine bumps the
# heartbeat and rolls expected_release_at forward to max(current, now + look-ahead), ONLY for the
# holder.json that names that run. Two beaters call it: the gate's lease-only keep-alive
# (rig_lease_keepalive_spawn, from acquire to release) and the holder's issue-281 refresher
# (scripts/lib/rig-heartbeat.sh rig_heartbeat_start, for the E2E / av-soak run itself). A live run
# therefore always shows a fresh heartbeat and a future release time. Beating stops at the hold
# CEILING (acquired_at + RIG_LEASE_MAX_HOLD_SECS) and when the refresher's owner dies, so a dead
# or stuck holder's lease still ages into the heartbeat-stale reclaim below.
#
# The restreamer half is filed as zbynekdrlik/restreamer#349 -- until it participates, this lease
# is one-directional (camera-box takes it; restreamer does not yet check it). That is fine: the
# direction camera-box closes first is the one that can CORRUPT the other repo's run (our harness
# rerouting program scenes underneath a live restreamer E2E), not merely delay it. The EXISTING
# OBS-state busy-check (scripts/rig-busy-gate.sh, #406/#312/#657) stays wired in as the FALLBACK
# for a busy rig with no lease participant on the other side -- never weakened by this file.
#
# PREMISE CORRECTION (#1277): the "both runners are literally the same machine" premise above is
# TRUE for camera-box's own full-path-e2e.yml runner, but FALSE for restreamer's OBS-driving E2E
# jobs, which run on the Windows STREAM BOX (10.77.9.204, SYSTEM-level self-hosted runner) -- a
# different host that can never see this lockdir on dev1's local filesystem. restreamer's
# pre-StartStream check reads the SAME lockdir over LAN HTTP instead: scripts/rig-lease-server.py
# (:8890/rig-lease.json), a read-only window with zero write surface and zero new credentials.
# See .claude/rules/rig-lease-http.md for the full consumer contract.
#
# Source-only: this file defines functions and runs nothing on its own. Callers:
#   - scripts/rig-busy-gate.sh   (acquires before the first OBS-touching action; wait/fail-fast
#                                 decision; releases on ITS OWN failure paths via trap)
#   - scripts/rig-lease-release.sh (tiny wrapper the workflow's `always()` step runs, so the lease
#                                 is ALSO released on the success path and on cancellation -- never
#                                 only on rig-busy-gate.sh's own success path, per the issue)
#   - scripts/lib/rig-heartbeat.sh (the holder's issue-281 refresher: rig_lease_refresh_if_mine on
#                                 every beat, issue 1383) + scripts/av-soak.sh (slot boundaries) +
#                                 scripts/rig-busy-gate.sh (busy-wait + the lease-only keep-alive)
#
# Tunables (env, all optional):
#   RIG_LEASE_DIR              lockdir path (default /var/tmp/rig-lease; tests override for isolation)
#   RIG_LEASE_RUN_STATUS_CMD   optional external "is the holder's run still alive" checker --
#                              receives "$repo" "$run_id" as $1 $2, must print exactly "in_progress"
#                              or "not_in_progress" on stdout. Unset (default) -> always
#                              "in_progress" (unknown != stale); staleness then rests entirely on
#                              the heartbeat-age check below, which is why that threshold is the
#                              ultimate self-healing backstop (mirrors #657's stray-recording
#                              self-heal: never a permanent deadlock, just a bounded worst case).
#   RIG_LEASE_LOOKAHEAD_SECS   how far past "now" rig_lease_refresh_if_mine rolls the holder's
#                              expected_release_at (default 900 = 15 min, issue 1383)
#   RIG_LEASE_MAX_HOLD_SECS    the hold CEILING: no beat past acquired_at + this (default 4500 =
#                              full-path-e2e.yml's timeout-minutes 75; the av-soak sets its own
#                              declared run window, issue 1383)
#   RIG_LEASE_KEEPALIVE_SEC    the gate keep-alive's beat period (default 30, issue 1383)

rig_lease_dir() {
  printf '%s\n' "${RIG_LEASE_DIR:-/var/tmp/rig-lease}"
}

rig_lease_holder_path() {
  printf '%s/holder.json\n' "$(rig_lease_dir)"
}

rig_lease_heartbeat_path() {
  printf '%s/heartbeat\n' "$(rig_lease_dir)"
}

# rig_lease_heartbeat_touch -> bump the heartbeat mtime. Safe no-op if the lease dir does not
# exist (never creates a heartbeat with no holder.json alongside it).
rig_lease_heartbeat_touch() {
  local d; d="$(rig_lease_dir)"
  [ -d "$d" ] && touch "$(rig_lease_heartbeat_path)" 2>/dev/null
  return 0
}

# rig_lease_heartbeat_age_seconds -> echo the heartbeat's age in seconds. A MISSING heartbeat
# always reads as a huge sentinel age (never as "fresh") -- a corrupt/absent heartbeat inside an
# existing lockdir must never be mistaken for a live holder.
rig_lease_heartbeat_age_seconds() {
  local hb; hb="$(rig_lease_heartbeat_path)"
  if [ ! -f "$hb" ]; then
    printf '%s\n' 999999999
    return 0
  fi
  local now mtime
  now="$(date +%s)"
  mtime="$(stat -c %Y "$hb" 2>/dev/null || echo 0)"
  printf '%s\n' "$(( now - mtime ))"
}

# rig_lease_is_fresh <age_seconds> <stale_secs> -> exit 0 (fresh) / 1 (stale). PURE arithmetic,
# mirrors scripts/lib/rig-heartbeat.sh's rig_heartbeat_is_fresh. Any non-numeric input is treated
# conservatively as STALE.
rig_lease_is_fresh() {
  local age="${1:-}" stale="${2:-}"
  case "$age$stale" in
    *[!0-9]* | "") return 1 ;;
  esac
  [ "$age" -ge 0 ] && [ "$age" -lt "$stale" ]
}

# rig_lease_read_holder_field <field> -> echo the field's string value from holder.json, or "" if
# the file is absent/corrupt/missing that field. Never aborts the caller on bad JSON.
rig_lease_read_holder_field() {
  local field="$1"
  local path; path="$(rig_lease_holder_path)"
  [ -f "$path" ] || { printf '\n'; return 0; }
  python3 -c '
import json, sys
field = sys.argv[1]
try:
    with open(sys.argv[2]) as f:
        data = json.load(f)
    print(data.get(field, ""))
except Exception:
    print("")
' "$field" "$path" 2>/dev/null || printf '\n'
}

# rig_lease_holder_summary -> echo "<repo>#<run_id> run_url=<url> job=<job> expected_release_at=<ts>"
# read from the CURRENT holder.json, or a clearly-marked placeholder if it is missing/corrupt.
#
# #970/#980: read holder.json ATOMICALLY -- ONE open()+json.load() that formats the whole line in a
# single python invocation, NEVER five separate rig_lease_read_holder_field reads. The five-read
# form could TEAR when a releasing foreign holder rm's the lockdir mid-summary (repo read succeeds,
# then the file vanishes and the rest read empty), leaking a garbled partial line
# ("<repo># run_url= job= expected_release_at=") that the "$repo$run_id" corrupt-guard did not even
# catch. With one read, a concurrent unlink after open() cannot truncate the already-open FD
# (Linux), so the result is all-or-nothing: a fully-consistent summary, or a clean placeholder.
rig_lease_holder_summary() {
  local path; path="$(rig_lease_holder_path)"
  python3 -c '
import json, sys
path = sys.argv[1]
try:
    with open(path) as f:
        data = json.load(f)
except FileNotFoundError:
    print("unknown (no holder.json present)")
    sys.exit(0)
except Exception:
    print("unknown (corrupt holder.json)")
    sys.exit(0)
repo = str(data.get("repo", "") or "")
run_id = str(data.get("run_id", "") or "")
if not (repo or run_id):
    print("unknown (corrupt holder.json)")
    sys.exit(0)
run_url = str(data.get("run_url", "") or "")
job = str(data.get("job", "") or "")
expected = str(data.get("expected_release_at", "") or "")
print(f"{repo}#{run_id} run_url={run_url} job={job} expected_release_at={expected}")
' "$path" 2>/dev/null || printf 'unknown (corrupt holder.json)\n'
}

# rig_lease_read_holder_repo_run_id -> print "<repo>\t<run_id>" from ONE atomic read of holder.json
# (both fields from a single open()+json.load(), so a concurrent lockdir removal BETWEEN them cannot
# tear the pair -- #970/#980, the same atomicity rig_lease_holder_summary needs). "\t" on absent /
# corrupt. Callers split on the tab.
rig_lease_read_holder_repo_run_id() {
  local path; path="$(rig_lease_holder_path)"
  python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as f:
        d = json.load(f)
    print(str(d.get("repo", "") or "") + "\t" + str(d.get("run_id", "") or ""))
except Exception:
    print("\t")
' "$path" 2>/dev/null || printf '\t\n'
}

# rig_lease_holder_run_status <repo> <run_id> -> echo "in_progress" or "not_in_progress".
# Pluggable via RIG_LEASE_RUN_STATUS_CMD (see header); defaults to "in_progress" (assume alive --
# an unknown/unreachable status checker must never itself cause a reclaim; only a confirmed-dead
# run or a stale heartbeat may).
rig_lease_holder_run_status() {
  local repo="$1" run_id="$2"
  if [ -n "${RIG_LEASE_RUN_STATUS_CMD:-}" ]; then
    "$RIG_LEASE_RUN_STATUS_CMD" "$repo" "$run_id" 2>/dev/null || printf 'in_progress\n'
  else
    printf 'in_progress\n'
  fi
}

# rig_lease_is_stale <stale_secs> -> exit 0 (stale, reclaimable) / 1 (still a live holder). A
# lockdir with NO holder.json at all is treated as stale/corrupt (reclaim rather than deadlock
# forever on a broken lease). Otherwise stale iff the heartbeat is too old OR the holder's own run
# is confirmed no-longer-in-progress -- either signal alone is enough (#830 design comment).
rig_lease_is_stale() {
  local stale_secs="${1:-5400}"
  local path; path="$(rig_lease_holder_path)"
  [ -f "$path" ] || return 0

  local age; age="$(rig_lease_heartbeat_age_seconds)"
  if ! rig_lease_is_fresh "$age" "$stale_secs"; then
    return 0
  fi

  local pair repo run_id status
  pair="$(rig_lease_read_holder_repo_run_id)"
  repo="${pair%%$'\t'*}"
  run_id="${pair#*$'\t'}"
  status="$(rig_lease_holder_run_status "$repo" "$run_id")"
  [ "$status" = "not_in_progress" ] && return 0

  return 1
}

# rig_lease_write_holder <repo> <run_id> <run_url> <job> <expected_release_at> -> (re)create the
# lease dir + holder.json + heartbeat for the CURRENT caller. Assumes the caller already either
# `mkdir`'d the lockdir itself (fresh acquire) or is reclaiming a stale one (dir already exists).
rig_lease_write_holder() {
  local repo="$1" run_id="$2" run_url="$3" job="$4" expected_release_at="$5"
  local d; d="$(rig_lease_dir)"
  mkdir -p "$d"
  local acquired_at; acquired_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  python3 -c '
import json, sys
repo, run_id, run_url, job, acquired_at, expected_release_at, path = sys.argv[1:8]
with open(path, "w") as f:
    json.dump({
        "repo": repo,
        "run_id": run_id,
        "run_url": run_url,
        "job": job,
        "acquired_at": acquired_at,
        "expected_release_at": expected_release_at,
    }, f)
' "$repo" "$run_id" "$run_url" "$job" "$acquired_at" "$expected_release_at" "$(rig_lease_holder_path)"
  touch "$(rig_lease_heartbeat_path)"
}

# rig_lease_refresh_if_mine <repo> <run_id> [<lookahead_secs>] -> the HOLDER's own keep-alive
# (issue 1383). ONLY when the current holder.json names exactly <repo> + <run_id> and the hold is
# still under its ceiling (acquired_at + RIG_LEASE_MAX_HOLD_SECS, default 4500 s = the full-path
# job's timeout-minutes): bump the heartbeat and roll expected_release_at forward to
# max(current, now + lookahead) -- never backward, so a holder that declared a longer run up front
# (the av-soak) keeps its declaration. lookahead defaults to RIG_LEASE_LOOKAHEAD_SECS, else 900 s.
# The ceiling keeps the #830 "never a permanent deadlock" backstop: a stuck-but-alive holder stops
# being beaten and ages into the heartbeat-stale reclaim.
#
# The check-and-update is ONE python process, scripts/rig_lease_refresh.py (its module doc holds the
# full safety contract): every write goes through a directory FD of the lease dir; holder.json is
# replaced by an O_EXCL temp + rename; a missing lease dir, a missing/corrupt holder.json, a foreign
# holder or an empty identity creates and touches nothing; holder.json and the lease path are
# re-checked just before the rename, so a concurrent reclaim or release wins (the remaining window
# between that check and the rename is unreachable while the holder beats).
#
# Prints exactly one `RIG_LEASE_REFRESH=<status>` line. Returns 0 refreshed; 1 not ours (not-mine /
# no-lease / no-holder / corrupt / no-identity / released-during-refresh); 2 a filesystem error (the
# lease may still be ours); 3 past the hold ceiling or no parseable acquired_at.
rig_lease_refresh_if_mine() {
  local repo="${1:-}" run_id="${2:-}" lookahead="${3:-${RIG_LEASE_LOOKAHEAD_SECS:-900}}"
  local max_hold="${RIG_LEASE_MAX_HOLD_SECS:-4500}"
  if [ -z "$repo" ] || [ -z "$run_id" ]; then
    printf 'RIG_LEASE_REFRESH=no-identity\n'
    return 1
  fi
  case "$lookahead" in
    "" | *[!0-9]*) lookahead=900 ;;
  esac
  case "$max_hold" in
    "" | *[!0-9]*) max_hold=4500 ;;
  esac
  python3 "$(dirname "${BASH_SOURCE[0]}")/../rig_lease_refresh.py" \
    "$(rig_lease_dir)" "$repo" "$run_id" "$lookahead" "$max_hold"
}

# rig_lease_keepalive_spawn <repo> <run_id> -> detach a LEASE-ONLY keep-alive (issue 1383) that runs
# rig_lease_refresh_if_mine every RIG_LEASE_KEEPALIVE_SEC (default 30 s). rig-busy-gate.sh starts it
# on its success path: it bridges the gap between the acquire and the E2E's own heartbeat refresher
# (the verdict-exe fetch step and the first preflight ran ~18 min unbeaten on 27.9.2026) and keeps
# beating until the lease is released or goes foreign (rc 1) or the hold ceiling is reached (rc 3);
# a filesystem error (rc 2) is retried on the next tick. Every fd is on /dev/null, so a GitHub
# Actions step or a test's captured pipes are never held open; the runner's end-of-job orphan
# cleanup ends it at the latest. Prints `RIG_LEASE_KEEPALIVE=started pid=<pid> every=<s>s for
# <repo>#<run_id>`; without an identity it starts nothing.
rig_lease_keepalive_spawn() {
  local repo="${1:-}" run_id="${2:-}"
  local interval="${RIG_LEASE_KEEPALIVE_SEC:-30}"
  if [ -z "$repo" ] || [ -z "$run_id" ]; then
    return 0
  fi
  case "$interval" in
    "" | 0 | *[!0-9]*) interval=30 ;;
  esac
  (
    trap - EXIT
    set +e
    while :; do
      sleep "$interval" || exit 0
      rig_lease_refresh_if_mine "$repo" "$run_id" >/dev/null 2>&1
      case "$?" in
        0 | 2) ;;
        *) exit 0 ;;
      esac
    done
  ) </dev/null >/dev/null 2>&1 &
  local kpid=$!
  disown "$kpid" 2>/dev/null || true
  printf 'RIG_LEASE_KEEPALIVE=started pid=%s every=%ss for %s#%s\n' \
    "$kpid" "$interval" "$repo" "$run_id"
}

# rig_lease_try_acquire -> the ATOMIC primitive: `mkdir` the lockdir. exit 0 iff WE created it
# just now (nobody held it); exit 1 iff it already existed. No other side effects -- callers write
# holder.json themselves after a successful acquire.
rig_lease_try_acquire() {
  mkdir "$(rig_lease_dir)" 2>/dev/null
}

# rig_lease_acquire <repo> <run_id> <run_url> <job> <expected_release_at> [<stale_secs>] -> the
# top-level entry point. Prints exactly one line to stdout:
#   RIG_LEASE_ACQUIRED                                    (nobody held it -- now ours)
#   RIG_LEASE_RECLAIMED                                   (a stale foreign holder was reclaimed --
#                                                           ALSO logs a loud ::warning citing #830)
#   RIG_LEASE_HELD_BY=<repo>#<run_id> run_url=<url> job=<job> expected_release_at=<ts>
#                                                          (a genuinely LIVE foreign holder)
# Returns 0 for the first two (the lease is now OURS) and 1 for the third.
rig_lease_acquire() {
  local repo="$1" run_id="$2" run_url="$3" job="$4" expected_release_at="$5"
  local stale_secs="${6:-5400}"

  # GC any lockdir aside-copies leaked by a release that crashed between its atomic rename and the
  # rm of the renamed copy (#857) -- acquire is the guaranteed GC point (every gate acquires).
  rig_lease_sweep_releasing

  if rig_lease_try_acquire; then
    rig_lease_write_holder "$repo" "$run_id" "$run_url" "$job" "$expected_release_at"
    printf 'RIG_LEASE_ACQUIRED\n'
    return 0
  fi

  if rig_lease_is_stale "$stale_secs"; then
    local old_summary; old_summary="$(rig_lease_holder_summary)"
    echo "::warning title=RIG LEASE STALE RECLAIM (#830)::previous holder (${old_summary}) looks dead -- heartbeat stale and/or its run is no longer in progress. Reclaiming the rig lease now rather than deadlocking (mirrors the #657 self-heal principle: never a permanent deadlock)." >&2
    rig_lease_write_holder "$repo" "$run_id" "$run_url" "$job" "$expected_release_at"
    printf 'RIG_LEASE_RECLAIMED\n'
    return 0
  fi

  printf 'RIG_LEASE_HELD_BY=%s\n' "$(rig_lease_holder_summary)"
  return 1
}

# rig_lease_sweep_releasing -> remove any leaked "<leasedir>.releasing.*" teardown copies left by
# a release that crashed BETWEEN its atomic rename-aside and the rm of the renamed copy (#857).
# Each such copy is already detached from the live lease path, so deleting it can never harm an
# active lease, and a concurrent release rm-ing the same copy just double-deletes harmlessly. A
# no-glob-match yields the literal pattern, which the `[ -e ]` guard skips. Never touches "$d".
rig_lease_sweep_releasing() {
  local d; d="$(rig_lease_dir)"
  local leftover
  for leftover in "${d}".releasing.*; do
    [ -e "$leftover" ] || continue
    rm -rf "$leftover" 2>/dev/null || true
  done
}

# rig_lease_release <run_id> -> release the lease -- ONLY if RUN_ID is still its current holder
# (never destroys a DIFFERENT, later holder's lease out from under it -- e.g. a run that lost the
# acquire race, or one whose lease was already reclaimed as stale by someone else). Idempotent /
# safe to call even when the lease dir does not exist.
rig_lease_release() {
  local run_id="$1"
  local d; d="$(rig_lease_dir)"
  rig_lease_sweep_releasing
  [ -d "$d" ] || return 0
  local holder_run_id; holder_run_id="$(rig_lease_read_holder_field run_id)"
  if [ "$holder_run_id" != "$run_id" ]; then
    echo "[rig-lease] NOT releasing -- current holder (run_id=${holder_run_id:-unknown}) is not us (run_id=${run_id}); it must have been reclaimed already." >&2
    return 0
  fi
  # ATOMIC teardown (#857): rename the whole lockdir aside in ONE syscall, THEN delete the renamed
  # copy. A concurrent reader/acquirer therefore sees "$d" as either a COMPLETE lease or entirely
  # absent -- never the dir-present-but-holder.json-gone intermediate that a recursive `rm -rf "$d"`
  # exposes (which lets a concurrent `mkdir "$d"` guard fail EEXIST against a holder-less dir, reclaim
  # into "$d", and have that fresh lease deleted by this release's still-running recursive delete).
  # If the rename fails, "$d" has already vanished (a concurrent release/reclaim) -- nothing to tear
  # down; the stale-reclaim heartbeat backstop self-heals any pathological fs-error leftover, so NO
  # `rm -rf "$d"` fallback (which would reintroduce the exact window) is added.
  local releasing="${d}.releasing.$$"
  if mv "$d" "$releasing" 2>/dev/null; then
    rm -rf "$releasing" 2>/dev/null || true
    echo "[rig-lease] released."
  elif [ -d "$d" ]; then
    # mv failed but "$d" is still present -- a genuine fs error, NOT the common "already vanished"
    # case. Do NOT claim it was released; leave it for the stale-reclaim heartbeat backstop.
    echo "[rig-lease] WARNING: could not rename lease dir aside; leaving it for the stale-reclaim backstop." >&2
  else
    echo "[rig-lease] released (lease already gone)."
  fi
}
