#!/usr/bin/env bash
#
# dantesync-gate.sh — the recording-E2E precondition gate (#7): every measured node must be
# BOTH NTP-synced AND PTP-locked before the recording run is allowed to proceed.
#
# WHY THIS GATE EXISTS (the user's hard requirement): the recording-based 4-node E2E measures
# cross-node per-hop latency and aligns per-frame timestamps across cam1, cam2, strih and stream.
# Those numbers are ONLY meaningful when the cluster's FINE servo is the µs-grade PTP servo
# (grandmaster 10.77.9.184 up), NOT the ±1 ms NTP-stepping sawtooth fallback. If ANY node is on
# NTP-only, the latency/timestamps are garbage and the whole run is worthless. So this gate runs
# FIRST and FAILS FAST (non-zero, with a clear per-node diagnostic) if any node is not both
# NTP-within-bound AND PTP-locked — the run MUST NOT reach the recording step otherwise.
#
# It REUSES the unit-tested pure parsers in scripts/clock-offset-guard.sh (offset_us_from_journal,
# offset_us_from_pipe_json, offset_check, ptp_locked_from_journal, ptp_locked_from_pipe_json,
# ptp_check) — it does NOT reinvent any parsing. This script is the FLOW that gathers each node's
# DanteSync status and applies BOTH the offset check (NTP) and the PTP-lock check.
#
# NODE ACCESS (this rig):
#   * Linux cams (cam1, cam2): journald over SSH (root/newlevel) — gathered directly here via
#     read_linux_node_journal(), below. Overridable per-node for tests/offline via
#     DANTESYNC_GATE_LINUX_JOURNAL_<NAME> (NAME uppercased, e.g. DANTESYNC_GATE_LINUX_JOURNAL_CAM1)
#     -- the SAME "caller pre-fetches the status to a file" pattern
#     clock-offset-painter-gate.sh uses (DEV1_DANTE_JOURNAL/PAINTER_DANTE_JOURNAL, #608), keyed by
#     node name (like --win-status NAME=FILE below) since this gate can measure MULTIPLE Linux
#     nodes at once, unlike the painter gate's fixed dev1<->painter pair.
#   * Windows OBS boxes (strih, stream): ssh/scp is DENIED, so this script cannot read their
#     `\\.\pipe\dantesync` status itself. The caller (the autopilot worker / operator, who HAS
#     the win-* MCP) writes each box's status-pipe JSON to a local file and passes it via
#     --win-status NAME=FILE. A Windows node with NO status file is UNKNOWN -> the gate fails
#     (never a silent pass). recording-e2e.sh populates these files before invoking the gate.
#
# Usage:
#   dantesync-gate.sh [--bound-us N] \
#       [--linux "cam1=10.77.9.61 cam2=10.77.9.62"] \
#       [--win-status strih=/tmp/dante-strih.json] [--win-status stream=/tmp/dante-stream.json]
#   dantesync-gate.sh --help
#
# Exit codes: 0 = ALL measured nodes NTP-within-bound AND PTP-locked (run may proceed),
#   20 = at least one node DRIFTED (offset) or PTP-DEGRADED (NTP-only fallback),
#   11 = at least one node UNREACHABLE / status UNKNOWN (incomplete — NOT clean),
#   1  = usage / environment error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Source the shared, unit-tested DanteSync parsers (its BASH_SOURCE!=$0 guard skips its own flow).
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"
# shellcheck source=scripts/lib/win-status-args.sh
. "$HERE/lib/win-status-args.sh"

GATE_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
# The four measured nodes by default: the two Linux cams over SSH; strih/stream need --win-status.
GATE_LINUX="${GATE_LINUX:-cam1=10.77.9.61 cam2=10.77.9.62}"
GATE_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"
# #550/#591/#595: a Linux node's freshest "[NTP] offset:" journal line must be no older than this
# many seconds behind its newest journal line, or the reading is STALE and must never be graded as
# the current offset (the #550 false-fail/false-pass bug this gate was still exposed to before
# #595 -- see dantesync_offset_verdict in clock-offset-guard.sh). Same default as
# verify-device.sh's DANTESYNC_OFFSET_FRESHNESS_S.
GATE_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"

# read_linux_node_journal NAME IP -> that Linux node's latest DanteSync journald lines over SSH,
# or "" if unreachable. Overridable for tests/offline via DANTESYNC_GATE_LINUX_JOURNAL_<NAME>
# (file path; NAME uppercased AND any "-" mapped to "_" so a hyphenated node name like "imag-nb"
# still yields a valid shell variable name, e.g. cam1 -> DANTESYNC_GATE_LINUX_JOURNAL_CAM1,
# imag-nb -> DANTESYNC_GATE_LINUX_JOURNAL_IMAG_NB) -- mirrors clock-offset-painter-gate.sh's
# read_painter_journal()/DEV1_DANTE_JOURNAL pattern (#608), so this gate's Linux SSH-gather path
# can be proven end-to-end offline instead of only indirectly via the shared
# dantesync_offset_verdict unit tests. Read-only; a down/absent daemon (or an unset override)
# collapses to empty output (caller maps empty -> UNKNOWN, never a silent pass).
read_linux_node_journal() {
  local name="$1" ip="$2" var
  var="DANTESYNC_GATE_LINUX_JOURNAL_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  # -o short-iso (+ a wider -n 400 window) so dantesync_offset_verdict can prove freshness
  # (#550/#595) -- the age-blind offset_us_from_journal/offset_check this loop used to call could
  # grade a stale multi-hour-old boot-STEP line as "the current offset".
  sshpass -p "${CLOCK_GUARD_SSH_PASS}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no -o "ConnectTimeout=${GATE_SSH_TIMEOUT}" \
    "${CLOCK_GUARD_SSH_USER}@${ip}" \
    'journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null' 2>/dev/null || true
}

usage() {
  cat <<EOF
dantesync-gate.sh — recording-E2E NTP+PTP precondition gate (#7).

FAILS FAST unless EVERY measured node is BOTH NTP-synced (|offset| <= bound) AND PTP-locked
(fine servo NANO/LOCK, not the NTP-only sawtooth fallback with GM 10.77.9.184 down). The
recording run must NOT proceed otherwise — cross-node latency/timestamps would be meaningless.

Usage:
  dantesync-gate.sh [--bound-us N] [--linux "name=ip ..."] [--win-status NAME=FILE ...]

Options:
  --bound-us N        max tolerated |NTP offset| in us (default ${GATE_BOUND_US}; see #8 rationale).
  --linux "n=ip ..."  Linux nodes queried via journald over SSH (default: ${GATE_LINUX}).
  --win-status N=FILE  a Windows node N whose DanteSync status-pipe JSON the caller wrote to FILE
                       (ssh to Windows is denied; the win-* MCP holder pre-fetches it). Repeatable.

A Linux node's NTP offset must be FRESH, not just in-bound: the freshest "[NTP] offset:" journal
line must be no older than DANTESYNC_OFFSET_FRESHNESS_S (default ${GATE_OFFSET_FRESHNESS_S}) seconds
behind that node's newest journal line, or its offset is STALE -> UNKNOWN (never a silent OK) --
see dantesync_offset_verdict() in clock-offset-guard.sh (#550/#591/#595).

Exit: 0 = all nodes NTP+PTP OK, 20 = a node DRIFTED or PTP-DEGRADED, 11 = a node UNREACHABLE/
UNKNOWN, 1 = usage error.
EOF
}

main() {
  local bound="$GATE_BOUND_US" linux="$GATE_LINUX"
  local -a win_status=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us)   shift; bound="${1:-}" ;;
      --linux)      shift; linux="${1:-}" ;;
      --win-status) shift; win_status+=("${1:-}") ;;
      -h|--help)    usage; exit 0 ;;
      --*)          echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)            echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if ! printf '%s' "$bound" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
    exit 1
  fi
  if ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to query the Linux cam DanteSync over SSH." >&2
    exit 1
  fi

  local -a linux_pairs=()
  set -f
  # shellcheck disable=SC2206
  linux_pairs=($linux)
  set +f
  if [ "${#linux_pairs[@]}" -eq 0 ] && [ "${#win_status[@]}" -eq 0 ]; then
    echo "ERROR: no nodes to gate (both --linux and --win-status are empty)." >&2
    echo "The recording-E2E gate cannot certify the cluster with zero nodes — refusing to pass." >&2
    exit 1
  fi

  echo "== dantesync-gate (#7): recording-E2E precondition — NTP within ${bound} us AND PTP LOCKED =="
  echo "   GM = 10.77.9.184 (PTP grandmaster); NTP master = strih; degraded PTP => meaningless latency"

  local bad=0 unknown=0 ok=0 name ip status offset rc_off rc_ptp ptp

  # --- Linux nodes (journald over SSH) -----------------------------------------------------
  local pair
  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"; ip="${pair#*=}"
    # read_linux_node_journal (#608) is the SSH-gather seam: live over SSH, or a fixture file via
    # DANTESYNC_GATE_LINUX_JOURNAL_<NAME> for tests/offline runs.
    status="$(read_linux_node_journal "$name" "$ip")"
    if [ -z "$status" ]; then
      printf '  %-14s UNREACHABLE  (no DanteSync journal over SSH @ %s)\n' "$name" "$ip"
      unknown=$((unknown + 1)); continue
    fi
    ptp="$(ptp_locked_from_journal "$status")"
    rc_off=0
    case "$(dantesync_offset_verdict "$status" "$GATE_OFFSET_FRESHNESS_S" "$bound")" in
      ok)
        printf '  %-14s NTP OK       (fresh offset within %s us bound)\n' "$name" "$bound" ;;
      drift)
        printf '  %-14s NTP DRIFT    (fresh offset exceeds %s us bound)\n' "$name" "$bound"
        rc_off=2 ;;
      stale)
        printf '  %-14s NTP STALE    (no FRESH [NTP] offset within %ss -- status incomplete, #550/#595)\n' \
          "$name" "$GATE_OFFSET_FRESHNESS_S"
        rc_off=3 ;;
      *)
        printf '  %-14s NTP UNKNOWN  (no [NTP] offset line at all -- status incomplete)\n' "$name"
        rc_off=3 ;;
    esac
    rc_ptp=0; ptp_check "$name" "$ptp" || rc_ptp=$?
    case "$(node_verdict "$rc_off" "$rc_ptp")" in
      OK) ok=$((ok + 1)) ;; BAD) bad=$((bad + 1)) ;; UNKNOWN) unknown=$((unknown + 1)) ;;
    esac
  done

  # --- Windows nodes (status-pipe JSON the caller pre-fetched via MCP) ----------------------
  # NOTE (#595 scope): the Windows status-pipe JSON blob carries no journal timestamp, so it has
  # no freshness signal to check the way the Linux path above now does -- this snapshot path is
  # left AGE-BLIND (offset_us_from_pipe_json) here. Tracked as part of #598 (Windows W32Time
  # verify-gate for strih+stream), not a new issue.
  local entry
  for entry in "${win_status[@]}"; do
    if ! win_status_parse_entry "$entry"; then
      unknown=$((unknown + 1)); continue
    fi
    name="$WIN_STATUS_NAME"; status="$WIN_STATUS_TEXT"
    offset="$(offset_us_from_pipe_json "$status")"
    ptp="$(ptp_locked_from_pipe_json "$status")"
    rc_off=0; offset_check "$name" "$offset" "$bound" || rc_off=$?
    rc_ptp=0; ptp_check    "$name" "$ptp"             || rc_ptp=$?
    case "$(node_verdict "$rc_off" "$rc_ptp")" in
      OK) ok=$((ok + 1)) ;; BAD) bad=$((bad + 1)) ;; UNKNOWN) unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} node(s) DRIFTED or PTP-DEGRADED." >&2
    echo "!! Cross-node latency/timestamps would be MEANINGLESS — recording run REFUSED." >&2
    echo "!! Bring GM 10.77.9.184 up + let DanteSync re-lock (NANO/LOCK), then re-run." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further node(s) UNREACHABLE/UNKNOWN — also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} node(s) UNREACHABLE or status UNKNOWN — NOT clean." >&2
    echo "!! Every measured node must report NTP+PTP before recording. (${ok} node(s) were OK.)" >&2
    exit 11
  fi
  echo "GATE PASS — ${ok} node(s) NTP-synced AND PTP-locked. Cross-node latency is meaningful; proceed."
  exit 0
}

# node_verdict OFFSET_RC PTP_RC -> OK | BAD | UNKNOWN. A node passes ONLY when BOTH the offset
# check (rc 0) AND the PTP-lock check (rc 0) pass. A DRIFT/DEGRADED (rc 2) on either => BAD. Any
# UNKNOWN (rc 3) with no hard failure => UNKNOWN. (Hard failure dominates UNKNOWN so a degraded
# node is reported as the actionable failure, not masked as merely "unknown".)
node_verdict() {
  local off="$1" ptp="$2"
  if [ "$off" = 2 ] || [ "$ptp" = 2 ]; then printf 'BAD'; return 0; fi
  if [ "$off" = 3 ] || [ "$ptp" = 3 ]; then printf 'UNKNOWN'; return 0; fi
  printf 'OK'
}

# Source-guard: when sourced by the unit tests, expose the functions and stop (do not run main).
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

main "$@"
