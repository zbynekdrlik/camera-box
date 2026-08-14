---
paths:
  - "src/painter_pacing.rs"
  - "src/painted_tick_gaps.rs"
  - "src/window_gate.rs"
---

# Attributing a DUPLICATE / `copies` residual — painter-stall vs downstream (issue 859)

The fused gate's `all_cambox_continuity` `copies` term (residual painted-tick DUPLICATES in the
recorded stream) can originate at THREE distinct stages of the one-camera-through-a-splitter rig.
Do NOT guess which — the painter emits its own ground truth and the discriminator is mechanical.

## The pipeline (where a duplicate can be born)
painter paints QR ticks on cam2's monitor (60fps DRM) → ONE camera films it → splitter → all
camboxes capture (30fps V4L2) → NDI → strih OBS (genlock) → stream OBS (genlock) → the E2E records
strih/stream OUTPUT and decodes ticks. A `copies` event = a painted tick decoded twice; it can be:
1. **painter stall** — a missed DRM-vsync deadline paints one tick for ≥2 refresh cycles;
2. **optical beat** — the monitor panel refresh vs the 30fps capture (steady-state noise floor);
3. **strih/stream genlock FIFO limit cycle** — copies≈gaps uniform
   (`.claude/rules/genlock-fifo-limit-cycle-diagnosis.md`), a convergence transient after an OBS
   restart.

## The discriminator: the painter's own CSV EXONERATES or incriminates stage 1
The painter logs `tick,gen_ts_ns,flip_ts_ns` (one row/painted-frame) to `painter-*.csv` in every
run dir (`/tmp/recording-e2e-*/`). `flip_ts_ns` = page-flip-COMPLETE = on-screen instant.
`src/painter_pacing.rs::analyze_csv(text)` computes, purely + Tier-0 tested:
- painted-tick **duplicates / skips / non-monotonic** (the painter emitting a bad logical sequence);
- **missed DRM-vsync deadlines**: an inter-flip interval `>= 1.5x` the run's OWN median (nominal)
  interval — integer-safe `iv*2 >= nominal*3`; `>= 2x` = a "duplicate-class stall" long enough to
  strand a captured duplicate at 30fps.

`PainterPacing::is_clean()` true ⇒ the painter is metronomic ⇒ a `copies` residual is DOWNSTREAM
(stage 2 or 3), NEVER the painter. `duplicate_attribution(total_copies)` returns the verdict string.

**Live finding (issue 859, 2026-08-14):** across 6 retained runs incl. the worst 187-copy transient,
the painter measured 0 duplicates / 0 skips / 0 missed deadlines (max inter-flip ~20ms at a 16.67ms
nominal). The painter is exonerated even when the recorded output shows 187 copies. A quick check on
a run dir: mine `painter-*.csv` for consecutive equal ticks and inter-flip intervals >25ms — both 0
on a healthy 60fps run.

## Surfaced report-only, NEVER gating
recording-verdict emits `all_cambox_continuity.painter_pacing` (+ `total_copies` / `attribution`)
when `--painter` is given. It does NOT gate and changes NO threshold — the walk-down to
`WINDOW_COPIES_GAPS_TOLERANCE`=0 stays on its own ticket
(`.claude/rules/window-gate-tolerance-walkdown.md`); this only ATTRIBUTES the residual so nobody
re-chases the painter. A residual whose painter is clean is a downstream (optical / genlock) problem,
not a per-box or painter software bug — and a per-box software fix can never be the answer's shape.
