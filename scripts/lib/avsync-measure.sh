#!/usr/bin/env bash
# scripts/lib/avsync-measure.sh -- #1331 PURE helpers for the dev2 lipsync measurer.
set -euo pipefail
#
# For scripts/avsync-measure-dev2.sh (the systemd --user one-pass measurer on dev2). No network,
# no ffmpeg, no python here -- pure string/arithmetic, unit-tested by sourcing (run_sourced), the
# SAME "pure decision library" shape as scripts/lib/avsync-heartbeat.sh / avsync_freshness.py.
#
# WHY THIS FILE EXISTS (#1331): the lipsync measurement moved OFF the encoding stream box (its ML
# load caused OBS render lag + audio buffer steps during live) onto dev2 (GPU). The measurement
# CONTRACT is unchanged -- ffmpeg grabs the SAME 35 s of the SAME production RTMP relay with the
# SAME encode params avsync-watchdog.ps1:99 uses, hands it to av_sync_measure.py, and writes the
# SAME "<epoch>\t<status>" heartbeat line avsync-watchdog.ps1's Write-Heartbeat writes -- so dev1's
# heartbeat watchdogs read it identically wherever it runs. This lib is the single source of truth
# for the encode params, the status-line composition, the fail-CLOSED freshness reason, and the
# clip temp path, so the orchestrator holds only I/O.
#
# Source-only: defines the functions below; runs nothing on its own (beyond sourcing the venue table).
#
# issue 1441: the measurement runs once PER VENUE (snv, pp, ...). Which venues exist, and the grab URL
# of each, come from the ONE venue table scripts/lib/avsync-venues.sh (derived from the declared fleet).
# The per-venue file names below (clip, heartbeat, day log) keep two venues' passes apart on dev2.

# shellcheck source=scripts/lib/avsync-venues.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/avsync-venues.sh"

# Canonical cadence (seconds). SINGLE SOURCE OF TRUTH for the systemd timer's OnUnitInactiveSec and
# any documentation of the loop period -- mirrors avsync-watchdog.ps1's ~90 s natural cadence.
AVSYNC_MEASURE_CADENCE_S="${AVSYNC_MEASURE_CADENCE_S:-90}"
# Grab length (seconds). MUST byte-match avsync-watchdog.ps1:99's `-t 35` so the SyncNet input
# window is identical wherever the grab runs.
AVSYNC_MEASURE_CLIP_SECS="${AVSYNC_MEASURE_CLIP_SECS:-35}"

# avsync_measure_cadence_s -> the canonical cadence in seconds (the timer + any loop use this ONE
# value; never a second literal). Echoed so the caller/test reads it without touching the global.
avsync_measure_cadence_s() {
  printf '%s' "$AVSYNC_MEASURE_CADENCE_S"
}

# avsync_measure_clip_dir -> a writable per-pass temp dir for the grabbed clip: $XDG_RUNTIME_DIR
# when set + a writable dir, else /tmp (design: cleaned per pass by the caller). Never $HOME (the
# clip is throwaway; keep it off any synced/persistent path).
avsync_measure_clip_dir() {
  local base="${XDG_RUNTIME_DIR:-}"
  if [ -n "$base" ] && [ -d "$base" ] && [ -w "$base" ]; then
    printf '%s/avsync-measure-dev2' "$base"
  else
    printf '/tmp/avsync-measure-dev2'
  fi
}

# avsync_measure_clip_path [DIR] [VENUE] -> the clip file path inside DIR (default:
# avsync_measure_clip_dir). With VENUE the name carries it (issue 1441): two venues' instances share the
# clip dir, and one clip name would let one pass grab over the other's clip.
avsync_measure_clip_path() {
  local dir="${1:-$(avsync_measure_clip_dir)}" venue="${2:-}"
  if [ -n "$venue" ]; then
    printf '%s/live-clip-%s.mp4' "$dir" "$venue"
  else
    printf '%s/live-clip.mp4' "$dir"
  fi
}

# avsync_measure_lock_path -> the dev2-wide SyncNet lock (issue 1441): every venue's instance takes it
# around its SyncNet run, so two live venues never run the model on the GPU at the same time. It sits
# in the shared clip dir (one per dev2 user).
avsync_measure_lock_path() {
  printf '%s/syncnet.lock' "$(avsync_measure_clip_dir)"
}

# avsync_measure_heartbeat_path HOME VENUE -> the venue's heartbeat file, the one the dev1 watchdogs
# read (`<HOME>/.camera-box/avsync-watchdog-heartbeat-<venue>.txt`, name from the venue table).
avsync_measure_heartbeat_path() {
  printf '%s/.camera-box/%s' "${1:-$HOME}" "$(avsync_venue_heartbeat_file "${2:-}")"
}

# avsync_measure_legacy_heartbeat_path HOME -> the old single heartbeat file (before issue 1441). The
# legacy venue's instance keeps it as a compatibility symlink to its per-venue file.
avsync_measure_legacy_heartbeat_path() {
  printf '%s/.camera-box/avsync-watchdog-heartbeat.txt' "${1:-$HOME}"
}

# Per-read I/O timeout (microseconds) on the RTMP INPUT -- a dead/half-open relay delivers no data,
# so ffmpeg exits non-zero within this window and the freshness gate yields an honest no-signal
# heartbeat, rather than the pass hanging until the systemd TimeoutStartSec SIGTERMs it (which would
# leave NO heartbeat). 15 s is far above a healthy 25 fps stream's inter-packet gap, so it never
# fires on a live grab; the timer RESETS on every read, so it does NOT cap the 35 s clip length.
AVSYNC_MEASURE_RW_TIMEOUT_US="${AVSYNC_MEASURE_RW_TIMEOUT_US:-15000000}"

# avsync_measure_ffmpeg_grab_argv URL CLIP [SECS] -> the ffmpeg grab argv, ONE ARG PER LINE, the
# SINGLE SOURCE OF TRUTH for the encode params. Encode params byte-mirror avsync-watchdog.ps1:99
# (scale=1280:-2,fps=25, libx264 veryfast crf26, aac 16 kHz mono); the leading `-rw_timeout` is an
# INPUT option (before -i) that the ps1 lacks -- a deliberate fail-closed improvement, see above:
#   ffmpeg -v error -y -rw_timeout US -i URL -t SECS -vf scale=1280:-2,fps=25 -c:v libx264
#          -preset veryfast -crf 26 -c:a aac -ar 16000 -ac 1 CLIP
# Emitted one-arg-per-line so the caller reads it with `mapfile -t` into an array (URL/CLIP may
# never be word-split). The caller prepends the ffmpeg binary.
avsync_measure_ffmpeg_grab_argv() {
  local url="$1" clip="$2" secs="${3:-$AVSYNC_MEASURE_CLIP_SECS}"
  printf '%s\n' \
    -v error -y -rw_timeout "$AVSYNC_MEASURE_RW_TIMEOUT_US" -i "$url" -t "$secs" \
    -vf 'scale=1280:-2,fps=25' -c:v libx264 -preset veryfast -crf 26 \
    -c:a aac -ar 16000 -ac 1 "$clip"
}

# avsync_measure_freshness_reason FRESH_RC FRESH_TEXT -> echoes the NO-SIGNAL reason string when the
# grab is NOT proven fresh, or NOTHING (empty) when it is OK. Mirrors avsync-watchdog.ps1's
# fail-CLOSED gate exactly: measure ONLY when avsync_freshness.py exited 0 AND printed a literal
# "OK"; ANY other outcome yields a reason (the gate's own "NO-SIGNAL: <reason>" when present, else a
# "freshness gate unavailable" fallback -- a verdict is never emitted on unprovable freshness).
# FRESH_TEXT is normalized for a trailing CR (a Windows-origin gate output would carry one; harmless
# on Linux) before the OK match.
avsync_measure_freshness_reason() {
  local rc="$1" text="$2"
  text="${text//$'\r'/}"
  if [ "$rc" = "0" ] && printf '%s' "$text" | grep -qE '^[[:space:]]*OK[[:space:]]*$'; then
    return 0   # fresh -- no reason
  fi
  local reason
  reason="$(printf '%s' "$text" | sed -nE 's/.*NO-SIGNAL:[[:space:]]*(.*)$/\1/p' | tail -1)"
  if [ -n "$reason" ]; then
    printf '%s' "$reason"
  else
    printf 'freshness gate unavailable (rc=%s): %s' "$rc" "$text"
  fi
}

# avsync_measure_status_line REASON DB MEASURE_OUT -> the heartbeat STATUS text, byte-identical to
# avsync-watchdog.ps1's Write-Heartbeat: a non-empty REASON -> "no-signal: <reason>"; else
# "measured: db=<db> <measure_out>". This is the contract scripts/avsync_lineup.py + the dev1
# heartbeat watchdogs parse -- never deviate from these two exact prefixes.
avsync_measure_status_line() {
  local reason="$1" db="$2" out="$3"
  if [ -n "$reason" ]; then
    printf 'no-signal: %s' "$reason"
  else
    printf 'measured: db=%s %s' "$db" "$out"
  fi
}

# avsync_measure_heartbeat_record EPOCH STATUS -> the single heartbeat line "<epoch>\t<status>"
# (the exact "<epoch_seconds>\t<status text>" contract avsync_heartbeat_last_epoch /
# avsync_heartbeat_last_status parse). No trailing newline -- the caller adds one when it writes.
avsync_measure_heartbeat_record() {
  printf '%s\t%s' "$1" "$2"
}

# ── #1331 verified-A/V report: the per-day DURABLE measurement log ─────────────────────────────
# The heartbeat file holds ONLY the last pass; a live broadcast's earlier measurements are
# overwritten every ~90 s, so the owner saw only "two measurements" on Discord. To report that the
# A/V was VERIFIED across the WHOLE broadcast, the measurer ALSO appends every pass's record as one
# row to a per-day TSV that the dev1 report (scripts/avsync_report.py) reads. These two builders are
# the single source of truth for the log PATH and the log ROW; the append I/O itself lives in the
# orchestrator (scripts/avsync-measure-dev2.sh), keeping this lib pure.

# avsync_measure_log_path [BASE] DAY [VENUE] -> the per-day log path
# "<BASE>/avsync/measurements-<VENUE>-<DAY>.tsv" ("measurements-<DAY>.tsv" without a VENUE, the
# pre-issue-1441 name). BASE defaults to $HOME; DAY is a YYYY-MM-DD string the caller derives from the
# pass epoch in LOCAL time. The dev1 report reads each venue's today + yesterday files by this shape --
# one file per venue, so two venues' broadcasts never merge into one session.
avsync_measure_log_path() {
  local base="${1:-$HOME}" day="$2" venue="${3:-}"
  if [ -n "$venue" ]; then
    printf '%s/avsync/measurements-%s-%s.tsv' "$base" "$venue" "$day"
  else
    printf '%s/avsync/measurements-%s.tsv' "$base" "$day"
  fi
}

# avsync_measure_log_line EPOCH STATUS -> one per-day-log ROW, BYTE-IDENTICAL to the heartbeat
# record ("<epoch>\t<status>") so the dev1 report parses a row exactly like it parses a heartbeat.
# No trailing newline -- the caller adds one when it appends.
avsync_measure_log_line() {
  avsync_measure_heartbeat_record "$1" "$2"
}
