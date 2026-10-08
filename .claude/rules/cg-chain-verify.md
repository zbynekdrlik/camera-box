---
paths:
  - "scripts/cg-chain-verify.sh"
  - "scripts/lib/cg-chain-verify.sh"
  - "tests/harness_cg_chain_verify_1300.rs"
  - "tests/python/test_cg_chain_verify_hops_1380.py"
  - "tests/python/test_cg_chain_verify_shallow_1302.py"
---

# CG-chain receiver-side verdict (#1300)

`scripts/cg-chain-verify.sh` is the CG chain's equivalent of `recording-e2e.sh` for the FIFO-audit
level: per hop (SongPlayer `SP-*` → cg OBS/RESOLUME-SNV → strih `cg`, stream only on request) it
reads ONE aligned `genlock-fifo audit` window, runs the cadence-agnostic playback verdict, reads
the per-source `asrc: source '<x>' estimated=` ppm residual, prints a per-hop table + overall
PASS/FAIL, and **exits non-zero (3) on FAIL** (a verdict tool, NOT an always-exit-0 preflight). It
is what ACCEPTS the songplayer genlock series (songplayer 146–151) from the receiver side — the
sender contract is issue 1294 (its §8 acceptance = this receiver audit). Pixel-level burn-id
contiguity through the chain is issue 1301, NOT here.

## The verdict is a REPLICA pinned to Rust — never re-derive the thresholds

The decision is `camera_box::resolume_playback::evaluate` (skew ≤ 20 ms, ZERO
`dropped_due`/`underruns`/`relocks`/`late_holds`/`backward_regime_ticks` deltas, samples ≥ 2) and
the window summary is `camera_box::jitter_audit` parse+`summarize` (last-minus-first saturating
deltas, `max_abs_head_skew_ms`). Those live in Rust (probe-free, default features), and the real
CLI `genlock-jitter-report --verdict-source '<name>'` already runs them — but it needs a built
binary and is not Tier-0 testable. So `scripts/lib/cg-chain-verify.sh` is a PURE bash/awk REPLICA
(`cg_chain_summarize_window` + `cg_chain_verdict`), and `tests/harness_cg_chain_verify_1300.rs` is
the PARITY GATE: it feeds the SAME raw-log + the SAME windows to BOTH the Rust functions and the
bash replica and asserts identical summaries + PASS/FAIL. **If you change the Rust bounds
(`PlaybackBounds`) or `jitter_audit::summarize`, the parity test goes RED — update the replica to
match, never let them drift.** Do not re-derive thresholds in bash from scratch.

**awk compares a `substr()` result as TEXT.** `getval()` returns a `substr()` string, so before
issue 1302 the window max compared skews lexicographically: 8 then 25 read 8, and 99 then 100 read
99. The parity fixtures of the time all happened to sort the same both ways. `absval` now does
`x = x + 0`, and the harness carries a fixture whose text order and number order differ
(`FIX_NUMERIC`). Coerce every awk value you compare or take a max of (`+ 0`), and give each new
parity fixture values whose text order differs from their number order.

## Shallow-latched inputs (issue 1302) — a TOOL-ONLY rule on top of the replica

**The problem it fixes.** A shallow N==1 input is held at a latched depth D by design (issue 1367,
`.claude/rules/genlock-n1-pin-derived-depth.md`). Its audit line carries `shallow_depth=` D. The
strih-lx `CG-obs` sits at a 3 ms pin. Its `ts_head_skew_ms` (`wall_now - head stamp`) therefore
reads about D × interval: 100 ms at D=3 on the 30 fps canvas. The absolute 20 ms bar FAILed every
window of a correctly latched input.

**`cg_chain_shallow_window <source>`** grades each sample's skew term:
- **Target-relative.** A SAMPLED tick (`ts_present` != 0) with D > 0 and a known canvas rate F gets
  `round(|ts_head_skew_ms − D × 1000 / F|)`. F comes from the `(≈N frames @ F fps)` parenthetical.
  This is the head age's excursion from the latched depth.
- **Absolute, unchanged.** Every other sample gets `|ts_head_skew_ms|`:
  - an unsampled tick (`genlock_clear_ts_sample` zeroes the skew) keeps its 0;
  - D = 0 (not N==1) stays absolute;
  - an unknown rate (`@ 0.000fps`) stays absolute, which is fail-closed.

Its output is `graded|skew_term|depth|target_ms|d_shallow_latches|fps`. It is EMPTY on a log that
carries neither shallow token.

**`cg_chain_verdict`'s optional fourth argument** takes that line:
- when `graded > 0`, its skew term replaces the absolute max, with a named `head-age excursion …
  from the latched shallow depth …` reason;
- `d_shallow_latches > 0` adds a `shallow re-latch(es)` reason (a lock event: ACQUIRE / GAP RESYNC
  / pin change);
- an absent or EMPTY argument is byte-identical to the replica. The parity harness pins that path to
  `evaluate` too (`verdict_with_an_empty_shallow_argument_matches_evaluate`).

**Why tool-only, not in `resolume_playback::evaluate`:**
- `evaluate`'s one consumer is `genlock-jitter-report --verdict-source`, a manual resolume
  maintenance verify;
- its `jitter_audit` input parses neither `shallow_depth=` nor the canvas rate, so porting the
  rule means new `AuditSample` fields (kept out of the byte-locked `--json`, #757) and a Rust change
  no Tier-0 lane can compile;
- the SongPlayer acceptance (the songplayer genlock series) runs this tool.

So `--verdict-source` still grades a shallow input's ABSOLUTE head age and FAILs it by design. Verify
the CG feed with this tool instead (the ops skill says so at its recipe).

**What the operator sees:**
- **table:** `SKEWms` shows the GRADED term. A `shallow:` line under the row names D, the target and
  the raw max. A new `dLTCH` column sits BEFORE `VERDICT`, because `rig-health-audit.py`'s
  `cg_chain_detail_from_output` reads the verdict as the LAST token of a row; keep VERDICT last.
- **CSV:** `d_shallow_latches` is APPENDED after the audio columns. `max_abs_skew_ms` carries the
  graded term, so the 24 h flatness plot reads the same ±20 ms bar.

Pinned by `tests/python/test_cg_chain_verify_shallow_1302.py`, which includes two real strih-lx
`CG-obs` lines.

**Live replay 8.10.2026:** strih-lx `CG-obs`, 58 samples, D=3. The window FAILs on real faults only:
- one 133 ms sample = a 33 ms excursion, one extra frame;
- `dropped_due` +5 and `underruns` +37;
- `d_shallow_latches` 0.

Before the fix it read `skew excursion 133 ms` on every window.

## Hops, sources, and the reader seam

- `cg-obs` → each SongPlayer video input, ENUMERATED dynamically from the log (never a static list
  — burn-target-enumeration discipline). The name pattern is an operator-overridable ERE,
  `CG_CHAIN_CGOBS_SRC_RE` (default `sp-.*_video`, the issue-1300 Work spec), matched
  CASE-INSENSITIVELY (so `SP-1_video`/`sp-1_video` both match). **The EXACT live RESOLUME-SNV source
  name is NOT yet confirmed against a real OBS log (the rig is off) — confirm it at the first live
  run and either pin it via `CG_CHAIN_CGOBS_SRC_RE` or update this default; a pattern that matches
  nothing makes the cg-obs hop report `NO SOURCES` and FAIL (fail-closed, never a false PASS).**
  `strih` → `cg` (expected: a missing `cg` audit line FAILs).
- **The default hop list is `cg-obs strih` (issue 1380, ROZHODNUTIE 27.9.2026).** The owner removed
  the stream input the stream hop used to read (`NDI obs hudba`). The stream hop runs only when asked
  for: `--hops "... stream"` or env `CG_CHAIN_HOPS`. Its input is `CG_CHAIN_STREAM_SRC` (default
  `NDIA cg stream`, the CG-named NDI input of the live stream production scene read 27.9.2026; it is
  currently senderless, see genlock-lock-facet.md). When that input has no audit line in the window
  (missing, or no frame received since OBS start) the hop prints a named `ABSENT` row + a reason and
  does NOT fail the run (the pure `cg_chain_hop_absent_ok`, stream only); an unreadable stream log
  still FAILs. A run where EVERY requested source is ABSENT verified nothing: `OVERALL: NO-DATA`,
  exit 3 (a typo in `CG_CHAIN_STREAM_SRC` never looks like success). rig-health-audit runs only
  `--hops strih` (strih is never ABSENT, so its `sources_absent` count stays 0; the counter is there
  for a run that asks for the stream hop). Pinned by
  `tests/python/test_cg_chain_verify_hops_1380.py`.
- The log tail per hop is supplied explicitly so the tool is self-contained and ships no untested
  ssh/MCP default: `CG_CHAIN_<HOP>_LOG=<file>` (tests; and the supervisor's path for cg-obs — paste
  the win-resolume MCP FileRead of the RESOLUME-SNV OBS log to a file) or `CG_CHAIN_<HOP>_CMD="<cmd>"`
  (live: a byte-safe `ssh ... powershell -c "gc <obslog> | select -last 4000"`, or a bundle-state
  `:8899` fetch). `<HOP>` is upper-cased with `-`→`_` (`cg-obs`→`CG_OBS`).
- Raw bytes are stripped byte-safe (`cg_chain_strip_high_bytes` = `LC_ALL=C tr -d '\200-\377'`)
  BEFORE any awk/python parse, per `ps-log-byte-safety-extraction.md` — the `genlock-fifo audit`
  line carries the `≈` glyph co-resident on the parsed line, so a PowerShell `gc` re-encode would
  otherwise blind grep / feed invalid UTF-8 downstream.

## asrc residual band

`cg_chain_parse_asrc_ppm` reads the newest `estimated=<X>ppm` per source; `cg_chain_asrc_in_band`
asserts `|X| ≤ floor` (default 10, `--asrc-floor-ppm`). Per `asrc-residual-floor.md`: a steady
≈ +8 ppm is the physical Dante-GM-vs-UTC floor (PASS), the ≈ −18 ppm DVS/PTP port-collision
signature is out of band (FAIL); an ABSENT asrc line is `UNKNOWN`, never a false out-of-band. An
out-of-band present reading folds into the hop verdict; `UNKNOWN` never fails.

## Soak + rig-health + Tier-0

- `--soak-hours N --csv <path>`: repeat per window, append `ts,hop,source,verdict,skew,deltas,ppm`
  rows (the 24 h ±20 ms flatness in issue 1294 §8 is then a plot, not a claim). The inter-window
  sleep is the `CG_CHAIN_SLEEP_CMD` seam (tests set it to `:`), so no test ever blocks on a sleep.
- `rig-health-audit.py` calls `check_cg_chain()` REPORT-ONLY: it emits the `CG_CHAIN_REPORT_VERDICT
  = "NOTE"` row (never PASS/WARN/FAIL, so it never changes the audit exit code). The `#787
  resolume-rate exemption` (`CAMERA_SRC_RE = ^NDI cam\d+$`) is unchanged.
- **Gotcha — an awk comment inside the single-quoted awk program must carry NO apostrophe.** The
  `cg_chain_summarize_window` / `cg_chain_enumerate_sources` awk bodies are bash single-quoted
  (`awk '…'`), so a `#`-comment line containing an apostrophe (`jitter_audit's`) terminates the
  bash string mid-program → `bash -n` / `shellcheck` syntax error (cost a cycle this session). Keep
  awk-internal comments apostrophe-free (and mind bare `(`/`)` after a stray quote).
- **Tier-0 (#557, no cargo of any compiling shape):** `bash -n` + `shellcheck -S warning`; source
  the lib and drive the pure functions over fixtures; pytest for the python part
  (`tests/python/test_cg_chain_rig_health_1300.py`); `cargo fmt --all --check`.
- **The Rust parity harness runs locally with plain rustc (issue 1302).** Build a std-only
  `camera_box` stub rlib from the REAL `src/resolume_playback.rs` plus a copy of
  `src/jitter_audit.rs` with its one serde function (`summaries_to_json`, never called by the
  harness) cut out. Compile the real `tests/harness_cg_chain_verify_1300.rs` against it with
  `CARGO_MANIFEST_DIR=<worktree> rustc --edition 2021 --test … --extern camera_box=<rlib>`, plus
  the same with `clippy-driver -D warnings`, and run the binary from the worktree root. For the RED
  proof, point `CARGO_MANIFEST_DIR` at an export of the pre-fix scripts. CI remains the
  authoritative run.
