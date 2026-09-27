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
# and EVENT mode never touches the stream program (the owner cuts to it himself). The two names are
# declared ONCE here (the python defaults in scripts/obs_phase2.py are pinned to these by a pytest); recording-e2e.sh
# and rig-mode.sh derive their STREAM_PROG_SCENE default from them. Owner hard rule 27.9.2026,
# verbatim: "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!" -- the production scene
# is only ever NESTED, never put on program or preview (obs_phase2.py refuses it).
#
#   stream_dev_scene_ensure  SCRIPTS_DIR HOST PASSWORD DEV_SCENE PRODUCTION_SCENE
#       -> obs_phase2.py dev-scene: creates the development scene and its nested production-scene
#          item when missing (idempotent, operator-wins, never writes to the production scene).
#          Its exit code is the caller's: non-zero (the production scene is missing, OBS is
#          unreachable, or an override naming the production scene itself) must stop the caller
#          before it switches program. Only the DEFAULT
#          development scene is ever seeded: an env override naming another scene (possibly one of
#          the owner's) is NOT touched -- a one-line note, rc 0, and the caller's switch then needs
#          that scene to exist already.
#   stream_program_scene_read SCRIPTS_DIR HOST PASSWORD
#       -> prints the current program scene name, or NOTHING when OBS cannot be read. ALWAYS 0 --
#          the caller prints it as a report-only line (an empty read = "unreadable").

# shellcheck disable=SC2034  # consumed by the sourcing scripts (recording-e2e.sh, rig-mode.sh)
STREAM_DEV_SCENE_DEFAULT="Development"
# shellcheck disable=SC2034  # consumed by the sourcing scripts (recording-e2e.sh, rig-mode.sh)
STREAM_PRODUCTION_SCENE_DEFAULT="PRO"

stream_dev_scene_ensure() {
  local scripts_dir="$1" host="$2" password="$3" dev="$4" production="$5"
  if [ "$dev" = "$production" ]; then
    echo "ERROR: issue 1380: the stream program scene override '${dev}' IS the production scene -- development never programs it (owner request 27.9.2026)" >&2
    return 1
  fi
  if [ "$dev" != "$STREAM_DEV_SCENE_DEFAULT" ]; then
    echo "issue 1380: stream program scene overridden to '${dev}' -- not seeding it (only '${STREAM_DEV_SCENE_DEFAULT}' is ever created/filled); it must already exist"
    return 0
  fi
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
