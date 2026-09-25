---
paths:
  - "src/genlock_backlog.rs"
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
  - "scripts/av_sync_calibrate.py"
  - "scripts/e2e_measurement_pins.py"
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

## The THIRD axis: a phase shed only STICKS on an N>=2 source — gate convergence to N>=2 (#1049, 2026-08-14, live)

After the floor fix above shipped, the convergence still limit-cycled — on a completely different
source: the stream box's DEEP N==1 `NDI 2ME PGM` (30-into-30, 990 ms). The floor fix did NOT cover
it (`floor = wall - newest_stamp` reads the FRESHEST frame ~33 ms old for a deep source, so
`target = max(reserve, floor) = reserve`; the natural grid-quantized hold ~1033 ms — one frame above
configured at frac 0.7 — still sat above the reserve-based threshold and sheds fired ~0.7/s forever).

**The root cause is structural, not a threshold-tuning miss: an N==1 phase shed cannot STICK.** An
N==1 source delivers exactly ONE frame per render tick, so presenting one frame fresher leaves the
queue unable to refill — the very next tick HOLDS and regains the shed frame within the throttle
window (`converge_sheds` and `holds` climbed in LOCKSTEP in the live audit, one pair per ~1.4 s —
the #998 dup+skip signature again). An N>=2 source delivers >=2 frames/tick, so the shed IS
sustainable and sticks — which is BOTH why the strih 60-into-30 ladder converges AND why only N>=2
sources exhibit the pathology (a per-camera acquire ladder exists only ACROSS the multi-camera N>=2
ingests; a single N==1 source has no cross-source spread, and its A/V offset is corrected by the
±50 ms 2ME PGM controller).

**Fix: gate convergence to N>=2** (`should_converge_phase` early-returns for `source_multiple < 2`,
mirrored in `genlock_phase_converge_due`). A hysteresis band was REJECTED: the natural hold overshoot
is frac-dependent (up to ceil+2 frames) and differs by n, so no fixed frame-multiple band separates
"natural hold" from "real error" across both n=1 and n=2 — whereas "does the shed stick" is exactly
n>=2 vs n==1. The lesson generalises: **a convergence/drain shed is only valid on a source that can
SUSTAIN the shallower state it produces; if presenting-fresher just triggers a hold+regain, the shed
is futile and gating it off is the fix, not widening its threshold.**

**Diagnostic tell (from the live audit, recognisable in one 3-line read):** `converge_sheds` AND
`holds` climbing in lockstep (one pair per ~throttle interval) with `dropped_due` pairing them,
`relocks=0`, `depth` stable, and `ts_head_skew_ms` CONSTANT and ABOVE `latency_ms` — the shed is
fighting a stable natural hold it cannot move.

**Issue 1367 did NOT reverse this gate — it added a DIFFERENT N==1 target.** The reserve-aimed
#1049 shed stays N>=2-only (`genlock_phase_converge_due` keeps `if (n < 2) return false;` byte for
byte). A deep N==1 source now converges to its PIN-DERIVED depth `base + 1`
(`src/genlock_n1_depth.rs`, routed by the SOURCE wrapper): that target IS the natural hold, so a
shed there removes only a frame a sender restart added and it sticks. Full rule:
`genlock-n1-pin-derived-depth.md`. Tell for a misfiring N==1 rule: `n1_grows=` and `converge_sheds=`
climbing in lockstep on the 2ME PGM in a steady window with no sender restart.

## The deep-latency release-phase QUANTUM (#1003) is STRUCTURAL — do NOT re-attempt the "grid pin"

`#1003`'s title asks to "pin the release to an absolute wall-clock frame grid" to remove the
±1–2-frame cross-camera A/V spread. That mechanism is REFUTED and already present — do not build it
(re-dispatched 5+ times, each concluding the same; supervisor decision 2026-08-19 "Stage-2
vendored-C grid-pin sa NEPÍŠE"). The grid pin ITSELF is `phase_pinned_deadline` (#940, floors the
reserve deadline to the receiver grid); it did not remove the residual, and no receiver-side change
can:

- The release cadence is a WHOLE-FRAME conveyor: it moves a source's on-air age `S` only in
  `interval/n` steps, so `S mod (interval/n)` is INVARIANT under every selection/shed — "pin the
  phase to a grid" / "converge the phase modulo the source interval" names a quantity the cadence
  physically cannot touch. After `should_converge_phase` the integer-frame part is already
  deterministic (each camera settles in the one-source-frame band `(floor+5, floor+21.7]` above
  its OWN floor), so the cross-camera residual is `Δskew` quantized to source frames (~33 ms = the
  #1168 budget-bound residual, ANTI-correlated with the floor — low-floor cams carry the largest).
- Equalizing needs a COMMON target ≥ max floor = ADDING latency (a frame can't present before it
  arrives) = the owner-rejected production-pin promotion. The only lever is pin/config-layer, owned
  by #1168's re-arm trail — never a receiver-side code change.

Full reasoning lives in code, where a worker lands first: the extended "WHY the #940 grid pin was
not enough" narrative block in `src/genlock_backlog.rs` (MECHANISM half) + the
`AV_OFFSET_GATE_TOLERANCE_MS` doc in `src/av_window.rs` (GATE half, why the A/V tolerance is ±30 —
owner ruling 17.9.2026 / issue 1333 — not the older ±90 or ±20).

## Sender render-freeze -> receiver relock STORM + a transient phase excursion (issue 1318, 2026-09-15)

Owner reported "kamery nedržia sync": the stream `NDI 2ME PGM` A/V offset wandered +13..+47 ms
at a CONSTANT pin. Two distinct phenomena share that symptom — separate them before theorising:

1. **The visible excursion is a SENDER stall, not a receiver policy bug.** strih's OBS PROGRAM
   render thread froze ~5-9 s once (`program-render-audit avg_frame_ms=782 lagged=228` — the ONLY
   lagged>0 window in 95 min), triggered by a scene switch (`User switched to scene 'Cam 6'`)
   kicking off a DistroAV `ndi_source_update` re-init cascade under an already-tight 4K multiview
   (rendered_fps 23-28 vs 30). Frames were not RENDERED -> not OFFERED to the 2ME PGM NDI output
   (`genlock-ndi-output` `offered` +43 over 9 s then a +222 catch-up burst, `dropped=0`,
   `max_send_wait_ms` flat = the SEND path was healthy). The stream receive FIFO underran to
   `depth=0`, then absorbed the catch-up burst as an overshoot to `depth=41` (13 over steady 28)
   -> a 462-relock storm in 17 s -> the presented `ts_head_skew_ms` sat +2 frames deep (1005/1038)
   for ~40 min before decaying. The DISCRIMINATOR: the storm is ONE tight cluster (all 462 relocks
   in 18:27:28-44), NOT spread — a one-off transition, not a steady limit cycle. Correlate the
   stream relock-burst window with the strih `program-render-audit lagged>0` window: a match = a
   sender render freeze; the fix belongs on the SENDER (render-budget / DistroAV re-init), NOT the
   receiver FIFO. A NETWORK microburst would instead show in stream `recv-timing cap_max`; a
   receiver bug would show as a persistent (not one-shot) storm.
2. **The baseline +-1-frame (33 ms) wander (938<->971) is the structural issue-1003 residual**
   (deep N==1 source, whole-frame conveyor; `S mod (interval/n)` invariant). REFUTED to fix
   receiver-side. Do NOT chase it with an aggressive relock erase — the phase-continuity
   "erase-nothing" relock is deliberate.

The detection metric shipped for this: the `relock_bursts` family in `src/jitter_audit.rs` +
`genlock-jitter-report`'s relock-burst table (a cumulative `relocks=` counter freezes after the
storm; the per-event burst metric preserves WHEN + HOW-INTENSE). A dev1 watchdog / bundle-state
facet paging on `bursts>=1` catches the NEXT sender stall in minutes instead of ~90 min via the
dock offset. The sender-side cure (strih render freeze on scene switch) is a separate scoped lane.

## The stream 'NDI 2ME PGM' pin is a FRAME-QUANTIZED actuator — split A/V correction (#1333, bod 4)

The E2E A/V controller (issue 856 / issue 1265) wrote the stream `NDI 2ME PGM`
`genlock_latency_ms_src` as an ARBITRARY integer ms (`av_sync_calibrate.required_delay_ms`:
`raw = round(current - offset)`). But the deep stream FIFO holds video FRAME-QUANTIZED
(hold = `ceil(pin / 33.333)` frames), so a pin whose `frac(pin/33.333) < 0.5` is in the same
`PHASE_PRONE_MAX_FRAC` limit-cycle band as everything above — the release toggles 29/30 frames
(±33 ms). LIVE evidence (17.9.2026, pin held constant at 974, frac 0.22): the av-sync dock
`LOCK-CORRECT measured offset` toggled 72 → 104 → 72 → 109 ms while `late_holds` climbed 41 → 127
— a ±33 ms (one frame @30) hunt at a CONSTANT pin. Between-run medians walked −1 … −69 … +29 ms,
the `full_chain.latency.strih_stream.p50` sitting on three plateaus 995 / 1029 / 1062 = exactly
±33.4 ms steps: the pin (a sub-frame actuator) requantizing the hold by a whole frame each time it
moved. **±30 ms cannot be held with a frame-quantized actuator.**

**The fix is to SPLIT the correction (`av_sync_calibrate.split_av_correction`):**

- **Whole frames → the pin**, `frames = round(gain·residual / frame_ms)`, and the written pin is
  ALWAYS phase-snapped through the issue-1003 `phase_snap_pin` (`e2e_measurement_pins.py`) so
  `frac ∈ [0.6, 0.8]` — `round == ceil`, the hold is deterministic, no 29/30 toggle. (Before #1333
  `phase_snap_pin` was applied ONLY to the strih per-camera pins; the stream pin was never snapped.)
  The ±`AV_SYNC_MAX_STEP_MS`/run step clamp + the [3, 2000] hardware clamp are kept; the snap is
  applied LAST so the WRITTEN pin is never in the prone band even when the step clamp bites (in the
  rare large-correction case the snap can move the pin up to `PHASE_SNAP_MAX_COST_MS` beyond the
  ±step window — phase-safety wins, and the issue-1265 guard HOLDs |residual| > 60 anyway).
- **Sub-frame remainder → the `mbc` audio sync offset** (obs-websocket `SetInputAudioSyncOffset`,
  ms; positive DELAYS audio). `residual_eff = residual + (ceil(pin_new/frame) −
  ceil(pin_cur/frame))·frame_ms` = the residual left after the pin's ACTUAL whole-frame video
  shift; `audio_new = current_audio + gain·residual_eff`, ±step-clamped, ±500 ms hardware-clamped.
  Nothing read/wrote this actuator before #1333 (the sub-frame residual had nowhere to go).

Sign convention (matches `src/av_window.rs`): `residual = video − audio`; `> 0` = video lags ⇒
pin DOWN (video earlier) AND audio DELAYED (offset up). Both actuators are read-back verified and
rolled back BOTH on any failure (never a half-set pair); the issue-1265 apply guard (unchanged)
HOLDs both writes. The applied audio offset + source are persisted additively into
`av-sync-last.json`. The two remaining continuous-drift terms are separate lanes: the ASRC buffer
drain (issue 1335) and the free-running camera/display-vs-grid sawtooth (≤ 17 ms, physics, inside
the ±30 budget). Acceptance is a green ±30 gate across three ≥ 6 h-apart runs.

## Arrival-burst STALE ANCHOR: a backlog relock that fires every tick for minutes (issue 1367, live 25.9.2026)

**Tell:** the `genlock-relock` lines of one input repeat on EVERY tick, with a small `erased=` (1)
and a large negative `sel_vs_newest_due` (−13 / −15), and `ts_head_skew_ms` stays at the burst lateness
(~300 ms). The sender is clean. Mechanism: the issue-1003 phase anchor was sampled while frames
arrived late. The anchor pick sheds only the frames that age past it, while a 60-into-30 source adds
two per tick, so the depth never drops below `steady_depth + 6·n`. The old `sel_1003 == 0`
stale guard never fires because the pick is 1.

**Fix (lane branch, pending integration):** `relock_anchor_is_stale(sel_anchor, sel_configured, n)`
(`src/genlock_backlog.rs`) with its byte-identical C twin. The BACKLOG branch now resets on
`(sel_1003 == 0 || stale_1367)` and logs `stale_reset=1`. `stale_reset=1` on the relock line is the
live tell that the reset fired. The C nearest scan is now an age-taking core
(`genlock_relock_select_nearest_age`) with two wrappers: the anchor pick and
`genlock_relock_select_configured`.

**Trap — measure the tolerance against a CORRECT conveyor, not the bare pin (review round 1).**
The configured-latency pick is NOT where a healthy N==1 conveyor sits. A deep N==1 source settles on
`base + 1` frames, and a governed shallow one on D ≤ base + 3. When the relock tick lands a few ms
late on the grid, a CORRECT anchor reads 2 frames behind the configured pick (pin 987 ms: gap 2 from
ε ≈ 5 ms). A tolerance of `n` frames therefore resets a healthy 2ME PGM anchor. The tolerance
decision went to the main as a Design-question on the ticket. Any future test of this rule must
include a `(base + 1) × interval` anchor with a non-zero ε; a fixture 1.05 frames apart proves
nothing.

**Verification recipe (Tier-0):** a `#[path]` harness `lib.rs` of genlock_grid + genlock_backlog +
genlock_n1_depth + `probe { genlock }` under `clippy-driver --test -D warnings` runs the authority,
the sims and the probe mirror. The integration parity files build against a stub `camera_box` rlib
of the first three modules. The C mutation proof points `CARGO_MANIFEST_DIR` at a scratch tree
holding only the mutated `obs-source.c`, and the test must be recompiled per mutation because `env!`
is resolved at compile time.
