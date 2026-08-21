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
#              [--wait[=SECS]] [--wait-host IP]
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
#   --wait[=SECS] after sending, POLL the target for reachability until it responds (exit 0,
#                WAKE-VERIFY UP) or SECS elapse (exit 4, WAKE-VERIFY STILL-DOWN). Default budget 120s.
#                This is the "verify availability after wake" half of remote recovery -- so a
#                detect-down (issue 1001) -> wake -> confirm-up loop is ONE composable command. The
#                poll host is the box's table ip; for a raw-MAC target pass --wait-host. The probe is
#                `${WOL_PING_CMD:-ping -c1 -W1}` (env-overridable, e.g. `nc -z host port`; split on
#                WHITESPACE into an argv -- a whitespace-separated command only, no shell quoting), the
#                poll interval `${WOL_WAIT_INTERVAL:-3}`s (a positive integer).
#   --wait-host IP  the host to poll for --wait (required for a raw-MAC target, which carries no IP;
#                overrides a box's table ip otherwise).
#
# Exit codes: 0 = packet sent (and, with --wait, box reachable);  2 = misuse (bad args / an
#   unresolvable --wait target);  3 = send error (a target rejected the packet) WITHOUT --wait;
#   4 = --wait budget elapsed, box still unreachable (WAKE-VERIFY STILL-DOWN). With --wait, a partial
#   send error does NOT short-circuit -- the box may have woken from a target that DID receive it, so
#   the reachability poll (the real proof) still runs.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/wol.sh
source "$HERE/lib/wol.sh"

usage() {
  cat <<'USAGE'
Usage: wake-box.sh <box|MAC> [--port P]... [--broadcast ADDR]... [--table FILE] [--dry-run]
                   [--wait[=SECS]] [--wait-host IP]
  <box>        box name from scripts/wol-targets.txt (strih | stream | imag-nb) -- MAC + subnet broadcast
               resolved from the table.
  <MAC>        raw MAC (5c:6a:.., 5C-6A-.., 5c6a.., or 5c6a80f66cf7).
  --port P     UDP port (repeatable). Default: 9 and 7.
  --broadcast  override broadcast address (repeatable). Default: subnet-directed + 255.255.255.255.
  --table FILE target table. Default: scripts/wol-targets.txt next to this script.
  --dry-run    print the packet + resolved targets and send NOTHING.
  --wait[=SECS] after sending, poll the target until reachable (exit 0) or SECS elapse (exit 4).
               Default 120s. Poll host = box table ip; raw-MAC target needs --wait-host.
  --wait-host IP  host to poll for --wait (required for a raw MAC; overrides a box's table ip).
See docs/wake-on-lan.md for the full runbook + per-box BIOS checklist.
USAGE
}

TARGET=""
TABLE="$HERE/wol-targets.txt"
DRYRUN=0
WAIT=0
WAIT_SECS=120
WAIT_HOST=""
PORTS=()
BROADCASTS=()

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --dry-run) DRYRUN=1; shift ;;
    --wait) WAIT=1; shift ;;
    --wait=*) WAIT=1; WAIT_SECS="${1#--wait=}"; shift ;;
    --wait-host) WAIT_HOST="${2:?--wait-host needs a value}"; shift 2 ;;
    --port) PORTS+=("${2:?--port needs a value}"); shift 2 ;;
    --broadcast) BROADCASTS+=("${2:?--broadcast needs a value}"); shift 2 ;;
    --table) TABLE="${2:?--table needs a value}"; shift 2 ;;
    --) shift; break ;;
    -*) echo "wake-box.sh: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$TARGET" ] || { echo "wake-box.sh: unexpected extra arg $1" >&2; exit 2; }; TARGET="$1"; shift ;;
  esac
done

# Validate the --wait inputs up front (a bad value must fail loud, never default silently). The
# budget is a non-negative integer; the poll interval must be a POSITIVE integer -- a 0 interval with
# a non-blocking probe (e.g. WOL_PING_CMD=false) would busy-spin the CPU for the whole budget.
if [ "$WAIT" -eq 1 ]; then
  if ! grep -qE '^[0-9]+$' <<<"$WAIT_SECS"; then
    echo "wake-box.sh: --wait budget must be a non-negative integer (got: $WAIT_SECS)" >&2
    exit 2
  fi
  if ! grep -qE '^[1-9][0-9]*$' <<<"${WOL_WAIT_INTERVAL:-3}"; then
    echo "wake-box.sh: WOL_WAIT_INTERVAL must be a positive integer (got: ${WOL_WAIT_INTERVAL:-3})" >&2
    exit 2
  fi
fi

[ -n "$TARGET" ] || { echo "wake-box.sh: missing <box|MAC> argument" >&2; usage; exit 2; }
[ "${#PORTS[@]}" -gt 0 ] || PORTS=(9 7)

# Resolve MAC + broadcast list. A target with exactly 12 hex nibbles (after stripping separators) is a
# raw MAC; otherwise it is a box name looked up in the table (which also yields its subnet broadcast).
stripped="${TARGET//[:.-]/}"
if [ "${#stripped}" -eq 12 ] && ! grep -qiE '[^0-9a-f]' <<<"$stripped"; then
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

# --wait: resolve the reachability host to poll AFTER the send. Do it BEFORE the dry-run exit and
# before the send, so an unresolvable --wait target (a raw MAC with no --wait-host, an unknown box)
# fails loud immediately (wol_verify_host returns non-zero -> set -e aborts) instead of sending a
# packet we then cannot verify.
VERIFY_HOST=""
if [ "$WAIT" -eq 1 ]; then
  VERIFY_HOST="$(wol_verify_host "${TABLE_TEXT:-}" "$TARGET" "$WAIT_HOST")"
  echo "wake-verify: will poll $VERIFY_HOST for up to ${WAIT_SECS}s after the wake"
fi

if [ "$DRYRUN" -eq 1 ]; then
  echo "DRY-RUN: no packet sent."
  echo "packet-hex: $PACKET_HEX"
  exit 0
fi

# Impure send: python3 sets SO_BROADCAST (bash /dev/udp cannot) and sends the raw magic packet.
# Capture the send's exit code WITHOUT letting set -e abort here (exit 3 = a target rejected the
# packet), so the --wait verify below still runs.
send_rc=0
python3 - "$PACKET_HEX" "${TARGETS[@]}" <<'PY' || send_rc=$?
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
        rc = 3
s.close()
sys.exit(rc)
PY

# A send failure (>=1 target rejected the packet). Without --wait that is the terminal result. WITH
# --wait, do NOT short-circuit: the box may have woken from a target that DID accept the packet, and
# the reachability poll below is the real proof either way.
if [ "$send_rc" -ne 0 ]; then
  echo "wake-box.sh: send error (a target rejected the packet, exit $send_rc)" >&2
  [ "$WAIT" -eq 1 ] || exit "$send_rc"
fi

# --wait: poll the target for reachability until it responds (WAKE-VERIFY UP, exit 0) or the budget
# elapses (WAKE-VERIFY STILL-DOWN, exit 4 -- distinct from exit 2 for bad args, so a recovery caller
# can tell "sent but never came up" from "misused"). The probe is env-injectable so a caller can swap
# in a TCP-port check, and a test can drive a deterministic verdict with no real network.
if [ "$WAIT" -eq 1 ]; then
  read -r -a probe_cmd <<<"${WOL_PING_CMD:-ping -c1 -W1}"
  interval="${WOL_WAIT_INTERVAL:-3}"
  echo "wake-verify: polling $VERIFY_HOST every ${interval}s for up to ${WAIT_SECS}s ..."
  start=$SECONDS
  up=0
  while :; do
    if "${probe_cmd[@]}" "$VERIFY_HOST" >/dev/null 2>&1; then up=1; break; fi
    [ "$(( SECONDS - start ))" -ge "$WAIT_SECS" ] && break
    sleep "$interval"
  done
  elapsed=$(( SECONDS - start ))
  if [ "$up" -eq 1 ]; then
    echo "WAKE-VERIFY UP: $BOX ($VERIFY_HOST) reachable after ~${elapsed}s"
    exit 0
  fi
  echo "WAKE-VERIFY STILL-DOWN: $BOX ($VERIFY_HOST) not reachable after ${WAIT_SECS}s" >&2
  exit 4
fi
