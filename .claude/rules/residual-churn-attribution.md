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
  copy); `G` = the emit-fill / starvation repeat (the raw material a copy can survive from).

## The discriminator — compare to the box's OWN baseline, NEVER an absolute floor
The under-cadence grabbers (cam1/cam2) carry a HUGE steady BACKGROUND: ~45 % of 5-s buckets show a
`S−M` deficit, ~90–110 emit-fills per run — present in EVERY run, including the fully-clean 0/0 runs.
Some boxes ALSO carry a persistent `corrupted` FLOOR (cam7 reads exactly `4 corrupted` on every
line). So the mere presence of a deficit or `corrupted>=1` at an event proves NOTHING. An event is
SOURCE only when the source shows an ANOMALY above the box's own run-wide baseline: a `late-dupe
copies emitted >= 1` (baseline is 0), a corruption RISE above the steady floor, or a burst deficit
`>= ANOMALY_DEFICIT_FLOOR` (3; the steady background is 1–2). Otherwise DOWNSTREAM. A first draft
that treated any `corrupted>=1` as SOURCE mis-flagged cam7's steady `4 corrupted` — the RED-catching
test `steady_corrupt_floor_is_background_not_source` pins this.

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
