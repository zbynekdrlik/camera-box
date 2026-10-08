#!/usr/bin/env bash
# scripts/cg-chain-verify.sh -- receiver-side ground-truth verdict for the CG chain (#1300).
set -euo pipefail

# Per-hop genlock-FIFO playback verdict for SongPlayer SP-* -> cg OBS (RESOLUME-SNV) -> strih `cg`
# (-> stream, only on request -- see HOPS below), the way recording-e2e.sh answers cam2 -> strih ->
# stream. For each hop
# it reads ONE aligned `genlock-fifo audit` window, runs the cadence-agnostic resolume_playback
# verdict REPLICA (scripts/lib/cg-chain-verify.sh -- pinned to camera_box::resolume_playback by
# tests/harness_cg_chain_verify_1300.rs), reads the per-source `asrc: source '<x>' estimated=` ppm
# residual and asserts it within the fleet floor band (.claude/rules/asrc-residual-floor.md), prints
# a per-hop table + overall PASS/FAIL, and EXITS NON-ZERO on FAIL (this is a verdict tool, never an
# always-exit-0 preflight). Pixel-level burn-id contiguity is issue 1301, NOT here; the sender
# contract is issue 1294 (its §8 acceptance = THIS receiver FIFO audit). This accepts the songplayer
# genlock series (songplayer 146-151) from the receiver side.
#
# READER SEAM (per hop): the log tail is supplied explicitly, so the tool is self-contained and
# testable and ships no untested ssh/MCP default:
#   * CG_CHAIN_<HOP>_LOG=<file>  -- read the tail from a file (the test path, and the supervisor's
#                                   path for cg-obs: paste the win-resolume MCP FileRead of the
#                                   RESOLUME-SNV OBS log to a file).
#   * CG_CHAIN_<HOP>_CMD="<cmd>"  -- run <cmd> and read its stdout as the tail (the live path for
#                                   strih/stream: a byte-safe `ssh ... powershell -c "gc <obslog> |
#                                   select -last 4000"`, or a bundle-state :8899 fetch).
# <HOP> is the hop name upper-cased with '-' -> '_' (cg-obs -> CG_OBS, strih -> STRIH, stream ->
# STREAM). Raw bytes are stripped byte-safe (cg_chain_strip_high_bytes) before parsing, per
# .claude/rules/ps-log-byte-safety-extraction.md (the audit line carries the `~=` glyph).
#
# Usage:
#   scripts/cg-chain-verify.sh [--hops "cg-obs strih"] [--skew-bound-ms 20] [--min-samples 2]
#       [--asrc-floor-ppm 10] [--soak-hours N] [--interval-s 300] [--csv <path>] [--report-only]
# Exit: 0 all hops PASS (or --report-only); 3 any hop/source FAIL, a hop log is UNREADABLE, or
#   NO source was verified at all (every requested source ABSENT -> OVERALL: NO-DATA).
#
# HOPS (issue 1380, ROZHODNUTIE 27.9.2026): the default is `cg-obs strih`. The owner removed the
# stream OBS input the stream hop used to read, so the stream hop runs ONLY when asked for
# (`--hops "... stream"` or env CG_CHAIN_HOPS). Its input is CG_CHAIN_STREAM_SRC (default
# `NDIA cg stream`, the CG-named NDI input of the live stream production scene read 27.9.2026; it is
# currently senderless). When that input has no `genlock-fifo audit` line in the window -- it is
# missing, or it has received no frame since OBS start -- the hop prints a named ABSENT row and does
# not fail the run (cg_chain_hop_absent_ok); a missing strih `cg` still FAILs.
#
# SHALLOW-LATCHED INPUTS (issue 1302): an input whose audit line carries `shallow_depth=` D > 0 (the
# strih-lx `CG-obs` at a 3 ms pin, issue 1367) is held at D frames by design, so its head age reads
# D x interval. Its skew term is the head age's EXCURSION from D x 1000 / <canvas fps> (the SKEWms
# column + the CSV skew column; a `shallow:` line under the row names D and the raw max), and a
# `shallow_latches=` delta (column dLTCH, CSV d_shallow_latches) FAILs as a lock event. A log without
# the token is graded exactly as before.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cg-chain-verify.sh
. "$HERE/lib/cg-chain-verify.sh"

HOPS="${CG_CHAIN_HOPS:-cg-obs strih}"
CG_CHAIN_STREAM_SRC="${CG_CHAIN_STREAM_SRC:-NDIA cg stream}"
# issue 1302: the strih input that receives SongPlayer's SP-program (named `CG-obs` on strih-lx since
# songplayer 221 B4). Overridable; the default keeps the historical `cg`.
CG_CHAIN_STRIH_SRC="${CG_CHAIN_STRIH_SRC:-cg}"
SKEW_BOUND_MS=20
MIN_SAMPLES=2
ASRC_FLOOR_PPM=10
SOAK_HOURS=0
INTERVAL_S=300
CSV_PATH=""
REPORT_ONLY=0

usage() { sed -n '2,48p' "${BASH_SOURCE[0]}"; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --hops)           HOPS="$2"; shift 2 ;;
    --skew-bound-ms)  SKEW_BOUND_MS="$2"; shift 2 ;;
    --min-samples)    MIN_SAMPLES="$2"; shift 2 ;;
    --asrc-floor-ppm) ASRC_FLOOR_PPM="$2"; shift 2 ;;
    --soak-hours)     SOAK_HOURS="$2"; shift 2 ;;
    --interval-s)     INTERVAL_S="$2"; shift 2 ;;
    --csv)            CSV_PATH="$2"; shift 2 ;;
    --report-only)    REPORT_ONLY=1; shift ;;
    -h | --help)      usage; exit 0 ;;
    *) echo "cg-chain-verify: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

# The sleep between soak windows is an injectable seam so tests never block on a real sleep.
CG_CHAIN_SLEEP_CMD="${CG_CHAIN_SLEEP_CMD:-sleep}"
# UTC timestamp for CSV rows -- also a seam so a test gets a deterministic value.
_now_utc() { if [ -n "${CG_CHAIN_NOW:-}" ]; then printf '%s\n' "$CG_CHAIN_NOW"; else date -u +%Y-%m-%dT%H:%M:%SZ; fi; }

# Sources per hop: cg-obs enumerates the SongPlayer video inputs dynamically (never a static list).
# The name pattern is an operator-overridable ERE (CASE-INSENSITIVE match in the lib) --
# CG_CHAIN_CGOBS_SRC_RE, default `sp-.*_video` (the issue-1300 Work spec) -- so the EXACT live
# RESOLUME-SNV source name, once confirmed from a real OBS log, is pinned without a code change.
# The downstream hops carry exactly one CG source each.
CG_CHAIN_CGOBS_SRC_RE="${CG_CHAIN_CGOBS_SRC_RE:-sp-.*_video}"
_hop_sources() {
  local hop="$1" log="$2"
  case "$hop" in
    cg-obs) printf '%s\n' "$log" | cg_chain_enumerate_sources "$CG_CHAIN_CGOBS_SRC_RE" ;;
    strih)  printf '%s\n' "$CG_CHAIN_STRIH_SRC" ;;
    stream) printf '%s\n' "$CG_CHAIN_STREAM_SRC" ;;
    *) : ;;
  esac
}

# Read a hop's raw log tail via the explicit seam. Empty stdout => unreadable (caller handles).
_read_hop() {
  local hop="$1" hopup cmd_var log_var
  hopup="$(printf '%s' "$hop" | tr 'a-z-' 'A-Z_')"
  log_var="CG_CHAIN_${hopup}_LOG"
  cmd_var="CG_CHAIN_${hopup}_CMD"
  if [ -n "${!log_var:-}" ]; then
    [ -f "${!log_var}" ] && cat "${!log_var}" || true
  elif [ -n "${!cmd_var:-}" ]; then
    eval "${!cmd_var}" 2>/dev/null || true
  else
    echo "cg-chain-verify: hop '$hop' has no reader -- set ${log_var} (file) or ${cmd_var} (live ssh/MCP/bundle-state tail)" >&2
  fi
}

# ceil(hours*3600/interval), floored at 1 (soak 0 => one window).
_iterations() {
  awk -v h="$SOAK_HOURS" -v i="$INTERVAL_S" 'BEGIN {
    if (i+0 <= 0) i = 300
    n = (h+0) * 3600 / i
    n = (n == int(n)) ? n : int(n) + 1
    if (n < 1) n = 1
    print n
  }'
}

if [ -n "$CSV_PATH" ] && [ ! -s "$CSV_PATH" ]; then
  cg_chain_csv_header > "$CSV_PATH"
elif [ -n "$CSV_PATH" ] && [ "$(head -n 1 "$CSV_PATH")" != "$(cg_chain_csv_header)" ]; then
  # issue 1302 appended a column: appending rows to a CSV written by an older tool version would make
  # it ragged, so refuse it loudly instead.
  echo "cg-chain-verify: $CSV_PATH carries a different column header (an older tool version?); appending would make it ragged -- use a new --csv path" >&2
  exit 2
fi

# VERDICT stays the LAST column: rig-health-audit.py cg_chain_detail_from_output reads it as the
# last token of a row (issue 1302 put the dLTCH column before it).
printf '%-7s %-14s %-4s %-5s %-7s %-6s %-5s %-5s %-6s %-5s %-9s %-12s %-6s %s\n' \
  HOP SOURCE LOCK SAMP SKEWms dDROP dUND dREL dLATE dBRT ASRCppm AUDIO dLTCH VERDICT

overall_fail=0
verified=0

run_one_window() {
  local hop log sources src summary verdict_out verdict ts asrc band
  local samples maxskew d_drop d_und d_rel d_late d_brt lck
  local shallow sh_graded sh_term sh_depth sh_target sh_dlatch sh_fps rawskew dltchw
  for hop in $HOPS; do
    log="$(_read_hop "$hop")"
    if [ -z "$log" ]; then
      printf '%-7s %-14s %s\n' "$hop" "-" "UNREADABLE (no audit window)"
      overall_fail=1
      continue
    fi
    sources="$(_hop_sources "$hop" "$log")"
    if [ -z "$sources" ]; then
      printf '%-7s %-14s %s\n' "$hop" "-" "NO SOURCES (no sp-*_video audit lines)"
      overall_fail=1
      continue
    fi
    while IFS= read -r src; do
      [ -z "$src" ] && continue
      summary="$(printf '%s\n' "$log" | cg_chain_summarize_window "$src")"
      # issue 1380: an optional hop whose input is not on that OBS (no audit line in the window) is
      # a named ABSENT row -- never a false FAIL, never counted as PASS.
      if [ -z "$summary" ] && cg_chain_hop_absent_ok "$hop"; then
        printf '%-7s %-14s %s\n' "$hop" "$src" "ABSENT"
        printf '         reason: no genlock-fifo audit line for %s on the %s hop (input missing, or no frame received since OBS start; set CG_CHAIN_STREAM_SRC)\n' "'$src'" "$hop"
        if [ -n "$CSV_PATH" ]; then
          ts="$(_now_utc)"
          cg_chain_csv_row "$ts" "$hop" "$src" "ABSENT" "" "" "" "" "" "" "" "" "" "" >> "$CSV_PATH"
        fi
        continue
      fi
      # issue 1302: the shallow-latch facet (EMPTY on a log from before issue 1367 -> the verdict and
      # every printed value stay exactly as before).
      shallow="$(printf '%s\n' "$log" | cg_chain_shallow_window "$src")"
      verdict_out="$(cg_chain_verdict "$summary" "$SKEW_BOUND_MS" "$MIN_SAMPLES" "$shallow")"
      verdict="$(printf '%s\n' "$verdict_out" | head -1)"
      asrc="$(printf '%s\n' "$log" | cg_chain_parse_asrc_ppm "$src")"
      band="$(cg_chain_asrc_in_band "$asrc" "$ASRC_FLOOR_PPM")"
      if [ -n "$summary" ]; then
        # field 2 (latency_ms) is discarded ('_'); the table does not print it.
        IFS='|' read -r samples _ maxskew d_drop d_und d_rel d_late d_brt lck <<<"$summary"
      else
        samples=0; maxskew="-"; d_drop="-"; d_und="-"; d_rel="-"; d_late="-"; d_brt="-"; lck=0
      fi
      sh_graded=0; sh_term=""; sh_depth=""; sh_target=""; sh_dlatch=""; sh_fps=""
      if [ -n "$shallow" ]; then
        IFS='|' read -r sh_graded sh_term sh_depth sh_target sh_dlatch sh_fps <<<"$shallow"
      fi
      # SKEWms (and the CSV skew column) carry the GRADED term: the excursion from the latched depth
      # for a shallow-latched input, the absolute max skew otherwise.
      rawskew="$maxskew"
      [ "${sh_graded:-0}" -gt 0 ] && maxskew="$sh_term"
      dltchw="${sh_dlatch:--}"
      # asrc out-of-band (a present reading beyond +/-floor, e.g. the -18 ppm port-collision
      # signature) folds into the verdict; UNKNOWN (no asrc line) never fails.
      if [ "$band" = "0" ]; then
        verdict="FAIL"
        verdict_out="$(printf '%s\nasrc residual %s ppm out of +/-%s band (asrc-residual-floor: -18 ppm is the port-collision signature)\n' "$verdict_out" "$asrc" "$ASRC_FLOOR_PPM")"
      fi
      local lockw="no"; [ "${lck:-0}" = "1" ] && lockw="yes"
      local asrcw="${asrc:-n/a}"; [ -z "$asrc" ] && asrcw="n/a"
      # #1303: receiver-side audio parity facet, REPORT-ONLY (a column, never folded into the
      # verdict) -- until the SongPlayer sender half (songplayer#151) ships, the sp-* audio is
      # uncalibrated, so a facet fault must not FAIL the CG-chain FIFO verdict. `n/a` = a pre-#1303
      # log with no audio tokens.
      local audio_facet aud_en aud_dl aud_po audiow
      audio_facet="$(printf '%s\n' "$log" | cg_chain_parse_audio_facet "$src")"
      if [ -n "$audio_facet" ]; then
        IFS='|' read -r aud_en aud_dl aud_po <<<"$audio_facet"
        local audonoff="off"; [ "${aud_en:-0}" = "1" ] && audonoff="on"
        audiow="${audonoff}/d${aud_dl}/p${aud_po}"
      else
        aud_en=""; aud_dl=""; aud_po=""; audiow="n/a"
      fi
      printf '%-7s %-14s %-4s %-5s %-7s %-6s %-5s %-5s %-6s %-5s %-9s %-12s %-6s %s\n' \
        "$hop" "$src" "$lockw" "$samples" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "$asrcw" "$audiow" "$dltchw" "$verdict"
      if [ "${sh_graded:-0}" -gt 0 ] && [ -n "$sh_target" ]; then
        printf '         shallow: worst sample at latched depth %s frame(s) = %s ms @ %s fps; SKEWms is its head-age excursion (raw max |skew| %s ms)\n' \
          "$sh_depth" "$sh_target" "$sh_fps" "$rawskew"
      elif [ "${sh_graded:-0}" -gt 0 ]; then
        printf '         shallow: the worst sample carried no latched depth or canvas rate, so SKEWms is its absolute head age (raw max |skew| %s ms)\n' \
          "$rawskew"
      fi
      verified=$((verified + 1))
      if [ "$verdict" != "PASS" ]; then
        overall_fail=1
        printf '%s\n' "$verdict_out" | tail -n +2 | sed 's/^/         reason: /'
      fi
      if [ -n "$CSV_PATH" ]; then
        ts="$(_now_utc)"
        cg_chain_csv_row "$ts" "$hop" "$src" "$verdict" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "${asrc:-}" "${aud_en:-}" "${aud_dl:-}" "${aud_po:-}" "${sh_dlatch:-}" >> "$CSV_PATH"
      fi
    done <<<"$sources"
  done
}

iters="$(_iterations)"
w=0
while [ "$w" -lt "$iters" ]; do
  w=$((w + 1))
  [ "$iters" -gt 1 ] && echo "--- window $w/$iters ---"
  run_one_window
  if [ "$w" -lt "$iters" ]; then "$CG_CHAIN_SLEEP_CMD" "$INTERVAL_S" || true; fi
done

if [ "$overall_fail" -eq 0 ] && [ "$verified" -eq 0 ]; then
  # issue 1380: every requested source was ABSENT -- nothing was verified, so never a PASS (a typo in
  # CG_CHAIN_STREAM_SRC must not look like success).
  echo "OVERALL: NO-DATA"
  [ "$REPORT_ONLY" -eq 1 ] && exit 0
  exit 3
elif [ "$overall_fail" -eq 0 ]; then
  echo "OVERALL: PASS"
  exit 0
else
  echo "OVERALL: FAIL"
  [ "$REPORT_ONLY" -eq 1 ] && exit 0
  exit 3
fi
