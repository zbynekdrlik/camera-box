#!/usr/bin/env bash
# scripts/cg-chain-verify.sh -- receiver-side ground-truth verdict for the CG chain (#1300).
set -euo pipefail

# Per-hop genlock-FIFO playback verdict for SongPlayer SP-* -> cg OBS (RESOLUME-SNV) -> strih `cg`
# / stream `NDI obs hudba`, the way recording-e2e.sh answers cam2 -> strih -> stream. For each hop
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
#   scripts/cg-chain-verify.sh [--hops "cg-obs strih stream"] [--skew-bound-ms 20] [--min-samples 2]
#       [--asrc-floor-ppm 10] [--soak-hours N] [--interval-s 300] [--csv <path>] [--report-only]
# Exit: 0 all hops PASS (or --report-only); 3 any hop/source FAIL or a hop log is UNREADABLE.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cg-chain-verify.sh
. "$HERE/lib/cg-chain-verify.sh"

HOPS="cg-obs strih stream"
SKEW_BOUND_MS=20
MIN_SAMPLES=2
ASRC_FLOOR_PPM=10
SOAK_HOURS=0
INTERVAL_S=300
CSV_PATH=""
REPORT_ONLY=0

usage() { sed -n '2,40p' "${BASH_SOURCE[0]}"; }

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
    strih)  printf 'cg\n' ;;
    stream) printf 'NDI obs hudba\n' ;;
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

printf '%-7s %-14s %-4s %-5s %-7s %-6s %-5s %-5s %-6s %-5s %-9s %s\n' \
  HOP SOURCE LOCK SAMP SKEWms dDROP dUND dREL dLATE dBRT ASRCppm VERDICT

overall_fail=0
if [ -n "$CSV_PATH" ] && [ ! -s "$CSV_PATH" ]; then
  cg_chain_csv_header > "$CSV_PATH"
fi

run_one_window() {
  local hop log sources src summary verdict_out verdict ts asrc band
  local samples maxskew d_drop d_und d_rel d_late d_brt lck
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
      verdict_out="$(cg_chain_verdict "$summary" "$SKEW_BOUND_MS" "$MIN_SAMPLES")"
      verdict="$(printf '%s\n' "$verdict_out" | head -1)"
      asrc="$(printf '%s\n' "$log" | cg_chain_parse_asrc_ppm "$src")"
      band="$(cg_chain_asrc_in_band "$asrc" "$ASRC_FLOOR_PPM")"
      if [ -n "$summary" ]; then
        # field 2 (latency_ms) is discarded ('_'); the table does not print it.
        IFS='|' read -r samples _ maxskew d_drop d_und d_rel d_late d_brt lck <<<"$summary"
      else
        samples=0; maxskew="-"; d_drop="-"; d_und="-"; d_rel="-"; d_late="-"; d_brt="-"; lck=0
      fi
      # asrc out-of-band (a present reading beyond +/-floor, e.g. the -18 ppm port-collision
      # signature) folds into the verdict; UNKNOWN (no asrc line) never fails.
      if [ "$band" = "0" ]; then
        verdict="FAIL"
        verdict_out="$(printf '%s\nasrc residual %s ppm out of +/-%s band (asrc-residual-floor: -18 ppm is the port-collision signature)\n' "$verdict_out" "$asrc" "$ASRC_FLOOR_PPM")"
      fi
      local lockw="no"; [ "${lck:-0}" = "1" ] && lockw="yes"
      local asrcw="${asrc:-n/a}"; [ -z "$asrc" ] && asrcw="n/a"
      printf '%-7s %-14s %-4s %-5s %-7s %-6s %-5s %-5s %-6s %-5s %-9s %s\n' \
        "$hop" "$src" "$lockw" "$samples" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "$asrcw" "$verdict"
      if [ "$verdict" != "PASS" ]; then
        overall_fail=1
        printf '%s\n' "$verdict_out" | tail -n +2 | sed 's/^/         reason: /'
      fi
      if [ -n "$CSV_PATH" ]; then
        ts="$(_now_utc)"
        cg_chain_csv_row "$ts" "$hop" "$src" "$verdict" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "${asrc:-}" >> "$CSV_PATH"
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

if [ "$overall_fail" -eq 0 ]; then
  echo "OVERALL: PASS"
  exit 0
else
  echo "OVERALL: FAIL"
  [ "$REPORT_ONLY" -eq 1 ] && exit 0
  exit 3
fi
