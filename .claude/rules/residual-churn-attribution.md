---
paths:
  - "scripts/residual_churn_attribution.py"
  - "tests/python/test_residual_churn_attribution_1242.py"
---

# Attributing a copies/gaps residual: SOURCE (grabber) vs DOWNSTREAM (FIFO/decimation) from burn logs (issue 1242)

`scripts/residual_churn_attribution.py` (pure, Tier-0, a supervisor mining instrument — no rig, no
gate) aligns every `all_cambox_continuity` residual event's `wall_clock_epoch_s` to that cambox's
OWN burn log and classifies the SOURCE side. Run it over run dirs or bare RUN_IDs:
`python3 scripts/residual_churn_attribution.py <RUN_ID|dir>... [--json out] [--markdown out]`.

## The three SOURCE-side burn-log signals (per cambox `camN-cbox-burn-<RUN>.log`)
- **5-s `Streaming: <e> fps emitted / <c> fps captured (<S> sent, <M> captured, ...)`** — `S − M` is
  the capture DEFICIT (`_STREAMING_RE`, reused from `arrival_floor_decompose.py`). `S`=emit demand
  (genlock SEND cadence), `M`=frames captured from the device.
- **1-s `#707 emit-1s:[..] cap-1s:[..]`** — finer `emit − cap` deficit (oldest-first buckets, bucket
  `i` of `n` on a line stamped `t` covers second `t-(n-1-i)`).
- **`(#889) dupe-preferring decimation: .. <L> late-dupe copies emitted .. <G> starvation last-frame
  repeats ..`** — `L` = a duplicate the cambox ITSELF emitted into NDI (a genuine source-origin
  copy); `G` = the emit-fill / starvation repeat (the emit gate finding NO new frame at a boundary
  with capture at 60.0 — a duplicate produced ON the box in the capture→emit hand-off, issue 889
  mechanics). **`G` is PER-INTERVAL, not cumulative** — `src/dupe_decimation/gate.rs` emits this line
  every ~5 s Streaming window and DRAINS+RESETS the accumulator each emit
  (`DupeShedLog::take_starvation_repeats` → `= 0`; the line's suffix reads "over the last ~5s"), so
  each `G` already IS that bucket's ΔG (no differencing). Parsed by `parse_starvation_series` (reuses
  the SAME `_DECIM_RE`, never a second regex).

## The discriminator — compare to the box's OWN baseline, NEVER an absolute floor
The under-cadence grabbers (cam1/cam2) carry a HUGE steady BACKGROUND: ~45 % of 5-s buckets show a
`S−M` deficit, ~90–110 emit-fills per run — present in EVERY run, including the fully-clean 0/0 runs.
Some boxes ALSO carry a persistent `corrupted` FLOOR (cam7 reads exactly `4 corrupted` on every
line). So the mere presence of a deficit or `corrupted>=1` at an event proves NOTHING. An event is
source-attributed only when the source shows an ANOMALY above the box's own run-wide baseline.
Otherwise DOWNSTREAM. A first draft that treated any `corrupted>=1` as SOURCE mis-flagged cam7's
steady `4 corrupted` — the RED-catching test `steady_corrupt_floor_is_background_not_source` pins this.

## Two source signals — the attribution splits (issue 1242 reopened)
The attribution is `SOURCE-DEFICIT` / `SOURCE-STARVATION` / `DOWNSTREAM` / `UNKNOWN`:
- **SOURCE-DEFICIT** — the CAPTURE-side anomaly: a `late-dupe copies emitted >= 1` (baseline 0), a
  corruption RISE above the steady floor, or a burst capture deficit `>= ANOMALY_DEFICIT_FLOOR`
  (3; the steady background is 1–2). Checked FIRST, so it WINS when both signals coincide.
- **SOURCE-STARVATION** — the EMIT-side anomaly: a starvation-repeat BURST. `starvation_burst_at`
  returns `(delta_G, baseline_median, is_burst)` where `delta_G` is the PEAK per-5-s-bucket ΔG within
  ±window and `is_burst = delta_G >= STARVATION_BURST_MIN (3) AND delta_G > baseline_median`. The
  baseline is the box's OWN run median ΔG per bucket — **baseline-relative, NEVER an absolute floor**:
  a steady 20/5 s starvation background is a COVARIATE (median 20, event 20 is not ABOVE it → not a
  burst), exactly the rule this playbook mandates (pinned by `steady_starvation_background_is_not_a_burst`).

**Reading a `SOURCE-STARVATION` verdict: the churn is the BOX EMIT PATH (the issue 889 capture→emit
valve), NEVER the genlock FIFO on strih.** This is the signal the capture-deficit discriminator is
BLIND to — a starvation burst can coincide with a perfectly clean 60/60 capture cadence. The CAM4 6/4
run (19.9.2026) is the reference case: 697 starvation repeats, ZERO capture deficit, filed DOWNSTREAM
by elimination before this signal; with it, that run reads 0 SOURCE-DEFICIT / 9 SOURCE-STARVATION / 3
DOWNSTREAM → verdict SOURCE / grabber-owned, pointing the next fix at the box emit valve, not the FIFO.
(The `STARVATION_BURST_MIN`/window constants are the design's chosen bar — re-validate them against
the per-run ΔG distribution before any downstream fix decision; a bursty-but-low-median grabber like
cam1 can flag a ΔG=3 spike as a burst, which the supervisor's before/after mining table resolves.)

## Live finding (issue 1242 task 1, 17.9.2026) — the churn is DOWNSTREAM
Across 5 post-cure splitter-fed runs (9 residual events): all DOWNSTREAM, 0 source. The cambox
emitted 0 `late-dupe copies` in every window; 609 emit-fill frames across the 2 clean runs produced
0 residuals; copy survival ratio ≈ 0.0026; the survivor lands on a healthy-cadence box (CAM7) while a
tied-worst grabber (CAM2) reads 0/0. Per-box capture cadence is a COVARIATE, not the cause — the
residual is an irreducible ≤1/≤1 genlock-FIFO / 60→30 decimation-phase noise floor. Full data +
the fix proposal (do NOT restore absolute strict-zero) is in
`.claude/rules/window-gate-tolerance-walkdown.md`'s issue-1242 task-1 data section.

## Limitation
The strih genlock-FIFO time-series (`received=`/`late_hold`/relock) is NOT persisted into the run
dirs (only aggregate head-skew jitter + decode-progress), so DOWNSTREAM is by ELIMINATION (source
clean + painter clean + the counterfactual), not a positive downstream read. To make it positive, a
future run must extract the strih `genlock-fifo audit` counters into the run dir.
