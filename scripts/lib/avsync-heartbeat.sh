#!/usr/bin/env bash
# scripts/lib/avsync-heartbeat.sh -- #812/#807 PURE heartbeat-freshness helpers.
set -euo pipefail
#
# For the dev1-side scripts/avsync-heartbeat-alert-watchdog.sh. No I/O, no ssh -- pure
# string/arithmetic, unit-tested directly (mirrors the "pure decision lib" shape of
# scripts/lib/obs-watchdog-decision.sh / scripts/lib/imag-obs-reachability.sh).
#
# Each on-box heartbeat file is ONE line: "<epoch_seconds>\t<status text>" (avsync-watchdog.ps1 /
# avsync-vlc-monitor.ps1's own Write-Heartbeat -- see issues 812/807's design comments). The
# dev1-side watchdog reads BOTH files in a single ssh round-trip (avsync_heartbeat_probe_cmd),
# separated by a marker line neither script's own status text could ever contain.
#
# Source-only: defines the functions below; runs nothing on its own.

AVSYNC_HB_SEP='---AVSYNC-HB-SEP---'

# avsync_heartbeat_probe_cmd [watchdog_path] [vlc_path] -> a remote cmd.exe command string (this
# box's default ssh shell is cmd.exe, confirmed live -- NOT bash/powershell). `type`-ing a missing
# file prints to stderr and fails; `2>nul` swallows that so a missing file just produces an EMPTY
# segment (never a false alert about the ssh call itself failing) and `&` (not `&&`) always runs
# the next segment regardless of the previous one's exit code.
avsync_heartbeat_probe_cmd() {
  local watchdog_path="${1:-C:\\avsync\\avsync-watchdog-heartbeat.txt}"
  local vlc_path="${2:-C:\\avsync\\avsync-vlc-monitor-heartbeat.txt}"
  printf 'type "%s" 2>nul & echo %s & type "%s" 2>nul' "$watchdog_path" "$AVSYNC_HB_SEP" "$vlc_path"
}

# avsync_heartbeat_extract_segment PROBE_OUTPUT WHICH -> echoes the "watchdog" or "vlc" half of a
# probe_cmd's combined output, split on the separator line. Empty when that half never appeared.
avsync_heartbeat_extract_segment() {
  local out="$1" which="$2"
  case "$which" in
    watchdog) printf '%s\n' "$out" | sed -n "1,/^${AVSYNC_HB_SEP}\$/p" | sed '$d' ;;
    vlc)      printf '%s\n' "$out" | sed -n "/^${AVSYNC_HB_SEP}\$/,\$p" | sed '1d' ;;
    *) return 1 ;;
  esac
}

# avsync_heartbeat_last_epoch SEGMENT -> the epoch (first TAB field) of the LAST non-empty line in
# SEGMENT that parses as digits -- robust to any stray earlier output. Empty when nothing parses.
avsync_heartbeat_last_epoch() {
  printf '%s\n' "$1" | awk -F'\t' '$1 ~ /^[0-9]+$/ {e=$1} END{if (e!="") print e}'
}

# avsync_heartbeat_is_stale EPOCH NOW STALE_SEC -> exit 0 (STALE, including unparseable/missing) /
# 1 (fresh). Inverted sense vs a plain "is_fresh" check ON PURPOSE -- this lib's caller wants
# "wedged=1 means alert", so a missing/corrupt heartbeat must default to the ALERTING answer, never
# to "fresh" (this repo's standing fail-loud-not-guess convention -- never silently assume health).
avsync_heartbeat_is_stale() {
  local epoch="${1:-}" now="${2:-}" stale="${3:-}"
  case "$epoch$now$stale" in
    *[!0-9]* | "") return 0 ;;   # any non-digit (or empty) field -> not provably fresh -> STALE
  esac
  local age=$(( now - epoch ))
  [ "$age" -ge 0 ] && [ "$age" -le "$stale" ] && return 1   # fresh
  return 0   # stale (negative age -- clock skew/corrupt -- or genuinely too old)
}
