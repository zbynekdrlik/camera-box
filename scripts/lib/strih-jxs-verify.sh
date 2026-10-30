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
  local windows="${1:-$STRIH_JXS_WINDOWS}" running_s="${2:-$STRIH_JXS_RUNNING_S}"
  awk -v W="$windows" -v R="$running_s" '
    function tod(s,   a) {
      if (s !~ /^[0-9][0-9]:[0-9][0-9]:[0-9][0-9]/) return -1
      split(substr(s, 1, 8), a, ":")
      return a[1] * 3600 + a[2] * 60 + a[3]
    }
    {
      t = tod($0)
      if (t >= 0) last = t
      i = index($0, "jxs-source '\''")
      if (i == 0) next
      rest = substr($0, i + 12)
      j = index(rest, "'\'': fps=")
      if (j == 0) next
      name = substr(rest, 1, j - 1)
      gsub(/\|/, "/", name)
      if (match(rest, / lost_slices=[0-9]+/) == 0) next
      lost = substr(rest, RSTART + 13, RLENGTH - 13) + 0
      if (!(name in n)) { order[++k] = name; n[name] = 0 }
      n[name]++
      ring[name, n[name] % W] = lost
      seen[name] = t
    }
    END {
      for (q = 1; q <= k; q++) {
        name = order[q]
        c = n[name] < W ? n[name] : W
        sum = 0
        for (r = 0; r < c; r++) sum += ring[name, (n[name] - r) % W]
        age = last - seen[name]
        if (seen[name] < 0 || last < 0) age = 0
        if (age < 0) age += 86400
        printf "%s|%s|%d|%d\n", (age <= R ? "running" : "stopped"), name, c, sum
      }
    }'
}

# strih_jxs_verify_row LOGFILE -> verify-strih item 49: one row per JPEG XS source (or one NOTE).
strih_jxs_verify_row() {
  local log="${1-}" state name windows lost any=0
  if [ -z "$log" ] || [ ! -r "$log" ]; then
    note "(jxs) no OBS log to read the JPEG XS sources from (issue 1387)"
    return 0
  fi
  while IFS='|' read -r state name windows lost; do
    [ -n "$name" ] || continue
    any=1
    if [ "$state" != running ]; then
      note "(jxs) '${name}': no window line in the last ${STRIH_JXS_RUNNING_S} s of the OBS log -- the source is not running (issue 1387)"
    elif [ "$lost" -gt 0 ]; then
      bad "(jxs) '${name}': lost_slices=${lost} in its last ${windows} window(s) -- the JPEG XS link loses datagrams (concealed on screen); read the sender's pace_src / rx_link_mbps and the source's link_mbps, then the switch port (issue 1387)"
    else
      ok "(jxs) '${name}': lost_slices=0 in its last ${windows} window(s)"
    fi
  done < <(strih_jxs_lost_summary < "$log" || true)
  if [ "$any" = 0 ]; then
    note "(jxs) no JPEG XS source (camera_box_jxs_source) in the running OBS log -- nothing to grade (issue 1387)"
  fi
  return 0
}
