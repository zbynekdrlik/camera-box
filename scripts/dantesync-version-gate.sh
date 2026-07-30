#!/usr/bin/env bash
# dantesync-version-gate.sh — fleet-wide dantesync VERSION-PARITY precondition gate (#862).
# See the extended header comment below for the full rationale/usage; set -e up front per
# script-failure-policy.md.
set -euo pipefail
#
# WHY THIS GATE EXISTS (the user's hard, repeated requirement: "nechcem vidieť rozídené verzie" —
# a mixed/stale fleet must never be discoverable only by eye or by post-mortem of a failed run).
# The existing preconditions (dantesync-gate.sh's NTP+PTP check, #7; version-integrity-gate.sh's
# OBS/DistroAV/NDI pinned-set check, #123) both measure LIVE BEHAVIOUR of a daemon/build — neither
# ever checks the dantesync DAEMON's OWN VERSION. So a fleet running a pre-#53-burst-filter
# dantesync (#836/#851: a strictly WORSE measurement instrument — coin-flip individual NTP
# samples instead of a filtered median) passes both existing gates cleanly and can still silently
# corrupt every downstream cross-node latency/timestamp number this harness produces.
#
# This is a SEPARATE script from version-integrity-gate.sh (which is scoped to the Windows
# strih/stream OBS stack only) because dantesync ALSO runs on every Linux cam box AND on dev1
# itself — the control box that RUNS this very gate. dev1 is checked exactly like every other
# node (#862 point 2: the harness's own host is never exempt just because it is convenient).
#
# COMPARISON MODEL (#862 point 3): every node's observed version is compared against ONE PINNED
# expected version (DANTESYNC_VERSION_PIN) — NOT merely "do the boxes agree with each other". A
# fleet that uniformly agrees on a STALE version must still FAIL; this is a pin check, not a
# parity-between-peers check (contrast scripts/drift-guard.sh's genlock_build_sha CROSS-BOX
# parity, which genuinely has no fixed pin because every build's SHA is unique).
#
# UNAVAILABLE NODES (#862 point 4): an unread node NEVER silently passes. It is either read, or
# explicitly EXCLUDED via the SAME CAMBOX_OFFLINE_ACK / rig-fleet.txt mechanism
# scripts/lib/cambox-offline-ack.sh already provides for an offline cambox (#758/#827) — reused
# here verbatim, never a second exclusion mechanism invented for this gate.
#
# VERSION SOURCE: dantesync has NO embedded Windows VersionInfo resource (its build.rs sets none),
# so — unlike bundle-state-server.py's ndi_runtime_version Get-Item trick — the version cannot be
# read off the binary's file metadata. It IS logged once per (re)start, on every platform: Linux
# prints "DanteSync v<ver>" to stdout (captured by journald under systemd), Windows's --service
# mode prints "Service Started: v<ver>" to its own log file. This gate's Linux/local read scans
# `journalctl -u dantesync`; strih/stream expose the SAME value as a NEW `dantesync_version` key
# in their standing bundle-state (:8899) payload (bundle_state_gather.py/bundle-state-server.py,
# #862 point 1) — read here via version-integrity-gate.sh's own state_json_value (sourced below,
# never re-derived) over the SAME --win-state JSON file recording-e2e.sh already fetches for the
# version-integrity gate.
#
# Usage:
#   dantesync-version-gate.sh [--pin VERSION] [--fleet-file PATH] \
#       --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 imag-nb=newlevel@10.77.9.182" \
#       --local dev1 \
#       --win-state strih=/tmp/version-strih.json --win-state stream=/tmp/version-stream.json
#   dantesync-version-gate.sh --help
#
# Exit codes: 0 = every node matches the pin (or is knowingly excluded) — rig test may proceed,
#   20 = at least one node DRIFTED (version present but wrong) — REFUSED,
#   11 = at least one node UNKNOWN (version unread, not excluded) — INCOMPLETE, NOT clean,
#   1  = usage / environment error.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"
# shellcheck source=scripts/version-integrity-gate.sh
# Sourcing this pulls in ONLY the pure functions declared ABOVE its own BASH_SOURCE!=$0
# source-guard (state_json_value, compare_args_from_state, ...) — its guard returns before
# sourcing drift-guard.sh or running its own main(), exactly like this file's own guard below.
. "$HERE/version-integrity-gate.sh"

DEFAULT_FLEET_FILE="$HERE/../rig-fleet.txt"
# The fleet-wide pin (#862 point 3). Default = the version already uniform across cam1-4 at the
# time this gate was written (2026-07-29) — a deliberate, bump-on-purpose value in the SAME spirit
# as verify-device.sh's NDI_VERSION_PIN: whoever executes a dantesync fleet upgrade (#851) bumps
# this alongside the deploy, so the gate never silently drifts to "whatever happens to be out".
DANTESYNC_VERSION_PIN="${DANTESYNC_VERSION_PIN:-1.8.21}"

# --- PURE functions (no network, no SSH — unit-tested by sourcing this file) ------------------

# dantesync_version_from_log TEXT -> the FRESHEST ("DanteSync v<ver>" console/journal line, OR
# "Service Started: v<ver>" Windows-service-log line) dantesync version found in TEXT. The LAST
# match wins, never the first: a box upgraded + restarted more than once still carries its OLDER
# startup line further back in the journal/log, and grading that stale line as "the current
# version" is exactly the #851 hazard this gate exists to catch. "" when TEXT has no match
# (UNKNOWN downstream — unread/absent, never guessed).
dantesync_version_from_log() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -oE '(DanteSync|Service Started:) v[0-9]+\.[0-9]+\.[0-9]+' \
    | tail -1 \
    | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true
}

# dantesync_version_verdict NAME VERSION PIN -> prints ONE box->version table row and returns
# 0 OK / 20 DRIFT / 11 UNKNOWN. VERSION empty -> UNKNOWN (never a silent pass on an unread node).
# VERSION non-empty but != PIN -> DRIFT — this is a PIN compare, not a peer-agreement compare, so
# a uniformly-stale fleet fails exactly as loudly as a mixed one (#862 point 3).
dantesync_version_verdict() {
  local name="$1" version="$2" pin="$3"
  if [ -z "$version" ]; then
    printf '  %-14s %-12s UNKNOWN  (dantesync version not read)\n' "$name" "-"
    return 11
  fi
  if [ "$version" != "$pin" ]; then
    printf '  %-14s %-12s DRIFT    (expected %s)\n' "$name" "$version" "$pin"
    return 20
  fi
  printf '  %-14s %-12s OK\n' "$name" "$version"
  return 0
}

# dantesync_fleet_report PIN ENTRY... -> ENTRY is "name=version" (version may be empty — an
# unread node). Consults CAMBOX_OFFLINE_ACK (cambox_offline_ack_is_acked/_reason,
# scripts/lib/cambox-offline-ack.sh) for a knowingly-offline node: reported EXCLUDED with its ack
# reason, never counted as UNKNOWN/DRIFT and never a reason to fail the gate (#862 point 4). Prints
# the full box->version table (#862 point 5) and returns 0 (every non-excluded node OK) /
# 20 (>=1 DRIFT) / 11 (>=1 UNKNOWN, no DRIFT).
dantesync_fleet_report() {
  local pin="$1"
  shift
  echo "== dantesync-version-gate (#862): fleet-wide dantesync version parity — pin ${pin} =="
  local bad=0 unknown=0 ok=0 entry name version rc
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      printf '  %-14s %-12s EXCLUDED (acked offline: %s)\n' "$name" "${version:--}" \
        "$(cambox_offline_ack_reason "$name")"
      continue
    fi
    rc=0
    dantesync_version_verdict "$name" "$version" "$pin" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)) ;;
    esac
  done
  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} box(es) run a dantesync version other than the pinned ${pin} — rig test REFUSED." >&2
    echo "!! A drifted clock daemon can measure the offset worse (#851) and hide behind an otherwise-healthy run." >&2
    echo "!! Upgrade the box(es) named DRIFT above to ${pin}, re-verify, then re-run." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further box(es) also UNKNOWN — status also incomplete.)" >&2
    return 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN (dantesync version not read) — NOT clean." >&2
    echo "!! Every managed node must report its dantesync version before this gate is trusted. (${ok} OK.)" >&2
    return 11
  fi
  echo "GATE PASS — ${ok} box(es) on the pinned dantesync ${pin} (any acked-offline box excluded above)."
  return 0
}

# --- source-guard: when sourced (the unit tests), stop here -----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ----------------------------------------------------

usage() {
  cat <<EOF
dantesync-version-gate.sh — fleet-wide dantesync VERSION-PARITY precondition gate (#862).

Compares every managed node's dantesync version against ONE pinned expected version
(DANTESYNC_VERSION_PIN, default ${DANTESYNC_VERSION_PIN}). REFUSES (non-zero) on any drifted or
unread-and-unexcluded node, printing a box->version table.

Usage:
  dantesync-version-gate.sh [--pin VERSION] [--fleet-file PATH] \\
      --linux "name=user@ip ..." [--local name ...] [--win-state name=file ...]

Options:
  --pin VERSION     the fleet-pinned expected dantesync version (default \$DANTESYNC_VERSION_PIN).
  --fleet-file PATH default CAMBOX_OFFLINE_ACK source when the env var is unset (default:
                    ${DEFAULT_FLEET_FILE}) — same file recording-e2e.sh's fleet preflight reads.
  --linux "N=U@IP ..."  one or more SSH-reachable Linux nodes (space-separated "name=user@ip"
                    pairs in ONE argument, mirrors dantesync-gate.sh's --linux). Repeatable.
  --local NAME      a node read LOCALLY (no ssh) — dev1, the box running this gate. Repeatable.
  --win-state N=FILE  a Windows box (strih/stream) whose bundle-state JSON (dantesync_version key)
                    was already fetched to FILE. Repeatable. A box with no file is UNKNOWN.

Exit: 0 = every node on the pin (or excluded) — proceed. 20 = a node DRIFTED (REFUSED).
  11 = a node UNKNOWN/unread (INCOMPLETE, not clean). 1 = usage error.
EOF
}

# read_dantesync_journal NAME [TARGET] -> raw `journalctl -u dantesync` text for NAME. TARGET
# empty -> LOCAL read (dev1, the box running this gate — no ssh, mirrors
# clock-offset-painter-gate.sh's dev1-is-local convention). TARGET set ("user@ip") -> SSH read.
# "" if unreachable/absent (UNKNOWN downstream, never guessed). Overridable for tests/offline via
# DANTESYNC_VERSION_GATE_JOURNAL_<NAME> (NAME uppercased, "-" -> "_") — mirrors
# dantesync-gate.sh's DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seam.
read_dantesync_journal() {
  local name="$1" target="${2:-}" var
  var="DANTESYNC_VERSION_GATE_JOURNAL_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  if [ -z "$target" ]; then
    journalctl -u dantesync --no-pager -o cat 2>/dev/null || true
    return 0
  fi
  sshpass -p "${DANTESYNC_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${DANTESYNC_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    'journalctl -u dantesync --no-pager -o cat 2>/dev/null' 2>/dev/null || true
}

main() {
  local pin="$DANTESYNC_VERSION_PIN" fleet_file="$DEFAULT_FLEET_FILE"
  local -a linux_raw=() local_names=() win_state=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --pin) shift; pin="${1:-}" ;;
      --fleet-file) shift; fleet_file="${1:-}" ;;
      --linux) shift; linux_raw+=("${1:-}") ;;
      --local) shift; local_names+=("${1:-}") ;;
      --win-state) shift; win_state+=("${1:-}") ;;
      -h | --help)
        usage
        exit 0
        ;;
      --*)
        echo "unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
      *)
        echo "unexpected argument: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
    shift || true
  done

  local -a linux_pairs=()
  local raw
  set -f
  for raw in "${linux_raw[@]}"; do
    # shellcheck disable=SC2206
    linux_pairs+=($raw)
  done
  set +f

  if [ "${#linux_pairs[@]}" -eq 0 ] && [ "${#local_names[@]}" -eq 0 ] && [ "${#win_state[@]}" -eq 0 ]; then
    echo "ERROR: no node to gate (--linux, --local and --win-state are all empty)." >&2
    echo "The dantesync version-parity gate cannot certify the fleet with zero nodes — refusing to pass." >&2
    exit 1
  fi

  CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$fleet_file")"
  export CAMBOX_OFFLINE_ACK

  local -a entries=()
  local pair name target version file

  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(dantesync_version_from_log "$(read_dantesync_journal "$name" "$target")")"
    entries+=("${name}=${version}")
  done

  for name in "${local_names[@]}"; do
    [ -z "$name" ] && continue
    version="$(dantesync_version_from_log "$(read_dantesync_journal "$name" "")")"
    entries+=("${name}=${version}")
  done

  for pair in "${win_state[@]}"; do
    name="${pair%%=*}"
    file="${pair#*=}"
    version=""
    if [ -n "$file" ] && [ -s "$file" ]; then
      version="$(state_json_value "$file" dantesync_version)"
    fi
    entries+=("${name}=${version}")
  done

  local rc=0
  dantesync_fleet_report "$pin" "${entries[@]}" || rc=$?
  exit "$rc"
}

main "$@"
