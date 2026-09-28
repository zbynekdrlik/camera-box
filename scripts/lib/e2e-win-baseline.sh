#!/usr/bin/env bash
# airuleset:script-ok source-only lib (never executed directly) -- must NOT set -e: sourcing runs it
# in the CALLER's shell (recording-e2e.sh), whose own `set -euo pipefail` would otherwise be changed
# here. Same convention as scripts/lib/manifest-autosource.sh / strih-platform.sh. Every function
# below is safe under the caller's `set -euo pipefail` and always returns 0.
#
# scripts/lib/e2e-win-baseline.sh -- issue 1357: the full-path E2E `[0/8]` gathers the Windows
# OBS-box baseline (power plan, sleep, hibernate, USB selective suspend, WER DontShowUI) ONCE and
# hands the raw gathers to the version-integrity gate's REPORT-ONLY `--win-baseline NAME=FILE`
# facet. Design-by: main (issue 1357, comment 5865794137, Approach 1).
#
# Why: on 26.9.2026 RESOLUME-SNV ran the Balanced power plan and stalled the FOH VBAN senders for
# hours; the E2E -- the one run that measures A/V -- never looked. Now its log names a drifted item.
#
# REPORT-ONLY by the owner's issue-1357 ruling: only the power plan is ever SET (by the Windows
# genlock deploy program, step 0b); the E2E never writes a setting and never fails on the baseline.
# A DRIFT, an unread box, a hung gather, a crashed check -- each is ONE run-log line, never fatal.
#
# Reuses, never re-implements:
#   * scripts/win-baseline-check.sh -- the gather (scp + `powershell -File` over ssh) and the grade,
#     over the obs-fleet `win-baseline` facet (stream, resolume); a traveling box that is away
#     (resolume, obs_fleet_poll_now false) is the check's own SKIPPED and leaves no gather file.
#   * version-integrity-gate.sh `--win-baseline` -- renders the rows (win_baseline_report_rows),
#     never touches the gate's ok/bad/unknown roll-up, so its exit code cannot change.
#
# Credentials: the check's own defaults (WIN_BASELINE_SSH_USER / WIN_SSH_USER / WIN_BASELINE_SSH_PASS),
# the same fleet default the E2E's dantesync version gate uses for stream.
#
# Test seams (offline, no box): the check's WIN_BASELINE_FETCH_CMD and obs-fleet's OBS_FLEET_HOME;
# E2E_WIN_BASELINE_TIMEOUT overrides the outer bound; E2E_WIN_BASELINE_CHECK the check path.
# Tests: tests/python/test_e2e_win_baseline_1357.py.

# The facet roster comes from the ONE obs-fleet list. Lazy-sourced (strih-platform.sh idiom), so a
# caller that already sourced it is not re-sourced.
command -v obs_fleet_facet_members >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/obs-fleet.sh"

E2E_WIN_BASELINE_LIB_DIR="${BASH_SOURCE[0]%/*}"

# _e2e_wb_uint VALUE DEFAULT -> VALUE as a base-10 integer (a leading zero is never read as octal)
# when it is 1-6 plain digits, else DEFAULT.
_e2e_wb_uint() {
  case "${1:-}" in
    '' | *[!0-9]* | ???????*) printf '%s' "$2" ;;
    *) printf '%s' "$((10#$1))" ;;
  esac
}

# _e2e_wb_gather_host FILE -> the COMPUTERNAME on the gather's `==WINBASELINE-BEGIN== v1 host=<name>`
# line (CRLF tolerated), or `<none>` when the file has no such line (an empty / failed gather).
_e2e_wb_gather_host() {
  local host
  host="$(awk '/^==WINBASELINE-BEGIN== / { gsub(/\r/, ""); for (i = 1; i <= NF; i++)
            if ($i ~ /^host=/) { sub(/^host=/, "", $i); print $i; exit } }' "$1" 2>/dev/null || true)"
  printf '%s' "${host:-<none>}"
}

# e2e_win_baseline_boxes -> the facet members (space-separated), empty when the roster is unreadable.
e2e_win_baseline_boxes() {
  obs_fleet_facet_members win-baseline 2>/dev/null || true
}

# e2e_win_baseline_timeout_s -> the outer bound (seconds) for ONE win-baseline-check.sh run, sized
# from the check's own per-box bounds, never a guessed constant:
#   per box = 2 x WIN_BASELINE_SSH_TIMEOUT (the scp of the gather and the ssh run, each under its own
#             `timeout`; default 20 s)
#           + OBS_FLEET_RESOLVE_TIMEOUT + OBS_FLEET_STATUS_TIMEOUT (the traveling-box home check,
#             DNS + the OBS-WS :4455 connect; defaults 2 + 4 s -- charged to every box, conservative)
#   total   = boxes x per box + 10 s (process start, the gather emit, the grade)
# Defaults with the two-box facet: 2 x (40 + 6) + 10 = 102 s. A box that answers takes a few
# seconds, so the bound only matters for a hung box. E2E_WIN_BASELINE_TIMEOUT (> 0) overrides it.
e2e_win_baseline_timeout_s() {
  local override boxes n=0 ssh res stat _b
  override="$(_e2e_wb_uint "${E2E_WIN_BASELINE_TIMEOUT:-}" 0)"
  if [ "$override" -gt 0 ]; then
    printf '%s\n' "$override"
    return 0
  fi
  boxes="$(e2e_win_baseline_boxes)"
  for _b in $boxes; do n=$((n + 1)); done
  [ "$n" -gt 0 ] || n=2
  ssh="$(_e2e_wb_uint "${WIN_BASELINE_SSH_TIMEOUT:-}" 20)"
  res="$(_e2e_wb_uint "${OBS_FLEET_RESOLVE_TIMEOUT:-}" 2)"
  stat="$(_e2e_wb_uint "${OBS_FLEET_STATUS_TIMEOUT:-}" 4)"
  printf '%s\n' "$((n * (2 * ssh + res + stat) + 10))"
  return 0
}

# e2e_win_baseline_gather OUT_DIR -> runs scripts/win-baseline-check.sh --out-dir OUT_DIR ONCE under
# a bounded `timeout`, prints the check's per-box summary lines plus ONE verdict line, and FILLS the
# global array WIN_BASELINE_GATE_ARGS with `--win-baseline <box>=OUT_DIR/<box>.txt` for every facet
# box whose gather file exists (a box the check read, or started to read before a timeout -- an empty
# or partial file grades UNKNOWN in the gate, which is the honest reading). No file -> an empty array.
# Stale gathers of an earlier run in a reused OUT_DIR are removed first, so a box SKIPPED now is never
# graded from an old file; a stale path that cannot be removed (a directory, a read-only dir) keeps
# that box out of the array for this run, named on one line. Only a regular file is ever handed on.
# The full check output is kept in OUT_DIR/win-baseline-check.log. When the bound hits, `timeout`
# TERMs its whole process group: the check (bash with an EXIT trap, so it catches TERM) removes its
# own mktemp work dir and exits, and the fetch child it was waiting on dies with it; each inner
# `timeout N scp/ssh` of the check runs in its own group and ends within its own N s.
# Always returns 0: the baseline is report-only and never aborts the caller's `set -euo pipefail`.
e2e_win_baseline_gather() {
  local out_dir="${1:-}" check log bound boxes box rc=0 verdict graded=0 unremovable=" "
  check="${E2E_WIN_BASELINE_CHECK:-$E2E_WIN_BASELINE_LIB_DIR/../win-baseline-check.sh}"
  WIN_BASELINE_GATE_ARGS=()
  if [ -z "$out_dir" ]; then
    echo "    Windows OBS-box baseline (issue 1357): SKIPPED -- no output dir given (report-only)"
    return 0
  fi
  if ! mkdir -p "$out_dir" 2>/dev/null; then
    echo "    Windows OBS-box baseline (issue 1357): SKIPPED -- cannot create $out_dir (report-only)"
    return 0
  fi
  boxes="$(e2e_win_baseline_boxes)"
  for box in $boxes; do
    rm -f -- "$out_dir/$box.txt" 2>/dev/null || true
    if [ -e "$out_dir/$box.txt" ]; then
      unremovable="$unremovable$box "
      echo "    Windows OBS-box baseline (issue 1357): an old $out_dir/$box.txt cannot be removed -- $box is not graded this run"
    fi
  done
  log="$out_dir/win-baseline-check.log"
  bound="$(e2e_win_baseline_timeout_s)"
  echo "    Windows OBS-box baseline (issue 1357, report-only): reading ${boxes:-<no boxes>} (bound ${bound} s)"
  timeout --kill-after=5 "$bound" bash "$check" --out-dir "$out_dir" </dev/null >"$log" 2>&1 || rc=$?
  grep -E '^box=[^ ]+ win_baseline=' "$log" 2>/dev/null | sed 's/^/      /' || true
  case "$rc" in
    0) verdict="OK -- every read box matches the baseline" ;;
    20) verdict="DRIFT -- a box drifted; the gate rows below name the items" ;;
    11) verdict="UNKNOWN -- a box could not be read, the check is incomplete" ;;
    124 | 137) verdict="TIMEOUT after ${bound} s -- the gather hung; a box read so far is still graded below" ;;
    *) verdict="FAILED (rc ${rc}) -- the check did not run to the end" ;;
  esac
  for box in $boxes; do
    case "$unremovable" in *" $box "*) continue ;; esac
    [ -f "$out_dir/$box.txt" ] || continue
    WIN_BASELINE_GATE_ARGS+=(--win-baseline "$box=$out_dir/$box.txt")
    graded=$((graded + 1))
    # Which machine really answered: resolume.lan can resolve to an address another PC also holds
    # (.claude/rules/obs-fleet-list.md), so the run log names the gather's own COMPUTERNAME.
    echo "      box=$box gathered from host=$(_e2e_wb_gather_host "$out_dir/$box.txt")"
  done
  echo "    Windows OBS-box baseline: ${verdict} (report-only, does NOT block the run; ${graded} box gather(s) to the gate; log ${log})"
  return 0
}
