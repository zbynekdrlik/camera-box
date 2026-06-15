#!/usr/bin/env bash
#
# clock-offset-guard.sh — cluster clock-offset regression guard (#8).
#
# The software genlock in src/ndi.rs aligns every camera's NDI send timecode to ABSOLUTE
# wall-clock frame boundaries (wait_for_next_boundary_100ns), which only yields a COMMON boundary
# across nodes if their wall clocks are synchronized (src/ndi.rs:25-35 states this verbatim). The
# cluster is disciplined by DanteSync (strih = master; NTP anchor + PTP fine servo — see SETUP.md
# "Cluster clock synchronization"). If a node's clock silently drifts past a fraction of the
# 16.7 ms (60 fps) frame period, multi-camera genlock degrades with NO error and cross-node
# latency becomes meaningless. This guard is the regression check #8 requires: it queries each
# REACHABLE node's DanteSync-reported absolute clock offset and FAILS LOUDLY (exit non-zero) if
# any node exceeds the documented bound, so drift cannot silently re-break genlock.
#
# Architecture mirrors scripts/drift-guard.sh: PURE functions (parse the offset from the two real
# DanteSync status formats, compare |offset| against the bound — unit-tested from
# tests/clock_offset_guard.rs by sourcing this file) and a flow that runs only when executed
# directly. The source-guard below (BASH_SOURCE != $0) lets the tests exercise the pure functions
# in isolation. Per the project Script Failure Policy + test-strictness: an UNREACHABLE node or an
# UNREADABLE offset is an explicit failure (DRIFT-status incomplete), NEVER a silent pass.
#
# The Linux cameras log DanteSync to journald:
#   Jun 15 09:11:53 CAM2 dantesync[3649]: [NTP] offset:+300us (threshold:520us, adaptive)
# The Windows OBS boxes (strih/stream) expose the same signal as JSON on the \\.\pipe\dantesync
# status pipe ("ntp_offset_us":1249). This script gathers the LINUX nodes over SSH (the part that
# runs unattended from dev1); the Windows offsets are gathered read-only via the win-* MCP tools
# (the offset_us_from_pipe_json parser here is the shared, unit-tested comparator for that path).
#
# Usage:
#   scripts/clock-offset-guard.sh [--bound-us N] [--targets "cam1=10.77.9.61 ..."]
#   scripts/clock-offset-guard.sh --help
#
# Exit codes: 0 = all reachable nodes within bound, 20 = DRIFT (a node exceeds the bound),
# 11 = at least one node UNREACHABLE / offset UNKNOWN (status incomplete — NOT clean),
# 1 = usage/IO error (e.g. no targets configured).

set -euo pipefail

# --- the documented bound -------------------------------------------------------------------
#
# DEFAULT_BOUND_US — the maximum tolerated ABSOLUTE clock offset, in microseconds.
#
# Rationale (tied to the measured baseline, per #8's acceptance criteria):
#   * The 60 fps frame period is 16.7 ms = 16667 µs. A clock offset of that magnitude would put a
#     camera one whole frame off the common genlock boundary; offsets of "tens-to-hundreds of ms"
#     (an unsynced clock — the exact failure mode #8 names) are 1-2 orders of magnitude past it.
#   * Observed steady-state offsets on the live DanteSync cluster (2026-06-15, read-only):
#       cam2  +281..+380 µs,  stream  +302 µs,  strih (master→GM)  +1249 µs.
#     The DanteSync daemon's own adaptive spike threshold sits ~520 µs on the cameras.
#   * 2000 µs (2 ms) is ~8x under the frame period (so genlock boundary divergence stays well
#     within a frame) yet comfortably above strih's legitimate ~1249 µs master-to-grandmaster
#     offset, so the guard does NOT false-positive on the healthy cluster while still catching the
#     tens-of-ms unsynced failure mode. Documented in SETUP.md alongside the baseline.
DEFAULT_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"

# CLOCK_GUARD_TARGETS — space-separated "name=ip" pairs of the LINUX nodes to query over SSH.
# Defaults to the four cameras (targets.md / CLAUDE.md IP table). strih + stream are Windows and
# are checked via the win-* MCP path (this guard's pipe-JSON parser), not over SSH.
CLOCK_GUARD_TARGETS="${CLOCK_GUARD_TARGETS-cam1=10.77.9.61 cam2=10.77.9.62 cam3=10.77.9.63 cam4=10.77.9.64}"

# SSH params for the read-only camera query (root/newlevel per CLAUDE.md / targets.md).
CLOCK_GUARD_SSH_USER="${CLOCK_GUARD_SSH_USER:-root}"
CLOCK_GUARD_SSH_PASS="${CLOCK_GUARD_SSH_PASS:-newlevel}"
CLOCK_GUARD_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"

# --- PURE functions (no network, no MCP — unit-tested) -------------------------------------

# offset_us_from_journal TEXT -> the SIGNED microsecond offset from the LAST DanteSync
# "[NTP] offset:+Nus" journald line ("" if none). The many "[PTP] ... Drift: Nns/s" lines are
# the fine servo's drift RATE, NOT the absolute offset, so they must be ignored. `tail -1` picks
# the most recent sample. A leading '+' is dropped (a negative '-' is kept) so the value is a
# plain signed integer offset_check can compare numerically. `|| true` keeps a no-match from
# tripping the caller's set -e.
offset_us_from_journal() {
  printf '%s\n' "$1" \
    | grep -oE '\[NTP\] offset:[+-]?[0-9]+us' \
    | sed -n 's/.*offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us/\1/p' \
    | tail -1 || true
}

# offset_us_from_pipe_json TEXT -> the SIGNED integer value of the JSON "ntp_offset_us" field
# ("" if absent). This is the Windows DanteSync status-pipe signal. It deliberately reads
# ntp_offset_us (the absolute NTP offset the master/boxes report), NOT offset_ns or
# accumulated_phase_us. The real status blob carries exactly one such field; if it ever carries
# more than one, the WORST (largest |offset|) is returned, never the first — a guard whose
# contract is "never silently pass an out-of-bound offset" must not let a later drifted value be
# masked by an earlier in-bound one. `|| true` survives a no-match under set -e.
offset_us_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_offset_us"[[:space:]]*:[[:space:]]*-?[0-9]+' \
    | sed -n 's/.*:[[:space:]]*\(-\{0,1\}[0-9][0-9]*\).*/\1/p' \
    | awk 'NR==1 || ($1<0?-$1:$1) > (m<0?-m:m) { m=$1 } END { if (NR) print m }' \
    || true
}

# abs_int N -> |N| (strips a leading '+' or '-'). Empty stays empty.
abs_int() {
  local n="${1#+}"
  printf '%s' "${n#-}"
}

# offset_check LABEL OFFSET_US BOUND_US -> prints a status line; returns 0 OK / 2 DRIFT /
# 3 UNKNOWN. OK iff |OFFSET_US| <= BOUND_US (NUMERIC compare). An empty OFFSET_US is UNKNOWN,
# never OK — an offset we could not read must never look in-bound (test-strictness: no silent
# pass). A non-numeric offset is also UNKNOWN (a malformed read is not a clean read).
offset_check() {
  local label="$1" offset="$2" bound="$3" mag
  if [ -z "$offset" ]; then
    printf '  %-14s UNKNOWN  (offset unread; bound %s us)\n' "$label" "$bound"
    return 3
  fi
  if ! printf '%s' "$offset" | grep -qE '^[+-]?[0-9]+$'; then
    printf '  %-14s UNKNOWN  (malformed offset %s; bound %s us)\n' "$label" "$offset" "$bound"
    return 3
  fi
  mag="$(abs_int "$offset")"
  if [ "$mag" -le "$bound" ]; then
    printf '  %-14s OK       (offset %s us, |%s| <= %s)\n' "$label" "$offset" "$mag" "$bound"
    return 0
  fi
  printf '  %-14s DRIFT    (offset %s us, |%s| > %s us bound)\n' "$label" "$offset" "$mag" "$bound"
  return 2
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------

usage() {
  cat <<EOF
clock-offset-guard.sh — cluster clock-offset regression guard (#8).

Queries each REACHABLE Linux node's DanteSync-reported absolute clock offset (the periodic
"[NTP] offset:+Nus" journald line) and FAILS LOUDLY if any node's |offset| exceeds the bound,
so clock drift cannot silently re-break the wall-clock genlock in src/ndi.rs.

Usage:
  scripts/clock-offset-guard.sh [--bound-us N] [--targets "name=ip name=ip ..."]
  scripts/clock-offset-guard.sh --help

Default bound: ${DEFAULT_BOUND_US} us (2 ms) — ~8x under the 16.7 ms 60 fps frame period yet
above the observed steady-state offsets (cam ~300 us, strih master ~1249 us). See SETUP.md
"Cluster clock synchronization" for the baseline + rationale.

Default targets (Linux cameras, over SSH): ${CLOCK_GUARD_TARGETS}
The Windows OBS boxes (strih/stream) are checked read-only via the win-* MCP tools using this
script's shared offset_us_from_pipe_json parser.

Exit codes: 0 = all reachable nodes within bound, 20 = DRIFT (a node exceeds the bound),
11 = a node UNREACHABLE / offset UNKNOWN (incomplete, NOT clean), 1 = usage/IO error.
EOF
}

# query_node_journal NAME IP -> echoes the node's latest DanteSync journald lines, or returns
# nonzero if the node is unreachable / the daemon has no output. Read-only (journalctl). Requires
# sshpass; an unreachable node is reported by the caller as UNKNOWN, never a silent pass.
query_node_journal() {
  local ip="$2"
  sshpass -p "$CLOCK_GUARD_SSH_PASS" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CLOCK_GUARD_SSH_TIMEOUT}" \
    "${CLOCK_GUARD_SSH_USER}@${ip}" \
    'journalctl -u dantesync --no-pager -n 200 2>/dev/null' 2>/dev/null
}

main() {
  local bound="$DEFAULT_BOUND_US" targets="$CLOCK_GUARD_TARGETS"
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us) shift; bound="${1:-}" ;;
      --targets)  shift; targets="${1:-}" ;;
      -h|--help)  usage; exit 0 ;;
      --*)        echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)          echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if ! printf '%s' "$bound" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
    exit 1
  fi

  # Normalise the target list and fail LOUDLY if it is empty — a guard that checks nothing must
  # never report "all clear" (test-strictness: a check that can't run fails, not passes). Split on
  # whitespace with globbing DISABLED (set -f): a target token containing a shell glob char
  # (`*`/`?`/`[`) must NOT be expanded against the cwd into a bogus node name.
  local -a pairs=()
  set -f
  # shellcheck disable=SC2206
  pairs=($targets)
  set +f
  if [ "${#pairs[@]}" -eq 0 ]; then
    echo "ERROR: no targets to check (CLOCK_GUARD_TARGETS / --targets is empty)." >&2
    echo "The clock-offset guard cannot certify the cluster with zero nodes — refusing to pass." >&2
    exit 1
  fi

  if ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to query the camera DanteSync offset over SSH." >&2
    exit 1
  fi

  echo "== clock-offset-guard (#8): bound ${bound} us (|offset| must stay within) =="
  echo "   master = strih (DanteSync NTP anchor + PTP servo); frame period @60fps = 16667 us"

  local drift=0 unknown=0 ok=0 rc pair name ip journal offset
  for pair in "${pairs[@]}"; do
    name="${pair%%=*}"; ip="${pair#*=}"
    journal="$(query_node_journal "$name" "$ip" || true)"
    if [ -z "$journal" ]; then
      printf '  %-14s UNREACHABLE  (no DanteSync journal over SSH @ %s)\n' "$name" "$ip"
      unknown=$((unknown + 1))
      continue
    fi
    offset="$(offset_us_from_journal "$journal")"
    rc=0
    offset_check "$name" "$offset" "$bound" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      2) drift=$((drift + 1)) ;;
      3) unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$drift" -gt 0 ]; then
    echo "!! CLOCK DRIFT: ${drift} node(s) exceed the ${bound} us offset bound." >&2
    echo "!! Genlock boundaries diverge by that offset — investigate DanteSync on the drifted node(s)." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further node(s) UNREACHABLE/UNKNOWN — status also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! ${unknown} node(s) UNREACHABLE or offset UNKNOWN — cluster status INCOMPLETE, NOT clean." >&2
    echo "!! Power up / fix DanteSync on those nodes, then re-run. (${ok} node(s) were within bound.)" >&2
    exit 11
  fi
  echo "ALL CLEAR — ${ok} node(s) within the ${bound} us offset bound. Genlock clock assumption holds."
  exit 0
}

main "$@"
