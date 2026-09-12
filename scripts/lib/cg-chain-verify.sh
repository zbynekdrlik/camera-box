#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (asio-starve-health.sh, frozen-input-health.sh,
# optical-preflight.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# runs it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (scripts/cg-chain-verify.sh) sets its own strict mode.
#
# scripts/lib/cg-chain-verify.sh -- #1300 CG-chain receiver-side verdict: the SHARED, PURE kernel
# for scripts/cg-chain-verify.sh. No I/O, no ssh, no OBS, so it is unit-testable exhaustively
# (tests/harness_cg_chain_verify_1300.rs sources it and feeds fixtures).
#
# It REPLICATES two Rust sources of truth so the tool is self-contained (no runtime dependency on a
# built binary) and Tier-0 testable, while a parity harness pins the replica to the real code so it
# can never drift:
#   * `camera_box::jitter_audit` parse + summarize  -> cg_chain_summarize_window
#   * `camera_box::resolume_playback::evaluate`      -> cg_chain_verdict  (skew <= bound, ZERO
#     dropped/underrun/relock/late-hold/backward-regime deltas, samples >= min)
# The asrc residual band follows `.claude/rules/asrc-residual-floor.md` (a steady +7..+8 ppm is the
# physical Dante-GM-vs-UTC floor, NOT a defect; a value far outside +/-10 -- e.g. the -18 ppm
# port-collision signature -- is what to chase), never "DVS off".
#
# Source-only: pure functions, no side effects at source time.

# cg_chain_strip_high_bytes -- stdin: raw OBS-log tail (possibly carrying invalid UTF-8 bytes, since
#   a `genlock-fifo audit` line carries the `(~= N frames @ ...)` glyph which a PowerShell `gc`
#   re-encode turns into bytes >= 0x80, co-resident on the very line we parse); stdout: the same text
#   with every byte >= 0x80 stripped. Per `.claude/rules/ps-log-byte-safety-extraction.md`: lossless
#   for the ASCII key=value tokens we read, and a downstream awk/python consumer that would reject
#   invalid UTF-8 stays clean. Always exits 0 (an empty input is a normal "no sample this pass").
cg_chain_strip_high_bytes() {
  LC_ALL=C tr -d '\200-\377' || true
}

# cg_chain_enumerate_sources [name_regex] -- stdin: OBS-log text; stdout: the distinct
#   `genlock-fifo audit '<name>'` source names, one per line, in FIRST-SEEN order (never a static
#   list -- the burn-target-enumeration discipline). An optional ERE filters the names (e.g.
#   'sp-.*_video' for the cg-obs hop). Always exits 0.
cg_chain_enumerate_sources() {
  local re="${1:-}"
  cg_chain_strip_high_bytes | awk -v RE="$re" '
    {
      mark = "genlock-fifo audit '\''"
      idx = index($0, mark)
      if (idx == 0) next
      rest = substr($0, idx + length(mark))
      q = index(rest, "'\''")
      if (q == 0) next
      src = substr(rest, 1, q - 1)
      if (RE != "" && src !~ RE) next
      if (!(src in seen)) { seen[src] = 1; order[++n] = src }
    }
    END { for (i = 1; i <= n; i++) print order[i] }
  ' || true
}

# cg_chain_summarize_window <source> -- stdin: OBS-log text; stdout: ONE pipe-delimited summary line
#   for <source>, or EMPTY if the source has no audit line in the window. Replicates
#   `jitter_audit::summarize`: last-minus-first (saturating) deltas + max |ts_head_skew_ms| + sample
#   count + the window's effective latency (from the last sample) + the last sample's locked flag.
#   Field order (pinned by the parity harness):
#     samples|latency_ms|max_abs_skew|d_dropped|d_underruns|d_relocks|d_late_holds|d_backward_regime|last_locked
#   Always exits 0.
cg_chain_summarize_window() {
  local source="${1:-}"
  cg_chain_strip_high_bytes | awk -v SRC="$source" '
    function getval(line, key,    toks, n, i, eq, k, v) {
      n = split(line, toks, /[ \t]+/)
      for (i = 1; i <= n; i++) {
        eq = index(toks[i], "=")
        if (eq == 0) continue
        k = substr(toks[i], 1, eq - 1)
        if (k != key) continue
        v = substr(toks[i], eq + 1)
        # strip a trailing non-numeric tail (e.g. a stray "," never expected here) -- keep an
        # optional leading minus then digits.
        if (v ~ /^-?[0-9]+$/) return v
        if (match(v, /^-?[0-9]+/)) return substr(v, RSTART, RLENGTH)
        return ""
      }
      return ""
    }
    function absval(x) { return x < 0 ? -x : x }
    {
      mark = "genlock-fifo audit '\''"
      idx = index($0, mark)
      if (idx == 0) next
      rest = substr($0, idx + length(mark))
      q = index(rest, "'\''")
      if (q == 0) next
      src = substr(rest, 1, q - 1)
      if (src != SRC) next

      skew = getval($0, "ts_head_skew_ms"); if (skew == "") skew = 0
      drp  = getval($0, "dropped_due");     if (drp  == "") drp  = 0
      und  = getval($0, "underruns");       if (und  == "") und  = 0
      rel  = getval($0, "relocks");         if (rel  == "") rel  = 0
      lat_h= getval($0, "late_holds");      if (lat_h== "") lat_h= 0
      brt  = getval($0, "backward_regime_ticks"); if (brt == "") brt = 0
      lck  = getval($0, "locked");          if (lck  == "") lck  = 0
      lms  = getval($0, "latency_ms");      if (lms  == "") lms  = 0

      n++
      if (n == 1) { f_drp = drp; f_und = und; f_rel = rel; f_lat = lat_h; f_brt = brt; maxskew = absval(skew) }
      else { a = absval(skew); if (a > maxskew) maxskew = a }
      l_drp = drp; l_und = und; l_rel = rel; l_lat = lat_h; l_brt = brt; l_lck = lck; l_lms = lms
    }
    function sat(last, first) { return (last - first) > 0 ? (last - first) : 0 }
    END {
      if (n == 0) exit 0
      printf "%d|%d|%d|%d|%d|%d|%d|%d|%d\n", \
        n, l_lms, maxskew, sat(l_drp, f_drp), sat(l_und, f_und), sat(l_rel, f_rel), \
        sat(l_lat, f_lat), sat(l_brt, f_brt), l_lck
    }
  ' || true
}

# cg_chain_verdict <summary_line> [skew_bound_ms] [min_samples] -- replicate
#   `resolume_playback::evaluate`. stdout: line 1 is `PASS` or `FAIL`; each subsequent line is one
#   evidence-carrying reason. An EMPTY summary_line (source absent from the window) -> FAIL with an
#   "absent" reason. Defaults mirror `PlaybackBounds::default()` (skew 20 ms, min_samples 2). Every
#   failing check contributes its own reason (never short-circuits) so the operator sees the whole
#   picture, exactly like `evaluate`. Always exits 0 (the verdict is in stdout, not the exit code).
cg_chain_verdict() {
  local line="${1:-}" skew_bound="${2:-20}" min_samples="${3:-2}"
  case "$skew_bound"  in '' | *[!0-9]*) skew_bound=20 ;; esac
  case "$min_samples" in '' | *[!0-9]*) min_samples=2 ;; esac
  if [ -z "$line" ]; then
    printf 'FAIL\n'
    printf 'ABSENT -- no genlock-fifo audit window for this source\n'
    return 0
  fi
  local samples lat maxskew d_drop d_und d_rel d_late d_brt lck
  # lat/lck are consumed positionally here (the orchestrator's table uses them); unused in verdict.
  # shellcheck disable=SC2034
  IFS='|' read -r samples lat maxskew d_drop d_und d_rel d_late d_brt lck <<<"$line"
  local -a reasons=()
  if [ "${samples:-0}" -lt "$min_samples" ]; then
    reasons+=("too few audit samples (${samples:-0} < ${min_samples}) -- window too short to confirm flat skew")
  fi
  if [ "${maxskew:-0}" -gt "$skew_bound" ]; then
    reasons+=("skew excursion ${maxskew} ms > bound ${skew_bound} ms -- presentation not flat")
  fi
  [ "${d_drop:-0}" -gt 0 ] && reasons+=("${d_drop} dropped frame(s) in window")
  [ "${d_und:-0}"  -gt 0 ] && reasons+=("${d_und} FIFO underrun(s) in window")
  [ "${d_rel:-0}"  -gt 0 ] && reasons+=("${d_rel} FIFO relock(s) -- clock discipline unstable")
  [ "${d_late:-0}" -gt 0 ] && reasons+=("${d_late} late hold(s) in window")
  [ "${d_brt:-0}"  -gt 0 ] && reasons+=("${d_brt} backward-regime tick(s) -- hold bypassed / frame jump (duplicate)")
  if [ "${#reasons[@]}" -eq 0 ]; then
    printf 'PASS\n'
  else
    printf 'FAIL\n'
    local r
    for r in "${reasons[@]}"; do printf '%s\n' "$r"; done
  fi
  return 0
}

# cg_chain_parse_asrc_ppm <source> -- stdin: OBS-log text; stdout: the NEWEST `estimated=<X>ppm`
#   value (float, sign preserved) for `asrc: source '<source>'`, or EMPTY if that source has no
#   asrc line. Mirrors the asio-starve-health.sh read convention (LC_ALL=C grep -aF ... | tail -1);
#   the trailing quote in the match anchors the exact name ('mbc' never matches 'mbc2'). Always
#   exits 0.
cg_chain_parse_asrc_ppm() {
  local source="${1:-}" line ppm
  line="$(cg_chain_strip_high_bytes | LC_ALL=C grep -aF "asrc: source '$source'" 2>/dev/null | LC_ALL=C grep -aF 'estimated=' | tail -1)" || true
  if [ -n "$line" ]; then
    ppm="$(printf '%s\n' "$line" | LC_ALL=C sed -n 's/.*estimated=\(-\{0,1\}[0-9][0-9]*\(\.[0-9][0-9]*\)\{0,1\}\)ppm.*/\1/p' | tail -1)"
    [ -n "$ppm" ] && printf '%s\n' "$ppm"
  fi
  return 0
}

# cg_chain_asrc_in_band <ppm> <band_ppm> -- stdout: `1` (|ppm| <= band), `0` (out of band), or
#   `UNKNOWN` (ppm empty / non-numeric -- no asrc line this pass, never a false out-of-band). band
#   defaults to 10 (the `.claude/rules/asrc-residual-floor.md` "far outside +/-10" boundary; +8 is
#   the physical floor and passes, the -18 port-collision signature fails). Always exits 0.
cg_chain_asrc_in_band() {
  local ppm="${1:-}" band="${2:-10}"
  case "$band" in '' | *[!0-9.]*) band=10 ;; esac
  if ! printf '%s' "$ppm" | grep -Eq '^-?[0-9]+(\.[0-9]+)?$'; then
    printf 'UNKNOWN\n'
    return 0
  fi
  awk -v p="$ppm" -v b="$band" 'BEGIN { a = (p < 0 ? -p : p); print (a <= b) ? "1" : "0" }'
  return 0
}

# cg_chain_csv_header -- the soak CSV column header (one source-window row per line).
cg_chain_csv_header() {
  printf 'ts_utc,hop,source,verdict,max_abs_skew_ms,d_dropped,d_underruns,d_relocks,d_late_holds,d_backward_regime,asrc_ppm\n'
}

# cg_chain_csv_row <ts> <hop> <source> <verdict> <maxskew> <d_dropped> <d_underruns> <d_relocks>
#   <d_late_holds> <d_backward_regime> <asrc_ppm> -- one CSV data line matching cg_chain_csv_header.
#   Commas in a source name are replaced with ';' so the row never gains a column.
cg_chain_csv_row() {
  local ts="${1:-}" hop="${2:-}" source="${3:-}" verdict="${4:-}" maxskew="${5:-}" \
    d_drop="${6:-}" d_und="${7:-}" d_rel="${8:-}" d_late="${9:-}" d_brt="${10:-}" asrc="${11:-}"
  source="${source//,/;}"
  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$ts" "$hop" "$source" "$verdict" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "$asrc"
}
