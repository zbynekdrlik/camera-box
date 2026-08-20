#!/usr/bin/env bash
set -euo pipefail
# scripts/wake-box.sh -- #1053: dev1-side Wake-on-LAN magic-packet sender for the strih/stream
# broadcast OBS boxes, the remote-recovery counterpart to issue 1001's outage-detection watchdog.
#
# WHY (#1053): issue 1001 ALERTS when strih/stream fall off the network; this makes such an outage
# remotely RECOVERABLE -- if the box has gone to sleep / been powered off (and its BIOS + NIC WoL are
# enabled), a magic packet from dev1 wakes it without hands on the box. On 2026-08-13 a magic packet
# for strih (5C:6A:80:F6:6C:F7) was sent from cam1 (same L2 segment) to broadcast + subnet-directed on
# UDP 9 and 7 and did NOT wake it; STEP-0 live probe (see issue 1053) proved the Windows NIC WoL is
# already fully enabled + wake_armed on BOTH boxes, so the remaining gap is the BIOS standby-power
# layer (a hands-on task) -- this tool is the send half, ready for once BIOS WoL is enabled.
#
# The magic-packet construction + MAC/table logic is the PURE, unit-tested core in
# scripts/lib/wol.sh; this wrapper only adds the impure UDP broadcast send. A raw magic packet needs
# SO_BROADCAST (which bash /dev/udp cannot set), so the actual send is a tiny inline python3 (already
# a hard dependency of this repo -- dozens of .py scripts; NOT a new dependency). By default it sends
# to BOTH the box's subnet-directed broadcast (a.b.c.255) AND the limited broadcast (255.255.255.255)
# on UDP ports 9 AND 7 -- exactly the delivery shape the 2026-08-13 attempt used.
#
# dev1 is on the same 10.77.9.0/24 as the rig, so a subnet-directed broadcast reaches the boxes at L2.
# If ever run from OFF the rig segment, run it from an on-segment box instead (scp this dir to a cam
# box) -- a routed directed-broadcast is commonly dropped by switches. See docs/wake-on-lan.md.
#
# Usage:
#   wake-box.sh <box|MAC> [--port P]... [--broadcast ADDR] [--table FILE] [--dry-run]
#   wake-box.sh --help
#
#   <box>        a box name from scripts/wol-targets.txt (strih | stream | imag-nb) -- MAC + subnet broadcast
#                are resolved from the table.
#   <MAC>        a raw MAC (any of 5c:6a:.., 5C-6A-.., 5c6a.., 5c6a80f66cf7) -- the given --broadcast
#                (if any) is targeted IN ADDITION to the limited broadcast 255.255.255.255 (always sent).
#   --port P     UDP port to send to (repeatable). Default: 9 and 7.
#   --broadcast  override the broadcast address (repeatable). Default: subnet + limited broadcast.
#   --table FILE the target table. Default: scripts/wol-targets.txt next to this script.
#   --dry-run    print the packet + resolved targets and send NOTHING.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/wol.sh
source "$HERE/lib/wol.sh"

usage() {
  cat <<'USAGE'
Usage: wake-box.sh <box|MAC> [--port P]... [--broadcast ADDR]... [--table FILE] [--dry-run]
  <box>        box name from scripts/wol-targets.txt (strih | stream | imag-nb) -- MAC + subnet broadcast
               resolved from the table.
  <MAC>        raw MAC (5c:6a:.., 5C-6A-.., 5c6a.., or 5c6a80f66cf7).
  --port P     UDP port (repeatable). Default: 9 and 7.
  --broadcast  override broadcast address (repeatable). Default: subnet-directed + 255.255.255.255.
  --table FILE target table. Default: scripts/wol-targets.txt next to this script.
  --dry-run    print the packet + resolved targets and send NOTHING.
See docs/wake-on-lan.md for the full runbook + per-box BIOS checklist.
USAGE
}

TARGET=""
TABLE="$HERE/wol-targets.txt"
DRYRUN=0
PORTS=()
BROADCASTS=()

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --dry-run) DRYRUN=1; shift ;;
    --port) PORTS+=("${2:?--port needs a value}"); shift 2 ;;
    --broadcast) BROADCASTS+=("${2:?--broadcast needs a value}"); shift 2 ;;
    --table) TABLE="${2:?--table needs a value}"; shift 2 ;;
    --) shift; break ;;
    -*) echo "wake-box.sh: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$TARGET" ] || { echo "wake-box.sh: unexpected extra arg $1" >&2; exit 2; }; TARGET="$1"; shift ;;
  esac
done

[ -n "$TARGET" ] || { echo "wake-box.sh: missing <box|MAC> argument" >&2; usage; exit 2; }
[ "${#PORTS[@]}" -gt 0 ] || PORTS=(9 7)

# Resolve MAC + broadcast list. A target with exactly 12 hex nibbles (after stripping separators) is a
# raw MAC; otherwise it is a box name looked up in the table (which also yields its subnet broadcast).
stripped="${TARGET//[:.-]/}"
if [ "${#stripped}" -eq 12 ] && ! printf '%s' "$stripped" | grep -qiE '[^0-9a-f]'; then
  MAC="$(wol_normalize_mac "$TARGET")"
  BOX="(raw MAC)"
else
  [ -s "$TABLE" ] || { echo "wake-box.sh: target table not found: $TABLE" >&2; exit 2; }
  TABLE_TEXT="$(cat "$TABLE")"
  MAC="$(wol_table_lookup "$TABLE_TEXT" "$TARGET" mac)"
  MAC="$(wol_normalize_mac "$MAC")"
  BOX="$TARGET"
  # subnet-directed broadcast for this box, unless the caller overrode --broadcast
  [ "${#BROADCASTS[@]}" -gt 0 ] || BROADCASTS+=("$(wol_table_lookup "$TABLE_TEXT" "$TARGET" broadcast)")
fi
# always also hit the limited broadcast
BROADCASTS+=("255.255.255.255")

PACKET_HEX="$(wol_magic_packet_hex "$MAC")"

# Build the addr:port target list (dedup exact pairs)
declare -a TARGETS=()
for b in "${BROADCASTS[@]}"; do
  for p in "${PORTS[@]}"; do
    pair="${b}:${p}"
    case " ${TARGETS[*]} " in *" $pair "*) : ;; *) TARGETS+=("$pair") ;; esac
  done
done

echo "wake-box.sh: box=$BOX mac=$MAC packet=$(( ${#PACKET_HEX} / 2 )) bytes"
printf 'targets: %s\n' "${TARGETS[*]}"

if [ "$DRYRUN" -eq 1 ]; then
  echo "DRY-RUN: no packet sent."
  echo "packet-hex: $PACKET_HEX"
  exit 0
fi

# Impure send: python3 sets SO_BROADCAST (bash /dev/udp cannot) and sends the raw magic packet.
python3 - "$PACKET_HEX" "${TARGETS[@]}" <<'PY'
import binascii, socket, sys
pkt = binascii.unhexlify(sys.argv[1])
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
rc = 0
for tgt in sys.argv[2:]:
    addr, port = tgt.rsplit(":", 1)
    try:
        s.sendto(pkt, (addr, int(port)))
        print("SENT %d bytes -> %s:%s" % (len(pkt), addr, port))
    except OSError as e:
        print("SEND-FAIL %s:%s -> %s" % (addr, port, e), file=sys.stderr)
        rc = 1
s.close()
sys.exit(rc)
PY
