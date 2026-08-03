---
paths:
  - "src/asrc_bench.rs"
  - "src/asrc*.rs"
  - "vendor/**/swresample*"
  - "vendor/**/*asrc*"
  - "scripts/av_sync_outer_loop_guard.py"
  - "scripts/av_sync_measure.py"
  - "vendor/**/RequestHandler_Inputs.cpp"
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

## #806's outer loop — `OuterLoopGuard` (new module, NOT an extension of the bench trait) + the C/WS/Python chain

Unlike #803 (which extends `RealtimeAsrcCompensator` in THIS file), #806 is a genuinely SEPARATE,
higher-level concept and lives in its own module `src/asrc_outer_loop.rs` — it operates on ~7-minute
SyncNet measurements, not per-audio-callback blocks, so it does not implement `AsrcCompensator` at
all. It only PRODUCES a `bias_ppm` value; `RealtimeAsrcCompensator` gained a small, separate
extension (`set_outer_bias_ppm`/`outer_bias_ppm`, `OUTER_BIAS_MAX_PPM=10.0` in THIS file) to
consume it, folded additively into the existing `target_ppm` calc before the `MAX_PPM` clamp.

**The full chain, five pieces, none of them the inner ASRC estimator itself:**
1. `src/asrc_outer_loop.rs::OuterLoopGuard` — the pure "brain" (3-sample sliding window, 40ms
   sustained-average threshold, 1ppm/step rate limit, ±10ppm hard clamp). Tier-0 tested.
2. `asrc-compensator.{h,c}` — `outer_bias_ppm` field + set/get, same file #803 already ported.
3. `obs.h`/`obs-source.c` — `obs_source_set/get_asrc_outer_bias_ppm`, a CORE (type-agnostic) export
   forwarding to `source->asrc`. Core, not DistroAV, because #803's own design comment already
   established the program-audio source ('mbc') is NOT an NDI/DistroAV source — a DistroAV-only
   settings key would silently do nothing for the one source that matters.
4. `vendor/obs-studio/plugins/obs-websocket/src/requesthandler/RequestHandler_Inputs.cpp` (+ the
   `.h` declaration + the `RequestHandler.cpp` dispatch-table entry) — a NEW request pair
   `SetAsrcOuterBiasPpm`/`GetAsrcOuterBiasPpm`, mirroring `SetInputMute`/`GetInputMute` field for
   field (`AcquireInput` by name/uuid, `ValidateNumber` for the range, then a straight call into
   the core export). **obs-websocket lives INSIDE `vendor/obs-studio` itself** (built as part of
   the same CMake project, unlike DistroAV) — it links directly against the new core export at
   compile time, no `resolve_obs_export`-style runtime symbol resolution needed (that dance is
   ONLY for DistroAV, which builds against stock SDK headers as a separate project).
5. `scripts/av_sync_outer_loop_guard.py` — a literal Python mirror of piece 1 (same constants,
   same formula, own pytest suite) for the actual watchdog, wired into
   `scripts/av_sync_measure.py`'s existing `--loop` mode via `--outer-loop`/`--outer-loop-state`/
   `--outer-loop-source`/`--ws-host`/`--ws-password`; applies via piece 4's requests with the same
   verify+rollback-on-mismatch pattern `av_sync_calibrate.py`'s `apply_latency()` already
   established for the genlock-latency knob (#358).

**Gotcha: a "sliding window across process iterations" caller must NOT reload the guard from disk
every call.** `--loop` mode calls `one_measurement()` fresh roughly every 7 minutes from the SAME
long-running watchdog process. `OuterLoopGuard`'s own window is deliberately NOT persisted to disk
(only `bias_ppm` is, for surviving a genuine process restart — see the struct's own doc comment) —
so a naive `run_outer_loop()` that does `load_outer_loop_guard(state_path)` fresh on every call
would re-create an EMPTY window every single time, and the "3-sample sustained average" the whole
design depends on would never accumulate across the ~21 minutes it is supposed to span. The fix
(`av_sync_measure.py`'s `_get_outer_loop_guard`/`_OUTER_LOOP_GUARDS`): cache the live `OuterLoopGuard`
object in a module-level dict keyed by state path, so it survives in-process across `--loop`
iterations; only the FIRST access per key loads the persisted bias from disk. A test that calls
`run_outer_loop()` `WINDOW_N` times in a row and expects a correction to fire on the last call is
exactly what catches a regression here (`test_sustained_correction_applies_persists_and_reports` in
`tests/python/test_av_sync_outer_loop_apply.py`) — a version that reloads from disk each time
passes every OTHER test but fails that one silently (0 WS calls, no exception).

**The sign convention (`residual_ms > 0` → nudge `bias_ppm` UP) is a DELIBERATE, DOCUMENTED, but
NOT live-validated choice** (see `src/asrc_outer_loop.rs`'s own doc comment for the full
reasoning). It is bounded safe either way (±10ppm max, 1ppm/step, only after a sustained window) —
if the first live watchdog run shows the residual growing FASTER after a correction instead of
shrinking, invert the single `direction`/`avg_residual_ms > 0.0` line in BOTH
`src/asrc_outer_loop.rs::OuterLoopGuard::observe` AND its Python mirror
`av_sync_outer_loop_guard.py`, and re-run both test suites (several tests pin the CURRENT sign
explicitly and will need their expected signs flipped too).

## #962's windowed measurement — per-block instantaneous ppm is unmeasurable noise for small blocks

The pre-#962 estimator computed `instantaneous_ppm` from ONE audio callback's own
`raw_advance_s`/`master_block_s` pair. For small blocks (mbc's 128-sample Dante VSC blocks,
2.667ms each), normal wall-clock delivery jitter (a few hundred microseconds) swings that ratio
into the hundreds-of-thousands-to-millions ppm range, tripping #960's `MAX_SANE_INSTANTANEOUS_PPM`
ceiling on almost every block — the guard was correctly protecting against garbage, but the
MEASUREMENT itself was broken at this block size (mbc ended up 100% starved-rejected, servo
permanently neutral). Fix: accumulate `raw_advance_s`/`master_block_s` DURATION-WEIGHTED SUMS
across consecutive `compensate()` calls into a running window (`WINDOW_S`/`ASRC_WINDOW_S`), and
compute ONE ppm value from the sums when the window closes — summing physical durations first
cancels arrival-timing jitter exactly, since a burst-then-catch-up pair still sums to the correct
total wall time and total delivered-sample duration. The #960 ceiling stays applied to this
WINDOWED value, so a genuinely starved source is still caught.

**`WINDOW_S = 1.0` was picked specifically so every pre-existing test needs ZERO changes** — it
degenerates EXACTLY to the old per-block behavior for any call whose own `master_block_s` already
reaches 1.0s (the window closes on that single call, the windowed ppm reduces algebraically to
that block's own instantaneous ratio). Every `RealtimeAsrcCompensator` test in this file already
calls `compensate()` with >=1.0s blocks per call — so picking a window size at or below the
smallest block size any EXISTING test uses is a reusable trick for migrating a per-block gate to a
per-window gate with no test-fixture rewrites, when that's compatible with the real-world block
sizes you're trying to fix (verify the target small-block source's true block size is much smaller
than the chosen window, so real windowing/averaging still happens there).

**Gotcha — restructuring a per-block early-return into a per-window early-return can silently
break UNCONDITIONAL trailing work in the C mirror.** The pre-#962 C `compensate()` used an
`if (starved) {...} else {EMA/target/slew...}` shape where BOTH branches fell through to shared
tail code (the `corrected_advance_s` computation + the UNCONDITIONAL
`cumulative_correction_ms`/`time_since_log_s` telemetry accumulation, explicitly documented as
"kept UNCONDITIONAL... so the ~60s log cadence never goes silent during a sustained starve"). A
naive `return` inside the windowed rejection branch (mirroring the RUST reference's own early
return, which has no telemetry fields to preserve) SKIPS that unconditional tail in C — silently
reintroducing exactly the "log goes silent during a sustained starve" defect the earlier guard
fixed. **Fix pattern: use a local bool flag (`window_rejected_this_call`) set inside the rejection
branch, and gate ONLY the target/slew block on `if (!flag)` — never a hard `return` — so the
shared telemetry tail always runs.** This asymmetry (Rust: safe early return; C: needs a flag
because of trailing unconditional telemetry) is worth checking EVERY time a future ticket adds an
early-exit branch to this pair — the Rust reference's simplicity can mask a C-side telemetry
regression if you port the shape 1:1 without checking what runs after the branch in C.

**Test fixture for a synthetic small-block bursty source** — `feed_bursty_small_blocks(compensator,
true_ppm, n_pairs)` in `src/asrc_bench.rs`'s test module: feeds PAIRS of fixed-size blocks (the
mbc 128-sample size) with a fixed total pair wall-time (derived from `true_ppm`) but split
UNEVENLY (10%/90%) between the two blocks in each pair — reproduces real bursty delivery (some
blocks arrive almost back-to-back, the next "catches up") while the pair's aggregate wall time
still correctly totals what `true_ppm` implies. Reusable for any future test needing a
small-block, jittery-but-honest audio source at a controllable true ppm.
