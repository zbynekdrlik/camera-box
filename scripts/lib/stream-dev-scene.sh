#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no side effects) -- the sibling
# scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes this in the
# CALLER's shell (recording-e2e.sh / rig-mode.sh, which set their own strict mode). Every function
# is safe under the caller's set -euo pipefail.
#
# scripts/lib/stream-dev-scene.sh -- issue 1380: the stream OBS DEVELOPMENT scene.
#
# Owner request 27.9.2026: development never programs the owner's production scene on the stream
# OBS. The tooling programs its own development scene, which holds the production scene as a
# nested scene source (same pixels, the same warm `NDI 2ME PGM` receiver, the same 911004 burn),
# and EVENT mode puts the production scene back on program. The two names are declared ONCE here
# (the python defaults in scripts/obs_phase2.py are pinned to these by a pytest); recording-e2e.sh
# and rig-mode.sh derive their STREAM_PROG_SCENE / STREAM_EVENT_SCENE defaults from them.
#
#   stream_dev_scene_ensure  SCRIPTS_DIR HOST PASSWORD DEV_SCENE PRODUCTION_SCENE
#       -> obs_phase2.py dev-scene: creates the development scene and its nested production-scene
#          item when missing (idempotent, operator-wins, never writes to the production scene).
#          Its exit code is the caller's: non-zero (the production scene is missing, OBS is
#          unreachable) must stop the caller before it switches program.
#   stream_program_scene_read SCRIPTS_DIR HOST PASSWORD
#       -> prints the current program scene name, or NOTHING when OBS cannot be read. ALWAYS 0 --
#          the caller's decision treats an empty read as a failure (fail closed), never this helper.

# shellcheck disable=SC2034  # consumed by the sourcing scripts (recording-e2e.sh, rig-mode.sh)
STREAM_DEV_SCENE_DEFAULT="Development"
# shellcheck disable=SC2034  # consumed by the sourcing scripts (recording-e2e.sh, rig-mode.sh)
STREAM_PRODUCTION_SCENE_DEFAULT="PRO"

stream_dev_scene_ensure() {
  local scripts_dir="$1" host="$2" password="$3" dev="$4" production="$5"
  python3 "$scripts_dir/obs_phase2.py" dev-scene --host "$host" --password "$password" \
    --scene "$dev" --nested "$production"
}

stream_program_scene_read() {
  local scripts_dir="$1" host="$2" password="$3" out=""
  out="$(python3 "$scripts_dir/obs_phase2.py" program-scene --host "$host" \
    --password "$password" 2>/dev/null)" || out=""
  printf '%s\n' "$out" | tr -d '\r' | sed -n '1p'
  return 0
}
