#!/usr/bin/env bash
# win-baseline-check.sh -- issue 1357: read + grade the Windows OBS-box baseline on every Windows OBS
# box (REPORT-ONLY). See the extended header below.
set -euo pipefail
#
# WHAT: for each box of the obs-fleet `win-baseline` facet (stream, resolume), copy the read-only
# gather program (scripts/lib/obs-box-baseline-win.sh win_baseline_gather_ps1) to the box with
# `scp -O`, run it BY PATH (`powershell -NoProfile -ExecutionPolicy Bypass -File ...`, never nested
# PowerShell over ssh -- .claude/rules/rig-state-inspection.md §2) and grade its output with the ONE
# grader win_baseline_grade. Items: power_scheme, sleep_ac, hibernate_ac, usb_selective_suspend,
# wer_dontshowui (OK / DRIFT / UNKNOWN; unread = UNKNOWN, never OK).
#
# REPORT-ONLY: it never writes a setting on any box. The only baseline mutation is the power plan,
# set by the Windows genlock deploy program (scripts/deploy-genlock-fleet.sh step 0b). The gather
# file it copies is the box's only trace (overwritten on every run).
#
# A traveling box that is away (obs_fleet_poll_now false -- resolume while not home) is SKIPPED.
#
# Output, per box:  box=<b> item=<i> verdict=<V> detail=<text>   (one line per item)
#                   box=<b> win_baseline=<OK|DRIFT|UNKNOWN|SKIPPED ...>[ drift=<items>][ unknown=<items>]
# The raw gather text can be kept (--out-dir DIR -> DIR/<box>.txt) and handed to
# `scripts/version-integrity-gate.sh --win-baseline <box>=DIR/<box>.txt` (its report-only facet).
#
# Usage:
#   scripts/win-baseline-check.sh [--box NAME ...] [--out-dir DIR]
#   scripts/win-baseline-check.sh --emit-ps1        (print the gather program only)
# Exit: 0 = every read box matches, 20 = a box DRIFTED, 11 = a box UNKNOWN and none drifted,
#   1 = usage error.
# Env: WIN_BASELINE_SSH_USER (default WIN_SSH_USER or the fleet default), WIN_BASELINE_SSH_PASS (the
#   fleet default the dantesync gates use), WIN_BASELINE_SSH_TIMEOUT (s, default 20),
#   WIN_BASELINE_REMOTE_PS1 (default C:/camera-box-win-baseline-gather.ps1).
# Test seam: WIN_BASELINE_FETCH_CMD, when set, is invoked as `<cmd> <box> <host> <outfile>` and
#   replaces the real scp + ssh read (no box needed); OBS_FLEET_HOME forces the home set.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/obs-box-baseline-win.sh
. "$HERE/lib/obs-box-baseline-win.sh"

SSH_USER="${WIN_BASELINE_SSH_USER:-${WIN_SSH_USER:-newlevel}}"
SSH_PASS="${WIN_BASELINE_SSH_PASS:-newlevel}"
SSH_TIMEOUT="${WIN_BASELINE_SSH_TIMEOUT:-20}"
REMOTE_PS1="${WIN_BASELINE_REMOTE_PS1:-C:/camera-box-win-baseline-gather.ps1}"

usage() { sed -n '2,3p;5,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

BOXES=""
OUT_DIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --box) BOXES="${BOXES:+$BOXES }${2:?--box needs a name}"; shift 2 ;;
    --out-dir) OUT_DIR="${2:?--out-dir needs a directory}"; shift 2 ;;
    --emit-ps1) win_baseline_gather_ps1; exit 0 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "win-baseline-check: unknown argument '$1' (try --help)" >&2; exit 1 ;;
  esac
done
[ -n "$BOXES" ] || BOXES="$(obs_fleet_facet_members win-baseline)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/win-baseline-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
[ -z "$OUT_DIR" ] || mkdir -p "$OUT_DIR"

# fetch_gather BOX HOST OUT -> the gather output of HOST into OUT (rc 0), or rc != 0.
fetch_gather() {
  local box="$1" host="$2" out="$3" ps1="$WORK/gather.ps1"
  if [ -n "${WIN_BASELINE_FETCH_CMD:-}" ]; then
    "$WIN_BASELINE_FETCH_CMD" "$box" "$host" "$out"
    return
  fi
  local -a opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR
                 -o ConnectTimeout="$SSH_TIMEOUT")
  win_baseline_gather_ps1 >"$ps1"
  sshpass -p "$SSH_PASS" timeout "$SSH_TIMEOUT" scp -O -q "${opts[@]}" "$ps1" "${SSH_USER}@${host}:${REMOTE_PS1}" || return 1
  sshpass -p "$SSH_PASS" timeout "$SSH_TIMEOUT" ssh "${opts[@]}" "${SSH_USER}@${host}" \
    "powershell -NoProfile -ExecutionPolicy Bypass -File \"${REMOTE_PS1//\//\\}\"" >"$out"
}

echo "== Windows OBS-box baseline (issue 1357): power plan, sleep, hibernate, USB suspend, WER -- report-only =="
DRIFTED="" UNKNOWNS=""
for box in $BOXES; do
  if ! host="$(obs_fleet_host "$box")"; then
    echo "box=$box win_baseline=UNKNOWN (not in OBS_FLEET)"
    UNKNOWNS="${UNKNOWNS:+$UNKNOWNS }$box"
    continue
  fi
  if ! obs_fleet_poll_now "$box"; then
    echo "box=$box win_baseline=SKIPPED (traveling box away -- obs_fleet_is_home false)"
    continue
  fi
  raw="${OUT_DIR:-$WORK}/$box.txt"
  : >"$raw"
  fetch_gather "$box" "$host" "$raw" >/dev/null 2>&1 || echo "  ($box: gather read failed -- every item grades UNKNOWN)" >&2
  box_rc=0
  graded="$(win_baseline_grade "$raw")" || box_rc=$?
  drift_items="" unknown_items=""
  while read -r item verdict detail; do
    [ -n "$item" ] || continue
    echo "box=$box item=$item verdict=$verdict detail=$detail"
    case "$verdict" in
      DRIFT) drift_items="${drift_items:+$drift_items,}$item" ;;
      *) unknown_items="${unknown_items:+$unknown_items,}$item" ;;   # UNKNOWN (or no verdict at all)
    esac
  done <<<"$graded"
  case "$box_rc" in
    0) overall=OK ;;
    20) overall=DRIFT; DRIFTED="${DRIFTED:+$DRIFTED }$box" ;;
    *) overall=UNKNOWN; UNKNOWNS="${UNKNOWNS:+$UNKNOWNS }$box" ;;
  esac
  echo "box=$box win_baseline=$overall${drift_items:+ drift=$drift_items}${unknown_items:+ unknown=$unknown_items}"
done

if [ -n "$DRIFTED" ]; then
  echo "!! WINDOWS BASELINE DRIFT on: $DRIFTED${UNKNOWNS:+ (and unread: $UNKNOWNS)} -- see the named items above (report-only)."
  exit 20
elif [ -n "$UNKNOWNS" ]; then
  echo "!! Windows baseline UNREAD on: $UNKNOWNS -- the check is INCOMPLETE, not clean."
  exit 11
fi
echo "OK: every read Windows OBS box matches the baseline."
exit 0
