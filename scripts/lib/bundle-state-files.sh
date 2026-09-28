#!/usr/bin/env bash
# airuleset:script-ok source-only lib (declares one array, no command runs) -- matches the sibling
# scripts/lib/*.sh convention (ndi-runtime.sh, remoteos-mcp.sh) of deliberately NOT setting
# `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so strict mode
# here would leak into whichever caller sources it. Each caller sets its own strict mode.
#
# scripts/lib/bundle-state-files.sh -- issue 1386: the ONE declared file set of the :8899
# bundle-state server tree, so setup-strih.sh (step 9) and setup-imag.sh (step 28) install the same
# files instead of each typing a literal list. The server imports bundle_state_gather, which
# resolves its flat facet-family siblings, plus obs_phase2 -- a box missing any of them serves no
# :8899 at all (the server exits on the ImportError and its supervisor restarts it in a loop).
#
# The plain-text twin scripts/lib/bundle-state-files.txt carries the same names for the Windows
# runbook (the boxes fetch it at the pinned commit, then each file). tests/python/
# test_bundle_state_files_1386.py pins this array == the .txt list == the server's real
# module-level local imports (transitively), so adding a module without listing it fails a test.
# Pure data: no external command, safe to source with an empty PATH.

# shellcheck disable=SC2034  # read by the sourcing setup scripts
BUNDLE_STATE_SERVER_FILES=(
  bundle-state-server.py
  bundle_state_gather.py
  bundle_state_log.py
  bundle_state_genlock.py
  bundle_state_audio.py
  bundle_state_vban.py
  bundle_state_av_offset.py
  bundle_state_host.py
  obs_phase2.py
)
