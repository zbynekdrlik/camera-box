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

## #803's real per-source servo — `RealtimeAsrcCompensator` (extends this file) + the C port

#803 added `RealtimeAsrcCompensator` to THIS file (not a new module) — a production-shaped
`AsrcCompensator` impl mirroring the same trait: TIME-based EMA (`alpha = 1 - exp(-block/tau)`,
`tau=20s` — block-COUNT EMA doesn't work here because real audio callbacks vary in frame count,
unlike this bench's fixed `BLOCK_S`), a hard clamp on the correction TARGET (`MAX_PPM=300`), a
slew limiter on the APPLIED correction (`MAX_SLEW_PPM_PER_S=5`, independent of how fast the
estimate itself moves), and a minimum-lock startup delay (`MIN_LOCK_S=5`, default-safe: zero
compensation before lock). Validated against the SAME 4h/50ppm/40ms gate, plus dedicated tests
for the ticket's own convergence text ("<5ppm @ ~2min, ~1ppm @ ~10min"), the pre-lock zero
guarantee, the hard clamp, and the slew limit — see `estimator_converges_within_the_tickets_own_bounds`
et al. in `src/asrc_bench.rs`'s test module for the exact numeric derivation (geometric decay:
pick `tau` so `(true_ppm) * e^(-t/tau)` clears the ticket's own bound at each named horizon).

**The C mirror lives in `vendor/obs-studio/libobs/media-io/asrc-compensator.{h,c}`** — a
line-by-line port (same constant names as `ASRC_MAX_PPM`/`ASRC_MAX_SLEW_PPM_PER_S`/
`ASRC_TIME_CONSTANT_S`/`ASRC_MIN_LOCK_S`, same formula shape). #805/#806 will touch this same
pair — keep any retuning numerically identical on both sides, and re-run the Rust gate tests
(the ONLY locally-runnable proof for this math — the vendored C has NO local build path, Tier 0
forbids it, CI is the first place a C mistake surfaces).

**Wiring gotchas hit integrating into libobs core (obs-source.c), reusable for #805/#806:**

- **A new `media-io/*.c` file needs the CMakeLists.txt `target_sources()` entry or it silently
  never compiles into libobs** — both Linux (`cmake --preset ubuntu-ci`) and Windows
  (`cmake --preset windows-x64`) presets read the SAME `vendor/obs-studio/libobs/CMakeLists.txt`,
  so one edit covers both platforms; no separate file list to update.
- **Both CI presets build with `CMAKE_COMPILE_WARNING_AS_ERROR: true`** (`CMakePresets.json`) —
  every warning in `cmake/linux/compilerconfig.cmake`'s enabled set (`-Wunused-parameter`,
  `-Wunused-variable`, `-Wparentheses`, `-Wswitch`, `-Wuninitialized`, `-Wformat`, ...) is a hard
  build failure. `-Wno-shadow`/`-Wno-unused-function`/`-Wno-missing-prototypes` etc. ARE disabled
  though — don't worry about those. Since the vendor tree has no local build (Tier 0), the ONLY
  way to catch a new warning before CI is careful manual review of every new function's parameters
  (all used?) and format-string args (types match?).
- **A function defined LATER in `obs-source.c` needs a forward declaration to be called EARLIER**
  (MSVC treats an implicit extern-int declaration as a hard C4013→C2220 error) — same pattern as
  the pre-existing `genlock_source_drop_cap` forward-decl; #803 added one for
  `genlock_wall_now_ns()` (defined ~line 4690, needed by the new audio-ingest code far earlier).
- **Forcing a resampler to exist for a source whose format already matches the mix**: the
  existing `reset_resampler()` fast-path skips creating an `audio_resampler_t` entirely when
  `src == dst` (no resampling needed) — but ASRC needs a real swresample context to drive via
  `swr_set_compensation()` regardless. Fix: widen the fast-path's skip condition with
  `formats_match && !source->asrc_enabled` instead of just `formats_match`. Also add
  `source->asrc_enabled && !source->resampler` to `process_audio()`'s reset-trigger condition, so
  toggling the flag ON *after* the source's format has already stabilized still gets picked up —
  lazily, on the NEXT audio callback (the correct single-writer thread), never by mutating
  `resampler` directly from whatever thread calls the setter.
- **`swr_set_compensation(ctx, sample_delta, compensation_distance)` is a one-shot RAMP, not a
  steady-state rate** — it linearly closes `sample_delta` OUTPUT samples over the next
  `compensation_distance` OUTPUT samples then HOLDS. To keep a continuous ppm-based correction
  applied, re-issue it every audio callback with a freshly-computed `sample_delta` for a fixed
  window (e.g. 1000ms) — the docs promise re-issuing before the window elapses REPLACES the
  pending ramp, so a steady ppm becomes a steadily-refreshed short ramp in practice. Wrapped in
  `audio_resampler_set_compensation_ppm()` (`media-io/audio-resampler.{h,c}`) so no caller touches
  swresample directly — mirrors how `audio_resampler_resample()` already layers over `swr_convert`.
