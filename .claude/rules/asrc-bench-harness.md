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

## #1084's estimator SWAP — the inner loop is a sliding REGRESSION now, NOT the EMA the sections above describe

Everything above about the inner estimator being a **TIME-based EMA** (`alpha = 1 - exp(-block/tau)`,
`TIME_CONSTANT_S=20s`, `MIN_LOCK_S=5s`) is **PRE-#1084 HISTORY**. `RealtimeAsrcCompensator`'s inner
estimator (and its C mirror) was **replaced** in #1084 by a **sliding least-squares RATE regression**.
Read `gh issue view 1084 --comments` for the full root cause; the short version a fresh session needs:

- **Why the EMA was wrong.** The 1s #962 window's master time telescopes to just its two endpoint
  wall reads; the audio-thread scheduling jitter in those endpoints does NOT average down with more
  callbacks per window. Live on `mbc` the EMA's `estimated` sd was **178 ppm** → `applied` became a
  ±75–103 ms/h random walk (the global A/V wander). An EMA RETUNE is regime-fragile (the required
  `τ` swings from ~1300s to ~80000s depending on the unmeasured window-noise color); the regression
  is regime-robust because it fits the CUMULATIVE (`cum_master_s`, `cum_raw_s − cum_master_s`) points
  where the endpoint jitter becomes iid per-point `y`-noise it averages over N points.
- **The new state + constants** (Rust `src/asrc_bench.rs` ↔ C `asrc-compensator.{h,c}`, kept
  numerically identical): `REGRESSION_SPAN_S=600`, `REGRESSION_MIN_POINTS=30`,
  `REGRESSION_LOCK_SPAN_S=60`, `REGRESSION_CAP=640`. The old `TIME_CONSTANT_S`/`MIN_LOCK_S` /
  `ASRC_TIME_CONSTANT_S`/`ASRC_MIN_LOCK_S` and the `elapsed_lock_s` field are **GONE**. The C mirror's
  point buffer is a fixed ring (`reg_x[]`/`reg_y[]`/`reg_head`/`reg_count`); the Rust authority is a
  `Vec` that evict-before-appends + age-evicts in the identical oldest→newest order (C↔Rust parity is
  a NUMERICAL contract on the point sequence + LS iteration order, verified bit-identical over a
  jittered sequence — memory layout need not match).
- **FLUSH-on-level-shift** (new, no EMA analogue): a #960 rail trip OR a non-positive `master_block_s`
  (NTP step) FLUSHES the buffer and DROPS the lock — a level shift would poison the slope for a full
  span. Consequence to remember: after a flush `applied_ppm` is held on that call, then DECAYS to 0
  over the ~60s re-lock window and re-converges (default-safe, bounded). The C keeps the
  `window_rejected_this_call` flag pattern so the unconditional telemetry tail still runs on a
  rejected window (the pre-existing #962 gotcha above).
- **Lock semantics changed** (EMA locked at 5s of accrued time; regression locks at buffer SPAN ≥60s
  AND ≥30 points). Every test that used to feed ~5–10s to "get past lock" now feeds ≥65s (see the
  adapted `realtime_compensator_*`, `starved_*_960`, `rejected_window_holds_*_962` tests).
- **The endpoint-jitter acceptance gate** the old EMA-era bench never had: `tests/asrc_endpoint_jitter_1084.rs`
  drives the servo through a stationary per-read Gaussian endpoint jitter (`sigma_t`) and asserts the
  drift is NULLED (steady applied sd < 2.8 ppm, 1h offset drift < 10 ms). The old
  `simulate_offset_trace_ms` bench used an EXACT-per-window clock (no endpoint jitter), which is
  exactly why it passed while the rig failed — any future estimator change here MUST re-satisfy the
  #1084 gate, not just the #804 gate.
- **#806 outer loop:** still present + inert (bias 0, no watchdog running on stream). With the
  regression nulling the inner drift to <1 ppm it has ~nothing to correct; recommend it stays
  OFF/report-only (a ±10ppm/7-min outer integrator on a now-slower inner loop could limit-cycle).
- **Lock-step anchors** for a vendored-C revert: `tests/genlock_preload.rs::vendored_source::
  asrc_uses_sliding_regression_estimator_1084` + byte-identical pwsh blocks in BOTH
  `windows-genlock.yml` and `windows-genlock-fast.yml` (positive: the regression constants/fields/
  slope/flush; negative: `exp(-window_master_s`/`ASRC_TIME_CONSTANT_S` must be ABSENT).

## #1325's servo→swresample SIGN + the servo's MASTER clock — the bench's blind spot, closed

The bench (`RealtimeAsrcCompensator`) asserts against `compensate()`'s RETURN (`corrected_advance_s`),
which is the compensator's OWN lock model `corrected = raw/(1+applied/1e6)` — it NEVER modelled the
value that reaches libswresample, so it was structurally blind to TWO real defects that drained the
live `mbc` mix buffer (issue 1325). Both are fixed in `obs-source.c asrc_process_audio()`:

- **Master clock.** The servo's `master_block_s` now comes from `os_gettime_ns()` (the monotonic QPC
  clock the OBS audio MIXER thread paces on — `media-io/audio-io.c`, and what `buffered_ms` is
  balanced against), NOT `genlock_wall_now_ns()` (the dantesync-slewed system clock). Measuring vs
  the slewed clock made `estimated ≈ −f_phase` (~18 ppm) — a drift the QPC mixer never sees — instead
  of the true source-vs-mixer residual (~−5 ppm). `asrc-residual-floor.md` carries the reading change.
  Since issue 1372 the Windows mixer clock (`os_gettime_ns()`) itself runs at the
  dantesync-disciplined rate, so the residual reads about −f_phase again. The rule stands: the servo
  measures against the MIXER's clock (`windows-disciplined-media-clock.md`).

- **Sign.** The compensator's convention (`applied<0` = slow source = STRETCH) is the RECIPROCAL of
  the swresample-native wrapper (`audio_resampler_set_compensation_ppm` → `sample_delta =
  round(ppm/1e6·distance)`, `output = input·(1+ppm/1e6)`, so `+ppm` = ADD samples = stretch). The
  call site now passes `-applied_ppm`, so a slow source (applied<0) becomes a POSITIVE `sample_delta`
  = stretch. The `swr_set_compensation` bullet above documents the wrapper is a one-shot ramp; THIS
  bullet documents its SIGN relative to the compensator.

**The Tier-0 gate that closes the blind spot** is a NEW pure function, NOT a bench change:
`src/asrc_compensation_quantization.rs::servo_applied_ppm_to_sample_delta(applied_ppm, distance_ms,
output_freq)` = `compensation_sample_delta(-applied_ppm, …)` — the composition of the negation and
the existing #929/#1016 integer quantization, i.e. the exact end-to-end value swresample receives.
Its parity test `servo_negates_applied_ppm_so_a_slow_source_stretches_1325` pins `applied<0 ⇒
sample_delta > 0` (and the fast-source symmetry). Keep the negation in exactly ONE place: the
`obs-source.c` call site AND this pure mirror must agree — the wrapper itself
(`audio_resampler_set_compensation_ppm`) keeps its swresample-native convention, so its own
`compensation_sample_delta` mirror and every #929/#1016 quantization test stay UNCHANGED (do NOT
negate inside the wrapper — that would conflate the sign fix with the quantization floor and break
those tests). Lock-step anchor: `tests/genlock_preload.rs::vendored_source::
asrc_servo_master_clock_is_os_gettime_ns_and_sign_negated_1325` + byte-identical pwsh blocks in BOTH
`windows-genlock.yml` and `windows-genlock-fast.yml` (positive: `const uint64_t mixer_now_ns =
os_gettime_ns();` + the `-applied_ppm` call; negative: the pre-fix non-negated `applied_ppm,` call
must be ABSENT). The `RealtimeAsrcCompensator` corrected-advance model itself is UNCHANGED by #1325.

## Tier-0 RED→GREEN for a SELF-CONTAINED pure module — plain `rustc --test`, no cargo (#1325)

The `# airuleset:build-ok` bypass is DISABLED (#477) and #557 blocks even `cargo test --no-run`, so
the old "cargo test --lib asrc_bench" observation path in the sections above is HISTORY. For a
crate-root pure module that has NO crate-internal deps (`src/asrc_compensation_quantization.rs` —
only std, no `use crate::`), the working Tier-0 RED→GREEN is a STANDALONE rustc compile of the file
itself: `rustc --test --edition 2021 src/asrc_compensation_quantization.rs -o /tmp/x && /tmp/x`. No
`CARGO_MANIFEST_DIR` is needed (that env is only for a `tests/*.rs` file that reads vendored source
via `env!("CARGO_MANIFEST_DIR")` or `use camera_box::…`, e.g. `genlock_preload.rs`). Used live for
#1325's sign gate: buggy body → 1 failed, negated body → 11 passed, a genuine local RED→GREEN with
zero cargo. The precondition is a module with no `use crate::`/`use super::` CODE deps (intra-doc
`[crate::…]` links in comments are fine — rustc ignores doc content); check with
`grep -nE '^use (crate|super)::' src/<module>.rs` before trusting the standalone compile.

## #1335's buffer-LEVEL holding integral — the RATE loop holds tempo, a slow I-term holds the LEVEL

The #1084 regression is a pure RATE servo: it estimates the source-vs-mixer ppm and slews `applied`
to it, but it NEVER reads the mix-buffer LEVEL. Any residual it cannot remove (live `mbc`: the 600 s
window lagging a ±1 ppm wandering true rate ⇒ a ~0.8 ppm MEAN error) integrates into `buffered_ms`
and drifts it monotonically (~3 ms/h, 105→68 over 12.5 h) toward an eventual underrun/resync jump.
#1335 adds a slow LEVEL integral INSIDE the compensator that nulls exactly that residual.

- **State + constants** (Rust `src/asrc_bench.rs` ↔ C `asrc-compensator.{h,c}`, numerically
  identical): `LEVEL_KI_PPM_PER_MS_S=0.0002`, `LEVEL_INTEGRAL_MAX_PPM=3.0`; fields
  `level_target_ms` / `level_integral_ppm` / `level_last_ms` / `level_captured`. Update, once per
  closed ACCEPTED window: capture `level_target_ms = buffered_ms` at first lock (re-captured after
  every flush/relock — the flush resets `level_integral_ppm=0` + `level_captured=false`), then
  `level_integral_ppm = clamp(level_integral_ppm − Ki·(target−buffered)·window_master_s, ±3)`.
  Anti-windup: skip the update while the composite rate target saturates at ±MAX_PPM or the servo is
  unlocked. Folded into `target_ppm = clamp(estimated + outer_bias + level_integral, ±MAX_PPM)`.
- **SIGN** (the design's live 17.9. −5 ppm outer-bias test): a DEFICIT (buffer below setpoint) drives
  the integral MORE NEGATIVE ⇒ more-negative `applied` ⇒ (per #1325) a POSITIVE swresample
  `sample_delta` = STRETCH ⇒ the buffer RISES back to setpoint. `err_ms = target − buffered`.
- **It is an I-ONLY loop on an integrator plant (the buffer), so it is MARGINALLY stable — bounded,
  not critically damped.** Closed-form: `d²b/dt² = −(1e-3·Ki)·b` ⇒ an undamped oscillation, period
  `2π/√(1e-3·Ki) ≈ 3.9 h`, transient amplitude `≈ 2.24·residual_ppm` ms for the lock-time step
  (~1.8 ms for the live 0.8 ppm). That is the design's stated intent ("pomalá slučka … drží ±5 ms") —
  it prevents the monotonic drain, it does not critically damp. The bench test asserts the settled
  MEAN returns to setpoint (±2 ms) + no monotonic trend + PEAK inside ±5 ms; a real re-buffer
  discontinuity (a flush) resets it, which is the production safety net the linear model omits.
- **Rust trait split (bench-only asymmetry, no C analogue):** the shared `AsrcCompensator::compensate
  (raw, master)` trait stays RATE-ONLY (`compensate_core(..., None)`) so `simulate_offset_trace_ms` +
  every pre-#1335 test are unchanged; the level integral lives in
  `RealtimeAsrcCompensator::compensate_with_level(raw, master, buffered_ms)` (`compensate_core(...,
  Some(buffered_ms))`). The C `asrc_compensator_compensate(c, raw, master, buffered_ms, &applied)`
  ALWAYS takes buffered_ms — it is the exact mirror of `compensate_with_level`, never the `None` path.
- **`buffered_ms` at the call site:** obs-source.c `asrc_process_audio` reads it from
  `source->audio_input_buf[0].size` against the mixer OUTPUT rate via the shared
  `obs_source_input_buf_ms()` helper in `obs-internal.h` — the SAME bytes→ms computation the #800
  audio telemetry (obs-audio.c) uses (extracted to ONE helper so the two can't drift). Telemetry: the
  `asrc:` line gains `level=<ms> target=<ms> integral=<ppm> (#1335)`, appended AFTER the byte-identical
  `starved_blocks=%u (#803/#806/#960)` suffix so every dev1 `asrc:`-line parser (asio-starve-health,
  cg-chain-verify — both extract by name with `.*`) is unaffected.
- **Lock-step anchors** for a vendored-C revert: `tests/genlock_preload.rs::vendored_source::
  asrc_holds_buffer_level_with_integral_1335` + byte-identical pwsh blocks in BOTH
  `windows-genlock.yml` and `windows-genlock-fast.yml` (constants + fold + update + telemetry).
- **C↔Rust parity is bit-identical** — proven by a standalone `cc` lift of asrc-compensator.c with a
  main running the SAME 12 h buffer sim as the Rust probe: both produce setpoint 99.652, mean 100.000,
  peak 1.789, integral −0.0773, last 98.885 (identical to the last decimal). Reuse that lift for any
  future change to this pair (per `vendored-libobs-change-safety.md`).

## #1335 follow-up — the LEVEL setpoint must FOLLOW a deliberate audio sync-offset change

The #1335 level integral above holds the buffer at a setpoint captured at first lock. But the mix
buffer depth is not a free variable: obs-source.c applies `in.timestamp += sync_offset` to the audio
timestamps BEFORE placement (obs-source.c ~1733-1734), so a DELIBERATE sync-offset change of Δ shifts
this source's placement — and therefore `audio_input_buf[0]` depth — by exactly Δ (live: `mbc` offset
−4 → −18 ms moved `level` 96.8 → 81.1). Left alone, the level integral reads that Δ as an error and
REFILLS the buffer back toward the OLD setpoint (integral winding toward its ±3 clamp = +11 ms/h),
silently cancelling the deliberate audio trim (the issue-1333 split writes a small mbc offset) within
~1–2 h. The next E2E re-applies the trim, the integral cancels it again — the servo and the audio
actuator FIGHT and A/V floats by the trim size between runs (the exact owner complaint).

- **Fix (Prístup 1): the setpoint FOLLOWS the offset.** A new pure
  `asrc_compensator_shift_level_target(c, delta_ms)` (Rust `RealtimeAsrcCompensator::shift_level_target`)
  — if `level_captured`: `level_target_ms += delta_ms; level_last_ms += delta_ms;` (the integral is
  left UNTOUCHED — no windup); no-op if not captured. Called from the EXISTING
  `last_sync_offset != sync_offset` branch in `source_output_audio_data` (audio thread, `source->asrc`
  lives there, no new lock) with `delta_ms = (double)(sync_offset − last_sync_offset) / 1e6` computed
  from the OLD `last_sync_offset` BEFORE it is overwritten. `sync_offset` is nanoseconds (obs
  `obs_source_set_sync_offset(int64_t)`), so `/1e6` → ms. SIGN: offset −14 ⇒ placement earlier ⇒
  depth −14 ⇒ `target += Δ` (−14).
- **Why NOT a blanket re-capture on any placement discontinuity:** an UNINTENDED discontinuity
  (dropout/relock) must KEEP the calibrated depth and self-heal — those go through
  `asrc_regression_flush()`, which drops `level_captured` so the setpoint re-captures from the
  post-relock depth and the buffer self-heals. Only a DELIBERATE `sync_offset` change reaches the
  `last_sync_offset != sync_offset` branch, so shifting exclusively there distinguishes the two:
  deliberate change ⇒ move the setpoint; disturbance ⇒ let the flush/re-capture self-heal. (Approaches
  2/3 — re-capture on every discontinuity, or freeze the integral N minutes — both lose that
  distinction or only delay the fight; rejected in the design.)
- **Bench** (`shift_level_target_holds_setpoint_after_offset_jump_1335`): lock+settle at depth L with
  NO hidden residual (buffer holds flat), model the offset change as an instantaneous −14 ms buffer
  WITHDRAWAL, observe 2 h. GREEN (shift announced): buffer settles at L−14, `level_integral_ppm` stays
  within ±0.05 ppm of its pre-jump value. Anti-tautology (no shift): the integral WINDS ≥0.3 ppm and
  drags the buffer back to L. C↔Rust parity bit-identical via the standalone `cc -Wall -Wextra -Werror`
  lift: both produce setpoint 85.700000, mean 85.700000, peak 0.000000, integral drift 0.000000, final
  85.700000.
- **Lock-step anchor:** `tests/genlock_preload.rs::asrc_setpoint_follows_sync_offset_1335` pins the
  new C decl + `.c` body + the obs-source.c call site byte-exact (squished). No new pwsh gate — the
  function rides the existing genlock build; add the 3-copy pwsh lock-step only if a future change
  needs a windows-genlock gate.

## #1335 follow-up 2 — step-tolerant regression + fast bounded level restore + a LEVEL P term

The #1335 level integral (above) holds the buffer at a captured setpoint, but two live pathologies
on 17.9. remained: (1) an OBS StartStream stall lost ~50 ms of `mbc` input samples PERMANENTLY
(buffered_ms 108 -> 51, starved_blocks=0) -- a step the #960 starve gate and the non-positive-master
guard both miss, so it entered the 600 s least-squares and biased the slope by ~= step/span = 50 ms
/ 600 s = 83 ppm (est +16 -> -83 -> -152 after a 2nd step); the existing `regression_flush` would
have ENSHRINED the shifted (50 ms lower) level as the new setpoint. (2) The I-only level loop
oscillated clamp-to-clamp (14:00-20:45, +-3 ppm, level +-10 ms). Three small changes fix both, all
mirrored C<->Rust (`asrc-compensator.{c,h}` <-> `src/asrc_bench.rs`), bit-identical.

- **STEP DETECTION -> RE-BASE** (not flush). After each closed ACCEPTED window while `reg_locked`,
  the SINGLE-WINDOW residual `r_s = (window_raw_s - window_master_s) - (estimated_ppm/1e6)*window_master_s`
  (this window's own advance increment minus the locked slope's expected increment -- the cumulative
  noise CANCELS, `pt_ymm - cum_ymm_s == this window's increment`, so it is a per-window quantity, NOT
  a cumulative-vs-OLS-line residual which would fire on the random-walk excursion of accumulated
  noise -- that mistake produced 158 spurious steps in bench (c)). If `|r_s*1000| > STEP_RESIDUAL_MS
  (10.0)`: RE-BASE -- `cum_master_s = pt_master; cum_ymm_s = pt_ymm - r_s` (leaves the anchor on the
  pre-step fit line so future points align), do NOT insert the point, keep the lock+slope+applied (no
  60 s decay), `step_count++`, `last_step_ms = r_s*1000`. Gated to the C path (Rust
  `buffered_ms.is_some()`; C always has buffered_ms so it is unconditional) so the rate-only bench
  trait entry -- which the slew/clamp tests + the #1084 endpoint-jitter gate feed deliberate outliers
  -- keeps the legacy insert unchanged.
- **FAST BOUNDED LEVEL RESTORE.** On a re-base, if the buffer level corroborates a real sample
  loss/dup (`|buffered - target| >= 0.5*|r_ms|` at detection), enter `level_restore`: fold
  `clamp(LEVEL_RESTORE_K_PPM_PER_MS (2.0)*(buffered - target), +-LEVEL_RESTORE_MAX_PPM (100.0))` into
  the correction target; the integral is FROZEN while restoring (anti-windup); exit at
  `|buffered - target| < 5 ms`. A wall-clock-only step (buffer unchanged) -> re-base only, no restore.
  A `regression_flush` (unintended discontinuity) clears `level_restore` (the setpoint re-captures).
- **LEVEL P TERM.** `target_ppm += clamp(LEVEL_KP_PPM_PER_MS (0.03)*(buffered - target), +-1)` every
  call once locked, to damp the I-only oscillation; Ki/clamp unchanged.

**SIGN CORRECTION (load-bearing).** The main design (comment 5720580172) wrote the P term as
`Kp*(target - level)` and the restore as `-Kr*(level - target)` -- BOTH of which, for a buffer
DEFICIT, add a POSITIVE ppm = compress = LOWER the buffer, the OPPOSITE of their own stated intent
("level below target => stretch") and of the proven #1335 integral. The integral is ground truth
(shipped, rig-verified 17.9. -5 ppm outer-bias test, and the passing `realtime_compensator_holds_
buffer_level_with_integral_1335` bench): a deficit drives its contribution NEGATIVE = stretch =
raises the buffer. Both new terms are implemented as the NEGATION of the design's literal formula --
`Kp*(buffered - target)` and `Kr*(buffered - target)` -- so a deficit yields a NEGATIVE contribution,
matching the integral. Routh-Hurwitz confirms the design's literal P sign makes the closed loop GROW
(unstable); the corrected sign damps. This is documented on the ticket (the follow-up-2
anchors-confirmed comment) for the main's review; a future edit MUST keep the `(buffered - target)`
argument order or the loop destabilizes.

**Constants underdeliver the design's stated quantitative acceptance (flagged, not retuned).** The
main design specified Kp=0.03, Kr=2, clamp +-1/+-100, threshold 10 -- used AS-IS -- but its stated
targets are not achievable with them: (a) at Kp=0.03 the level loop's damping ratio is only ~0.034,
so a 20 ms disturbance still overshoots ~90% (NOT the design's `<3 ms overshoot`; that needs Kp~0.6 +
a wider P clamp, which then bang-bangs the +-1 clamp); (b) the restore is PROPORTIONAL so it decays
with a ~500 s time constant (k*Kr = 2e-3/s), returning a 50 ms step to within +-5 ms in ~19-25 min,
NOT the design's `+-5 ms in 12 min` (that needs the +-100 clamp to BIND for most of the return,
i.e. Kr~20 so the burst stays near-constant 100 ppm for the design's own "50 ms -> 100 ppm -> ~8 min"
reasoning). The re-base (the actual -83 ppm-swing fix) is unaffected and works fully; the P/restore
are correctly-signed and HELP, just gentler than the design's aspirational numbers. Retuning Kp/Kr is
a main-owned control-design decision.

- **Bench** (`src/asrc_bench.rs`, four `*_1335` follow-up-2 tests, calibrated from a measured
  standalone-rustc probe, deterministic): (a) 50 ms input-loss step + level -50 -> est held within
  +-2 ppm (RED rate-only path swings >=40), buffer restored within +-5 ms + restore exits (~19-25 min
  observed); (b) +50 ms wall-clock jump, level unchanged -> re-base, no restore, est +-2; (c) bounded
  +-3 ms window noise over 2 h -> 0 steps, a 15 ms outlier DOES step; (d) 5 ms LEVEL disturbance
  (linear regime, no rail) -> the P term decays the oscillation (2nd/1st-half peak ratio 0.89 vs 1.00
  I-only vs 1.11 wrong-sign P) + integral off its +-3 rail. RED->GREEN proven by neutralizing the
  compensate_core logic (all 4 fail; 26 pre-existing pass).
- **C<->Rust parity is bit-identical** -- proven by a standalone `cc -Wall -Wextra -Werror` lift of
  asrc-compensator.c running the SAME (a) recovery + (d) oscillation sims as a Rust probe: both
  produce `(a) final=95.021353289 steps=1 last_step=-50.000000000 integral=-0.638579073` and
  `(d) imin=-1.911140876 imax=2.123645773 p1=4.999848999 p2=4.463095233 ratio=0.892646005` (identical
  to 9 decimals). Reuse that lift for any future change.
- **Telemetry:** the `asrc:` line appends `steps=%u last_step_ms=%.1f restore=%d (#1335)` AFTER the
  byte-identical `... integral=%.3fppm (#1335)` suffix, so every dev1 parser (asio-starve-health,
  cg-chain-verify -- both extract by name with `.*`) is unaffected.
- **Lock-step anchor:** `tests/genlock_preload.rs::vendored_source::asrc_step_tolerant_regression_and_p_term_1335`
  pins the new C constants + re-base + restore + P fold + integral-freeze + the obs-source.c telemetry
  byte-exact (squished). No new pwsh gate (the change rides the existing genlock build); the existing
  `level=/target=/integral= (#1335)` pwsh substring is preserved intact.

## #1335 follow-up 3/4 — a deliberate setpoint shift AND a sustained level error ARM the fast restore (the restore has THREE arm sources)

The fast bounded level restore (`ASRC_LEVEL_RESTORE_K_PPM_PER_MS` / `..._MAX_PPM`, follow-up 2) is now
armed from THREE independent places, never just one:

1. **Step-tolerant regression, level-corroborated** (follow-up 2, `asrc-compensator.c` re-base branch):
   a permanent input sample-loss/dup whose buffer deficit corroborates the residual.
2. **A deliberate setpoint shift `|Δ| >= ASRC_LEVEL_RESTORE_ARM_MS` (5 ms)** (follow-up 3,
   `asrc_compensator_shift_level_target` / Rust `shift_level_target`): a deliberate audio sync-offset
   trim (issue 1333's split) moves `level_target_ms` by Δ, so the level error jumps to Δ.
3. **A SUSTAINED level error `|level - target| >= ASRC_LEVEL_RESTORE_ARM_ERR_MS` (12 ms) for
   `ASRC_LEVEL_RESTORE_ARM_WINDOWS` (10) consecutive accepted windows** (follow-up 4, the
   accepted-window branch, counter `level_err_windows`): a level disturbance that arrives with NO
   same-window residual step and NO deliberate shift — an OBS StartStream input-sample loss, a
   mic/Dante re-plug, a mixer hiccup — the case both 1 and 2 miss. A below-band window resets the
   count; reaching the threshold arms the restore and resets the count; the count also resets next to
   every `level_restore` reset (flush/init/restore-exit). The band (12 ms) sits above the ±8 ms 1-s
   level scatter so ordinary noise never arms; a false arm needs 10 consecutive ≥12 ms MAGNITUDE
   readings (either sign — the arm is on `|level − target|`, not a direction), which ±8 ms cannot do.

WHY follow-up 4 exists: the 18.9. 12:00 StartStream (obs.dll 52813623a, E2E rerun 35329592422 attempt
2) dropped `mbc` `buffered_ms=118 → 92` and `level=100.0 → 68.4` with `steps=1 last_step_ms=-14.3` but
`restore=0` — the step's timestamp jump was detected in a window where the level had not yet drained,
so the follow-up-2 level-corroboration (`|buffered − target| ≥ 0.5·|residual|`) failed, the regression
re-based, and no later window produced a step. The level then sat 10–25 ms low for 40 min (the I term
railed at −3 ppm from 12:17), the run measured **+15 ms** rig-wide (attempt 1, level ON target: −0.6
ms, identical pins), and cleanup applied a −12 ms `mbc` trim to a transient. The sustained-error arm
catches exactly this — any disturbance source, no step or shift required. Trade-off: a 10 s detection
delay plus the bounded proportional burst; at the shipped Kr=2 a 25 ms drop settles to ±5 ms in ~804 s
(the SAME ~500 s time-constant clamp-does-not-bind calibration follow-ups 2/3 document — the design's
aspirational ≤600 s is flagged for main ratification alongside the Kp/Kr note), the integral never
rails (peak ~1.8 ppm, the fast restore does the heavy lifting), "minutes instead of hours". Bench (all
`src/asrc_bench.rs`, calibrated from a standalone-rustc probe, deterministic):
`sustained_level_error_arms_fast_restore_1335` (25 ms drop, no step → armed ≤12 windows + settle
≤900 s + integral off its ±3 rail), `level_scatter_never_arms_fast_restore_1335` (±8 ms scatter → never
arms), `sub_band_level_offset_never_arms_but_band_is_live_1335` (6/11 ms never arm, 13 ms DOES arm —
the band is live at 12 ms). Lock-step anchor: the two new ARM constants + the accepted-window
`++level_err_windows` counter are pinned byte-exact in
`tests/genlock_preload.rs::asrc_setpoint_follows_sync_offset_1335`.

WHY follow-up 3 exists: the 18.9. 12 h acceptance series showed a +12 ms setpoint shift being worked
off by the +/-3 ppm I term alone — it took ~1 h, RAILED the integral (34 samples at the -3 clamp), then
rang with a decaying +-8 ms / ~4 h cycle for hours. The restore burst exists for exactly this move;
arming it in the shift settles a 12 ms trim in minutes (integral frozen per follow-up 2), no windup, no
ringing. The arm band equals the restore's own exit band (`|buffered - target| < 5 ms`) — arming below
it would exit on the first tick. Kr/clamp/exit/integral-freeze are UNCHANGED; at the shipped Kr=2 a
12 ms move settles to +-5 ms in ~433 s (a ~500 s time constant, the SAME clamp-does-not-bind calibration
follow-up 2 documents for its 50 ms step, NOT the design's aspirational ~2 min — flagged for main
ratification alongside the follow-up-2 Kp/Kr retune note). Bench:
`shift_level_target_arms_fast_restore_on_deliberate_shift_1335` (armed + settle <=600 s + integral never
rails for the +12 ms case; arms nothing for +3 ms). Lock-step anchor: the new ARM constant + arming line
are pinned byte-exact in `tests/genlock_preload.rs::asrc_setpoint_follows_sync_offset_1335`.

## #1335 follow-up 5 — a SMOOTHED proportional term becomes the NORMAL LAW of the level loop

The 18.9. 12 h live series showed follow-ups 2-4 could not HOLD the buffer level: with `Ki` 0.0002
(a 15 ms error winds the integral to its +-3 clamp in ~17 min, 3 ppm moves the level only 10 ms/h)
and the P term at `Kp` 0.03 clamped +-1 ppm on the RAW per-window level, the mean level wandered
+-10-15 ms around the setpoint over hour-scale spans and the E2E A/V reading inherited it (-0.6 ms
vs +15.0 ms 40 min apart, identical pins). Follow-up 5 makes a SMOOTHED proportional term the normal
law:

- **`Kp` 0.03 -> 2.0, clamp +-1 -> +-`LEVEL_KP_MAX_PPM` (50 ppm)**, and the P term now reads a
  SMOOTHED error `level_err_ema_ms` (an EMA of `buffered - target`, `alpha = window_master_s /
  (LEVEL_EMA_TAU_S + window_master_s)`, `LEVEL_EMA_TAU_S` 10 s) instead of the raw per-window level.
  Loop time constant `~= 1/(Kp*1e-3) = 500 s` (a 15 ms error is ~3 ms low at ~13 min, a 25 ms drop
  in ~13 min, at inaudible rates). The 66x stronger gain is usable ONLY because the error is smoothed
  first: a raw 2 ppm/ms on the +-10 ms mixer-tick phase noise would jitter the rate +-20 ppm/s; the
  EMA attenuates that below the +-50 clamp's resolution (bench (b): mean `|applied - estimated|`
  ~0.95 ppm). The integral (`Ki`, +-3) is KEPT but now only carries the DC residual; the restore
  paths (follow-ups 2-4) become rare backstops.
- **New state:** `level_err_ema_ms` + `level_err_ema_seeded` (seed with the first error after
  capture, reset on flush/relock). New constants `LEVEL_KP_MAX_PPM` (50.0) / `LEVEL_EMA_TAU_S` (10.0)
  mirrored C<->Rust. `LEVEL_KP_PPM_PER_MS` is now 2.0 both sides.
- **SIGN CORRECTION on the shift (load-bearing, follow-up-2 class).** The main's Architektura wrote
  `level_err_ema_ms -= delta_ms` in `shift_level_target`. IMPLEMENTED AS NO ADJUSTMENT: a deliberate
  shift moves BOTH `level_target_ms` (+delta) AND the buffer level itself (+delta, via the sync-offset
  re-stamp -- the 18.9. live level 80 -> 108 ms in one second), so the smoothed error
  (`buffered - target`) is UNCHANGED and the EMA needs no adjustment -- leaving it alone is exactly
  what "the smoothed error must not see a false transient" requires. A standalone-rustc probe shows
  the literal `-= delta` INJECTS a -delta transient and slews `applied` to the +-5 ppm/window cap on
  the next window, FAILING the design's own follow-up-5 test (c) `|d applied| <= 2 ppm`; with no
  adjustment the swing is 0. Documented on the ticket for the main's review (same as the follow-up-2
  P/restore SIGN CORRECTION). A future edit MUST NOT reintroduce the `-= delta`.
- **The design's aspirational quantitative targets underdeliver at the shipped Kp=2 (flagged, not
  retuned).** Test (a)'s design line was "MEAN within +-3 ms after <=600 s"; at Kp=2 (tau ~500 s) a
  15 ms error is ~4.5 ms at 600 s and reaches +-3 ms at ~777 s (measured), so the bench uses a <=900
  window cap with a comment -- the SAME ~500 s clamp-does-not-bind calibration follow-ups 2-4 document
  for their settle times, flagged for main ratification. The loop still holds the level to a few ms in
  ~13 min vs the old hour-scale wander, which is the ticket's whole point.
- **Bench** (`src/asrc_bench.rs`, three `*_1335` follow-up-5 tests, calibrated from a standalone-rustc
  probe, deterministic): `smoothed_p_term_holds_level_against_tick_noise_1335` (15 ms deficit + +-10 ms
  tick noise -> mean back within +-3 ms by <=900 windows, integral off its +-3 rail; RED on base: dev
  14.3 ms at 600 s, never within +-3 ms), `smoothed_p_term_does_not_chatter_on_tick_noise_1335` (+-10
  ms noise at target -> mean `|applied - estimated|` <= 3 ppm, restore never arms -- guards the EMA
  in), `deliberate_shift_does_not_spike_the_p_term_1335` (shift +12 with the level jumping +12 same
  window -> `|d applied|` <= 2 ppm -- guards the shift SIGN CORRECTION).
- **Lock-step anchor:** `Kp` 2.0 + the P-term line (`Kp * c->level_err_ema_ms`, clamp
  +-`ASRC_LEVEL_KP_MAX_PPM`) are pinned in `asrc_step_tolerant_regression_and_p_term_1335` (the old
  `0.03` + `-1.0, 1.0` anchors updated there); the two new constants + the two new fields + the EMA
  update line are pinned in `asrc_setpoint_follows_sync_offset_1335`. No new pwsh gate (the change
  rides the existing genlock build; the existing `level=/target=/integral= (#1335)` substring is
  preserved intact). C<->Rust parity is a NUMERICAL contract; re-run a standalone lift for any future
  change to this pair (per `vendored-libobs-change-safety.md`).

## #1355 — the level setpoint is ABSOLUTE (`LEVEL_TARGET_MS` + the placement offset), bounded when unreachable

Everything above that says "capture `level_target_ms = buffered_ms` at first lock" is PRE-#1355
HISTORY for a mixed, non-genlock source. The depth-at-lock capture froze a random `mbc` depth per
stream-OBS launch (captured 58.9 … 126.1 ms over 10 launches, 18.–23.9.) and the loop then held it,
so every launch had its own A/V level (dock + `mbc` level ≈ 135 ± 6 ms in every launch). Now:

- **Capture = `LEVEL_TARGET_MS` (100.0) + `level_offset_ms`** (Rust `src/asrc_bench.rs` ↔ C
  `ASRC_LEVEL_TARGET_MS`, numerically identical). `level_offset_ms` is CALLER state, set every audio
  callback through the pure store `set_level_offset_ms` / `asrc_compensator_set_level_offset_ms`.
  obs-source.c `asrc_process_audio` passes `last_sync_offset / 1e6`, the offset the samples ALREADY
  in the buffer were placed with. It must be `last_sync_offset`, not the pending `sync_offset`.
  `asrc_process_audio` runs BEFORE `source_output_audio_data` places the same block, and a change in
  that same callback still reaches the target through the existing shift
  (`sync_offset - last_sync_offset`). Using `sync_offset` would count a same-callback change twice.
  A flush never clears `level_offset_ms`, so a relock re-captures the SAME absolute level.
- **Absolute ONLY for a mixed, non-genlock source** (`set_level_absolute` /
  `asrc_compensator_set_level_absolute`, obs-source.c passes `!genlock_fifo && monitoring_type !=
  OBS_MONITORING_TYPE_MONITOR_ONLY` every callback; default true). A `genlock_fifo` source's depth
  is the #1303 video-paired hold plus a ~0–25 ms transport base. On 23.9. stream's `fallback repro`
  sat at 967 and 1001 ms under a ~976 ms hold. Forcing it to hold + 100 would add ~80 ms of audio
  delay against its video. A MONITOR_ONLY source never places into the mix buffer (depth 0), so
  100 is unreachable for it. Both keep the pre-#1355 depth-at-lock capture. The 100 ms base is
  calibrated on `mbc` (ASIO, direct timestamps) only, so any OTHER non-genlock mixed source (none
  on stream today besides silent never-locking ASIO/test inputs) gets it on trust; audit its
  `asrc:` line after deploy. A rule CHANGE while captured drops the capture for the level loop only
  (the rate lock and the integral are kept), so the next window re-captures under the new rule.
- **The offset setter never moves a captured target.** After capture the target follows deliberate
  changes 1:1 through `shift_level_target` from TWO call sites in `source_output_audio_data`. The
  first is the existing sync-offset branch. The second is (#1355) a change of the applied genlock
  audio hold (`genlock_audio_delay_ms` vs its previous value: a pin write, genlock toggled). Before
  the second call site, a pin write without a flush left a genlock source's captured target stale,
  so the level loop walked the depth back and undid the pin's audio hold (a pre-#1335-follow-up gap).
- **No new arm.** The walk to the target uses the existing law. The smoothed P term (Kp 2, clamp 50)
  starts at once. The sustained-error arm (≥ 12 ms for 10 windows) fires the restore burst (≤ 100
  ppm). A < 12 ms offset is walked by P alone (10 ms: ~410 s). Arming the restore at capture was
  rejected: a capture on a noisy reading (±8–10 ms) would arm on noise, which breaks the #1335
  `level_scatter_never_arms…` / `…does_not_chatter…` guarantees.
- **Unreachable bound:** `LEVEL_TARGET_UNREACHABLE_WINDOWS` (2400) CONSECUTIVE accepted windows with
  the SMOOTHED `|level_err_ema_ms| >= LEVEL_RESTORE_ARM_MS` (the restore's own exit band) trigger a
  fallback. The fallback sets target = the SMOOTHED live depth (`target + level_err_ema_ms`, never
  one noisy reading), turns the restore off, zeroes the EMA error, bumps the saturating counter
  `level_fallback_count` and raises the one-shot `level_fallback_pending`. In C, obs-source.c logs a
  `LOG_WARNING … UNREACHABLE … (#1355)` line and clears it; in Rust, `take_level_fallback_pending`
  reads and clears it. **At most ONE fallback per capture** (`level_fallback_done`, cleared by a
  flush or a rule-change re-capture). Without that latch, a residual the rate loop cannot see and
  the P+I terms hold at ≥ 5 ms (above ~13 ppm) re-trips every 40 min and ratchets the target (review
  bench: 8 fallbacks, target 100 → 136 ms in 6 h at 14 ppm). The EMA, not the raw level, is counted,
  so ±10 ms tick noise at target never counts (bench: 3 h, 0 fallbacks). 2400 ≈ 2× the slowest
  legitimate walk measured (98 ms, 26 → 124 ms under ±10 ms noise: ~1180 s to within 5 ms).
- **Bench** (six `*_1355` tests, run by `rustc --test`):
  - Walks: start 64 / 90 / 126 ms (± 10 ms noise) all hold at 100 ± 3 ms (RED: 63.70 / 89.70 /
    125.70). Max per-window rate step = the existing 5 ppm slew limit, and max
    `|applied − estimated|` stays inside the restore + P + integral clamps.
  - Offset and trims: a +24 ms pre-lock offset is captured and held as 124. A −14 ms trim moves the
    hold 1:1 against a no-trim control and leaves the integral within 0.05 ppm of that control. A
    flush + relock re-captures 110.
  - Unreachable target: a 60 ms mixer floor under a 40 ms target falls back exactly once (window
    2459, noise-free) and the flag is one-shot.
  - Synthetic-reading bookkeeping: an in-band dip restarts the CONSECUTIVE count, a flush restarts
    it, and on the exact fallback window under ±7 ms noise the restore is off, the EMA is 0 and the
    target is the smoothed depth.
  - A 14/20/30 ppm unseen residual over 6 h gives ≤ 1 fallback.
  - Non-absolute sources: a depth-0 source captures 0 and is never pushed, a 64 ms source holds 64,
    and a rule change re-captures and walks to 100.
  - Eight hand mutants of the bound/rule bookkeeping (consecutive reset, fallback-restore,
    fallback-EMA, flush-reset, latch, raw-reading fallback target, absolute rule, raw-error bound)
    are ALL killed.
- **Bench-harness trap (not a live defect):** the harness feeds ONE level reading per 1 s window. An
  alternating ±10 ms reading around an error of ~0 therefore never lands inside the restore's RAW
  ±5 ms exit band. A shift-armed restore then never exits and slowly drags the level (~5 ms/h).
  Live, `compensate` runs every audio callback and the buffer sweeps its whole ±10 ms tick sawtooth
  within one mixer tick, so the raw exit fires at once. Model a shift/trim scenario with zero tick
  noise, or with per-callback readings, never with a one-reading-per-window alternating ±10.
  Switching the exit to the EMA was tried and rejected: a re-based step window skips the EMA update,
  so an EMA exit would cancel a step-armed restore on its first call. The same trap makes a
  synthetic "restore active at the fallback" scenario need readings that stay ≥ 12 ms off (so the
  sustained arm fires) AND ≥ 5 ms off the smoothed depth: ±7 around +20, not ±10.
- **C↔Rust parity:** a `gcc -std=gnu11 -Wall -Wextra -Werror` lift of the REAL
  `asrc-compensator.c` (include dir `vendor/obs-studio/libobs/media-io`, `-lm`) with a `main` runs
  eight scenarios: walks, a ±10 ms walk, the +24 ms offset, the unreachable floor, a non-absolute
  source, a 20 ppm ratchet and a synthetic ±7 ms fallback. It prints the same 9-decimal numbers as
  a Rust probe that `#[path]`-includes `src/asrc_bench.rs` (e.g.
  `ratchet20 … target=91.914315223 … fallbacks=1 fb_at=2768`). The obs-source.c wiring is checked
  with `gcc -fsyntax-only -Wformat=2` against the real headers plus a scratch `obsconfig.h` (the
  `obs-drm-output.md` net).
- **Lock-step anchor:** `tests/genlock_preload.rs::vendored_source::asrc_absolute_level_setpoint_1355`
  pins:
  - the constants, the fields and both setter declarations;
  - the capture ternary, plus the ABSENCE of the old
    `c->level_target_ms = buffered_ms; c->level_captured = true;`;
  - the latched bound and the smoothed fallback line;
  - both obs-source.c setter calls and the genlock-hold shift;
  - the `UNREACHABLE` and `fallbacks=%u (#1355)` strings.

  No new pwsh gate is needed: every existing windows-genlock*.yml asrc substring keeps its presence
  (checked mechanically). The telemetry `fallbacks=%u (#1355)` is appended AFTER the
  byte-identical `restore=%d (#1335)`.

## Issue 1372 — a CONFIRMED step is paid back at 1000 ppm, not restored proportionally

At the first dantesync fleet date step the stream `mbc` lost ~44 ms of Dante samples upstream of
OBS (a real loss: `last_step_ms=-43.7`, `restore=1`, `ts_lag_ms` flat). The follow-up-2 restore
(Kr 2, ±100 ppm, decaying) took 3–4 min. ROZHODNUTÉ 5841039244: recover a confirmed step at
1000 ppm (1 ms per second, the #1303/#1367 placement-slew pitch budget).

- **Where:** the follow-up-2 corroboration on a re-base, now SIGNED (the buffer moved the same way as
  the samples, by ≥ half of them), BOOKS the step instead of arming `level_restore`
  (`asrc_step_recover_book` ↔ `step_recover_book`): the MEASURED loss `−r·1000` is added to
  `step_recover_ms`, the total capped at ±`STEP_RECOVER_MAX_MS` (100 ms), and `level_target_ms` moves
  by the booked amount. The level only confirms: one callback's reading is up to ~10 ms off within a
  block (the bench's step callback read 35 ms for a 43 ms loss); booking the smaller of the two left
  the rest to the slow level loop. Every call then pays (`asrc_step_recover_pay` ↔
  `step_recover_pay`) `min(owed, STEP_RECOVER_PPM·1e-6·master_block_s·1000)`,
  reports it as `step_recover_ppm` (servo sign: negative = stretch), and moves the setpoint and the
  open window's level sum back up by the payment. The P term, the restore arms and the unreachable
  bound see no error (the level and the setpoint move together), so the servo's own `applied_ppm`
  stays in its steady band.
- **Separate term, clamps unchanged:** `ASRC_MAX_PPM`, the 5 ppm/s slew limit and the ±100 restore
  clamp are untouched; the returned corrected advance and `obs-source.c`'s resampler call carry
  `applied + recover`. The shift/sustained restore arms still use the ±100 proportional burst.
- **Cleared by** a flush, a capture-rule change, and any call without a captured setpoint.
- **Held, not dropped,** while the #1367 audio-placement slew runs: obs-source.c calls
  `asrc_compensator_set_step_recover_hold(&source->asrc, genlock_audio_slew_remaining_ns != 0)` before
  `compensate`, so the two 1000 ppm terms never stack.
- **Bench:** `src/genlock_wall_step_bench.rs` (per-callback `mbc` plant, 1 ms bursty delivery, the
  logged 44 ms gap): the level is within 2 ms of target from +60 s on (legacy: up to ~36 ms off);
  no event, a 5 ms loss and a master-only jump never set the term.
- **Parity:** `tests/asrc_compensator_parity_1367.rs` traces `rec=` / `recp=`; the step scenario must
  show a −1000 ppm payment AND a held window (the driver holds windows 903–905). Scratch C mutants
  (half payment, no setpoint move, half booking, hold ignored) all diverge. Unit:
  `a_confirmed_step_books_the_measured_loss_capped_and_signed_1372`. Full contract:
  `genlock-wall-step.md`.

## Issue 1367 — the level loop reads the per-window MEAN, not the window-closing reading

- **Why:** the mixer drains the input buffer in 1024-sample ticks (21.33 ms), so one `buffered_ms`
  reading lands at a random point of a ~21 ms sawtooth (live `level=` sd 6.44 ms vs 6.16 predicted
  for a uniform tick phase). A 1 s window is ~46.9 ticks, so the closing reading aliases the
  sawtooth into a slow pattern the 10 s EMA partly passes, and Kp 2 dithered the resampler rate
  (live `applied − estimated` sd ≈ 7 ppm). The true depth held within ±2 ms the whole time.
- **What changed:** every call's `buffered_ms` is summed into the open window
  (`window_level_sum_ms` / `window_level_count`, reset with the other window sums at every close).
  Capture (non-absolute depth at lock), the EMA, the integral, the sustained arm and the unreachable
  bound read the window MEAN. The per-call restore burst/exit and the re-base corroboration keep the
  live reading (the one-reading bench trap above still applies to them). `level_last_ms` (`level=`)
  stays the raw closing reading; the mean is `level_avg_ms` (`level_avg=` on the `asrc:` line).
  `shift_level_target` also moves the open window sum by `delta × count`, so a deliberate trim that
  lands mid-window never feeds the EMA a blended half-old/half-new mean (a −0.55 ms EMA step, a
  −1.1 ppm P kick, otherwise).
- **Bench** (`*_1367`): `run_tick_sawtooth_1367` is the first per-CALLBACK level bench — a +20 ppm
  source, 128-sample callbacks, a physical 1024-sample drain, and bursty delivery from an LCG (each
  callback up to J late, monotonic so the flush path never fires). Chatter = sd of
  `applied − estimated` sampled once per second after a 6000 s warm-up: 1 ms jitter 3.71 → 0.87 ppm,
  2 ms 3.57 → 0.35 ppm. With NO jitter the callback/tick phase walks only 0.02 ms per window and the
  mean still carries a slow phase bias of at most half a block, so that case is held to a per-second
  change < 0.25 ppm rms plus sd < 1.5 ppm (single reading: 0.27 / 11.7). Two unit benches pin the
  mean as the loop input and the mid-window shift; both were watched failing under a scratch mutation.
- **C↔Rust parity is now a COMMITTED gate:** `tests/asrc_compensator_parity_1367.rs` compiles the
  REAL `asrc-compensator.c` (whole file, `-I` media-io, `-Wall -Wextra -Wconversion -Wformat=2
  -Werror`, `-lm`) with a C driver and requires its 9-decimal trace to equal the Rust authority's
  line for line over three scenarios (tick sawtooth 2 h; non-absolute mid-window shift + 40 ms loss
  → restore; absolute 50 ms step re-base + starved window + zero master block). It fails loud when
  `cc` is missing. Two scratch C mutations (EMA back on the raw reading; no window-sum shift)
  diverge at trace lines 1 and 131. Local Tier-0 run: copy the test, swap the `use camera_box::…`
  line for a `#[path]` include of `src/asrc_bench.rs`, and `rustc --test` it with
  `CARGO_MANIFEST_DIR` + `CARGO_TARGET_TMPDIR` set. `clippy-driver --test -D warnings` on the same
  copy covers the lints. To mutate the C for a bite check, point the copy's `C_SRC` at a scratch
  `.c` (its own `#include "asrc-compensator.h"` still resolves through `-I` media-io) — never edit
  `vendor/` in place.
- **Known, harmless side effect:** the sustained arm now reads the mean, but the restore's per-call
  exit still reads the live reading on the ±10.7 ms tick sawtooth. A steady 12–16 ms error therefore
  arms reliably after 10 windows and the exit ends the burst within about one tick, so the restore
  toggles on/off about every 10 s. The burst lasts a few calls and the applied rate is slew-limited,
  so the rate barely moves; the P term (24–32 ppm at that error) carries the correction. Switching
  the exit to the mean needs a per-call mean the open window does not have yet; leave it unless a
  live `restore=` flapping trace shows a real cost.
- **Review-round additions (same branch):** `shift_level_target` moves the open window sum even
  before capture (a shift inside the capture window of a non-absolute source captures the new-frame
  mean); the parity driver puts the 50 ms loss on the window-closing callback (so re-base
  corroboration on the mean would diverge), shifts once inside the capture window, traces
  `level_avg` right after the captured shift, and compiles with `-ffp-contract=off`. Four scratch
  C mutants now diverge: `level_avg -=` (line 132), the window-sum shift back under the capture gate
  (line 121), re-base corroboration on the mean (line 224), and the EMA on the raw reading (line 1).
- **Lock-step anchor:** `tests/genlock_preload.rs::vendored_source::asrc_level_loop_reads_the_window_mean_1367`
  (fields, accumulate/close/reset lines, the three mean-reading lines, the shift line, the telemetry
  tail); the #1355 capture anchor now ends `: window_level_ms;`. No pwsh change: the anchored
  `ASRC_LEVEL_KI_PPM_PER_MS_S * err_ms * window_master_s` and `level=%.1fms target=%.1fms
  integral=%.3fppm (#1335)` lines stay byte-identical.
