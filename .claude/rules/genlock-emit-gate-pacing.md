---
paths:
  - "src/dupe_decimation.rs"
  - "src/genlock_pacing.rs"
  - "src/ndi.rs"
  - "src/emit_skip_log.rs"
---

# Cam-box capture→emit genlock pacing (the #707/#889/#1111 gate)

The cam box captures faster than 60 (ShadowCast free-runs ~61–64 fps) and DECIMATES onto a
wall-clock 60 fps grid before NDI-emitting to the strih genlock-FIFO. Four cooperating pieces,
all pure `cfg(target_os="linux")` logic (NOT probe-gated → Tier-0 testable via `cargo test
--no-run --lib` then running `target/debug/deps/camera_box-*` directly):

The pacing GATE math lives in its own crate-root module `src/genlock_pacing.rs` (issue 1113 —
extracted verbatim out of the then-2555-line `ndi.rs`), gated `#[cfg(target_os="linux")]` in
lock-step with `ndi`. Gotcha when you move doc-commented code between modules like this: BARE-name
intra-doc links (`[`next_boundary_100ns`]`) that resolved inside `ndi.rs` BREAK in the new module —
re-qualify them to `[`crate::ndi::…`]`. `ndi.rs` still owns the NDI-timecode grid
(`next_boundary_100ns` / `fps_from_frame_rate`) the gate complements but does not depend on.

- `genlock_pacing::genlock_emit_gate(now, next_boundary, interval)` → `(would_emit, next)` — the
  wall-clock grid. Emits the first capture at/after each boundary; `#707` resync branch leaps
  forward only when lag > `GENLOCK_MAX_CATCHUP_INTERVALS` (8) = a real clock STEP.
- `genlock_pacing::genlock_emit_on_time(...)` (#1111) → is this an ON-TIME/surplus crossing vs a
  LATE catch-up crossing? Shares `genlock_latched_boundary` with the gate so the two never disagree.
- `dupe_decimation::DecimationGate` (#889) — dupe-preferring victim selection: at an over-rate
  the surplus shed prefers a byte-identical grabber dupe over the unique tick.
- `genlock_pacing::boundary_skip_count` (#707) + `emit_skip_log` (#752) — the `#707 SKIPPED
  boundaries` diagnostic (the WARN is throttled to one aggregate per 5s report).

## GOTCHA — a #889 dupe DEFERRAL must NEVER hold the boundary in the catch-up regime (#1111)

Deferring a dupe (hold the boundary, wait one more capture) is lag-neutral ONLY in the
ON-TIME/surplus regime (the replacement capture lands inside the SAME interval). At a genuine
over-rate a dupe often arrives while the gate is ALREADY LATE (catch-up); deferring THERE holds
the boundary while the wall clock runs on, **ratcheting the gate's lag +1 interval per deferral
until it crosses 8 and trips the #707 resync → ~9 boundaries leapt at once → sub-60 irregular
emit → strih genlock-FIFO relock → visible judder** (issue 1110/1111, live on CAM1). The fix:
gate the deferral on `genlock_emit_on_time`; a LATE dupe is EMITTED instead. Signature of the
symptom in the journal: `#707 ... SKIPPED boundaries ... totalling 9 boundary interval(s)`
repeating every ~10–15 s with `0 capture-dropped` (deterministic beat, NOT random CPU
starvation).

## The over-rate copies ARITHMETIC (not a defect — a mathematical floor)

A grabber at N fps with M byte-identical dupes/sec delivers only **(N−M) UNIQUE fps**. Emitting a
steady 60 from that inherently requires **~2 repeated frames/sec** (58 unique → 60). The receiver
needs steady 60 (no underrun/relock), so those copies are unavoidable — buffering a substitute
frame does NOT help (it also repeats a frame). The alternative (emit <60) is exactly the churn.
So the fix TRADES #707 skips (gaps + relock) for ~2 steady byte-identical copies/s; the copies
land in the E2E verdict `copies` windows and must be re-checked against
`WINDOW_COPIES_GAPS_TOLERANCE` at deploy (that tolerance predates them).

**SUPERSEDED at a GENUINE over-rate by #1145 — the copies are the floor ONLY when the source is
UNIQUE-STARVED (unique rate < 60), never at a plain over-rate.** The arithmetic above conflated two
cases. A grabber over-rating a true-60 source (cam1/cam2 ShadowCast, takt 61.x) delivers ~60 UNIQUE
fps (the over-rate delta IS the dupe rate), so ZERO copies are needed — every dupe can be shed. The
~2 copies/s the pre-#1145 valve emitted there were a BUG: at over-rate the unique rate is exactly 60,
so the emit-gate lag is a driftless random walk and jitter pushes an on-time deferral over the lag==0
hair-trigger, so the next dupe arrives LATE and #1111 copies it (a delta-0 downstream) + a
compensating dropped-unique = the paired "15fps-judder" the #1142 uniformity gate REDs. Only a source
whose UNIQUE rate is genuinely < 60 (a 58-unique grabber, or a 50->60 pulldown padding a sub-60
source by duplication) truly needs copies to hold a steady 60.

## #1145 — stale-boundary RETIREMENT: absorb the over-rate takt without emitting a copy

The FIFTH cooperating piece. A content-dupe crossing an ALREADY-STALE boundary (`lag >= 1` —
`genlock_pacing::genlock_lag_intervals`, the numeric sibling of `genlock_emit_on_time`) is RETIRED
instead of copied: shed the dupe AND advance the stale boundary one interval, emitting NOTHING. The
boundary's downstream hold already happened one interval ago, so retiring it costs no new artifact,
sacrifices no unique, AND drains the dupe-driven lag (the restoring force the bounded-defer variant
lacked — "defer iff lag<BOUND" only postpones the copy, it never CANCELS the debt, so a driftless
walk still eventually trips the resync). Bounded by `RETIRE_MAX_LAG_INTERVALS=4` (<< the resync 8);
above it the #1111 copy valve fires (a panic floor). `genlock_emit_gate` + its resync are UNTOUCHED.

- **Retirement is gated on the UNIQUE rate, NOT the capture takt.** A trailing 2 s `VecDeque` COUNT
  of unique (non-dupe) captures (`RETIRE_MIN_UNIQUES_IN_WINDOW=118`, ~59 fps) is the robust "enough
  distinct content to hold 60 without copies" signal — a windowed COUNT reads the true unique rate
  regardless of per-frame jitter / dupe clustering (an interval EMA does NOT: it reads local capture
  spacing during a run of consecutive uniques and leaks). A capture-TAKT gate is WRONG: a takt>60.3
  excess-dupe deficit (unique < 60) would be wrongly retired, dropping the emit rate + blinding the
  duplication-masked pulldown detector (`dup_cadence.rs`) + tripping the #666 emit-deficit gate. The
  unique-rate gate keeps a genuinely starved source (50->60 pulldown) on the #1111 copy path
  byte-identical (holds 60, keeps the content-dupes in the recording for `dup_cadence`).
- Decision is the pure `dupe_decimation::dupe_shed_action(...) -> ShedAction {Emit{copy}, Defer,
  Retire, BlindShed}` (replaced `dupe_preferring_decimate`). `DupeShedLog` gained a `retired` counter
  (the summary line is now 4-count; `main.rs` wires the 4-tuple). Live: retired ≈ over-rate delta,
  copies ≈ 0 on cam1/cam2, all-zero on cam3.

## GOTCHA — verify pacing changes against the REAL modules, never a hand-simplified re-model (#1145)

The rule below ("faithful Python port") is right that a port reproduces the live behavior — but a
hand-SIMPLIFIED re-model silently DIVERGES. A shortened `DecimationGate`/`Cur` re-write disagreed
with the real #1111 test (it read emit 58 at 62/period-15 where the real code holds ~60 via copies),
which would have mis-designed the fix. The authoritative off-rig check is the CLAUDE.md/#557 SCRATCH
route: copy the ACTUAL `src/genlock_pacing.rs` + `src/dupe_decimation.rs` into a scratch dir with a
`root.rs` that `mod`s both, `sed 's/crate::genlock_pacing:://g'` inside dupe_decimation, then
`rustc --edition 2021 --test root.rs` runs the REAL `DecimationGate::poll` + the real test suite. For
a design sweep, drive that real gate with a synthetic capture stream (periodic isolated dupes at the
over-rate delta + INDEPENDENT timestamp jitter — content-dupeness is a hash property, NOT a
sampling-phase artifact; a source-sampling model that ties dupeness to the jittered timestamp
mis-models the ShadowCast, which stays clean at exactly 60). The downstream uniformity a genlocked
strih sees is the emitted source-tick sequence decimated in-order by 2 (NOT resampled by the jittery
emit timestamps — the emit grid is wall-clock-gridded, the FIFO genlocked).

## GOTCHA — the #707 resync is QUEUE-BLIND; gate it on the dequeue signal, not just a lag bound (#1131)

`genlock_emit_gate`'s forward-resync (`lag > GENLOCK_MAX_CATCHUP_INTERVALS`) is BLIND to whether the
skipped boundaries actually had captured frames. On a sick/wobbly grabber a single poll's wall-clock
lag can exceed 8 intervals while the V4L2 driver has REAL captured frames buffered (the live
signature: `#707 SKIPPED ... 9 boundary interval(s)` with **0 capture-dropped** = the frames exist,
just delivered late) — the resync leaps past them and they are decimated (discarded in a run) = the
visible multi-frame content judder (issue 1110/1130 P0).

The fix (#1131): thread a per-frame `queue_had_frame` bool — `capture_stall::frame_from_nonempty_queue(dequeue_duration_ms, capture_interval_ms)`,
true when the blocking VIDIOC_DQBUF returned in `< 0.5×` the capture interval (the driver already had
it buffered) — from `main.rs` → `DecimationGate::poll` → `genlock_emit_gate`, and change the resync
trigger to `lag > GENLOCK_MAX_CATCHUP_INTERVALS && !queue_had_frame`. A buffered frame catches up ONE
interval (fills its boundary, never leaps); an EMPTY-queue frame (the loop genuinely WAITED for it — a
device stall that produced nothing, or a real clock STEP) keeps the resync (honest skip).

Why the dequeue signal is the RIGHT discriminator (and why raising the bound alone is wrong):
- A long **DQBUF** block ⟺ the device produced nothing (that's WHY dequeue blocked) → those
  boundaries genuinely had no content → resync is honest. `dequeue_duration_ms` is large → `false`.
- An emit-loop-side block (send/processing) leaves the device producing → frames buffer → on resume
  DQBUF returns fast → `dequeue_duration_ms` small → `true` → catch up (the frames exist).
- A cold-boot NTP step (#131) inflates `now_ns` (CLOCK_REALTIME) but NOT the dequeue duration (a
  monotonic `Instant::elapsed()`), so the post-step frame reads as a normal single-frame wait →
  `false` → still resyncs. **This is why #131 is preserved for free** — do NOT just raise the fixed
  bound (that would make a genuine multi-second step creep through hundreds of stale-frame emits).

Three-band read of the ONE `dequeue_duration_ms` signal: `(0, 0.5×)` buffered / `[0.5×, 1.5×)` normal
single-frame wait / `≥ 1.5×` stall (`CAPTURE_STALL_FACTOR`). `frame_from_nonempty_queue` and
`is_capture_stall` are the two ends. Fail-safe: an unknown/non-finite measurement → `false`
(queue-blind = today's behavior), so a bad reading can never SUPPRESS an honest skip — and guard
`frame_interval_ms` for finiteness too (a `+inf` interval would otherwise falsely read "buffered",
the unsafe direction). All Tier-0 (`cfg(target_os="linux")`, not probe-gated): verify via
`rustc --edition 2021 --test src/genlock_pacing.rs` (and `src/capture_stall.rs`) standalone — a
combined build with `genlock_pacing`/`capture_stall` as submodules runs the real `DecimationGate::poll`.

## Root-causing method that worked: faithful Python port + live journal cross-check

Port `genlock_emit_gate` + `DecimationGate` + `boundary_skip_count` verbatim to a Python sim and
drive it with the real pattern (62 fps, isolated dupe every ~15 captures). It reproduced the
EXACT live `9 boundary interval(s)` skip and the 18-skips/8 s rate — pinning the deterministic
root cause before touching code, and giving exact RED/GREEN test thresholds. Read the live
proof read-only over ssh: `journalctl -u camera-box | grep -E 'Streaming:|#707|dupe-preferring'`.
