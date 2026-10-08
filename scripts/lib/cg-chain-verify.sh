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
# Issue 1302 adds a TOOL-ONLY rule on top of the replica (never in the Rust evaluate, whose
# jitter_audit input parses neither shallow_depth= nor the canvas rate): a SHALLOW-latched input
# (audit `shallow_depth=` D > 0, issue 1367) is held at D frames by design, so its head age reads
# D x interval; cg_chain_shallow_window grades it by its EXCURSION from that target, and a
# `shallow_latches=` delta (a lock event) FAILs. cg_chain_verdict takes that line as an OPTIONAL
# fourth argument; without it (or with it empty) the verdict is byte-identical to the replica.
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
#   'sp-.*_video' for the cg-obs hop); the match is CASE-INSENSITIVE (both source and regex are
#   tolower()'d) so a SongPlayer input named `SP-1_video`/`sp-1_video` matches either way -- the
#   exact live RESOLUME-SNV source name is operator-overridable at the orchestrator
#   (`CG_CHAIN_CGOBS_SRC_RE`), this just avoids a case-only miss. Always exits 0.
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
      if (RE != "" && tolower(src) !~ tolower(RE)) next
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
        # STRICT integer only -- mirror Rust jitter_audit set-macro val.parse, which leaves the
        # field 0 on any non-integer value. A lenient leading-digit extraction would diverge from
        # the Rust source of truth on a malformed token; the real genlock-fifo audit line only ever
        # emits clean integer tokens, so a non-match returns empty.
        if (v ~ /^-?[0-9]+$/) return v
        return ""
      }
      return ""
    }
    # issue 1302: x + 0 makes the value a NUMBER. getval returns a substr() string, and awk
    # compares two strings as text ("8" above "25"), which made the window max diverge from the
    # numeric max of jitter_audit summarize.
    function absval(x) { x = x + 0; return x < 0 ? -x : x }
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

# cg_chain_shallow_window <source> -- issue 1302, TOOL-ONLY (not part of the evaluate replica).
#   stdin: OBS-log text; stdout: ONE pipe-delimited line for <source>
#     graded|skew_term_ms|depth|target_ms|d_shallow_latches|fps
#   or EMPTY when no audit line of <source> carries a `shallow_depth=` / `shallow_latches=` token
#   (a log from before issue 1367 -> the caller grades exactly as before).
#   Per sample, the skew term is:
#     * a SAMPLED tick (`ts_present` != 0) of a line with `shallow_depth=` D > 0 and a known canvas
#       rate F (the `(~N frames @ F fps)` parenthetical, F > 0): round(|ts_head_skew_ms - D x 1000 / F|)
#       -- the head age's excursion from the latched depth (issue 1367 holds it at D frames);
#     * any other sample: |ts_head_skew_ms|, the replica's absolute term. An unsampled tick
#       (genlock_clear_ts_sample zeroes ts_present + the skew) keeps its 0; D = 0 (not N==1) and an
#       unknown rate (fps 0.000) keep the absolute grading, which is fail-closed.
#   skew_term_ms = the window max over every sample; graded = how many samples were graded
#   target-relative (0 -> the verdict keeps the absolute rule); depth = the last sample's D;
#   target_ms / fps = the last graded sample's round(D x 1000 / F) and its printed rate (empty when
#   none was graded); d_shallow_latches = last-minus-first (saturating) of `shallow_latches=` over the
#   samples carrying it, empty when none does. Always exits 0.
cg_chain_shallow_window() {
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
        if (v ~ /^-?[0-9]+$/) return v
        return ""
      }
      return ""
    }
    function absval(x) { x = x + 0; return x < 0 ? -x : x }
    {
      mark = "genlock-fifo audit '\''"
      idx = index($0, mark)
      if (idx == 0) next
      rest = substr($0, idx + length(mark))
      q = index(rest, "'\''")
      if (q == 0) next
      src = substr(rest, 1, q - 1)
      if (src != SRC) next

      dep = getval($0, "shallow_depth")
      lat = getval($0, "shallow_latches")
      if (dep != "" || lat != "") have_token = 1
      skew = getval($0, "ts_head_skew_ms"); if (skew == "") skew = 0
      skew = skew + 0
      tsp = getval($0, "ts_present")
      d = (dep == "") ? 0 : dep + 0
      fps_s = ""
      if (match($0, /@ [0-9]+(\.[0-9]+)?fps/)) fps_s = substr($0, RSTART + 2, RLENGTH - 5)
      fps = fps_s + 0

      if (d > 0 && fps > 0 && tsp != "0") {
        tgt = d * 1000 / fps
        term = int(absval(skew - tgt) + 0.5)
        graded++
        g_tgt = int(tgt + 0.5); g_fps = fps_s
      } else {
        term = absval(skew)
      }
      n++
      if (n == 1 || term > maxterm) maxterm = term
      l_dep = d
      if (lat != "") {
        if (!have_lat) { f_lat = lat + 0; have_lat = 1 }
        l_lat = lat + 0
      }
    }
    END {
      if (!have_token) exit 0
      dl = ""
      if (have_lat) dl = (l_lat - f_lat) > 0 ? (l_lat - f_lat) : 0
      tg = ""; fp = ""
      if (graded > 0) { tg = g_tgt; fp = g_fps }
      printf "%d|%d|%d|%s|%s|%s\n", graded, maxterm, l_dep, tg, dl, fp
    }
  ' || true
}

# cg_chain_hop_absent_ok <hop> -- issue 1380: 0 when a hop's input may legitimately be absent from
#   its OBS (the optional stream hop: the owner removed its old input on 27.9.2026), so a source with
#   no audit line in the window is reported ABSENT instead of FAIL; 1 for every other hop (the
#   cg-obs SongPlayer sources and the strih `cg` input are expected, a missing one is a FAIL).
cg_chain_hop_absent_ok() {
  [ "${1:-}" = "stream" ]
}

# cg_chain_verdict <summary_line> [skew_bound_ms] [min_samples] [shallow_line] -- replicate
#   `resolume_playback::evaluate`. stdout: line 1 is `PASS` or `FAIL`; each subsequent line is one
#   evidence-carrying reason. An EMPTY summary_line (source absent from the window) -> FAIL with an
#   "absent" reason. Defaults mirror `PlaybackBounds::default()` (skew 20 ms, min_samples 2). Every
#   failing check contributes its own reason (never short-circuits) so the operator sees the whole
#   picture, exactly like `evaluate`. Always exits 0 (the verdict is in stdout, not the exit code).
#   issue 1302, TOOL-ONLY: the optional shallow_line is cg_chain_shallow_window output. When it graded
#   any sample target-relative, its skew term replaces the absolute max skew in the skew check (a
#   named head-age-excursion reason); a d_shallow_latches > 0 adds a re-latch reason. An absent or
#   EMPTY shallow_line leaves the output byte-identical to the evaluate replica.
cg_chain_verdict() {
  local line="${1:-}" skew_bound="${2:-20}" min_samples="${3:-2}" shallow="${4:-}"
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
  local sh_graded="" sh_term="" sh_depth="" sh_target="" sh_dlatch="" sh_fps=""
  if [ -n "$shallow" ]; then
    IFS='|' read -r sh_graded sh_term sh_depth sh_target sh_dlatch sh_fps <<<"$shallow"
  fi
  local -a reasons=()
  if [ "${samples:-0}" -lt "$min_samples" ]; then
    reasons+=("too few audit samples (${samples:-0} < ${min_samples}) -- window too short to confirm flat skew")
  fi
  if [ "${sh_graded:-0}" -gt 0 ]; then
    if [ "${sh_term:-0}" -gt "$skew_bound" ]; then
      reasons+=("head-age excursion ${sh_term} ms from the latched shallow depth ${sh_depth} frame(s) (${sh_target} ms @ ${sh_fps} fps) > bound ${skew_bound} ms -- presentation not flat")
    fi
  elif [ "${maxskew:-0}" -gt "$skew_bound" ]; then
    reasons+=("skew excursion ${maxskew} ms > bound ${skew_bound} ms -- presentation not flat")
  fi
  [ "${d_drop:-0}" -gt 0 ] && reasons+=("${d_drop} dropped frame(s) in window")
  [ "${d_und:-0}"  -gt 0 ] && reasons+=("${d_und} FIFO underrun(s) in window")
  [ "${d_rel:-0}"  -gt 0 ] && reasons+=("${d_rel} FIFO relock(s) -- clock discipline unstable")
  [ "${d_late:-0}" -gt 0 ] && reasons+=("${d_late} late hold(s) in window")
  [ "${d_brt:-0}"  -gt 0 ] && reasons+=("${d_brt} backward-regime tick(s) -- hold bypassed / frame jump (duplicate)")
  [ "${sh_dlatch:-0}" -gt 0 ] && reasons+=("${sh_dlatch} shallow re-latch(es) in window -- each is a lock event (ACQUIRE, GAP RESYNC or pin change)")
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

# cg_chain_parse_audio_facet <source> -- stdin: OBS-log text; stdout: the NEWEST audio parity facet
#   for <source> as ONE pipe line `enabled|delay_ms|pairing_offset_ms`, or EMPTY if the source has
#   no `genlock-fifo audit` line carrying the #1303 audio tokens (a pre-#1303 log). Mirrors the
#   src/jitter_audit.rs AuditSample.audio_* parse: last-seen line wins, the tokens are read by the
#   same whitespace key=value scan (a strict-integer match; a missing token -> its default 0). Always
#   exits 0.
cg_chain_parse_audio_facet() {
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
        if (v ~ /^-?[0-9]+$/) return v
        return ""
      }
      return ""
    }
    {
      mark = "genlock-fifo audit '\''"
      idx = index($0, mark)
      if (idx == 0) next
      rest = substr($0, idx + length(mark))
      q = index(rest, "'\''")
      if (q == 0) next
      src = substr(rest, 1, q - 1)
      if (src != SRC) next
      # only a line that carries the #1303 audio facet counts (pre-#1303 lines have none)
      if (index($0, "audio_delay_ms=") == 0) next
      en = getval($0, "audio_enabled");        if (en  == "") en  = 0
      dl = getval($0, "audio_delay_ms");        if (dl  == "") dl  = 0
      po = getval($0, "audio_pairing_offset_ms"); if (po == "") po = 0
      have = 1; l_en = en; l_dl = dl; l_po = po
    }
    END { if (have) printf "%d|%d|%d\n", l_en, l_dl, l_po }
  ' || true
}

# cg_chain_asrc_in_band <ppm> <band_ppm> -- stdout: `1` (|ppm| <= band), `0` (out of band), or
#   `UNKNOWN` (ppm empty / non-numeric -- no asrc line this pass, never a false out-of-band). band
#   defaults to 10 (the `.claude/rules/asrc-residual-floor.md` "far outside +/-10" boundary; +8 is
#   the physical floor and passes, the -18 port-collision signature fails). This is a single
#   newest-sample gate (cg_chain_parse_asrc_ppm reads `tail -1`), but `estimated=` is itself an
#   EMA-smoothed servo output, so one read is a settled value, not an instantaneous spike; the ~2
#   ppm margin over the +8 floor is deliberate per the floor rule. Always exits 0.
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

# cg_chain_csv_header -- the soak CSV column header (one source-window row per line). The #1303
# audio parity columns (enabled/delay/pairing) are APPENDED after asrc_ppm, then the issue-1302
# d_shallow_latches column, so an existing consumer's earlier columns are byte-stable. For a
# shallow-latched input max_abs_skew_ms carries the GRADED skew term (the head age's excursion from
# its latched depth, cg_chain_shallow_window), so the 24 h flatness plot reads the same bar.
cg_chain_csv_header() {
  printf 'ts_utc,hop,source,verdict,max_abs_skew_ms,d_dropped,d_underruns,d_relocks,d_late_holds,d_backward_regime,asrc_ppm,audio_enabled,audio_delay_ms,audio_pairing_offset_ms,d_shallow_latches\n'
}

# cg_chain_csv_row <ts> <hop> <source> <verdict> <maxskew> <d_dropped> <d_underruns> <d_relocks>
#   <d_late_holds> <d_backward_regime> <asrc_ppm> [audio_enabled] [audio_delay_ms]
#   [audio_pairing_offset_ms] [d_shallow_latches] -- one CSV data line matching cg_chain_csv_header.
#   The three #1303 audio columns and the issue-1302 latch delta are OPTIONAL (default empty) so a
#   caller that has no audio facet / no latch token still emits a column-count-matching row. Commas
#   in a source name are replaced with ';' so the row never gains a column.
cg_chain_csv_row() {
  local ts="${1:-}" hop="${2:-}" source="${3:-}" verdict="${4:-}" maxskew="${5:-}" \
    d_drop="${6:-}" d_und="${7:-}" d_rel="${8:-}" d_late="${9:-}" d_brt="${10:-}" asrc="${11:-}" \
    aud_en="${12:-}" aud_dl="${13:-}" aud_po="${14:-}" d_latch="${15:-}"
  source="${source//,/;}"
  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$ts" "$hop" "$source" "$verdict" "$maxskew" "$d_drop" "$d_und" "$d_rel" "$d_late" "$d_brt" "$asrc" \
    "$aud_en" "$aud_dl" "$aud_po" "$d_latch"
}
