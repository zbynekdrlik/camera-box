#!/usr/bin/env bash
# camera-box-version-gate.sh — cross-box camera-box BINARY version-parity precondition gate (#875).
# See the extended header below for the full rationale/usage; set -e up front per
# script-failure-policy.md.
set -euo pipefail
#
# WHY THIS GATE EXISTS (issue 875, a deliberate follow-up split from issue 862). The camera-box
# binary is deployed CONTINUOUSLY (`1.7.0-dev.NNN` grows on almost every PR), so a box can silently
# fall behind and run objectively different behaviour than the rest of the fleet with nothing
# shouting about it — live 2026-07-29 cam4 ran three builds behind cam1/2/3 and was the ONLY box
# missing the publish-30p fix, found only by hand. The existing preconditions do not catch this:
# dantesync-version-gate.sh (issue 862) checks the dantesync DAEMON version, version-integrity-gate.sh
# (#123) checks the Windows OBS stack — neither ever reads the camera-box app binary's own version.
#
# COMPARISON MODEL — RELATIVE CROSS-BOX PARITY, NO FIXED PIN. This is the SECOND of the two models
# .claude/rules/dantesync-version-reading.md documents, and the OPPOSITE of issue 862's gate: a
# continuously-deployed dev build has no single stable value to pin against (pinning would need a
# bump on every deploy and would spuriously fail the whole fleet on a stale pin). The only checkable
# invariant is that every ACTIVE box AGREES with every other — exactly the model scripts/drift-guard.sh's
# genlock_build_sha parity engine already uses (no pin, because every build's value is unique). Any
# disagreement between active boxes = FAIL. Unlike genlock (per-path git equivalence), the version
# string is atomic, so this is a plain exact-string peer compare.
#
# WHICH NODES. camera-box runs ONLY on the Linux cam boxes (the NDI senders) — NOT on imag-nb / dev1 /
# strih / stream. So this gate only takes --linux nodes, and the caller derives the active fleet from
# CAMERA_ACTIVE_SET (never a literal cam range — see .claude/rules/camera-active-set.md).
#
# UNAVAILABLE NODES. An unread node NEVER silently passes: it is either read, or explicitly EXCLUDED
# via the SAME CAMBOX_OFFLINE_ACK / rig-fleet.txt mechanism scripts/lib/cambox-offline-ack.sh already
# provides (#758/#827), reused verbatim exactly as issue 862's gate does — never a second mechanism.
#
# VERSION SOURCE. `/usr/local/bin/camera-box --version` prints `camera-box X.Y.Z-dev.NNN` and is read
# over the SAME SSH transport (absolute path, NOT relied on via the remote PATH — mirrors
# scripts/deploy-fleet.sh's own per-box version read).
#
# Usage:
#   camera-box-version-gate.sh [--fleet-file PATH] \
#       --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 cam3=root@10.77.9.63"
#   camera-box-version-gate.sh --help
#
# Exit codes: 0 = every active box agrees on ONE camera-box version (or is knowingly excluded) —
#   rig test may proceed, 20 = active boxes DISAGREE (a box drifted) — REFUSED,
#   11 = at least one box UNKNOWN (version unread, not excluded) — INCOMPLETE, NOT clean,
#   1  = usage / environment error.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"

DEFAULT_FLEET_FILE="$HERE/../rig-fleet.txt"

# --- PURE functions (no network, no SSH — unit-tested by sourcing this file) ------------------

# camera_box_version_from_version_output TEXT -> the camera-box version found in TEXT (the raw
# stdout of `camera-box --version`, i.e. "camera-box X.Y.Z-dev.NNN"). The LAST match wins — purely
# defensive robustness against an SSH banner/MOTD ever landing ahead of the real line, mirroring
# dantesync_version_from_version_output. "" when TEXT has no match (UNKNOWN downstream — an
# unreachable/unread box, never guessed).
camera_box_version_from_version_output() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -oE 'camera-box [0-9]+\.[0-9]+\.[0-9]+(-[.A-Za-z0-9]+)?' \
    | tail -1 \
    | sed 's/^camera-box //' || true
}

# camera_box_modal_version LIST_NEWLINE -> the MOST FREQUENT version among the non-empty lines in
# LIST_NEWLINE (the relative-parity REFERENCE the per-box verdict compares each box against), "" if
# none. Ties are broken by the lexicographically-SMALLEST version so the result is deterministic —
# the tie-break is presentational only (any disagreement fails the gate regardless of which value is
# named the "majority"). Empty lines (unread boxes) are ignored; they are graded UNKNOWN, never DRIFT.
camera_box_modal_version() {
  local list="$1"
  printf '%s\n' "$list" | sed '/^$/d' | sort | uniq -c \
    | sort -k1,1nr -k2,2 | awk 'NR == 1 { print $2; exit }'
}

# camera_box_version_verdict NAME VERSION MODAL -> prints ONE box->version table row and returns
# 0 OK / 20 DRIFT / 11 UNKNOWN. VERSION empty -> UNKNOWN (never a silent pass on an unread box).
# VERSION non-empty but != MODAL (the fleet majority) -> DRIFT. This is a RELATIVE peer compare, not
# a pin compare (issue 875): MODAL is derived from the fleet itself, there is no fixed expected value.
camera_box_version_verdict() {
  local name="$1" version="$2" modal="$3"
  if [ -z "$version" ]; then
    printf '  %-14s %-16s UNKNOWN  (camera-box version not read)\n' "$name" "-"
    return 11
  fi
  if [ "$version" != "$modal" ]; then
    printf '  %-14s %-16s DRIFT    (peers run %s)\n' "$name" "$version" "$modal"
    return 20
  fi
  printf '  %-14s %-16s OK\n' "$name" "$version"
  return 0
}

# camera_box_fleet_report ENTRY... -> ENTRY is "name=version" (version may be empty — an unread box).
# Consults CAMBOX_OFFLINE_ACK (cambox_offline_ack_is_acked/_reason, scripts/lib/cambox-offline-ack.sh)
# for a knowingly-offline box: reported EXCLUDED with its ack reason, never counted as UNKNOWN/DRIFT
# and never a reason to fail the gate. Computes the fleet's modal version from the READ (non-excluded,
# non-empty) boxes, prints the full box->version table, and returns 0 (every active box agrees) /
# 20 (>=1 box DRIFTED from the majority — the active fleet disagrees) / 11 (>=1 box UNKNOWN, no DRIFT).
camera_box_fleet_report() {
  local entry name version
  echo "== camera-box-version-gate (#875): cross-box camera-box binary version parity — relative, no pin =="
  # First pass: collect the READ (non-excluded, non-empty) versions to compute the majority reference.
  local read_versions=""
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    cambox_offline_ack_is_acked "$name" && continue
    [ -n "$version" ] || continue
    read_versions="${read_versions}${version}"$'\n'
  done
  local modal
  modal="$(camera_box_modal_version "$read_versions")"
  # Second pass: print the table + roll up the verdict.
  local bad=0 unknown=0 ok=0 rc
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      printf '  %-14s %-16s EXCLUDED (acked offline: %s)\n' "$name" "${version:--}" \
        "$(cambox_offline_ack_reason "$name")"
      continue
    fi
    rc=0
    camera_box_version_verdict "$name" "$version" "$modal" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)) ;;
    esac
  done
  echo
  local distinct
  distinct="$(printf '%s' "$read_versions" | sed '/^$/d' | sort -u | tr '\n' ' ')"
  # Name BOTH the DRIFT and UNKNOWN counts in the SAME banner (the issue-862 follow-up lesson): a
  # fleet that is mostly UNREADABLE must never read as a single minor drift.
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: active boxes run DIFFERENT camera-box versions — seen: ${distinct}(peer reference ${modal}); ${unknown} box(es) UNKNOWN — rig test REFUSED." >&2
    echo "!! A box on a different camera-box build runs objectively different behaviour than the rest of the fleet (issue 875 — cam4 once silently lacked the publish-30p fix)." >&2
    echo "!! Redeploy every active box to the SAME camera-box version (scripts/deploy-fleet.sh); fix SSH reachability for any UNKNOWN box; then re-run." >&2
    return 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN (camera-box version not read), 0 box(es) DRIFTED — NOT clean." >&2
    echo "!! Every active box must report its camera-box version before this parity gate is trusted. (${ok} on ${modal}.)" >&2
    return 11
  fi
  if [ "$ok" -eq 0 ]; then
    # No box was actually checked (bad==0 && unknown==0 here), so every listed box was acked-offline
    # -> a vacuous pass. Say so plainly rather than "agree on <none>", which reads like a real result.
    echo "GATE PASS (vacuous) — every listed box is acked-offline; no active box remained to compare."
    return 0
  fi
  echo "GATE PASS — ${ok} active box(es) agree on camera-box ${modal} (any acked-offline box excluded above)."
  return 0
}

# --- usage + SSH read (impure; not unit-tested) ----------------------------------------------

usage() {
  cat <<EOF
camera-box-version-gate.sh — cross-box camera-box BINARY version-parity precondition gate (#875).

Compares every active cam box's camera-box binary version against EACH OTHER (relative cross-box
parity, NO fixed pin — the continuously-deployed dev build has no canonical value to pin against).
REFUSES (non-zero) on ANY disagreement between active boxes, or on any unread-and-unexcluded box,
printing a box->version table.

Usage:
  camera-box-version-gate.sh [--fleet-file PATH] \\
      --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 cam3=root@10.77.9.63"

Options:
  --fleet-file PATH default CAMBOX_OFFLINE_ACK source when the env var is unset (default:
                    ${DEFAULT_FLEET_FILE}) — same file recording-e2e.sh's fleet preflight reads.
  --linux "N=U@IP ..."  one or more SSH-reachable cam boxes (space-separated "name=user@ip" pairs
                    in ONE argument, mirrors dantesync-version-gate.sh's --linux). Repeatable. Read
                    via \`/usr/local/bin/camera-box --version\` over SSH.

Exit: 0 = active boxes agree (or excluded) — proceed. 20 = boxes DISAGREE (REFUSED).
  11 = a box UNKNOWN/unread (INCOMPLETE, not clean). 1 = usage error.
EOF
}

# read_camera_box_version_output NAME TARGET -> raw `camera-box --version` stdout for NAME (a cam
# box, "user@ip"). Read over SSH via the ABSOLUTE binary path (NOT the remote PATH — mirrors
# deploy-fleet.sh). "" if unreachable/absent (UNKNOWN downstream, never guessed). Overridable
# per-node for tests/offline via CAMERA_BOX_VERSION_GATE_VERSION_<NAME> (NAME uppercased, "-" -> "_")
# — mirrors dantesync-version-gate.sh's DANTESYNC_VERSION_GATE_VERSION_<NAME> fixture seam.
read_camera_box_version_output() {
  local name="$1" target="${2:-}" var
  var="CAMERA_BOX_VERSION_GATE_VERSION_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  [ -n "$target" ] || { printf ''; return 0; }
  sshpass -p "${CAMERA_BOX_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CAMERA_BOX_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    '/usr/local/bin/camera-box --version' 2>/dev/null || true
}

# --- source-guard: when sourced (the unit tests), stop here -----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ----------------------------------------------------
main() {
  local fleet_file="$DEFAULT_FLEET_FILE"
  local -a linux_raw=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --fleet-file) shift; fleet_file="${1:-}" ;;
      --linux) shift; linux_raw+=("${1:-}") ;;
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

  if [ "${#linux_pairs[@]}" -eq 0 ]; then
    echo "ERROR: no node to gate (--linux is empty)." >&2
    echo "The camera-box version-parity gate cannot certify the fleet with zero nodes — refusing to pass." >&2
    exit 1
  fi

  CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$fleet_file")"
  export CAMBOX_OFFLINE_ACK

  local -a entries=()
  local pair name target version
  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(camera_box_version_from_version_output "$(read_camera_box_version_output "$name" "$target")")"
    entries+=("${name}=${version}")
  done

  local rc=0
  camera_box_fleet_report "${entries[@]}" || rc=$?
  exit "$rc"
}

main "$@"
