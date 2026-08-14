---
paths:
  - "src/genlock_backlog.rs"
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
---

# Diagnosing a genlock FIFO limit-cycle from a failed E2E verdict (#998, 2026-08-06)

The issue-998 root-cause (settle-back drain target used ROUND instead of CEIL, so any
`frac(latency_ms / 33.333ms) < 0.5` made the natural hold depth permanently exceed the target →
one drain drop + one late-hold regain every ~2.3 s = 1 duplicated + 1 skipped program frame,
forever). The fix is `drain_target_frames` (ceil) in `src/genlock_backlog.rs`, mirrored in
`genlock_should_drain_one` (obs-source.c). `steady_depth_frames` (round) is UNTOUCHED — the
issue-940 relock-margin caller needs round; do not "unify" them. What survives for the NEXT
verdict investigation is the diagnostic toolkit that found it:

1. **`frozen_leg` entries are per-WINDOW AGGREGATES, not events.** Each entry's `since` is the
   WINDOW's start_ns, so N defects uniformly spread inside a window all report the same `since` —
   which reads as "periodic global stutter every ~30 s" when the window schedule alternates
   cameras (~30.25 s cadence). Before believing any "periodic every ~30 s" theory, check whether
   the period equals the window schedule. (This artifact spawned the wrong #997 theory.)

2. **copies ≈ gaps, balanced AND uniformly distributed across the run = FIFO limit-cycle
   signature** (a drop/regain pair repeats on a throttle cadence — DRAIN_MIN_TICK_INTERVAL).
   Head-clustered defects = transition-related (relock, mode switch) instead.

3. **The cheap decisive discriminator: stream 'NDI 2ME PGM' genlock audit deltas over the
   recording window.** Read `dropped_due` and `late_holds` counters from the OBS log before/after
   the recording. A defective run measured +152/+151; a clean run +0/+0. One log read settles
   "is the FIFO itself misbehaving" without any decode.

4. **The frac discriminator:** limit-cycle fires only when `frac(latency_ms / 33.333) < 0.5`
   (round undershoots ceil). Evidence table lived on #998: latencies .90/.68/.73 clean,
   .45/.23 anomalous. If a defect appears/disappears as the calibrated latency drifts across the
   .5 boundary between runs, suspect a floor/round-vs-ceil bug in a depth target.

5. **OBS log lines carry NO date.** A time-only regex over a multi-day log matches every day —
   disambiguate by finding the midnight `^00:00:` line-number cluster and slicing line ranges,
   before comparing "today's" rate to anything.

Tolerance history context: `WINDOW_COPIES_GAPS_TOLERANCE` was recalibrated 2→3 (2026-08-06)
against the chronic ~5-8 copies + ~5-8 gaps residual burden of the 62.15 fps over-rate decimation
lane; commitment on #889 — when that lane shrinks the burden, the tolerance comes back DOWN.

## The SKEW-AXIS variant: a "converge toward configured latency" target must floor at the ACHIEVABLE phase (#1049, 2026-08-14)

The #998 limit cycle above is on the DEPTH/LATENCY axis (round-vs-ceil target undershoots the
natural hold). #1049 added a bounded PHASE-convergence shed (`should_converge_phase` in
`src/genlock_backlog.rs`, mirrored in `obs-source.c genlock_phase_converge_due`) that pulls a
persistent per-camera acquire-phase back toward the configured latency — and its first cut hit the
EXACT same drop/regain limit cycle on a NEW axis, the transport SKEW.

The trap: the natural steady on-air age `S = wall - locked_boundary` is FLOORED by the stamp→arrival
skew — a frame physically cannot present before it ARRIVES. A target of `reserve + interval/n +
hysteresis` ignores that floor, so on a SHALLOW-reserve source whose skew exceeds it (the rig runs
~20 ms cam→strih skew at the 3 ms prod floor; up to 59 ms live on `NDI cam5`), the shed fires
forever at the natural phase: shed pushes the boundary below what arrived → next tick(s) HOLD/regain
→ 30 ticks later shed again. One dup + one skip per ~second, on air, indefinitely — indistinguishable
from the #998 symptom, via a different mechanism.

**The invariant, reusable for ANY genlock target that converges toward a configured value: floor the
target at the ACHIEVABLE phase.** #1049's fix: `target = max(reserve, floor)`, `floor = wall -
newest_queued_stamp` (`array[num-1]` in C, `queue.back()` in the probe — the freshest presentable
frame's age). Then post-shed `S' = S - quantum > target >= floor` can never go below what arrived, so
the cycle is structurally impossible on the skew axis too (the #998 "upper-bound the natural steady
state" lesson, applied to skew instead of frac).

**How it was caught — and how it was ALMOST missed: the test skew ENVELOPE was too narrow.** The
committed Tier-0 conveyor sim (`SimConveyor1049`) used a single 8 ms skew and passed while
limit-cycling at 15-30 ms; the existing probe cadence sims that WOULD have caught it
(`cadence_survives_deep_arrival_skew` at skew 20, `cadence_releases_every_frame_once_at_grid_aligned_reserve`)
are CI-only. An adversarial review + a default-feature replica (per
`probe-mirror-replica-testing.md`) SWEEPING skew 8/15/20/30/59 ms across reserves 3/8/20/26/36
exposed 19-22 spurious drops. **Rule: any no-limit-cycle test for a genlock target MUST sweep BOTH
the reserve AND the transport-skew axes — a single skew value is exactly the hole that hides this.**
The natural-phase no-shed test (`convergence_never_sheds_at_the_natural_steady_phase_1049`) now
sweeps both.
