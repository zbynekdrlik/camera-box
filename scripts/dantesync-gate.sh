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
#     node name, since this gate can measure MULTIPLE Linux nodes at once, unlike the painter
#     gate's fixed dev1<->painter pair.
#   * Windows OBS boxes (strih, stream): queried LIVE over HTTP from dantesync#47's own network
#     status endpoint (http://HOST:PORT/status, #648) via --win-http, below — no win-* MCP, no
#     human/agent pre-fetch, unattended-CI-safe, and it grades freshness from the payload's own
#     "updated_ts" field. (#835: the PRIOR flow — a human/agent with the win-* MCP writing each
#     box's `\\.\pipe\dantesync` status-pipe JSON to a local file, passed via --win-status
#     NAME=FILE — was REMOVED outright, not merely deprecated: it had zero live callers left
#     in this repo once recording-e2e.sh switched to --win-http, and it was deliberately
#     AGE-BLIND with no way to detect a stale/leftover file — exactly the false-GREEN hazard a
#     stale runbook could walk an operator into. --win-http covers the same two nodes strictly
#     better. See `scripts/lib/win-status-args.sh` if you need the shared NAME=FILE parser for a
#     DIFFERENT gate — `scripts/w32time-gate.sh` still legitimately uses it for the W32Time
#     service-state invariant, which has no HTTP equivalent to migrate to.)
#
# Usage:
#   dantesync-gate.sh [--bound-us N] \
#       [--linux "cam1=10.77.9.61 cam2=10.77.9.62"] \
#       [--win-http strih=10.77.9.202] [--win-http stream=10.77.9.204]
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

GATE_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
# The four measured nodes by default: the two Linux cams over SSH; strih/stream need --win-http.
GATE_LINUX="${GATE_LINUX:-cam1=10.77.9.61 cam2=10.77.9.62}"
GATE_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"
# #550/#591/#595: a Linux node's freshest "[NTP] offset:" journal line must be no older than this
# many seconds behind its newest journal line, or the reading is STALE and must never be graded as
# the current offset (the #550 false-fail/false-pass bug this gate was still exposed to before
# #595 -- see dantesync_offset_verdict in clock-offset-guard.sh). Same default as
# verify-device.sh's DANTESYNC_OFFSET_FRESHNESS_S.
GATE_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"
# #648: the port dantesync#47's network status endpoint listens on (http://<box>:PORT/status),
# shared across all --win-http nodes (same convention as --bound-us: one value, all nodes) --
# same default/env-var name recording-e2e.sh already used for its own (now-removed) pre-fetch.
GATE_WIN_HTTP_PORT="${WIN_DANTE_PORT:-8898}"
GATE_WIN_HTTP_TIMEOUT="${CLOCK_GUARD_HTTP_TIMEOUT:-10}"

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

# read_linux_node_http_status NAME IP -> that Linux camera's LIVE DanteSync status JSON, fetched
# from dantesync#47's own network endpoint (http://IP:GATE_WIN_HTTP_PORT/status, #648) -- the SAME
# authoritative signal read_win_http_status already reads for the Windows boxes, deployed
# fleet-wide and verified responding on cam1-cam6, 2026-07-11.
#
# #686: this is now the gate's PRIMARY signal for a Linux node (tried BEFORE the journal parser).
# Regression: after #679 throttled DanteSync's periodic journal reports (~1-in-30), the servo
# "[PTP] (NANO|LOCK) Drift:" lines and the "[NTP] offset:" lines land at nearly the SAME cadence,
# so ptp_locked_from_journal's "whichever logged last wins" POSITION comparison can flip a
# genuinely LOCKED node to DEGRADED with roughly coin-flip odds -- observed live, cam2 flip-
# flopped LOCKED->DEGRADED->LOCKED across three gate runs within 20 minutes while its own
# :8898/status continuously reported is_locked:true, mode NANO/LOCK. HTTP carries the daemon's
# own CURRENT is_locked/mode/ntp_offset_us/updated_ts directly -- no log-cadence ambiguity.
#
# "" if unreachable / non-200 (curl -fsS fails closed on either) -- the caller then FALLS BACK to
# read_linux_node_journal. That fallback is ONLY for the HTTP endpoint being unreachable/disabled
# -- never a second opinion once HTTP has answered (a reachable-but-STALE HTTP payload must fail
# the gate, not silently fall through to a possibly-misleading journal read).
#
# Overridable for tests/offline via DANTESYNC_GATE_LINUX_HTTP_<NAME> (NAME uppercased, "-" -> "_")
# -- mirrors read_win_http_status's DANTESYNC_GATE_WIN_HTTP_<NAME> and read_linux_node_journal's
# DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seams above.
read_linux_node_http_status() {
  local name="$1" ip="$2" var
  var="DANTESYNC_GATE_LINUX_HTTP_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  curl -fsS --max-time "$GATE_WIN_HTTP_TIMEOUT" "http://${ip}:${GATE_WIN_HTTP_PORT}/status" 2>/dev/null || true
}

# read_win_http_status NAME HOST -> that Windows box's LIVE DanteSync status JSON, fetched
# directly from dantesync#47's own network endpoint (http://HOST:GATE_WIN_HTTP_PORT/status,
# #648) — no win-* MCP, no human pre-fetch, so an unattended CI run has a real data source.
# "" if unreachable / non-200 (curl -fsS fails closed on either). Overridable for tests/offline
# via DANTESYNC_GATE_WIN_HTTP_<NAME> (NAME uppercased, "-" -> "_") -- mirrors
# read_linux_node_journal's DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seam above.
read_win_http_status() {
  local name="$1" host="$2" var
  var="DANTESYNC_GATE_WIN_HTTP_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  curl -fsS --max-time "$GATE_WIN_HTTP_TIMEOUT" "http://${host}:${GATE_WIN_HTTP_PORT}/status" 2>/dev/null || true
}

usage() {
  cat <<EOF
dantesync-gate.sh — recording-E2E NTP+PTP precondition gate (#7).

FAILS FAST unless EVERY measured node is BOTH NTP-synced (|offset| <= bound) AND PTP-locked
(fine servo NANO/LOCK, not the NTP-only sawtooth fallback with GM 10.77.9.184 down). The
recording run must NOT proceed otherwise — cross-node latency/timestamps would be meaningless.

Usage:
  dantesync-gate.sh [--bound-us N] [--linux "name=ip ..."] \
                     [--win-http NAME=HOST ...] [--win-http-port N]

Options:
  --bound-us N        max tolerated |NTP offset| in us (default ${GATE_BOUND_US}; see #8 rationale).
  --linux "n=ip ..."  Linux nodes -- HTTP status endpoint FIRST, journald-over-SSH fallback
                      (#686; default: ${GATE_LINUX}).
  --win-http N=HOST    a Windows node N queried LIVE over HTTP from dantesync#47's own network
                       status endpoint (http://HOST:PORT/status, #648) -- no win-* MCP, no human
                       pre-fetch; unattended-CI-safe. Repeatable.
  --win-http-port N    port for the HTTP status endpoint (default ${GATE_WIN_HTTP_PORT}) -- shared
                       by --win-http nodes AND the --linux nodes' HTTP-first reads (#686); the
                       whole fleet serves one port, so one knob covers both.

#686: LINUX nodes now try the SAME network status endpoint (http://IP:PORT/status) FIRST --
authoritative, immune to journal log-cadence throttling (#679). The journal parser is the
FALLBACK, used ONLY when a Linux node's HTTP endpoint is unreachable/disabled -- never a second
opinion once HTTP has answered (a reachable-but-stale HTTP payload fails the gate, it does not
fall through to the journal).

A Linux node's NTP offset must be FRESH, not just in-bound. Via HTTP (the primary path, #686):
its "updated_ts" field must be no older than DANTESYNC_OFFSET_FRESHNESS_S seconds behind the
gate's own wall clock, exactly like a --win-http node (#648). Via the journal (the fallback
path): the freshest "[NTP] offset:" journal line must be no older than
DANTESYNC_OFFSET_FRESHNESS_S (default ${GATE_OFFSET_FRESHNESS_S}) seconds behind that node's
newest journal line -- see dantesync_offset_verdict() in clock-offset-guard.sh (#550/#591/#595).
Either way a stale reading is STALE -> UNKNOWN, never a silent OK. (#835: the old Windows-node
FILE-RELAY path, which WAS age-blind, was removed outright -- --win-http covers the same nodes
and always grades freshness.)

Exit: 0 = all nodes NTP+PTP OK, 20 = a node DRIFTED or PTP-DEGRADED, 11 = a node UNREACHABLE/
UNKNOWN, 1 = usage error.
EOF
}

main() {
  local bound="$GATE_BOUND_US" linux="$GATE_LINUX" win_http_port="$GATE_WIN_HTTP_PORT"
  local -a win_http=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us)      shift; bound="${1:-}" ;;
      --linux)         shift; linux="${1:-}" ;;
      --win-http)      shift; win_http+=("${1:-}") ;;
      --win-http-port) shift; win_http_port="${1:-}" ;;
      -h|--help)       usage; exit 0 ;;
      --*)             echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)               echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if ! printf '%s' "$bound" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
    exit 1
  fi
  if ! printf '%s' "$win_http_port" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --win-http-port must be a positive integer (got '${win_http_port}')." >&2
    exit 1
  fi
  GATE_WIN_HTTP_PORT="$win_http_port"
  if ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to query the Linux cam DanteSync over SSH." >&2
    exit 1
  fi

  local -a linux_pairs=()
  set -f
  # shellcheck disable=SC2206
  linux_pairs=($linux)
  set +f
  # #686: Linux nodes now try the network status endpoint FIRST too (read_linux_node_http_status),
  # not just --win-http nodes -- curl is required whenever EITHER array is non-empty.
  if { [ "${#linux_pairs[@]}" -gt 0 ] || [ "${#win_http[@]}" -gt 0 ]; } && ! command -v curl >/dev/null 2>&1; then
    echo "ERROR: curl not found — required to query DanteSync status over HTTP (#648/#686)." >&2
    exit 1
  fi
  if [ "${#linux_pairs[@]}" -eq 0 ] && [ "${#win_http[@]}" -eq 0 ]; then
    echo "ERROR: no nodes to gate (--linux and --win-http are both empty)." >&2
    echo "The recording-E2E gate cannot certify the cluster with zero nodes — refusing to pass." >&2
    exit 1
  fi

  echo "== dantesync-gate (#7): recording-E2E precondition — NTP within ${bound} us AND PTP LOCKED =="
  echo "   GM = 10.77.9.184 (PTP grandmaster); NTP master = strih; degraded PTP => meaningless latency"

  local bad=0 unknown=0 ok=0 name ip status offset rc_off rc_ptp ptp
  local now
  now="$(date +%s)"

  # --- Linux nodes (HTTP status endpoint FIRST -- #686; journald over SSH as FALLBACK) ------
  local pair
  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"; ip="${pair#*=}"
    # #686: try the network status endpoint FIRST -- authoritative, immune to the #679
    # journal-throttle false-DEGRADE. read_linux_node_journal (#608) remains the FALLBACK for a
    # node whose HTTP endpoint is unreachable/disabled -- never consulted once HTTP answers.
    status="$(read_linux_node_http_status "$name" "$ip")"
    if [ -n "$status" ]; then
      rc_off=0
      case "$(pipe_json_freshness_verdict "$status" "$now" "$GATE_OFFSET_FRESHNESS_S")" in
        stale)
          printf '  %-14s NTP STALE    (updated_ts older than %ss -- status incomplete, #686)\n' \
            "$name" "$GATE_OFFSET_FRESHNESS_S"
          rc_off=3 ;;
        absent)
          printf '  %-14s NTP UNKNOWN  (no updated_ts field -- status incomplete, #686)\n' "$name"
          rc_off=3 ;;
        *)
          offset="$(offset_us_from_pipe_json "$status")"
          offset_check "$name" "$offset" "$bound" || rc_off=$? ;;
      esac
      ptp="$(ptp_locked_from_pipe_json "$status")"
      rc_ptp=0; ptp_check "$name" "$ptp" || rc_ptp=$?
      case "$(node_verdict "$rc_off" "$rc_ptp")" in
        OK) ok=$((ok + 1)) ;; BAD) bad=$((bad + 1)) ;; UNKNOWN) unknown=$((unknown + 1)) ;;
      esac
      continue
    fi
    # HTTP unreachable/disabled -> FALLBACK to the journal parser (unchanged pre-#686 path).
    # read_linux_node_journal (#608) is the SSH-gather seam: live over SSH, or a fixture file via
    # DANTESYNC_GATE_LINUX_JOURNAL_<NAME> for tests/offline runs.
    status="$(read_linux_node_journal "$name" "$ip")"
    if [ -z "$status" ]; then
      printf '  %-14s UNREACHABLE  (no DanteSync HTTP @ %s:%s/status nor journal over SSH)\n' \
        "$name" "$ip" "$GATE_WIN_HTTP_PORT"
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

  # --- Windows nodes fetched LIVE over HTTP (dantesync#47's network status endpoint, #648) ----
  # No win-* MCP, no human pre-fetch -- the whole point of #648 is an unattended CI run has
  # neither. The endpoint's JSON carries "updated_ts" (unix epoch seconds), so this path CAN and
  # DOES grade freshness: a box whose dantesync died but whose HTTP server keeps serving a cached
  # snapshot must be caught, never silently graded on stale numbers. `$now` was already captured
  # above (shared with the #686 Linux HTTP freshness check). (#835: this is now the ONLY Windows-
  # node input path -- the old --win-status file-relay path, which was age-blind, was removed.)
  local entry
  for entry in "${win_http[@]}"; do
    name="${entry%%=*}"; ip="${entry#*=}"
    status="$(read_win_http_status "$name" "$ip")"
    if [ -z "$status" ]; then
      printf '  %-14s UNREACHABLE  (no DanteSync HTTP status @ %s:%s/status -- #648)\n' \
        "$name" "$ip" "$GATE_WIN_HTTP_PORT"
      unknown=$((unknown + 1)); continue
    fi
    rc_off=0
    case "$(pipe_json_freshness_verdict "$status" "$now" "$GATE_OFFSET_FRESHNESS_S")" in
      stale)
        printf '  %-14s NTP STALE    (updated_ts older than %ss -- status incomplete, #648)\n' \
          "$name" "$GATE_OFFSET_FRESHNESS_S"
        rc_off=3 ;;
      absent)
        printf '  %-14s NTP UNKNOWN  (no updated_ts field -- status incomplete, #648)\n' "$name"
        rc_off=3 ;;
      *)
        offset="$(offset_us_from_pipe_json "$status")"
        offset_check "$name" "$offset" "$bound" || rc_off=$? ;;
    esac
    ptp="$(ptp_locked_from_pipe_json "$status")"
    rc_ptp=0; ptp_check "$name" "$ptp" || rc_ptp=$?
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
