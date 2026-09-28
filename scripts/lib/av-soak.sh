#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs this in
# the CALLER's shell (scripts/av-soak.sh, which sets its own strict mode). Every function is safe
# under the caller's `set -euo pipefail`.
#
# scripts/lib/av-soak.sh -- issue 1367: the pure builders + the small read-only probes of the 8 h
# stream-output A/V soak (scripts/av-soak.sh). The decision half is scripts/av_soak_decision.py.
#
# Everything the soak DOES on the rig is an existing primitive (obs_phase2.py record/switch/
# program-scene, obs_burn_filter.py, the issue-1271 rig-busy guard, the issue-830 lease, the
# strih-lx + stream in-place decode wrappers, recording-verdict's merge). This file only holds:
#   - the slot/window arithmetic,
#   - the remote TEXT of two READ-ONLY cam2 reads (the permanent painter's state + a tail of its
#     QPSK marker log) -- cam2 is never written,
#   - the argv builders of the two on-box extracts and the dev1 merge, used by BOTH the --plan
#     printout and the --run execution (plan == run by construction),
#   - the record-volume free-space read: the SAME `:8899/record-dir-stats.json` fetch and the SAME
#     helper, `recordings_free_line_from_stats` (scripts/lib/recordings-free-line.sh, over
#     `bundle_state_gather.recordings_free_line`), that recording-e2e.sh's
#     `check_recordings_free_space` uses (issue 1386).

# av_soak_windows_count DURATION_S SLOT_S -> the number of windows: one at every slot start from 0
# up to and including DURATION_S (so an 8 h run's last window starts at 8 h and the run's window
# starts span the whole duration). Non-numeric / zero slot -> 1.
av_soak_windows_count() {
  local d="${1:-0}" s="${2:-0}"
  case "$d$s" in *[!0-9]* | "") printf '1\n'; return 0 ;; esac
  [ "$s" -gt 0 ] || { printf '1\n'; return 0; }
  printf '%s\n' "$(( d / s + 1 ))"
}

# av_soak_min_secs WINDOW_S -> the merge's --min-secs: 90 % of the recorded sweep (the verdict
# trims the lead/tail edge frames and a ~1 s transition guard per switch; a window that did not
# cover 90 % of its sweep is not a full window).
av_soak_min_secs() {
  local w="${1:-0}"
  case "$w" in *[!0-9]* | "") printf '0\n'; return 0 ;; esac
  printf '%s\n' "$(( w * 9 / 10 ))"
}

# av_soak_marker_rows WINDOW_S -> how many trailing rows of the painter's marker log to copy for a
# window: the markers are emitted every ~0.5 s (2/s); keep (window + 120 s) at 4/s = 2x margin.
av_soak_marker_rows() {
  local w="${1:-0}"
  case "$w" in *[!0-9]* | "") w=0 ;; esac
  printf '%s\n' "$(( (w + 120) * 4 ))"
}

# av_soak_painter_probe_cmd MARKER_LOG -> REMOTE (cam2, read-only) text printing three key=value
# lines: `active=` (systemctl is-active cam2-painter), `run_id=` (the run_id the running painter
# announced on its `frame-probe start:` line this boot -- the QR payload the verdict pins with
# --cam2-run-id; empty when not found) and `markers=`/`markers2=` (the marker log's line count read
# 2 s apart: a growing log = the QPSK marker is being emitted). Ends with an explicit `;` (the
# newline-strip gotcha of a `$(...)`-embedded helper).
av_soak_painter_probe_cmd() {
  local log="${1:-/run/rig-qpsk-markers.csv}"
  printf '%s' "st=\$(systemctl is-active cam2-painter 2>/dev/null || true); "
  printf '%s' "rid=\$(journalctl -b -u cam2-painter -o cat --no-pager -g 'frame-probe start' 2>/dev/null | sed -n 's/.*frame-probe start: .*run_id=\\([0-9][0-9]*\\).*/\\1/p' | tail -n 1); "
  printf '%s' "n1=\$(wc -l < '${log}' 2>/dev/null || echo 0); sleep 2; n2=\$(wc -l < '${log}' 2>/dev/null || echo 0); "
  printf '%s\n' "printf 'active=%s\\nrun_id=%s\\nmarkers=%s\\nmarkers2=%s\\n' \"\$st\" \"\$rid\" \"\$n1\" \"\$n2\";"
}

# av_soak_kv KEY TEXT -> the value of the first `KEY=value` line in TEXT (empty when absent).
av_soak_kv() {
  local key="$1" text="${2:-}"
  printf '%s\n' "$text" | sed -n "s/^${key}=//p" | head -n 1 || true
}

# av_soak_painter_ok ACTIVE MARKERS MARKERS2 -> 0 iff the permanent painter is active AND its
# marker log grew between the two reads (the marker the A/V measurement pairs with is live).
av_soak_painter_ok() {
  local active="${1:-}" m1="${2:-}" m2="${3:-}"
  [ "$active" = "active" ] || return 1
  case "$m1$m2" in *[!0-9]* | "") return 1 ;; esac
  [ "$m2" -gt "$m1" ]
}

# av_soak_marker_snapshot_cmd MARKER_LOG MAX_ROWS -> REMOTE (cam2, read-only) text printing the
# marker log's header line + its last MAX_ROWS data rows (the window's markers, never the whole
# since-boot log). Ends with `;` like the probe above.
av_soak_marker_snapshot_cmd() {
  local log="${1:-/run/rig-qpsk-markers.csv}" rows="${2:-2000}"
  case "$rows" in *[!0-9]* | "") rows=2000 ;; esac
  # The emitter log (src/qpsk_marker.rs) opens with a `# qpsk-params` line, then the column header:
  # keep the non-data lines of its first three (the params line and/or the header), then its last
  # ROWS data rows (read from the file's tail only; data rows start with the marker index).
  printf '%s\n' "head -n 3 '${log}' | grep -v '^[0-9]'; tail -n $((rows + 3)) '${log}' | grep '^[0-9]' | tail -n ${rows};"
}

# av_soak_marker_csv_ok FILE -> 0 iff FILE starts with the QPSK emit-log header
# (`index,frame_id,emit_ts_ns`, src/qpsk_marker.rs serialize_qpsk_marker_log) and has >= 1 row.
av_soak_marker_csv_ok() {
  local f="$1" head skip
  [ -s "$f" ] || return 1
  # leading `#` lines (the emitter's `# qpsk-params` line) precede the header
  skip="$(grep -c '^#' "$f" || true)"
  head="$(sed -n "$((skip + 1))p" "$f" | tr -d '\r')"
  [ "$head" = "index,frame_id,emit_ts_ns" ] || return 1
  [ "$(wc -l < "$f")" -ge $((skip + 2)) ]
}

# av_soak_unacked_cams CAMS -> CAMS minus every camera acked offline in CAMBOX_OFFLINE_ACK (the
# cambox-offline-ack.sh helper must be sourced). An acked box renders black and would fail its cut.
av_soak_unacked_cams() {
  local c out=""
  for c in ${1:-}; do
    cambox_offline_ack_is_acked "$c" && continue
    out="${out:+$out }$c"
  done
  printf '%s\n' "$out"
}

# av_soak_strih_decode_kill_cmd STAMP -> REMOTE (strih-lx) text stopping THIS run's recording-verdict
# strih extract only: the pattern also needs the run's own output name (av-soak-<STAMP>-s...), so
# no other decode on the box is touched. The bracketed pattern never matches the remote shell's own
# command line (the issue-626 pkill self-match gotcha). Ends with `;`.
av_soak_strih_decode_kill_cmd() {
  local stamp="$1"
  printf '%s\n' "pkill -f 'recording-verdic[t] --extract-partial strih .*av-soak-${stamp}-s' || true;"
}

# av_soak_stream_decode_kill_ps STAMP -> PowerShell (stream box) stopping THIS run's
# recording-verdict.exe only: matched by the run's own output name on its command line, never by
# the process name alone.
av_soak_stream_decode_kill_ps() {
  local stamp="$1"
  # shellcheck disable=SC2016  # PowerShell variables, expanded on the box
  printf '%s\n' "Get-CimInstance Win32_Process -Filter \"Name='recording-verdict.exe'\" | Where-Object { \$_.CommandLine -like '*av-soak-${stamp}-s*' } | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue }"
}

# av_soak_onbox_cleanup_lines STRIH_HOST STRIH_OUT_DIR OUT_DIR_WIN STAMP -> the plan lines (never run
# by the soak) removing this run's own on-box decode artifacts (partials, pixel proofs, marker logs,
# schedules), scoped to the run's stamp.
av_soak_onbox_cleanup_lines() {
  local host="$1" sdir="$2" wdir="$3" stamp="$4"
  printf "strih-lx ssh:         ssh <STRIH_USER>@%s \"rm -rf -- %s/av-soak-%s-*\"\n" "$host" "$sdir" "$stamp"
  printf "win-stream-snv Shell: Remove-Item -Recurse -Force '%s'\n" \
    "$(av_soak_win_join "$wdir" "av-soak-${stamp}-*")"
}

# av_soak_win_join DIR NAME -> DIR\NAME (a Windows path on the stream box).
av_soak_win_join() {
  printf '%s\\%s\n' "${1%\\}" "$2"
}

# av_soak_strih_extract_argv OUTVAR DECODE_SCRIPT VERDICT_BIN REMOTE_OUT_DIR LOCAL_OUT_DIR REC
#   CAPTURE_FPS PARTIAL_NAME -> fills the array OUTVAR with the strih-lx in-place extract call, the
# SAME shape recording-e2e.sh's run_strih_extract uses for a Linux strih. The --burn-*-run-id flags
# are omitted on purpose: the soak deploys no camera capture burn, and the OBS measurement burns it
# turns on (strih / stream) stamp the verdict's own default run ids -- the single source.
av_soak_strih_extract_argv() {
  local -n _av_sx="$1"
  local script="$2" bin="$3" rdir="$4" ldir="$5" rec="$6" fps="$7" name="$8"
  _av_sx=("$script" --verdict-bin "$bin" --out-dir "$rdir" --local-out-dir "$ldir"
    --strih-rec "$rec" -- --extract-partial strih --strih "$rec" --capture-fps "$fps"
    --out "$rdir/$name")
}

# av_soak_stream_extract_argv OUTVAR DECODE_SCRIPT WIN_EXE_LOCAL OUT_DIR_WIN LOCAL_OUT_DIR REC
#   STRIH_FPS STREAM_FPS CAM2_RUN_ID MARKER_WIN SCHEDULE_WIN PARTIAL_WIN -> fills OUTVAR with the
# stream-box in-place extract call (recording-verdict-on-stream.sh --execute), the SAME argument
# shape recording-e2e.sh's run_stream_extract passes, plus the window's A/V marker log + schedule.
av_soak_stream_extract_argv() {
  local -n _av_st="$1"
  local script="$2" exe="$3" odir="$4" ldir="$5" rec="$6" sfps="$7" tfps="$8" rid="$9"
  local marker="${10}" sched="${11}" partial="${12}"
  _av_st=("$script" --out-dir "$odir" --stream-rec "$rec" --execute --verdict-exe-local "$exe"
    --local-out-dir "$ldir" -- --extract-partial stream --stream "$rec" --capture-fps "$tfps"
    --strih-emit-fps "$sfps" --stream-capture-fps "$tfps" --cam2-run-id "$rid"
    --av-marker-log "$marker" --switch-schedule "$sched" --out "$partial")
}

# av_soak_merge_argv OUTVAR VERDICT_BIN STRIH_PARTIAL STREAM_PARTIAL MIN_SECS STRIH_FPS STREAM_FPS
#   CAM2_RUN_ID ACK SCHEDULE PIXEL_DIR JSON [AV_EXPECTED_MS] -> fills OUTVAR with the dev1 merge of
# the two partials (the recording-e2e.sh MERGE_ARGS shape without the imag leg: the soak measures
# the stream output). --av-expected-ms is passed only when given; otherwise the verdict's own
# default (av_window::RIG_VIDEO_LEG_OFFSET_MS, the single source) applies.
av_soak_merge_argv() {
  local -n _av_mg="$1"
  local bin="$2" sp="$3" tp="$4" mins="$5" sfps="$6" tfps="$7" rid="$8" ack="$9"
  local sched="${10}" pix="${11}" json="${12}" expected="${13:-}"
  _av_mg=("$bin" --merge-partials "strih=$sp" --merge-partials "stream=$tp" --min-secs "$mins"
    --capture-fps "$sfps" --strih-emit-fps "$sfps" --stream-capture-fps "$tfps"
    --cam2-run-id "$rid" --offline-ack-cams "$ack" --switch-schedule "$sched"
    --out-dir "$pix" --json "$json")
  if [ -n "$expected" ]; then
    _av_mg+=(--av-expected-ms "$expected")
  fi
}

# av_soak_free_space_verdict HOST PORT MIN_FREE_GB SCRIPTS_DIR -> prints "<VERDICT> <free_gb>" for
# the box's OBS record volume: OK / WARN (strictly below MIN_FREE_GB) / UNKNOWN (unreachable or
# unreadable -- never a false WARN). The read is recording-e2e.sh check_recordings_free_space's
# (:8899/record-dir-stats.json); the verdict line is recordings_free_line_from_stats
# (scripts/lib/recordings-free-line.sh, which the caller sources -- scripts/av-soak.sh does), the
# same helper the E2E harness reads it through (issue 1386). Always returns 0.
av_soak_free_space_verdict() {
  local host="$1" port="$2" min_gb="$3" here="$4" stats out
  stats="$(curl -fsS --max-time 30 "http://${host}:${port}/record-dir-stats.json" 2>/dev/null)" || {
    printf 'UNKNOWN -1\n'
    return 0
  }
  out="$(recordings_free_line_from_stats "$stats" "$min_gb" "$here")" || out="UNKNOWN -1"
  printf '%s\n' "${out:-UNKNOWN -1}"
}
