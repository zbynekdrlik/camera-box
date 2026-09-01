---
paths:
  - "scripts/arrival_floor_decompose.py"
  - "tests/python/test_arrival_floor_decompose_1168.py"
---

# Per-box arrival-floor stage decomposition (`scripts/arrival_floor_decompose.py`, issue 1168 task 1)

A dev1-side SUPERVISOR mining tool: run it over a FINISHED E2E run's collected logs to get one
per-camera table decomposing each camera's arrival floor into grabber / NDI-transport / strih stages,
so "which box/stage owns the cross-camera presented-age offset" is answered from data. Wired into NO
gate, drives NO rig. Tasks 2 (reduce the highest floor) and 3 (re-tighten `[4i/8align]`) are
downstream consumers of this tool's output.

## How to run

```
python3 scripts/arrival_floor_decompose.py --run-dir /tmp/recording-e2e-<RUN>        # text table
python3 scripts/arrival_floor_decompose.py --run-dir /tmp/recording-e2e-<RUN> --json # machine output
```

### `--multi` (issue 1168 task 2): aggregate MANY runs, never trust one

A SINGLE run's slowest-box verdict is unstable (transient grabber DQBUF stalls / load shuffle the
noisy middle), so task 2 picks the target box from MANY runs — and re-runs this after every
reduction attempt. `--multi` folds several runs (REUSING the same per-run `decompose()` via
`mine_run_dir`, never a new parser):

```
python3 scripts/arrival_floor_decompose.py --multi --runs-glob '/tmp/recording-e2e-*' \
    --only-uniform --min-cameras 7                # the clean ~50ms-offset regime; add --json for a dict
python3 scripts/arrival_floor_decompose.py --multi --run-dir <A> --run-dir <B> ...   # explicit set
```

`--run-dir` is repeatable; `--runs-glob` adds a glob; `--only-uniform` keeps only
`transport_uniform=True` runs (a transport-degraded / rate-halving run is a DIFFERENT fault, not
the constant offset); `--min-cameras N` drops runs with fewer than N non-phantom cameras (mixing
the older 4-camera fleet era with the current 7-camera fleet destroys the ranking — see the
finding). Output: per-run digest, a per-camera aggregate table (floor median/min/max/pstdev,
mean floor-RANK, latency-pin set) ordered by median floor, and a STABILITY verdict — is one camera
the anchor (fastest) / slowest in ≥ `RANK_MODE_STABLE_FRAC` (0.6) of usable runs, or is the variance
run-level? The pure core (`aggregate`, `_keep_run`, `mine_run_dir`) is Tier-0 fixture-tested with a
SECOND real fixture (`recording-e2e-659887078`, anchor cam4 / slowest cam2) that contrasts the
first (`recording-e2e-1363366080`, anchor cam3 / slowest cam1) so the smoke test folds two
genuinely disagreeing runs.

It auto-discovers the three stage artefacts by their standard names inside the run dir:
`qr-align-jitter-<RUN>.json`, `qr-align-strih-<RUN>.log`, `cam*-cbox-burn-<RUN>.log`. Override with
`--jitter-json` / `--strih-log`; restrict cameras with `--cameras "1,2,3"`. The strih source name is
FIXED at `NDI cam{n}` (the current 1:1 rig NDI mapping) — NOT a CLI knob, because the reused
`arrival_floors_from_jitter` hardcodes that naming, so any other template resolves zero floors.

## The model — algebraic, NOT fitted (do not "improve" it into a curve fit)

Each camera's arrival floor is EXACTLY `floor = latency_ms + mean_head_skew_ms` — the definition
`qr_align_pins.arrival_floors_from_jitter` already uses. So the per-camera EXCESS over the FASTEST
camera decomposes exactly:

```
excess = Δlatency_ms  (strih-config pin difference)  +  Δmean_head_skew_ms  (everything UPSTREAM of the pin)
```

`recv-timing #797` `cap_avg` is the NDI transport arrival CADENCE. When it is UNIFORM across cameras
(spread ≤ `TRANSPORT_UNIFORM_SPREAD_MS` = 3 ms) transport is NOT the per-camera differentiator, so the
upstream (Δskew) excess is attributed to the CAMBOX GRABBER — corroborated (never replaced) by the
cambox burn-log `Streaming:` emit-vs-capture parity + `#707 DEQUEUE STALL` health. A DQBUF stall is a
grabber-side JITTER signal (anti-correlated with real loss, issue 1198), so it corroborates a per-box
floor, it never proves loss.

## REUSE, never re-parse (the load-bearing constraint)

- Total floor + phantom-floor guard → `qr_align_pins.arrival_floors_from_jitter` (drops a samples<3
  floor, issue 1253). NEVER hand-derive `latency + skew` with a fresh regex.
- Transport cadence → `ndi_halving_decision.parse_recv_timing` (each line's OWN timestamp, the issue-797
  phantom-rate discipline — never a wall-clock divisor).
- Skew / FIFO pin → the harness's `qr-align-jitter-<RUN>.json` artefact (= `genlock-jitter-report
  --json`, i.e. `src/jitter_audit.rs`). The tool CONSUMES that JSON — it does not re-parse the
  `genlock-fifo audit` `ts_head_skew_ms` line with a new regex.
- Only the cambox `Streaming:`/`#707` grabber parsers are genuinely new (that signal isn't parsed
  elsewhere for this).

## Tier-0 (issue 557) — pure Python, full local RED→GREEN

Pure decision core (`decompose`, `parse_streaming`, `parse_dqbuf_stalls`, `cap_avg_by_source`,
`floors_and_fields`) with no I/O below the CLI, so `python3 -m pytest
tests/python/test_arrival_floor_decompose_1168.py` gives a complete local RED→GREEN with zero cargo —
the issue-1199/1203/1226 python-mirror precedent. The two real-data smoke tests run over a COMMITTED fixture
(`tests/fixtures/arrival_floor_1168/`), so they always run (test-strictness: no skips).

## SINGLE-RUN observation (2026-09-01, verdicts 1363366080 / 1168855508) — SUPERSEDED by the multi-run aggregate below

On these two runs cam1 owned the highest per-box floor (~15–18 ms above the pack), grabber-attributed
(transport cap_avg uniform; latency pin 3 ms). **This was a two-run snapshot and does NOT generalize**
— see the multi-run finding below: cam1 is the slowest in only 5/28 clean runs; it was the reliable
slow box in the OLDER 4-camera fleet era (18/19), which is where a wide-window read of its floor still
looks high. Do NOT start task 2 from "reduce cam1's grabber floor" — that is a run/era artifact.

## MULTI-RUN finding (`--multi`, 69 mineable run dirs on dev1, 2026-08-28 … 2026-09-01)

Mined every `/tmp/recording-e2e-*` with `--multi`. Stratified because per-box floors SHIFTED with the
fleet: the 4-camera era (2026-08-23…28) and the current 7-camera fleet rank cameras differently, and a
transport-degraded / rate-halving run (transport NOT uniform, spread 120–191 ms) is a different fault.

**Clean regime = `--only-uniform --min-cameras 7` (28 runs):**

- **Anchor (fastest) = cam4, STABLE (19/28 = 68%)**, cam5 the stable #2 (mean floor-rank cam4 1.71,
  cam5 2.61). The FAST end of the ranking is a stable per-box property.
- **Slowest = NOT stable** — cam2 leads the mode at only 10/28 (36%), then cam3 7, cam1 5, cam6 3.
  The slow end shuffles run-to-run (transient grabber DQBUF stalls / load), which is exactly why a
  single run disagrees (1363366080→cam1, 1556876186→cam5).
- **The stable cross-camera spread is ~8 ms of MEDIAN floor (cam5 70.8 → cam2 79.0), NOT ~50 ms.**
  The 50 ms+ figures live only in transient runs (DQBUF-stall episodes, the 4-cam era, transport
  degradation). By median floor: cam5 70.8 ≈ cam4 71.3 < cam7 73.5 ≈ cam1 73.9 ≈ cam6 74.0 < cam3 77.1
  < cam2 79.0.
- **cam2 is the marginal steady slowest and the ONLY box carrying a config pin** (`latency_pins`
  `3,6` — every other box is a flat `3`). Its +3 ms is a deterministic, config-fixable strih
  `latency_ms=6` pin; the rest of its ~21 ms median excess is grabber-skew shared with the whole
  non-cam4/cam5 band. (The single-run "cam2 grabber skew is the LOWEST" note above was run-specific and
  does NOT hold in aggregate.)

**Task-2 target recommendation (data-backed):** there is NO single stable "slowest box" to optimize —
so do NOT chase one run's slowest. Two stable levers were named: (1) align cam2's strih `latency_ms=6`
pin down to 3 ms (deterministic −3 ms, config only); (2) the residual ~8 ms median band as a fleet
grabber-skew difference vs the cam4/cam5 anchors. Re-run `--multi` after each reduction attempt.

**OUTCOME (issue 1168, worktree lane 2026-09-01) — what actually landed + the reshaping:**
- **Lever 1 DONE (code):** `latency-pins-baseline.json` `NDI cam2` 6→3 (cam2 is the projection probe,
  EXCLUDED from the align set, so the aligner never floors it — 6 was the only leftover non-floor pin).
  Supervisor applies live + re-verifies cam2's A/V. Does NOT move the `[4i/8align]` residual (cam2 not
  in the align gate) — it drops cam2 as the marginal steady slowest in THIS decompose.
- **Lever 2 is NOT a clean config lever — it is grabber MODEL + per-box variance (deferred to a rig
  investigation).** Grabber map (run 1556876186): cam1=Cam Link 4K, cam3=ShadowCast 2, cam4=NZXT Signal
  HD60 (anchor), cam5=Cam Link 4K (anchor), cam6/cam7=Cam Link 4K. The two anchors are DIFFERENT models,
  and three IDENTICAL Cam Link 4K boxes (cam5/6/7) still span ~3 ms of the band — so "match grabber
  config toward cam4/cam5" is not a single well-defined lever; it is a per-box rig task.
- **The ~50 ms was a mis-frame; the align-gate residual is the N=2 QUANTUM, not a floor gap.** Mining
  the live align status of every green run: `report_only_residual_ms` ≈ 2 source frames (~33 ms), which
  ANTI-correlates with the floor — so NEITHER lever reduces it. Task 3 therefore landed as a
  TRANSIENT/QUANTUM-BOUNDED re-tighten (`budget_bound_verdict`, `DEFAULT_ALIGN_RETIGHTEN_BUDGET_MS=45`),
  not the naive "reduce floors then hard-fail on any residual" the ticket first envisioned. See
  `qr-align.md` "Re-tighten (issue 1168)". Re-arm (lower the budget toward parity) only once the N=2
  quantum itself is addressed — floor reduction alone will not move it.
