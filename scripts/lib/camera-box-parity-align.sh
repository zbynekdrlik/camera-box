#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the scripts/lib/cbox-burn-log-persist.sh / cambox-offline-ack.sh
# convention (no top-level `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/camera-box-parity-align.sh -- pre-gate auto-align of the active cam fleet to THIS
# run's candidate camera-box build (issue 1202). [RED stub -- real decision lands in the GREEN
# commit.]
_CBPA_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$_CBPA_HERE/cambox-offline-ack.sh"

# cambox_align_action CANDIDATE ENTRY... -- [RED stub] not yet implemented; always MIXED.
cambox_align_action() {
  printf 'MIXED\n'
  return 0
}
