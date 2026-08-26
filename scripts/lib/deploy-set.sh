#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# scripts/lib/cambox-offline-ack.sh convention.
#
# scripts/lib/deploy-set.sh (#1136) — compute WHICH cam boxes a fleet deploy targets: the active
# set MINUS any box knowingly acked-offline. Used by the push-to-main auto-deploy CI job so it
# NEVER tries to deploy to a box the rig-fleet.txt/CAMBOX_OFFLINE_ACK mechanism already says is
# offline (which would otherwise make deploy-fleet.sh mark it FAILED and fail the whole job), while
# still failing LOUDLY on an ACTIVE-and-unacked box that is unreachable. Reuses the SAME exclusion
# mechanism the version/parity gates use (cambox-offline-ack.sh) — never a second mechanism.

DEPLOY_SET_LIB_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$DEPLOY_SET_LIB_HERE/cambox-offline-ack.sh"

# deploy_set_active_minus_acked ACTIVE_SET [ACK] -> prints (space-separated, stdout) every box in
# ACTIVE_SET that is NOT acked-offline. ACK (optional) is a CAMBOX_OFFLINE_ACK-format value
# (comma-separated "box:reason" pairs); when omitted the ambient CAMBOX_OFFLINE_ACK is used. Pure
# string logic (delegates the membership test to cambox_offline_ack_is_acked — the SAME exact-name
# match the gates use, never a substring). Order of ACTIVE_SET is preserved.
deploy_set_active_minus_acked() {
  local active="$1"
  # A LOCAL override is dynamically scoped, so cambox_offline_ack_is_acked (which reads
  # ${CAMBOX_OFFLINE_ACK:-}) sees THIS value when an explicit ACK arg is given.
  local CAMBOX_OFFLINE_ACK="${2:-${CAMBOX_OFFLINE_ACK:-}}"
  local box out=""
  for box in $active; do
    cambox_offline_ack_is_acked "$box" && continue
    out="${out:+$out }$box"
  done
  printf '%s' "$out"
}
