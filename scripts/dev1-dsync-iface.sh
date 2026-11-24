#!/usr/bin/env bash
# scripts/dev1-dsync-iface.sh -- issue 1545: dev1's dsync on its declared PTP NIC (header below).
set -euo pipefail
#
# Usage:
#   scripts/dev1-dsync-iface.sh [--check]       read-only: grade dsync's /status (127.0.0.1:8898)
#                                               against the NIC the fleet declares for this node
#   scripts/dev1-dsync-iface.sh --apply          install the dsync drop-in that pins that NIC
#                                               (compare-then-write; dsync restarted ONLY when the
#                                               file changed; sudo -n)
#   scripts/dev1-dsync-iface.sh --node NAME ...  another local node name (default: the ONE fleet row
#                                               whose homegate is `local`, i.e. dev1)
#
# The node and its NIC come from the dantesync fleet (scripts/lib/dantesync-fleet.sh: the row with
# homegate `local`, the NIC from DANTESYNC_FLEET_PTP_NICS). Why: on 20.11.2026 dev1's dsync (unit
# without --interface) joined the link-DOWN enp39s0 and ran NTP-only; the hand drop-in that fixed it
# is reproduced from this fact, so a reinstall keeps it. The rig-dev-handover check runs `--check`
# (its dsync_nic item).
#
# Exit (--check): 0 joined the declared NIC, 1 configured elsewhere / not configured / joined
# elsewhere (fix: --apply), 2 /status unreadable or no declared node/NIC / usage.
# Exit (--apply): 0 installed or unchanged, 1 the write or the restart failed, 2 no node/NIC / usage.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/dantesync-fleet.sh
. "$HERE/lib/dantesync-fleet.sh"
# shellcheck source=scripts/lib/dsync-iface.sh
. "$HERE/lib/dsync-iface.sh"
# shellcheck source=scripts/lib/dsync-local-iface.sh
. "$HERE/lib/dsync-local-iface.sh"

mode=check
node=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) mode=check ;;
    --apply) mode=apply ;;
    --node) node="${2:-}"; [ -n "$node" ] || { echo "dev1-dsync-iface: --node needs a name" >&2; exit 2; }; shift ;;
    -h | --help) sed -n '5,23p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "dev1-dsync-iface: unknown argument '$1' (try --help)" >&2; exit 2 ;;
  esac
  shift
done

if [ -z "$node" ]; then
  node="$(dantesync_fleet_names homegate=local)"
  case "$node" in
    "" | *" "*) echo "dev1-dsync-iface: expected exactly one fleet row with homegate=local, got '$node'" >&2; exit 2 ;;
  esac
fi

if ! nic="$(dantesync_fleet_ptp_nic "$node")"; then
  echo "dev1-dsync-iface: no PTP NIC declared for '$node' in DANTESYNC_FLEET_PTP_NICS (scripts/lib/dantesync-fleet.sh) -- nothing done" >&2
  exit 2
fi

if [ "$mode" = apply ]; then
  dsync_local_iface_install "$nic" || exit 1
  exit 0
fi
rc=0
dsync_local_iface_check "$node" "$nic" || rc=$?
exit "$rc"
