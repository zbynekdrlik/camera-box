#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes this
# in the CALLER's shell (recording-e2e.sh, which sets its own strict mode).
#
# scripts/lib/connect-on-show-hold.sh -- issue 1242: the E2E connect-on-show HOLD.
#
# On strih-lx a program-path camera input (`NDI camN`, genlock_connect_on_show) PARKS -- releases its
# NDI receiver -- while nothing shows it (owner ruling 24.9.2026: full bandwidth only for shown
# cameras). The E2E measurement needs EVERY program-path input connected full-bandwidth for the whole
# run (the [4c/8] received= gate, the mv-reverify escalation, the per-camera recordings), so the harness
# HOLDS the flag off right after its cleanup trap arms (behind its own rig-busy guard) and cleanup()
# restores it. Added through this sourced helper (the #675 pattern) so recording-e2e.sh gains two
# plain call lines and no new anchor text.
#
#   connect_on_show_e2e_hold    HERE STRIH STATE_FILE -> 0 held (read back) | non-zero: the run must
#                                                        abort (a hidden input would be measured cold)
#   connect_on_show_e2e_restore HERE STRIH STATE_FILE -> ALWAYS 0 (cleanup-safe); WARNs on a failed
#                                                        restore (fail-SAFE: the inputs just stay
#                                                        connected, and the next strih OBS launch
#                                                        re-applies the roles)

connect_on_show_e2e_hold() {
  local here="$1" strih="$2" state="$3"
  echo "    issue 1242 connect-on-show hold: every strih program-path camera input stays connected for this run (state $state)"
  if ! timeout "${CONNECT_ON_SHOW_HOLD_TIMEOUT_S:-60}" python3 "$here/obs_phase2.py" connect-on-show \
      --host "$strih" --password "${OBS_PASSWORD:-}" --hold "$state"; then
    echo "ERROR: issue 1242 connect-on-show hold FAILED on $strih -- a hidden program-path input would be measured cold/parked; aborting the run" >&2
    return 1
  fi
  return 0
}

connect_on_show_e2e_restore() {
  local here="$1" strih="$2" state="$3"
  if [ ! -f "$state" ]; then
    return 0
  fi
  if timeout "${CONNECT_ON_SHOW_HOLD_TIMEOUT_S:-60}" python3 "$here/obs_phase2.py" connect-on-show \
      --host "$strih" --password "${OBS_PASSWORD:-}" --restore "$state"; then
    return 0
  fi
  echo "WARNING: issue 1242 connect-on-show restore FAILED on $strih (state kept at $state) -- the held inputs stay connected (full bandwidth) until the next strih OBS launch re-applies the roles or a manual: python3 scripts/obs_phase2.py connect-on-show --host $strih --restore $state" >&2
  return 0
}
