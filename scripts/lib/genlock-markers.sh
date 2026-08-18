#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# same source-only convention as scripts/lib/manifest-autosource.sh / ahk-watchdog.sh: sourcing
# this file runs it in the CALLER's shell, so `set -euo pipefail` here would leak into the caller
# (scripts/deploy-genlock-fleet.sh, and setup-imag.sh's inline sibling copy) -- each caller owns its
# own strict mode. Every function does explicit error checks and returns rather than relying on -e.
#
# scripts/lib/genlock-markers.sh -- issue 789 (bod 4 + bod 5): the SINGLE SOURCE OF TRUTH for the
# genlock deploy PRIMITIVES shared across the deploy legs -- the marker writes (bod 4) and the
# box-backup retention plan (bod 5) -- so the ONE deploy path (scripts/deploy-genlock-fleet.sh) and
# the provisioning path (scripts/setup-imag.sh) can never drift on HOW a box records "which build is
# deployed here".
# setup-imag.sh ships STANDALONE to the box (scp to /tmp, run there) so it cannot source this sibling
# lib -- it carries a behaviorally-identical inline copy, locked to this one by the parity test in
# tests/deploy_genlock_fleet.rs. The imag on-box program emitted by deploy-genlock-fleet.sh DOES
# source this lib (it is scp'd alongside the artifacts), so both live deploy legs write markers the
# same way.
#
# The three markers, written TOGETHER (they version as one build) and ATOMICALLY (temp file in the
# same dir, then `mv -f` -- a POSIX same-directory rename, so a concurrent reader such as
# drift-guard.sh / version-integrity-gate.sh never sees a half-written marker):
#   GENLOCK_BUILD_SHA.txt   -- the deployed genlock-monorepo build commit (libobs/obs-frontend)
#   DISTROAV_BUILD_SHA.txt  -- the deployed distroav build commit
#   DEPLOYED_AT             -- an ISO-8601 timestamp of this deploy

# _genlock_marker_atomic DEST CONTENT -> write CONTENT + one trailing newline to DEST atomically
# (temp-then-rename). Returns 0 on success, 1 on any I/O failure (cleaning up the temp file). Private
# helper; callers use genlock_write_markers.
_genlock_marker_atomic() {
  local dest="$1" content="$2" tmp
  tmp="${dest}.tmp.$$"
  if ! printf '%s\n' "$content" > "$tmp" 2>/dev/null; then
    echo "genlock_write_markers: could not write temp file $tmp" >&2
    rm -f "$tmp" 2>/dev/null
    return 1
  fi
  if ! mv -f "$tmp" "$dest" 2>/dev/null; then
    echo "genlock_write_markers: could not rename $tmp -> $dest" >&2
    rm -f "$tmp" 2>/dev/null
    return 1
  fi
  return 0
}

# genlock_write_markers MARKER_DIR GENLOCK_SHA DISTROAV_SHA [DEPLOYED_AT]
#   Writes the three genlock deploy markers into MARKER_DIR, each atomically (temp-then-rename).
#   DEPLOYED_AT defaults to `date -Is`. A missing MARKER_DIR / GENLOCK_SHA / DISTROAV_SHA is a
#   fail-loud usage error (return 2) -- never a silent partial write. Returns 0 on success, 1 on an
#   I/O failure, 2 on a usage error.
genlock_write_markers() {
  local marker_dir="${1:-}" genlock_sha="${2:-}" distroav_sha="${3:-}" deployed_at="${4:-}"
  if [ -z "$marker_dir" ]; then
    echo "genlock_write_markers: MARKER_DIR (arg 1) is required" >&2; return 2
  fi
  if [ -z "$genlock_sha" ]; then
    echo "genlock_write_markers: GENLOCK_SHA (arg 2) is required" >&2; return 2
  fi
  if [ -z "$distroav_sha" ]; then
    echo "genlock_write_markers: DISTROAV_SHA (arg 3) is required" >&2; return 2
  fi
  [ -n "$deployed_at" ] || deployed_at="$(date -Is)"
  if ! mkdir -p "$marker_dir" 2>/dev/null; then
    echo "genlock_write_markers: could not create marker dir $marker_dir" >&2; return 1
  fi
  _genlock_marker_atomic "$marker_dir/GENLOCK_BUILD_SHA.txt"  "$genlock_sha"  || return 1
  _genlock_marker_atomic "$marker_dir/DISTROAV_BUILD_SHA.txt" "$distroav_sha" || return 1
  _genlock_marker_atomic "$marker_dir/DEPLOYED_AT"           "$deployed_at"  || return 1
  return 0
}

# genlock_retention_delete_plan KEEP PARENT GLOB
#   issue 789 bod 5 -- box-backup hygiene. Lists the directories under PARENT matching GLOB, newest
#   first by mtime, and prints (one absolute path per line) the ones BEYOND the newest KEEP -- i.e.
#   the delete CANDIDATES. It never deletes anything: the caller (the fleet script's imag leg, and
#   the tests) treats the output as a PLAN the operator confirms, mirroring the Windows leg's
#   print-only retention. A missing PARENT is a no-op (nothing deployed there yet). KEEP<=0 lists
#   every match. Returns 0 (2 on a usage error: no PARENT).
genlock_retention_delete_plan() {
  local keep="${1:-3}" parent="${2:-}" glob="${3:-*}" d i=0
  if [ -z "$parent" ]; then
    echo "genlock_retention_delete_plan: PARENT (arg 2) is required" >&2; return 2
  fi
  [ -d "$parent" ] || return 0
  # `ls -1dt` sorts by mtime, newest first; the "$parent"/$glob prefix makes each line an absolute
  # path. `|| true` so a no-match (glob unexpanded) is an empty plan, never a pipefail abort.
  while IFS= read -r d; do
    [ -d "$d" ] || continue
    i=$((i + 1))
    if [ "$i" -gt "$keep" ]; then
      printf '%s\n' "$d"
    fi
  done < <(ls -1dt "$parent"/$glob 2>/dev/null || true)
  return 0
}
