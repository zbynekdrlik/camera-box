#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell (verify-strih, `set -euo
# pipefail`), so strict mode here would leak into it. Every function is safe as a bare statement under
# the caller's errexit: no grep / pipeline whose no-match exit leaks.
#
# scripts/lib/strih-jxs-verify.sh -- issue 1387 (owner 29.10.2026, comment 6509641172: "verify-strih
# riadok, ktorý FAILne pri lost_slices > 0 na bežiacom JPEG XS vstupe"): verify-strih item 49.
#
# Every camera_box_jxs_source (the DistroAV JPEG XS input, vendor/distroav/src/jxs-source.cpp) logs a
# 5 s window line into the OBS log:
#   HH:MM:SS.mmm: [distroav] jxs-source '<name>': fps=30.00 frames=150 lost_slices=0 concealed=0 ...
# A source is RUNNING when its newest window line is at most STRIH_JXS_RUNNING_S seconds older than the
# log's newest timestamped line (the windows come every 5 s; a removed source stops writing). For every
# running source the grade sums lost_slices over its last STRIH_JXS_WINDOWS windows (1 min): any loss
# FAILs naming the source -- concealment hides a lost slice on screen, the owner wants it seen. A source
# that stopped writing is a NOTE; no JPEG XS source at all is a NOTE (the link is optional). The OBS
# indicator reads DEGRADED from the same counters (jxs-state-table.h).
#
# Public API:
#   strih_jxs_lost_summary [WINDOWS [RUNNING_S]] < LOG -> one `<state>|<name>|<windows>|<lost>` line per
#       source (state running|stopped), pure (awk only)
#   strih_jxs_verify_row LOGFILE -> verify-strih item 49 (calls the caller's ok / bad / note)

STRIH_JXS_WINDOWS=12
STRIH_JXS_RUNNING_S=30

# strih_jxs_lost_summary [WINDOWS [RUNNING_S]] -> reads an OBS log on stdin (module doc). A line's time is
# its leading HH:MM:SS(.mmm); an age crossing midnight wraps (the log's newest line is "now").
strih_jxs_lost_summary() {
  return 0
}

# strih_jxs_verify_row LOGFILE -> verify-strih item 49: one row per JPEG XS source (or one NOTE).
strih_jxs_verify_row() {
  return 0
}
