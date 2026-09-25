#!/usr/bin/env bash
# dantesync-config-drift.sh -- issue 1372 part B: every dantesync node's config.json vs ONE canonical
# template per role. See the extended header below.
set -euo pipefail
#
# WHY: the owner keeps finding dantesync boxes that are "differently configured" (verbatim,
# 25.9.2026: "neviem preco stale nachadzam neupgradnute dantesync appky alebo ze su rozdielne
# nastavene!"). On that day mbc had no gm_allowlist and fohabl had neither gm_allowlist nor
# phase_slew, and nothing had ever read either file. This script reads the config of EVERY node of
# the one declared dantesync fleet (scripts/lib/dantesync-fleet.sh) and compares it with its role's
# canonical template (scripts/dantesync-canonical-config.json) through the pure decision in
# scripts/dantesync_fleet.py, printing a NAMED diff per node.
#
# REPORT-ONLY. It never writes a config to any box: turning a drift into a change is a deliberate,
# separate step (a policy flip belongs to dantesync#117's role contract and is rolled out from
# there). The clock policy since dantesync 1.9.0 (issue 1372): system.clock_discipline absent or
# "ptp_phase_lock", "legacy" = drift; system.phase_slew is no longer graded ({"$ignore": true}).
# No E2E step calls this as a blocking gate.
#
# READS (read-only): dev1's own /etc/dantesync/config.json (local), a Linux node's
# /etc/dantesync/config.json and a Windows node's C:\ProgramData\DanteSync\config.json over
# `scp -O` (the raw BYTES, so a PowerShell-written byte-order mark -- which makes dantesync ignore the
# file -- is caught). A traveling box that is away (obs_fleet_is_home false) is SKIPPED; a node acked
# offline in rig-fleet.txt / CAMBOX_OFFLINE_ACK is EXCLUDED; any other unread node is UNKNOWN, never
# a silent pass.
#
# CREDENTIALS: a node whose fleet row names a credvar (fohabl: DANTESYNC_FOHABL_SSH_PASS) is read with
# that variable's value, taken from the environment or the dev1-local credential file
# (DANTESYNC_FLEET_CRED_FILE, default ~/.config/camera-box/dantesync-fleet.env, mode 0600, never
# committed). Unset -> that node reads UNKNOWN. Every other node uses DANTESYNC_CONFIG_DRIFT_SSH_PASS
# (default: the same fleet default the version gate uses).
#
# Usage:
#   scripts/dantesync-config-drift.sh [--only "name ..."] [--fleet-file PATH]
#   scripts/dantesync-config-drift.sh --help
# Exit: 0 = every read node matches its template, 20 = at least one node DRIFTED, 11 = at least one
#   node UNKNOWN (unread / not JSON) and none drifted, 1 = usage / environment error.
# Test seam: DANTESYNC_CONFIG_DRIFT_FETCH_CMD, when set, is invoked as
#   `<cmd> <name> <os> <target> <outfile>` and replaces the real read (no box needed).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/dantesync-fleet.sh
. "$HERE/lib/dantesync-fleet.sh"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"

DECIDE="${DANTESYNC_CONFIG_DRIFT_DECIDE:-$HERE/dantesync_fleet.py}"
TEMPLATES="${DANTESYNC_CONFIG_DRIFT_TEMPLATES:-$HERE/dantesync-canonical-config.json}"
SSH_PASS="${DANTESYNC_CONFIG_DRIFT_SSH_PASS:-${DANTESYNC_VERSION_GATE_SSH_PASS:-newlevel}}"
SSH_USER="${DANTESYNC_CONFIG_DRIFT_SSH_USER:-${WIN_SSH_USER:-newlevel}}"
SSH_TIMEOUT="${DANTESYNC_CONFIG_DRIFT_SSH_TIMEOUT:-10}"
FLEET_FILE="$HERE/../rig-fleet.txt"
ONLY=""

usage() { sed -n '2,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --only) ONLY="${2:?--only needs \"name ...\"}"; shift 2 ;;
    --fleet-file) FLEET_FILE="${2:?--fleet-file needs a path}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "dantesync-config-drift: unknown argument '$1' (try --help)" >&2; exit 1 ;;
  esac
done

command -v python3 >/dev/null 2>&1 || { echo "dantesync-config-drift: python3 is required" >&2; exit 1; }
[ -r "$DECIDE" ] || { echo "dantesync-config-drift: decision module not readable: $DECIDE" >&2; exit 1; }

CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$FLEET_FILE")"
export CAMBOX_OFFLINE_ACK
dantesync_fleet_load_credentials

WORK="$(mktemp -d "${TMPDIR:-/tmp}/dantesync-config-drift.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# fetch_config NAME OS TARGET OUT -> the node's config.json bytes into OUT (rc 0), or rc != 0.
fetch_config() {
  local name="$1" os="$2" target="$3" out="$4" var pass remote
  if [ -n "${DANTESYNC_CONFIG_DRIFT_FETCH_CMD:-}" ]; then
    "$DANTESYNC_CONFIG_DRIFT_FETCH_CMD" "$name" "$os" "$target" "$out"
    return
  fi
  if [ "$target" = "local" ]; then
    cat "$DANTESYNC_FLEET_CONFIG_LINUX" >"$out"
    return
  fi
  var="$(dantesync_fleet_cred_var_for_target "$target")"
  if [ -n "$var" ]; then
    pass="${!var:-}"
    if [ -z "$pass" ]; then
      echo "  ($name: credential $var is not set -- export it or add it to $DANTESYNC_FLEET_CRED_FILE)" >&2
      return 1
    fi
  else
    pass="$SSH_PASS"
  fi
  case "$os" in
    windows) remote="${DANTESYNC_FLEET_CONFIG_WINDOWS//\\//}" ;;
    *) remote="$DANTESYNC_FLEET_CONFIG_LINUX" ;;
  esac
  sshpass -p "$pass" timeout "$SSH_TIMEOUT" scp -O -q -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout="$SSH_TIMEOUT" \
    "${target}:${remote}" "$out"
}

echo "== dantesync config drift (issue 1372): every fleet node vs its role's canonical template =="
echo "   templates: $TEMPLATES   (report-only -- nothing is written to any box)"
DRIFTED="" UNKNOWNS="" rc_total=0
while IFS='|' read -r name addr os role homegate user _; do
  if [ -n "$ONLY" ]; then
    case " $ONLY " in *" $name "*) ;; *) continue ;; esac
  fi
  if cambox_offline_ack_is_acked "$name"; then
    echo "node=$name role=$role verdict=EXCLUDED (acked offline: $(cambox_offline_ack_reason "$name"))"
    continue
  fi
  if [ "$homegate" = "obsfleet" ] && ! dantesync_fleet_present "$name"; then
    echo "node=$name role=$role verdict=SKIPPED (traveling box away -- obs_fleet_is_home false)"
    continue
  fi
  if [ "$homegate" = "local" ]; then
    target="local"
  else
    [ "$user" = "-" ] && user="$SSH_USER"
    target="${user}@${addr}"
  fi
  out="$WORK/$name.json"
  : >"$out"
  fetch_config "$name" "$os" "$target" "$out" >/dev/null || : >"$out"
  node_rc=0
  python3 "$DECIDE" drift --role "$role" --name "$name" --templates "$TEMPLATES" "$out" || node_rc=$?
  case "$node_rc" in
    0) : ;;
    20) DRIFTED="${DRIFTED:+$DRIFTED }$name" ;;
    *) UNKNOWNS="${UNKNOWNS:+$UNKNOWNS }$name" ;;
  esac
done < <(dantesync_fleet_rows)

echo
if [ -n "$DRIFTED" ]; then
  echo "!! DANTESYNC CONFIG DRIFT on: $DRIFTED${UNKNOWNS:+ (and unread: $UNKNOWNS)} -- see the named diffs above (report-only)."
  rc_total=20
elif [ -n "$UNKNOWNS" ]; then
  echo "!! dantesync config UNREAD on: $UNKNOWNS -- the drift check is INCOMPLETE, not clean."
  rc_total=11
else
  echo "OK: every read dantesync node matches its role's canonical config."
fi
exit "$rc_total"
