#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), the sibling
# scripts/lib/*.sh convention: recording-e2e.sh sources it and runs the helper as a BARE statement
# under its own set -euo pipefail, so the helper itself never changes a shell option of the caller.
#
# scripts/lib/e2e-rundir-retention.sh -- bounded retention of the dev1 E2E run dirs (issue 1395).
#
# WHY: recording-e2e.sh writes every run into its own /tmp/recording-e2e-<RUN_ID> (the strih/stream
# recordings, the verdict, the pixel proof: tens to hundreds of MB a run) and nothing ever removed an
# older one -- 268 dirs / 5.9 GB on 23.9.2026 on dev1's ONE disk, shared by ~8 Claude sessions and
# two projects' CI runners. The dir must OUTLIVE the run, so this is a bounded retention, never a
# delete-on-exit: full-path-e2e.yml reads verdict-<RUN_ID>.json, uploads the PNG / pixel-proof files
# and derives the failure-alert stage from the dir AFTER the script returns, and the mining tools
# (arrival_floor_decompose.py --runs-glob, window_gate_walkdown.py, the verdict-JSON calibration
# recipes in .claude/rules/) read several archived runs.
#
# A wider mining set than the newest few runs: raise E2E_RUNDIR_KEEP for the runs you plan to mine,
# or pull verdict-*.json from each run's recording-e2e-full-path CI artifact
# (gh run download <run-id> -n recording-e2e-full-path).
#
# CONTRACT -- e2e_rundir_retention <parent_dir> <current_run_dir> [keep]:
#   * keeps <current_run_dir> whatever its age, plus the newest <keep> OTHER directories in
#     <parent_dir> whose name is exactly recording-e2e-<digits> (newest by mtime);
#   * keep = the 3rd argument, else E2E_RUNDIR_KEEP, else 8; anything but a plain non-negative
#     integer (1-6 digits) removes nothing;
#   * "digits" means the ASCII 0-9 only, spelled out in every bracket set: under dev1's
#     en_US.UTF-8 a bash bracket RANGE also matches Arabic-Indic, superscript and fullwidth
#     digits (the issue-1302 locale trap; GitHub's C.UTF-8 runners never see it);
#   * removes the older ones with rm -rf, which unlinks a symlink INSIDE a run dir and never
#     descends into its target;
#   * never touches any other name, a regular file, or a symlink named like a run dir (never
#     followed, never removed), and keeps an entry whose mtime it cannot read;
#   * prints exactly ONE line: what it kept, removed and freed (or why it did nothing), plus how
#     many dirs it could not remove;
#   * ALWAYS returns 0 and never aborts a caller running under set -euo pipefail, on any input (an
#     empty or missing parent, no match, a failing stat/du/rm): the work runs in a subshell, so even
#     an unexpected expansion error stays inside it.

e2e_rundir_retention() {
  (_e2e_rundir_retention_run "$@") || true
  return 0
}

# The body, always run in the subshell above (never call it directly from a caller under set -e).
_e2e_rundir_retention_run() {
  set +e
  local parent="${1:-}" current="${2:-}" keep="${3:-${E2E_RUNDIR_KEEP:-8}}"
  local tag="e2e-rundir-retention:" run_re='^recording-e2e-[0123456789]+$'
  if [ -z "$parent" ] || [ ! -d "$parent" ]; then
    echo "$tag no run-dir parent '${parent}' -- nothing to prune"
    return 0
  fi
  if ! [[ $keep =~ ^[0123456789]{1,6}$ ]]; then
    echo "$tag keep '${keep}' is not a non-negative integer -- removed nothing under ${parent}"
    return 0
  fi
  keep=$((10#$keep))

  local cur_name="${current%/}"
  cur_name="${cur_name##*/}"
  local entry name mtime
  local -a rows=()
  for entry in "$parent"/recording-e2e-*; do
    name="${entry##*/}"
    [[ $name =~ $run_re ]] || continue
    [ -L "$entry" ] && continue
    [ -d "$entry" ] || continue
    [ -n "$cur_name" ] && [ "$name" = "$cur_name" ] && continue
    [ -n "$current" ] && [ "$entry" -ef "$current" ] && continue
    mtime="$(stat -c %Y -- "$entry" 2>/dev/null)"
    [[ $mtime =~ ^[0123456789]+$ ]] || continue
    rows+=("$mtime $name")
  done

  local kept=0 removed=0 failed=0 freed_kb=0 kb
  if [ "${#rows[@]}" -gt 0 ]; then
    while read -r mtime name; do
      [ -n "$name" ] || continue
      if [ "$kept" -lt "$keep" ]; then
        kept=$((kept + 1))
        continue
      fi
      entry="$parent/$name"
      kb="$(du -sk -- "$entry" 2>/dev/null)"
      kb="${kb%%[!0123456789]*}"
      [[ $kb =~ ^[0123456789]+$ ]] || kb=0
      if rm -rf -- "$entry" 2>/dev/null && [ ! -e "$entry" ]; then
        removed=$((removed + 1))
        freed_kb=$((freed_kb + kb))
      else
        failed=$((failed + 1))
      fi
    done < <(printf '%s\n' "${rows[@]}" | sort -k1,1nr -k2,2r)
  fi

  local msg="$tag ${parent}: kept the current run + ${kept} older (keep ${keep}), removed ${removed}"
  msg="$msg ($((freed_kb / 1024)).$(((freed_kb % 1024) * 10 / 1024)) MB freed)"
  if [ "$failed" -gt 0 ]; then
    msg="$msg, ${failed} could not be removed"
  fi
  echo "$msg"
  return 0
}
