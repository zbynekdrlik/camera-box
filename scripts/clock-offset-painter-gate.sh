#!/usr/bin/env bash
#
# clock-offset-painter-gate.sh — dev1<->painter clock-offset precondition for the all-cambox
# sweep (#326; a #312 Phase-2 robustness gate).
#
# WHY THIS GATE EXISTS: in the ALL_CAMBOX sweep (recording-e2e.sh ALL_CAMBOX=1) each program-
# switch window boundary is stamped on dev1's CLOCK_REALTIME (obs_phase2.py switch ->
# time.time_ns(), plus the final `date +%s%N`), while recording-verdict --switch-schedule
# partitions the ONE stream recording by the cam2-PAINTER burn gen_ts_ns. Those are two DIFFERENT
# machines' clocks. The whole per-cambox attribution holds ONLY if dev1's clock is aligned to the
# painter's gen-ts clock to within the verdict's transition guard (DEFAULT_TRANSITION_GUARD_NS =
# 1_000_000_000 ns = 1 s; the verdict already discards ±guard around every boundary). If dev1
# drifts past the guard (independent NTP discipline, a DanteSync hiccup), frames near every window
# boundary are misattributed to the WRONG cambox window -> false gaps/copies (false FAIL) or a real
# boundary loss hidden (false PASS) — the silent #312 mis-attribution #326 is about. So this gate
# measures the dev1<->painter offset and FAILS FAST before the multi-minute sweep whose attribution
# it would otherwise silently corrupt — the same fail-fast spirit as the #7 DanteSync gate and the
# #123 version-integrity gate (don't run a long sweep whose result can't be trusted).
#
# Both dev1 and the painter (cam2) are DanteSync-slaved to the SAME NTP master (strih), so each
# reports its own absolute "[NTP] offset:+Nus" in journald. The dev1<->painter RELATIVE offset is
# |dev1_offset - painter_offset| on that shared basis. dev1 is read LOCALLY (journalctl); the
# painter is read over SSH (root/newlevel) — the same access pattern as clock-offset-guard.sh.
# This script REUSES that script's unit-tested pure parsers/comparator (offset_us_from_journal,
# abs_int, painter_offset_check) — it does NOT reinvent any parsing; it is the FLOW that gathers
# the two journals and applies the comparator. Per the project Script Failure Policy +
# test-strictness an UNREACHABLE node or an UNREADABLE offset is an explicit failure, NEVER a
# silent pass.
#
# Override the bound with --guard-us N / PAINTER_OFFSET_GUARD_US. Skip the gate entirely with
# SKIP_CLOCK_OFFSET_ASSERT=1 (consistent with the other gates' env opt-outs; ON by default). For
# unit tests / offline runs, feed pre-captured journald text via DEV1_DANTE_JOURNAL /
# PAINTER_DANTE_JOURNAL (file paths) instead of reading the live nodes.
#
# Usage:
#   clock-offset-painter-gate.sh [--guard-us N] [--painter NAME=IP]
#   clock-offset-painter-gate.sh --help
#
# Exit codes: 0 = within guard (or SKIPped), 20 = DRIFT (offset exceeds the guard),
#   11 = a node UNREACHABLE / offset UNKNOWN (incomplete — NOT clean), 1 = usage/IO error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Source the shared, unit-tested DanteSync parsers + the painter_offset_check comparator (its
# BASH_SOURCE!=$0 guard skips its own flow).
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"

# --- the dev1<->painter offset bound --------------------------------------------------------
#
# PAINTER_OFFSET_GUARD_US — the maximum tolerated dev1<->painter RELATIVE clock offset, in µs.
#
# Default 200000 µs (200 ms) = 1/5 of the verdict's 1 s switch-guard
# (recording-verdict --switch-guard-ns default = DEFAULT_TRANSITION_GUARD_NS = 1_000_000_000 ns).
# The verdict discards ±guard (±1 s) around each window boundary, so a true switch stays inside
# that discard zone — and frames are NOT misattributed — as long as |dev1<->painter| < the
# switch-guard. Tripping at 200 ms gives a 5x safety margin BEFORE the boundary's true painter-time
# switch could escape the discard zone, while clearing the healthy cluster's steady-state offsets
# (cam ~300 µs, strih master ~1249 µs — so dev1<->painter is normally well under 2 ms). Override
# with --guard-us / PAINTER_OFFSET_GUARD_US for a stricter or looser bound.
PAINTER_OFFSET_GUARD_US="${PAINTER_OFFSET_GUARD_US:-200000}"

# The painter node (cam2 — the box that paints the dual-QR monitor) "name=ip". Mirrors PAINTER_IP.
PAINTER_GATE_TARGET="${PAINTER_GATE_TARGET:-cam2=10.77.9.62}"
PAINTER_GATE_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"
# #550/#591/#595: EITHER box's freshest "[NTP] offset:" journal line must be no older than this
# many seconds behind that box's OWN newest journal line, or its offset is STALE and must never be
# fed into the relative dev1<->painter comparison below -- a stale reading on one side can mask a
# real current divergence, or manufacture a false one against the other box's genuinely fresh
# value (the age-blind offset_us_from_journal this gate used to call could not tell the two apart).
# Same default as verify-device.sh's DANTESYNC_OFFSET_FRESHNESS_S.
PAINTER_GATE_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"

usage() {
  cat <<EOF
clock-offset-painter-gate.sh — dev1<->painter clock-offset precondition for the all-cambox sweep (#326).

Asserts that dev1's CLOCK_REALTIME (the timeline the all-cambox switch windows are stamped on) is
aligned to the painter (cam2) DanteSync clock (the timeline the painted ticks/burns ride) to within
the guard, so the per-cambox window attribution cannot silently misattribute frames. FAILS FAST
(non-zero) otherwise — the same spirit as the #7 DanteSync gate. ON by default.

Usage:
  clock-offset-painter-gate.sh [--guard-us N] [--painter NAME=IP]
  clock-offset-painter-gate.sh --help

Default guard: ${PAINTER_OFFSET_GUARD_US} us (200 ms) — 1/5 of the verdict's 1 s switch-guard
(DEFAULT_TRANSITION_GUARD_NS), giving 5x margin before a boundary's true painter-time switch could
escape the ±guard discard zone. Default painter: ${PAINTER_GATE_TARGET}.

dev1 is read locally (journalctl -u dantesync); the painter is read over SSH. For offline/unit
runs, feed pre-captured journald text via DEV1_DANTE_JOURNAL / PAINTER_DANTE_JOURNAL (file paths).
Skip the whole gate with SKIP_CLOCK_OFFSET_ASSERT=1.

Each box's offset must be FRESH: the freshest "[NTP] offset:" journal line must be no older than
DANTESYNC_OFFSET_FRESHNESS_S (default ${PAINTER_GATE_FRESHNESS_S}) seconds behind that box's own
newest journal line, or the comparison is refused (INCOMPLETE, never a silent PASS on a stale
reading) -- see freshest_offset_us() in clock-offset-guard.sh (#550/#591/#595).

Exit codes: 0 = within guard (or SKIPped), 20 = DRIFT (offset exceeds the guard),
11 = a node UNREACHABLE / offset UNKNOWN (incomplete, NOT clean), 1 = usage/IO error.
EOF
}

# read_dev1_journal -> dev1's latest DanteSync journald lines (local journalctl), or "" if none.
# Overridable for tests/offline via DEV1_DANTE_JOURNAL=<file>. Read-only; a down/absent daemon
# collapses to empty output (caller maps empty -> UNKNOWN, never a silent pass).
read_dev1_journal() {
  if [ -n "${DEV1_DANTE_JOURNAL:-}" ]; then
    cat "$DEV1_DANTE_JOURNAL" 2>/dev/null || true
    return 0
  fi
  # -o short-iso (+ a wider -n 400 window) so freshest_offset_us can prove this box's offset
  # reading is current (#550/#595), not a stale multi-hour-old boot-STEP line.
  journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null || true
}

# read_painter_journal IP -> the painter's latest DanteSync journald lines over SSH, or "" if
# unreachable. Overridable for tests/offline via PAINTER_DANTE_JOURNAL=<file>. BatchMode=no so
# sshpass can feed the password; an auth failure / down host is collapsed to empty output (caller
# maps empty -> UNKNOWN, never a silent pass), without leaking SSH banners into the report.
read_painter_journal() {
  local ip="$1"
  if [ -n "${PAINTER_DANTE_JOURNAL:-}" ]; then
    cat "$PAINTER_DANTE_JOURNAL" 2>/dev/null || true
    return 0
  fi
  sshpass -p "$CLOCK_GUARD_SSH_PASS" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${PAINTER_GATE_SSH_TIMEOUT}" \
    "${CLOCK_GUARD_SSH_USER}@${ip}" \
    'journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null' 2>/dev/null || true
}

main() {
  local guard="$PAINTER_OFFSET_GUARD_US" target="$PAINTER_GATE_TARGET"
  while [ $# -gt 0 ]; do
    case "$1" in
      --guard-us) shift; guard="${1:-}" ;;
      --painter)  shift; target="${1:-}" ;;
      -h|--help)  usage; exit 0 ;;
      --*)        echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)          echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  # The env opt-out (ON by default): skip the whole gate, but say so LOUDLY so an unguarded run
  # is never mistaken for a verified one.
  if [ "${SKIP_CLOCK_OFFSET_ASSERT:-0}" = "1" ]; then
    echo "== clock-offset-painter-gate (#326): SKIPPED (SKIP_CLOCK_OFFSET_ASSERT=1) =="
    echo "   dev1<->painter offset NOT verified — all-cambox window attribution is UNGUARDED this run."
    exit 0
  fi

  if ! grep -qE '^[0-9]+$' <<<"$guard"; then
    echo "ERROR: --guard-us must be a positive integer (got '${guard}')." >&2
    exit 1
  fi
  local name ip
  name="${target%%=*}"; ip="${target#*=}"
  if [ -z "$name" ] || [ -z "$ip" ] || [ "$name" = "$target" ]; then
    echo "ERROR: --painter must be NAME=IP (got '${target}')." >&2
    exit 1
  fi
  # sshpass is only needed for the LIVE painter read; PAINTER_DANTE_JOURNAL (tests/offline) skips it.
  if [ -z "${PAINTER_DANTE_JOURNAL:-}" ] && ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to read the painter DanteSync offset over SSH." >&2
    exit 1
  fi

  echo "== clock-offset-painter-gate (#326): dev1<->painter offset must stay within ${guard} us =="
  echo "   (all-cambox switch windows ride dev1 CLOCK_REALTIME; painted ticks/burns ride the painter clock)"

  local dev1_journal painter_journal dev1_off painter_off rc=0
  dev1_journal="$(read_dev1_journal)"
  painter_journal="$(read_painter_journal "$ip")"
  if [ -z "$dev1_journal" ]; then
    printf '  %-18s UNREACHABLE  (no local DanteSync journal — is dantesync running on dev1?)\n' "dev1"
  fi
  if [ -z "$painter_journal" ]; then
    printf '  %-18s UNREACHABLE  (no DanteSync journal over SSH @ %s)\n' "$name" "$ip"
  fi
  # #550/#595: read the FRESHEST offset on EACH box (never the age-blind offset_us_from_journal
  # tail -1) so a stale boot-step line -- or a died/hung dantesync whose journal simply stopped
  # advancing -- can never be fed into the relative comparison below. Only when BOTH boxes report
  # a FRESH reading does the (unchanged) painter_offset_check run.
  dev1_off="$(freshest_offset_us "$dev1_journal" "$PAINTER_GATE_FRESHNESS_S")"
  painter_off="$(freshest_offset_us "$painter_journal" "$PAINTER_GATE_FRESHNESS_S")"
  if [ -z "$dev1_off" ] || [ -z "$painter_off" ]; then
    printf '  %-18s UNKNOWN  (no FRESH [NTP] offset within %ss: dev1=%s painter=%s)\n' \
      "dev1<->$name" "$PAINTER_GATE_FRESHNESS_S" "${dev1_off:-<none>}" "${painter_off:-<none>}"
    rc=3
  else
    painter_offset_check "dev1<->$name" "$dev1_off" "$painter_off" "$guard" || rc=$?
  fi

  echo
  case "$rc" in
    0)
      echo "GATE PASS — dev1<->${name} offset within ${guard} us; all-cambox window attribution is trustworthy."
      exit 0 ;;
    2)
      echo "!! GATE FAILED: dev1<->${name} clock offset exceeds the ${guard} us guard." >&2
      echo "!! All-cambox switch windows (dev1 clock) would misattribute frames vs the painted ticks (painter clock)." >&2
      echo "!! Re-sync DanteSync (dev1 + painter both slaved to strih), then re-run — or SKIP_CLOCK_OFFSET_ASSERT=1 to bypass." >&2
      exit 20 ;;
    *)
      echo "!! GATE INCOMPLETE: dev1 and/or ${name} DanteSync offset has no FRESH reading (#550/#595) — cannot certify attribution, NOT clean." >&2
      echo "!! Ensure dantesync is running + actively logging [NTP] offset on dev1 and ${name} (not a stale boot-step line), then re-run." >&2
      exit 11 ;;
  esac
}

# Source-guard: when sourced by the unit tests, expose the functions and stop (do not run main).
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

main "$@"
