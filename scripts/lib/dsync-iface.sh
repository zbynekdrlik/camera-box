#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup/verify scripts and
# the remote programs that embed these functions via `declare -f`), and strict mode here would leak.
#
# scripts/lib/dsync-iface.sh -- issue 1439 (ROZHODNUTE 1.11.2026: dsync binds PTP to a NIC the box
# declares, never by chance): the ONE grade of "which NIC is dsync on", for every dsync node class.
# dsync >= 1.21 serves `ptp_interface_configured` (the --interface it was told, null without one) and
# `ptp_interface` (the NIC it joined) on its own http_status endpoint (127.0.0.1:8898/status).
#
# Consumers:
#   - the Linux OBS boxes: verify-strih item 6d through the strih_dantesync_iface_* names that
#     scripts/lib/strih-dantesync.sh keeps as thin wrappers;
#   - the camboxes: verify-device (ba) and the live apply (scripts/lib/cambox-dsync-unit.sh), which
#     embed these functions into on-box text via `declare -f`, so they stay plain bash + python3.
#
# Helpers:
#   dsync_iface_verdict READ CONFIGURED JOINED NIC  pure, always rc 0:
#       unreadable (READ != 1) | norig (no NIC) | not-configured (no --interface) |
#       configured-elsewhere:<c>:<nic> | joined-elsewhere:<j|->:<nic> | ok:<nic>
#   dsync_iface_status_fields BODY   -> `<locked>\t<configured>\t<joined>` from a /status JSON body
#                                       (locked = true|false|empty), rc 1 when BODY is not a JSON object
#   dsync_iface_read_status          -> the same three fields read ONCE (bounded) from 127.0.0.1:8898
#   dsync_iface_grade NIC            -> one read + the verdict; a dead endpoint is `unreadable`, never a pass

dsync_iface_verdict() {
  local read="${1-}" conf="${2-}" joined="${3-}" nic="${4-}"
  if [ "$read" != 1 ]; then echo unreadable
  elif [ -z "$nic" ]; then echo norig
  elif [ -z "$conf" ]; then echo not-configured
  elif [ "$conf" != "$nic" ]; then echo "configured-elsewhere:${conf}:${nic}"
  elif [ "$joined" != "$nic" ]; then echo "joined-elsewhere:${joined:--}:${nic}"
  else echo "ok:${nic}"
  fi
}

dsync_iface_status_fields() {
  local body="${1-}"
  [ -n "$body" ] || return 1
  # python reads the whole stdin, so the printf never takes a SIGPIPE under a caller's pipefail.
  printf '%s' "$body" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(1)
if not isinstance(d, dict):
    sys.exit(1)
lk = d.get("is_locked")
lk = "true" if lk is True else ("false" if lk is False else "")
v = [d.get("ptp_interface_configured"), d.get("ptp_interface")]
print("\t".join([lk] + [x if isinstance(x, str) else "" for x in v]))
' 2>/dev/null
}

dsync_iface_read_status() {
  local body
  body="$(curl -s -m 3 http://127.0.0.1:8898/status 2>/dev/null || true)"
  dsync_iface_status_fields "$body"
}

dsync_iface_grade() {
  local nic="${1-}" f="" rest read=0 conf="" joined=""
  if f="$(dsync_iface_read_status)"; then
    read=1
    rest="${f#*$'\t'}"
    conf="${rest%%$'\t'*}"
    joined="${rest#*$'\t'}"
  fi
  dsync_iface_verdict "$read" "$conf" "$joined" "$nic"
}
