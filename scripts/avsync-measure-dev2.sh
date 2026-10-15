#!/usr/bin/env bash
# scripts/avsync-measure-dev2.sh -- #1331 one lipsync-measurement pass on dev2 (see header below).
set -euo pipefail
#
# WHY (#1331): the A/V-sync (SyncNet) measurement used to run on the ENCODING stream box, where its
# ML load caused OBS render lag + audio buffer steps during live (nález 1335). This runs the SAME
# measurement on dev2 (RTX 5050, CUDA torch) instead -- pulling the SAME production RTMP relay over
# the LAN and writing the SAME heartbeat contract, so dev1's watchdogs read it identically. ONE
# PASS per invocation; a systemd --user timer (systemd/avsync-measure-dev2@.timer, 90 s cadence)
# drives the cadence -- this script never loops.
#
# issue 1441: ONE INSTANCE PER VENUE (`--venue snv|pp|...`, the template avsync-measure-dev2@<venue>).
# The venue's grab URL comes from the venue table (scripts/lib/avsync-venues.sh, derived from the
# declared fleet -- never a hand-typed address); its heartbeat, clip and day log carry the venue. The
# legacy venue (snv, also the default) keeps the old single heartbeat path as a compatibility symlink.
# Only one SyncNet run at a time on dev2: every instance takes the shared SyncNet flock around it.
#
# The pass, mirroring avsync-watchdog.ps1's own loop body exactly:
#   1. ffmpeg-pull a 35 s clip of the venue's RTMP relay (SAME encode params, single-sourced in the
#      pure scripts/lib/avsync-measure.sh) -- a pull failure/timeout leaves a stale/no clip.
#   2. gate the clip through the shared #814 avsync_freshness.py (fail-CLOSED: measure only on a
#      proven-fresh grab; else "no-signal: <reason>").
#   3. on a fresh clip: read the audio dB (ffmpeg volumedetect, SAME clip -- the #813 content-
#      liveness signal) + run av_sync_measure.py under a 180 s cap (a hung run is killed, never
#      wedges the pass), holding the dev2 SyncNet lock (waiting up to the cap for another venue's run).
#   4. write ~/.camera-box/avsync-watchdog-heartbeat-<venue>.txt ATOMICALLY as "<epoch>\t<status>".
#
# NO Discord here -- delivery/alerting is the dev1 watchdogs' job (they read this heartbeat via
# scripts/lib/avsync-heartbeat.sh with AVSYNC_HEARTBEAT_HOST=dev2). This script only measures.
#
# Usage:
#   scripts/avsync-measure-dev2.sh [--venue VENUE] [--grab-url URL] [--repo DIR] [--python PY]
#                                  [--heartbeat FILE]
#   scripts/avsync-measure-dev2.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/avsync-measure.sh
. "$HERE/lib/avsync-measure.sh"
# avsync-measure.sh sets -euo pipefail (for its own sourcing safety); keep it -- this is a one-shot
# script and every fallible external call below is explicitly guarded (`|| rc=$?` / `|| true`), so
# `-e` never aborts on an EXPECTED failure (a dead relay's ffmpeg rc!=0 is handled, not fatal).
set -euo pipefail

# ── config (all env / flag overridable) ──────────────────────────────────────
# issue 1441: the venue this instance measures. The default is the legacy venue (snv) -- the venue
# the old single unit measured, so an instance started without --venue keeps doing exactly that.
VENUE="${AVSYNC_MEASURE_VENUE:-$AVSYNC_LEGACY_HEARTBEAT_VENUE}"
# Empty = derived from the venue (main/resolve_venue_config): the table's RTMP URL and the per-venue
# heartbeat file. An explicit value (env or flag) wins.
GRAB_URL="${AVSYNC_MEASURE_GRAB_URL:-}"
REPO="${AVSYNC_MEASURE_REPO:-$HOME/avsync/syncnet_python}"
PYTHON_BIN="${AVSYNC_MEASURE_PYTHON:-$HOME/avsync/venv/bin/python}"
MEASURE_SCRIPT="${AVSYNC_MEASURE_SCRIPT:-$HOME/avsync/av_sync_measure.py}"
FRESHNESS_SCRIPT="${AVSYNC_MEASURE_FRESHNESS:-$HOME/avsync/avsync_freshness.py}"
HEARTBEAT="${AVSYNC_MEASURE_HEARTBEAT:-}"
MEASURE_CAP_S="${AVSYNC_MEASURE_CAP_S:-180}"
# issue 1441: the dev2-wide SyncNet lock and how long a pass waits for another venue's run (default:
# the measurement cap -- one whole other run fits; the template unit's TimeoutStartSec covers it).
LOCK_FILE="${AVSYNC_MEASURE_LOCK:-$(avsync_measure_lock_path)}"
LOCK_WAIT_S="${AVSYNC_MEASURE_LOCK_WAIT_S:-}"
# #1331 review 🔴: neutralize av_sync_measure.py's own Discord delivery on dev2 -- AIRULESET_NOTIFY
# is pointed here; /dev/null (an empty python script) makes its `python3 <bin> notify ...` a no-op.
# Overridable ONLY for tests; production MUST stay a neutralizing path (never the real airuleset.py).
MEASURE_NOTIFY_BIN="${AVSYNC_MEASURE_NOTIFY_BIN:-/dev/null}"
CLIP_SECS="$AVSYNC_MEASURE_CLIP_SECS"   # default 35, single-sourced in scripts/lib/avsync-measure.sh
FFMPEG="${AVSYNC_MEASURE_FFMPEG:-ffmpeg}"
FFPROBE="${AVSYNC_MEASURE_FFPROBE:-ffprobe}"
# The freshness gate is stdlib-only python -- use the system python3 for it (never requires the
# heavy SyncNet venv), so a broken venv still yields an honest "no-signal" rather than crashing.
FRESHNESS_PYTHON="${AVSYNC_MEASURE_FRESHNESS_PYTHON:-python3}"

case "${1:-}" in
  --help|-h)
    sed -n '4,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

while [ "$#" -gt 0 ]; do
  case "$1" in
    --venue)      VENUE="$2"; shift 2 ;;
    --grab-url)   GRAB_URL="$2"; shift 2 ;;
    --repo)       REPO="$2"; shift 2 ;;
    --python)     PYTHON_BIN="$2"; shift 2 ;;
    --measure-script) MEASURE_SCRIPT="$2"; shift 2 ;;
    --freshness)  FRESHNESS_SCRIPT="$2"; shift 2 ;;
    --heartbeat)  HEARTBEAT="$2"; shift 2 ;;
    --cap-s)      MEASURE_CAP_S="$2"; shift 2 ;;
    --clip-secs)  CLIP_SECS="$2"; shift 2 ;;
    *) echo "avsync-measure-dev2: unknown arg '$1' (try --help)" >&2; exit 2 ;;
  esac
done

log() { printf '%s [avsync-measure-dev2] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# The venue names a file on dev2 (clip, heartbeat, day log): refuse anything but a plain venue name
# before it reaches a path. Whether the venue EXISTS is resolve_venue_config's check (the table).
if ! avsync_venue_name_ok "$VENUE"; then
  log "FATAL: --venue '${VENUE}' is not a venue name (lower-case letters and digits)"
  exit 2
fi

CLIP_DIR="$(avsync_measure_clip_dir)"
CLIP="$(avsync_measure_clip_path "$CLIP_DIR" "$VENUE")"
mkdir -p "$CLIP_DIR"
# Clean this pass's clip on exit (design: cleaned per pass). Only the clip file -- never the dir
# itself (reused every pass; rmdir would race a concurrent pass, and never a broad rm -rf).
trap 'rm -f "$CLIP" 2>/dev/null || true' EXIT

# ── resolve the venue's grab URL + heartbeat (issue 1441) ────────────────────────────────────────
# LEGACY_LINK=1 when this instance keeps the old single heartbeat path pointing at its own file (the
# legacy venue writing its default per-venue file -- never for an operator-chosen --heartbeat path).
LEGACY_LINK=0
resolve_venue_config() {
  if ! avsync_venue_box "$VENUE" >/dev/null; then
    log "FATAL: venue '${VENUE}' is not in the venue table (scripts/lib/avsync-venues.sh) -- nothing measured"
    exit 2
  fi
  if [ -z "$GRAB_URL" ]; then
    GRAB_URL="$(avsync_venue_rtmp_url "$VENUE")" || { log "FATAL: no grab URL for venue '${VENUE}'"; exit 2; }
  fi
  if [ -z "$HEARTBEAT" ]; then
    HEARTBEAT="$(avsync_measure_heartbeat_path "$HOME" "$VENUE")"
    [ "$VENUE" = "$AVSYNC_LEGACY_HEARTBEAT_VENUE" ] && LEGACY_LINK=1
  fi
  return 0
}

# ── keep the old single heartbeat path (the dev1 readers before issue 1441) on the legacy venue ──
# A relative symlink in the same dir, swapped in atomically (ln to a temp name + mv -T), so a reader
# never sees the path missing. Replaces a regular file the old single unit left. Never fatal.
ensure_legacy_heartbeat_link() {
  local legacy name tmp
  legacy="$(avsync_measure_legacy_heartbeat_path "$HOME")"
  name="${HEARTBEAT##*/}"
  if [ -L "$legacy" ] && [ "$(readlink "$legacy" 2>/dev/null || true)" = "$name" ]; then
    return 0
  fi
  tmp="${legacy}.link.$$"
  if ln -sfn "$name" "$tmp" 2>/dev/null && mv -Tf "$tmp" "$legacy" 2>/dev/null; then
    log "legacy heartbeat path $legacy -> $name"
  else
    rm -f "$tmp" 2>/dev/null || true
    log "WARN: could not point $legacy at $name (non-fatal)"
  fi
  return 0
}

# ── write the heartbeat ATOMICALLY (tmp + mv, same file the dev1 watchdogs read) ──────────────────
write_heartbeat() {
  local record="$1" tmp
  mkdir -p "$(dirname "$HEARTBEAT")" 2>/dev/null || true
  tmp="$(mktemp "${HEARTBEAT}.XXXXXX" 2>/dev/null || echo "${HEARTBEAT}.tmp")"
  printf '%s\n' "$record" > "$tmp"
  mv -f "$tmp" "$HEARTBEAT"
}

# ── #1331: ALSO append this pass to the per-day DURABLE log the dev1 report reads ────────────────
# The heartbeat above holds only the LAST pass (overwritten every ~90 s); this appends the SAME
# "<epoch>\t<status>" row to ~/avsync/measurements-<local-day>.tsv so a live broadcast's whole
# history survives for scripts/avsync_report.py to aggregate. A single printf append is atomic for a
# short line (< PIPE_BUF) on a local fs. NEVER fails the pass on an append error -- the heartbeat
# contract is unchanged; a missing durable row only costs the report one sample (log + continue).
avsync_measure_append_day_log() {
  local epoch="$1" status="$2" day path line
  day="$(date -d "@$epoch" +%Y-%m-%d 2>/dev/null || date +%Y-%m-%d)"
  path="$(avsync_measure_log_path "$HOME" "$day" "$VENUE")"
  line="$(avsync_measure_log_line "$epoch" "$status")"
  mkdir -p "$(dirname "$path")" 2>/dev/null || true
  if printf '%s\n' "$line" >> "$path" 2>/dev/null; then
    log "day-log appended: $path"
  else
    log "WARN: day-log append failed for $path (non-fatal)"
  fi
}

# ── measure the audio dB on the SAME clip (#813 content-liveness signal) ──────────────────────────
get_max_db() {
  local clip="$1" vd m
  vd="$("$FFMPEG" -hide_banner -nostats -i "$clip" -af volumedetect -f null /dev/null 2>&1 || true)"
  m="$(printf '%s\n' "$vd" | sed -nE 's/.*max_volume:[[:space:]]*(-?[0-9]+(\.[0-9]+)?)[[:space:]]*dB.*/\1/p' | tail -1)"
  if [ -n "$m" ]; then printf '%s' "$m"; else printf 'unreadable'; fi
}

# ── run av_sync_measure.py bounded to CAP seconds (a hung run is killed, never wedges the pass) ────
# issue 1441: under the dev2-wide SyncNet lock. A pass waits up to LOCK_WAIT_S (default the cap) for
# another venue's run; past that it reports an ERROR status (never two models on the GPU at once). The
# lock fd is inherited by the measurement process, so a run outliving this function still holds it.
run_measure() {
  local clip="$1" out_f err_f rc last_out last_err lock_fd wait_s
  wait_s="${LOCK_WAIT_S:-$MEASURE_CAP_S}"
  if ! command -v flock >/dev/null 2>&1; then
    printf 'ERROR: flock (util-linux) is missing on this box -- the SyncNet lock cannot be taken, not measured'
    return 0
  fi
  mkdir -p "$(dirname "$LOCK_FILE")" 2>/dev/null || true
  if ! : >>"$LOCK_FILE" 2>/dev/null; then
    printf 'ERROR: cannot open the SyncNet lock %s -- not measured' "$LOCK_FILE"
    return 0
  fi
  exec {lock_fd}>>"$LOCK_FILE"
  if ! flock -w "$wait_s" "$lock_fd"; then
    exec {lock_fd}>&-
    printf "ERROR: SyncNet lock busy -- another venue's pass held it longer than %ss, not measured this pass" "$wait_s"
    return 0
  fi
  # Guarded under `set -euo pipefail`: a (rare) mktemp failure must not abort the pass before the
  # heartbeat is written -- degrade to an honest measure-error status instead.
  out_f="$(mktemp 2>/dev/null)" || { exec {lock_fd}>&-; printf 'ERROR: mktemp failed (no temp space?)'; return 0; }
  err_f="$(mktemp 2>/dev/null)" || { exec {lock_fd}>&-; rm -f "$out_f"; printf 'ERROR: mktemp failed (no temp space?)'; return 0; }
  rc=0
  # #1331 review 🔴: av_sync_measure.py's own DEFAULT alert delivery (deliver_alert -> airuleset.py
  # notify, on |offset| >= threshold) would make dev2 PING DISCORD ITSELF -- a duplicate of the dev1
  # watchdog alert and a violation of the "NO Discord on dev2" contract (delivery is the dev1
  # watchdogs' job) + the owner's near-zero-Discord rule. av_sync_measure.py reads its notify binary
  # from AIRULESET_NOTIFY (default ~/devel/airuleset/airuleset.py, which IS deployed on dev2), so
  # neutralize it: point AIRULESET_NOTIFY at an empty python script (/dev/null) -- `python3 /dev/null
  # notify ...` runs nothing, ignores the argv, exits 0, never posts. The measurement itself is
  # UNCHANGED; only the (unwanted) self-delivery is disabled.
  AIRULESET_NOTIFY="$MEASURE_NOTIFY_BIN" \
    timeout "$MEASURE_CAP_S" "$PYTHON_BIN" "$MEASURE_SCRIPT" --media "$clip" --repo "$REPO" \
    >"$out_f" 2>"$err_f" || rc=$?
  exec {lock_fd}>&-
  if [ "$rc" -eq 124 ]; then
    rm -f "$out_f" "$err_f"
    # Wording byte-matches avsync-watchdog.ps1's Invoke-Measurement TIMEOUT line (the retired
    # stream-box measurer) so the heartbeat status tail is identical wherever it runs.
    printf 'TIMEOUT: av_sync_measure.py did not complete within %ss -- killed to prevent a wedged watchdog loop' "$MEASURE_CAP_S"
    return 0
  fi
  last_out="$(tail -n 1 "$out_f" 2>/dev/null || true)"
  last_err="$(tail -n 1 "$err_f" 2>/dev/null || true)"
  rm -f "$out_f" "$err_f"
  if [ -n "$last_out" ]; then printf '%s' "$last_out"; else printf '%s' "$last_err"; fi
}

# ── the single pass ───────────────────────────────────────────────────────────
main() {
  resolve_venue_config
  log "pass start (venue=$VENUE grab_url=$GRAB_URL repo=$REPO heartbeat=$HEARTBEAT)"

  # 1. grab
  local -a grab_argv=()
  mapfile -t grab_argv < <(avsync_measure_ffmpeg_grab_argv "$GRAB_URL" "$CLIP" "$CLIP_SECS")
  rm -f "$CLIP" 2>/dev/null || true
  local grab_rc=0
  "$FFMPEG" "${grab_argv[@]}" >/dev/null 2>&1 || grab_rc=$?

  # 2. gather grab facts (fail-safe sentinels: -1 means "unknown/absent", matching avsync_freshness)
  local size=-1 age=-1 dur=-1 now mt
  if [ -f "$CLIP" ]; then
    size="$(stat -c %s "$CLIP" 2>/dev/null || echo -1)"
    now="$(date +%s)"
    mt="$(stat -c %Y "$CLIP" 2>/dev/null || echo '')"
    if [ -n "$mt" ]; then age=$(( now - mt )); [ "$age" -ge 0 ] || age=0; fi
    dur="$("$FFPROBE" -v error -show_entries format=duration -of csv=p=0 "$CLIP" 2>/dev/null || echo -1)"
    [ -n "$dur" ] || dur=-1
  fi

  # 3. freshness gate (shared #814 pure decider; fail-CLOSED to no-signal)
  local fresh_rc=0 fresh_text
  fresh_text="$("$FRESHNESS_PYTHON" "$FRESHNESS_SCRIPT" \
    --grab-rc "$grab_rc" --size-bytes "$size" --mtime-age-s "$age" --duration-s "$dur" 2>&1)" \
    || fresh_rc=$?
  local reason
  reason="$(avsync_measure_freshness_reason "$fresh_rc" "$fresh_text")"

  # 4. compose + write the heartbeat
  local db="" measure_out="" status record
  if [ -n "$reason" ]; then
    log "NO-SIGNAL - no verdict ($reason)"
  else
    db="$(get_max_db "$CLIP")"
    measure_out="$(run_measure "$CLIP")"
    log "measured: db=$db :: $measure_out"
  fi
  status="$(avsync_measure_status_line "$reason" "$db" "$measure_out")"
  local epoch
  epoch="$(date +%s)"
  record="$(avsync_measure_heartbeat_record "$epoch" "$status")"
  write_heartbeat "$record"
  [ "$LEGACY_LINK" = 1 ] && ensure_legacy_heartbeat_link
  # #1331: mirror this pass into the per-day durable log the dev1 report reads (non-fatal).
  avsync_measure_append_day_log "$epoch" "$status"
  log "pass end (heartbeat written: $status)"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
