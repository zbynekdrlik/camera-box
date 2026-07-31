---
paths:
  - "src/asrc_bench.rs"
  - "src/asrc*.rs"
  - "vendor/**/swresample*"
  - "vendor/**/*asrc*"
---

# ASRC bench harness (#804, epic #800 A/V-desync endgame round)

`src/asrc_bench.rs` is a PURE, Tier-0 (default-features) closed-form simulation of two
independent free-running clock domains — it does NOT render/decode any audio or video, on
purpose (see the module's own "Rejected alternative" doc comment and issue #804's design comment,
`gh issue view 804 --comments`). Read the module doc comment first; this file only adds what a
fresh session would otherwise have to re-derive.

## The mechanism, in one line

OBS timestamps audio by SAMPLE COUNT (48000 samples = stamped as exactly 1s), so a foreign audio
clock domain running `ppm` parts-per-million off nominal makes the RAW (uncompensated) advance per
master-clock block `block_s * (1 + ppm/1e6)` — this is why the drift is LINEAR and UNBOUNDED, and
why a constant video-delay knob (the pre-ASRC mitigation, now report-only per #861) can only zero
it at one instant.

## The `AsrcCompensator` trait is the seam #803 mirrors, not reuses

`compensate(&mut self, raw_advance_s, master_block_s) -> f64` is deliberately at the same level of
abstraction #803's REAL per-source rate estimator + `swr_set_compensation` resample-ratio
application will sit at in libobs. #803 does NOT import this bench module — it implements the
SAME shape against real measured sample counts / wall-clock time, so this bench's acceptance gate
(`WORST_CASE_PPM = 50.0`, `GATE_DURATION_S = 4h`, `GATE_MAX_OFFSET_MS = 40.0`) stays the reusable
validation target: port the real per-source estimator's ratio-estimation logic into a new
`AsrcCompensator` impl here (or a thin adapter) and run it through `simulate_offset_trace_ms`
before trusting it on the rig — never invent a second, unrelated proof.

## The EMA convergence math (closed form, don't re-derive)

`EmaRateCompensator` converges geometrically: after `n` blocks the estimate error decays as
`(1 - alpha)^n`, and the STEADY-STATE residual offset (once converged) is
`block_s * ppm/1e6 / alpha` seconds — e.g. at `BLOCK_S=0.1`, `ppm=50`, `alpha=0.3`, residual ≈
0.017 ms, far inside the 40ms bound. This means:

- Raising `alpha` shrinks the steady-state residual but lengthens nothing (convergence is already
  geometric); LOWERING `alpha` trades a smaller transient sensitivity to jitter for a larger
  steady-state residual — if a future ticket adds sample-level jitter/noise to the simulated
  audio clock (this bench's `ppm` is currently a CONSTANT, not noisy), re-tune `alpha` against the
  gate bound rather than assuming 0.3 still holds.
- The anti-tautology test (`a_pass_through_stub_does_not_satisfy_the_gate_bound`) is the guard
  that a compensator genuinely has to estimate something — when adding a new compensator impl,
  add the equivalent "a broken/no-op version of this MUST fail the gate" test alongside it.

## TDD RED/GREEN pattern used for this module (reusable for #803/#805/#806)

To get a real RED commit for a not-yet-implemented pure-math module: implement the FULL final
module, then temporarily replace only the new implementation's body with a pass-through/stub
(referencing any now-otherwise-unused struct fields via `let _ = self.field;` so
`clippy -D warnings` still passes on the RED commit — a genuinely unused field is a hard clippy
error, not just a lint), run the GREEN-target tests to confirm they fail, commit that as
`test(#N): [red]`, then restore the real implementation, re-run tests to confirm all pass, and
commit as `feat(#N): [green]`. Verified locally both times with the `# airuleset:build-ok` bypass
on `cargo test --lib asrc_bench` (Tier-0 forbids a bare `cargo test`, see project CLAUDE.md).
