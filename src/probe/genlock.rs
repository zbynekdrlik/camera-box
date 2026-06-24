//! Genlock FIFO consume / preload decision logic (camera-box #42 → #70 → #97 → #102).
//!
//! This is the camera-box-side, pure, unit-tested MIRROR of the C decision logic
//! baked into the vendored OBS genlock FIFO
//! (`vendor/obs-studio/libobs/obs-source.c`, the `genlock_fifo` branch of
//! `ready_async_frame()` + `genlock_parse_preload()` + `genlock_decide()`). Keeping
//! the contract here lets CI prove the parse/clamp + consume rules without an OBS
//! build, and the `tests/genlock_preload.rs` vendored-source guard keeps the C side
//! in lock-step.
//!
//! ## The preload is a video-delay; the consume gate must not drop distinct frames
//!
//! The genlock FIFO (#42) consumes queued frames against a wall-clock render tick.
//! `preload` is the depth of a deliberate jitter buffer / **video delay** (#97):
//! `preload` frames buffered = `preload` frames of genlock-disciplined delay, used to
//! push the program video back ~1 s to match late audio on stream.lan.
//!
//! The #70 attempt gated consumption on `queue_depth > preload` on EVERY tick, so any
//! NDI arrival-*jitter* dip below the reserve REPEATED the last frame and lost one
//! DISTINCT frame; a deep #97 preload (≈1 s) made this catastrophic (11.6 % @
//! preload=1 → 34.3 % @ preload=30 on the live rig, underrun-dominated). #102 fixes
//! it: BUILD the delay once at startup ([`genlock_decide`] with the `filled` latch),
//! then consume a distinct frame on EVERY tick a frame is queued, repeating ONLY on a
//! true empty. A deep preload is then a CLEAN delay line — it holds the delay but
//! never drops a distinct frame ⇒ ~0 distinct-frame loss at any depth.

/// Default reserve when `OBS_GENLOCK_PRELOAD_FRAMES` is unset/invalid: one frame
/// (= one frame of latency per hop, the "1 frame per hop" the task calls for).
pub const GENLOCK_PRELOAD_DEFAULT: u32 = 1;

/// Hard cap on the reserve.
///
/// History (#70): originally 28, because the FIFO drop-cap was a single global
/// `MAX_ASYNC_FRAMES = 30` and the steady-state queue parks at `preload + 1`, so
/// any `preload` whose `preload + 1` reached 30 force-drained every refill and
/// FROZE the source. 28 ⇒ steady depth 29 < 30 was the highest safe reserve.
///
/// (#97) The preload is now a per-source, runtime-settable **video-delay** control
/// (one preload frame = one frame of genlock-disciplined delay), used to push the
/// program video back ~1 s to line up with late audio on stream.lan. At 30 fps,
/// ~1 s is 30 frames — already above the old cap — so the ceiling is raised to
/// **128** (≈ 4.3 s @ 30 fps / ≈ 2.1 s @ 60 fps), enough headroom for any realistic
/// audio offset. The old "must stay below MAX_ASYNC_FRAMES" invariant is replaced
/// by the per-source drop-cap ([`genlock_drop_cap`]): a delayed source's FIFO is
/// allowed to hold `preload + RESERVE` frames, so a deep preload no longer
/// force-drains. See [`genlock_drop_cap`] / [`GENLOCK_DROP_CAP_RESERVE`].
pub const GENLOCK_PRELOAD_MAX: u32 = 128;

/// Headroom above a genlock source's `preload` for the per-source FIFO drop-cap.
///
/// The drop-cap must sit a few frames ABOVE the steady-state depth (#102: the
/// consume-when-queued gate parks at `preload`) so normal producer/consumer jitter
/// never reaches it and force-drains the buffer (an overrun that resets the FIFO and
/// re-introduces a glitch). `+4` leaves slack above steady state. See
/// [`genlock_drop_cap`].
pub const GENLOCK_DROP_CAP_RESERVE: u32 = 4;

/// libobs' fixed async FIFO drop-cap for NON-genlock sources (`MAX_ASYNC_FRAMES`).
/// Mirrored here so the per-source drop-cap logic ([`genlock_drop_cap`]) can be
/// unit-tested without an OBS build; the vendored-source guard
/// (`tests/genlock_preload.rs`) keeps this in lock-step with the C `#define`.
pub const MAX_ASYNC_FRAMES: u32 = 30;

/// Parse the `OBS_GENLOCK_PRELOAD_FRAMES` env value into a reserve depth.
///
/// This is a FAITHFUL mirror of the C `genlock_parse_preload()`, which uses
/// `strtol(env, &end, 10)` and then `if (end == env || *end != '\0' || v < 0)
/// return default; if (v > MAX) return MAX;`. To match it exactly (the test crate
/// exists to prove the C contract), it replicates `strtol`'s quirks rather than
/// using Rust's `parse`, which differs on two pathological inputs:
/// * `strtol` skips only *leading* whitespace; a trailing non-digit (e.g. `"5 "`)
///   leaves `*end != '\0'` ⇒ default. (Rust `trim()` would have accepted `"5 "`.)
/// * `strtol` *saturates* an out-of-range magnitude to `LONG_MAX`, which then
///   passes the `v >= 0` guard and hits the `v > MAX` clamp ⇒ MAX. (Rust
///   `parse::<i64>()` would `Err` on overflow and fall to default.)
///
/// Net contract: `None`/empty/leading-junk/trailing-junk/negative ⇒ default;
/// any in-range or overflowing non-negative integer ⇒ clamped to
/// [`GENLOCK_PRELOAD_MAX`]; `0` is valid (reproduces the old zero-slack FIFO).
pub fn parse_preload(env: Option<&str>) -> u32 {
    let Some(raw) = env else {
        return GENLOCK_PRELOAD_DEFAULT;
    };
    // strtol skips leading ASCII whitespace, then reads an optional sign + digits;
    // a trailing non-digit leaves `*end != '\0'`. `trim_start` + an all-ASCII-digit
    // body (after an optional leading '+') reproduces that without a hand-rolled
    // arithmetic loop. `i64::from_str` does the accumulation, and its
    // `PosOverflow` error is the strtol LONG_MAX-saturation case ⇒ clamp to MAX.
    let body = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = body.strip_prefix('+').unwrap_or(body);
    // Reject empty / leading-sign-only / any non-digit char (incl. trailing junk
    // and a leading '-', so every negative falls to default like the C `v < 0`).
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return GENLOCK_PRELOAD_DEFAULT;
    }
    match digits.parse::<i64>() {
        Ok(v) => v.min(GENLOCK_PRELOAD_MAX as i64) as u32,
        // `digits` is non-empty and all-ASCII-digit (no sign), so the ONLY
        // reachable parse error is positive overflow — strtol saturates that to
        // LONG_MAX, which then hits the `> MAX` clamp ⇒ MAX. No other error kind
        // can occur, so there is no separate default-on-error arm (a guarded arm
        // here would leave an equivalent, untestable mutant).
        Err(_) => GENLOCK_PRELOAD_MAX,
    }
}

/// The genlock consume decision for one render tick (#102).
///
/// `consume` — hand a distinct queued frame to the compositor this tick.
/// `filled`  — the new value of the source's one-time startup-fill latch (the
///             delay line has reached its `preload` depth at least once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockDecision {
    pub consume: bool,
    pub filled: bool,
}

/// At a render tick, decide whether the genlock FIFO consumes a distinct frame
/// (#102 — the frame-loss fix).
///
/// The #70 gate held consumption until `queue_depth > preload` on **every** tick,
/// so any NDI arrival-jitter dip below the reserve REPEATED the last frame and lost
/// one DISTINCT frame. At a deep #97 preload (≈1 s) it was catastrophic: after any
/// drain the FIFO had to refill PAST the whole reserve before a single new frame
/// escaped (11.6 % @ preload=1 → 34.3 % @ preload=30, underrun-dominated on the live
/// stream box).
///
/// The fix splits the FIFO's life into two phases via a one-time `filled` latch:
///
/// 1. **Build (`filled == false`)** — establish the delay line. Hold (no consume)
///    until `queue_depth > preload`, i.e. `preload` frames are buffered plus one to
///    emit; then **latch `filled` and consume** the first (now `preload`-frames-late)
///    frame. This startup fill is the ONLY place repeats are emitted, and only once.
///
/// 2. **Steady (`filled == true`)** — **consume a distinct frame on EVERY tick a
///    frame is queued** (`queue_depth >= 1`). A jitter dip below the reserve still
///    delivers a distinct frame (the reserve shrinks and refills naturally) — it is
///    never repeated. The ONLY hold is a TRUE empty (`queue_depth == 0`): a genuine
///    underrun with no frame to deliver. `filled` stays `true` across a transient
///    empty, so a momentary dip does NOT re-trigger the whole startup refill run.
///
/// Result: a deep preload is a CLEAN delay line — it holds the delay but emits a
/// distinct frame every tick one is queued ⇒ ~0 distinct-frame loss at ANY depth.
/// Mirrors the C `genlock_decide()` in the vendored OBS genlock branch. The latch is
/// reset (back to `filled == false`) only on an overrun force-drain (`cache_video`
/// empties the FIFO), so the delay rebuilds after a drain.
pub fn genlock_decide(queue_depth: usize, preload: u32, filled: bool) -> GenlockDecision {
    if !filled {
        // Build phase: fill to the preload delay depth before emitting anything.
        if queue_depth > preload as usize {
            GenlockDecision {
                consume: true,
                filled: true,
            }
        } else {
            GenlockDecision {
                consume: false,
                filled: false,
            }
        }
    } else {
        // Steady phase: consume whenever a distinct frame is queued; hold only on
        // a true empty (an unavoidable underrun, never a repeat-while-queued).
        GenlockDecision {
            consume: queue_depth >= 1,
            filled: true,
        }
    }
}

/// The peak steady-state queue depth seen at the consume DECISION instant when
/// producer and consumer run at the same rate.
///
/// #102: the FIFO builds to `preload + 1` (the latch fires when depth first exceeds
/// `preload`), then each tick the producer adds one (depth → `preload + 1`) and the
/// gate consumes one (depth → `preload`). So the depth oscillates `preload + 1` (at the
/// decision, before consuming) ↔ `preload` (after) — leaving `preload` frames of reserve
/// at the instant of consumption, the SAME single-tick jitter tolerance the #70 gate
/// gave (it takes `preload + 1` consecutive missed deliveries to reach a true empty).
/// The drop-cap must clear this `preload + 1` peak, which is what
/// [`genlock_drop_cap`]'s `+ RESERVE` guarantees.
pub fn steady_state_depth(preload: u32) -> u32 {
    preload + 1
}

/// How many OLDEST frames to ERASE at the build-latch instant so the FIFO settles
/// at exactly the target depth, regardless of the NDI startup burst (#116).
///
/// ## The bug this fixes
///
/// The #102 build latch ([`genlock_decide`]) latches `filled = true` the instant
/// `queue_depth > preload`, at WHATEVER depth the NDI startup burst left in the
/// queue, and consumes exactly one frame — it never trims down. So:
///
/// 1. Two inputs with the SAME preload but DIFFERENT startup bursts freeze at
///    different depths ⇒ different per-camera latency ⇒ a time-jump when switching
///    cameras (live: NDI cam5(=CAM1) latched depth 6 vs cam1/cam3 depth 2 at an
///    identical preload=1, a ~133 ms spread).
/// 2. A preload DECREASE re-arms the latch ([`obs_source_set_genlock_preload`]),
///    but the deep queue re-latches `filled` immediately at the OLD deep depth, so
///    the lower delay never takes effect ("preload only goes up").
/// 3. Each restart's random NDI arrival phase gives a different frozen depth ⇒
///    non-deterministic latency after an OBS restart.
///
/// ## The fix
///
/// When the build latch is about to fire (`queue_depth > preload`), erase the
/// `queue_depth - target` OLDEST frames so the FIFO holds exactly `target =
/// steady_state_depth(preload) = preload + 1` frames, then the same-tick consume
/// leaves `preload` (the steady-state reserve). Every input — and every restart —
/// then settles at the IDENTICAL deterministic depth, and a preload change takes
/// effect immediately in BOTH directions (a decrease drains to the lower target on
/// the next build latch; an increase rebuilds up to the higher target). The C
/// `ready_async_frame` calls this at the build latch and erases that many oldest
/// frames via the `da_erase(async_frames, 0)` + `remove_async_frame()` idiom (the
/// same per-frame free path the `async_unbuffered` drain uses — no leak, no
/// double-free).
///
/// The drain fires ONLY at the build latch (and on a preload-change re-arm) — NEVER
/// in steady state. The #102 consume-when-queued 0-loss gate ([`genlock_decide`]'s
/// steady branch) is untouched. Below the latch (`queue_depth <= preload`, still
/// building) and at/under the target there is nothing to trim ⇒ 0 (saturating, so
/// it can never underflow/wrap).
pub fn genlock_build_drain(queue_depth: usize, preload: u32) -> usize {
    let target = steady_state_depth(preload) as usize;
    queue_depth.saturating_sub(target)
}

/// Re-arm threshold: the number of CONSECUTIVE true-empty (underrun) render ticks a
/// genlock FIFO must see in steady state before a resume counts as a reconnect and
/// re-arms the build latch (#126).
///
/// Chosen at **30 ticks ≈ 1 s @ 30 fps** — deliberately FAR above any realistic
/// arrival-jitter dip. The #102 reserve already makes a single-tick (or even a few
/// consecutive) empties impossible without a real outage: in steady state the queue
/// parks at `preload` after each consume, so it takes `preload + 1` consecutive missed
/// deliveries even to REACH a true empty, and only a genuine upstream disconnect (the
/// strih OBS restart this fixes) sustains true-empties for ~1 s. A LOWER threshold
/// would risk a spurious re-arm at the shallow cam `preload = 1`, where a 1–2 tick
/// transient empty must NOT re-arm — a spurious re-arm forces a ~`preload`-frame
/// rebuild HOLD on every blip. The cost of the chosen value is bounded and acceptable:
/// recovery after a real reconnect is ~1 s (the detection window) plus one rebuild hold
/// (~`preload + 1` frames) — versus the silent video-delay collapse (A/V drift) the bug
/// causes until a manual nudge. Mirrors the C `#define GENLOCK_REARM_EMPTY_TICKS`.
pub const GENLOCK_REARM_EMPTY_TICKS: u32 = 30;

/// Advance the per-source consecutive-true-empty counter for one render tick (#126).
///
/// `consumed` — whether a frame was QUEUED (`num >= 1`) this tick. The counter is only
/// ever nonzero in steady state, where the #102 invariant guarantees a queued frame is
/// always consumed (`queue_depth >= 1` ⇒ consume) — so "a frame was queued again" and
/// "a frame was consumed" coincide for every tick that can reset a nonzero counter, and
/// modelling the reset as `consumed=true` is exact. (The C side resets at the same
/// instant: on entry to the genlock branch of `ready_async_frame`, which is reached only
/// with `num >= 1`.)
/// * a queued/consumed tick **resets** the run to 0 (so a flickering empty/non-empty
///   queue — normal jitter — can NEVER accumulate to [`GENLOCK_REARM_EMPTY_TICKS`]; only
///   a genuine sustained disconnect can);
/// * a true empty **increments** the run (saturating, so a very long outage never wraps).
///
/// Mirrors the C `source->genlock_empty_run` bookkeeping in `obs-source.c`
/// (`++` at the `get_closest_frame` num==0 underrun site, `= 0` on the next tick a frame
/// is queued — i.e. on entry to the `ready_async_frame` genlock branch).
pub fn genlock_empty_run_next(empty_run: u32, consumed: bool) -> u32 {
    if consumed {
        0
    } else {
        empty_run.saturating_add(1)
    }
}

/// Decide whether a resuming genlock FIFO should RE-ARM its build latch (#126).
///
/// On an upstream OBS (strih) restart the downstream NDI source underruns to EMPTY, but
/// DistroAV's default `KEEP_CONTENT` blocks the only NULL-emit reset, and an underrun
/// (not an overrun) never fires the `cache_video` force-drain reset — so `genlock_filled`
/// stays **true**. The #102 steady branch then consumes 1/tick the instant the queue
/// refills WITHOUT rebuilding the preload reserve, so the deliberate ~26-frame video
/// delay silently collapses to ~0 (A/V drift) until a manual preload nudge.
///
/// The fix: when frames RESUME after a SUSTAINED true-empty run, re-arm `genlock_filled`
/// to `false` so the EXISTING #102 build path + #116 drain rebuild the reserve to exactly
/// `preload + 1` — no manual nudge, NO new draining logic.
///
/// Returns `true` (re-arm) only when BOTH:
/// * the source is in STEADY state (`filled == true`) — while building, the reserve is
///   already being rebuilt, so re-arming is meaningless; AND
/// * the consecutive-empty run is at/above [`GENLOCK_REARM_EMPTY_TICKS`] — the
///   jitter-safety guard that keeps a brief (sub-threshold) dip from spuriously
///   re-arming (which would force a ~`preload`-frame hold on every blip, catastrophic at
///   the shallow cam `preload = 1`).
///
/// Mirrors the C `genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS && source->genlock_filled`
/// guard taken on the resume tick in `ready_async_frame`.
pub fn genlock_rearm_on_resume(empty_run: u32, filled: bool) -> bool {
    filled && empty_run >= GENLOCK_REARM_EMPTY_TICKS
}

/// Convert a preload depth (frames of genlock-disciplined delay) into the
/// equivalent **delay in milliseconds** at a given output frame rate.
///
/// `ms = frames * 1000 * fps_den / fps_num` (e.g. 30 frames @ 30000/1001 ≈ 1001 ms;
/// 30 frames @ 30/1 = 1000 ms). This is the EXACT integer arithmetic the C/C++ side
/// performs (`obs_get_video_info()` → `fps_num`/`fps_den`), mirrored here so the GUI
/// info-text label and the audit-log ms are unit-tested. Uses `u64` intermediates so
/// `frames * 1000 * fps_den` cannot overflow at the cap (128 * 1000 * fps_den).
///
/// Returns 0 if `fps_num` is 0 (no valid video info yet) — the caller shows a
/// "fps unknown" label rather than dividing by zero.
pub fn preload_to_ms(frames: u32, fps_num: u32, fps_den: u32) -> u64 {
    if fps_num == 0 {
        return 0;
    }
    (frames as u64) * 1000 * (fps_den as u64) / (fps_num as u64)
}

/// The per-source async-FIFO drop-cap (#97).
///
/// libobs force-drains a source's async FIFO when it reaches the drop-cap (an
/// overrun). For a NON-genlock source the cap stays the fixed [`MAX_ASYNC_FRAMES`]
/// (30) — those sources never deliberately buffer, so a deep cap would only mask a
/// runaway producer. For a **genlock** source the cap =
/// `max(MAX_ASYNC_FRAMES, preload + RESERVE)`, clamped to an absolute maximum of
/// `GENLOCK_PRELOAD_MAX + RESERVE` (= 132).
///
/// The [`MAX_ASYNC_FRAMES`] floor is load-bearing: before #97 every source (genlock
/// included) had the fixed 30-frame cap, which absorbed NDI catch-up bursts after a
/// LAN hiccup. Scaling the cap to `preload + RESERVE` *alone* would, at the
/// production default `preload = 1`, drop the cap to 5 — a 6× cut in burst tolerance
/// on exactly the jittery sources the genlock FIFO exists to protect, so a momentary
/// stall delivering a 5-frame catch-up burst would force-drain the whole buffer.
/// Keeping the 30-frame floor preserves the pre-#97 burst tolerance; the cap only
/// GROWS above it once the operator dials in a deep delay (memory stays bounded —
/// only a deliberately-delayed source holds a big buffer, stream.lan is RAM-tight #89).
pub fn genlock_drop_cap(genlock_fifo: bool, preload: u32) -> u32 {
    if !genlock_fifo {
        return MAX_ASYNC_FRAMES;
    }
    let want = preload.saturating_add(GENLOCK_DROP_CAP_RESERVE);
    let abs_max = GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE;
    want.min(abs_max).max(MAX_ASYNC_FRAMES)
}

// ---- #136: timestamp-aligned release (multi-source IN-SYNC) ----------------
//
// The count-based gate above ([`genlock_decide`]) keeps a per-source jitter buffer of
// a fixed DEPTH and consumes one frame per render tick. That cannot hold MULTIPLE
// sources in sync: each source's depth drifts independently (the render pass consumes
// slightly slower than the cameras produce, and any per-source dropout/reconnect/
// preload-change leaves that source at a different depth that never re-converges), so
// camera A ends up N frames behind camera B — visible desync (measured ~300 ms / 9
// frames spread live, #136). A depth/count buffer fundamentally only chooses WHERE the
// rate difference accumulates; it cannot eliminate it.
//
// Timestamp-aligned release fixes it. Every camera-box frame carries its real
// DanteSync wall-clock CAPTURE instant (src/ndi.rs stamps the NDI timecode; DistroAV
// passes it into `obs_source_frame->timestamp` in Source-Timecode mode). The strih
// render tick is ALSO on the shared DanteSync wall clock. So at each tick we present,
// from every source, the frame captured at the SAME instant `present_ts = tick_wall -
// COMMON_DELAY`. Identical capture instant on every source ⇒ **in-sync by
// construction**, regardless of buffered depth, delivery jitter, or a transient.
// Latency is exactly `COMMON_DELAY` (bounded + uniform, never drifting), and a slow/
// lagged render pass just drops the stale past-due frames uniformly instead of filling
// the buffer toward the overrun cap. `preload` (a frame COUNT) is reinterpreted as a
// TIME delay via the wall clock — same operator knob, sync-correct semantics.

/// A timestamp-aligned genlock RELEASE decision for one source at one render tick (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockRelease {
    /// Free this many OLDEST queued frames UNSHOWN — they are already past the
    /// presentation deadline (stale). A lagged render pass makes this > 0; at matched
    /// rates it is 0. This replaces the unbounded FIFO fill of the count gate.
    pub drop_oldest: usize,
    /// Present a newly-due frame this tick (the head after `drop_oldest`). If `false`,
    /// no queued frame has reached its presentation deadline yet (source early or
    /// stalled) → repeat the current frame, drop nothing.
    pub present: bool,
}

/// The presentation deadline (capture instant due NOW) for a render tick.
///
/// `present_ts = tick_wall_ns - delay_frames * interval_ns` (saturating, so an early
/// boot wall clock can never wrap below 0). `delay_frames` is the genlock `preload`
/// reinterpreted as a COMMON delay shared by every source — the single knob that sets
/// the uniform end-to-end latency and the jitter headroom (it must cover worst-case
/// capture→strih delivery, like `preload` did, but now as a true time budget).
pub fn genlock_present_ts(tick_wall_ns: u64, delay_frames: u32, interval_ns: u64) -> u64 {
    // Half-interval tolerance (#136 boundary-churn fix). A frame whose capture
    // timestamp lands exactly on the nominal deadline (wall - delay*interval) jitters
    // in/out of the `ts <= present_ts` test from tick to tick — the wall-clock render
    // tick has ±slew, so present_ts hovers around the frame's timestamp and the frame
    // alternates due/not-due, producing hold-then-catch-up churn (measured ~3 fps on
    // the deep-preload chained strih->stream PGM feed: every boundary-aligned frame
    // landed on the deadline). Biasing the deadline FORWARD by half a frame makes a
    // boundary-landing frame robustly due (the ±2 ms slew is far below interval/2 ≈
    // 16 ms @ 30 fps), giving a clean one-frame-per-tick release. Every source shares
    // the same bias, so multi-source alignment is preserved; effective latency just
    // drops by interval/2 (~16 ms, negligible).
    tick_wall_ns
        .saturating_sub((delay_frames as u64) * interval_ns)
        .saturating_add(interval_ns / 2)
}

/// Decide the timestamp-aligned release for one source at one render tick (#136).
///
/// `present_ts_ns` — the shared presentation deadline ([`genlock_present_ts`]).
/// `queued_ts_ascending` — the source's queued frames' CAPTURE timestamps, oldest
/// first (a single NDI source delivers in monotonic capture order, so the due frames
/// are a prefix).
///
/// Rule: let `due` = the queued frames whose capture ts is at/before the deadline. If
/// none are due, HOLD (`present = false`, drop nothing) — the source is early or
/// stalled, repeat the current frame. Otherwise present the NEWEST due frame (the head
/// after dropping the `due - 1` stale older ones). Because every genlock source shares
/// `present_ts` and a common wall-clock capture cadence, the presented frame's
/// timestamp is identical across sources ⇒ in-sync.
pub fn genlock_release(present_ts_ns: u64, queued_ts_ascending: &[u64]) -> GenlockRelease {
    let due = queued_ts_ascending
        .iter()
        .take_while(|&&ts| ts <= present_ts_ns)
        .count();
    if due == 0 {
        GenlockRelease {
            drop_oldest: 0,
            present: false,
        }
    } else {
        GenlockRelease {
            drop_oldest: due - 1,
            present: true,
        }
    }
}

/// Lower plausible-wall-clock bound (Unix epoch ns): 2020-01-01T00:00:00Z.
pub const WALLCLOCK_TS_MIN_NS: u64 = 1_577_836_800_000_000_000;
/// Upper plausible-wall-clock bound (exclusive, Unix epoch ns): 2100-01-01T00:00:00Z.
pub const WALLCLOCK_TS_MAX_NS: u64 = 4_102_444_800_000_000_000;

/// Is a frame timestamp a plausible DanteSync wall-clock instant (Unix-epoch ns)?
///
/// Timestamp-aligned release ([`genlock_release`]) is correct ONLY when frames carry a
/// real shared wall-clock capture instant — the camera-box genlock inputs in
/// Source-Timecode mode. A `0` (no timecode), a small monotonic-style value, or any
/// out-of-range garbage (a non-camera source: CG, preview, lyrics) fails this, and the
/// C side then falls back to the count-based [`genlock_decide`] gate so non-broadcast
/// sources are never broken by the new path.
pub fn is_wallclock_ts(ts_ns: u64) -> bool {
    (WALLCLOCK_TS_MIN_NS..WALLCLOCK_TS_MAX_NS).contains(&ts_ns)
}

/// The new high-water mark of the genlock FIFO depth, given the previous peak and a depth
/// OBSERVED right now (#99 point 2).
///
/// ## Why this exists
///
/// `genlock_peak_depth` is the audit log's high-water mark of the jitter buffer — its whole
/// purpose is to tell the operator how close the queue got to the drop-cap so the preload
/// depth can be tuned. The original #70 instrumentation updated it ONLY on the CONSUMER side
/// (inside `ready_async_frame`, at the render-tick consume decision). But the PRODUCER (NDI
/// arrival → `obs_source_output_video_internal` → `da_push_back(async_frames)`) can push the
/// queue to a momentary high depth BETWEEN two render ticks and have it drained back down
/// before the next tick observes it — so the consumer-side-only peak UNDER-reports the true
/// high-water mark. The fix (#99) is to also fold the depth the producer reaches into the
/// peak, at the push site, under the same `async_mutex`. Both sites call THIS pure function so
/// the "peak = max so far" rule lives in one tested place and the C side mirrors it exactly.
///
/// Pure `max`; saturating is unnecessary (a `u32` max of two `u32`s cannot overflow), but the
/// monotone-non-decreasing invariant (the return is never below `current_peak`) is what the
/// callers rely on.
pub fn genlock_peak_update(current_peak: u32, observed_depth: u32) -> u32 {
    current_peak.max(observed_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #136: timestamp-aligned release (multi-source IN-SYNC) -------------

    const NS30: u64 = 33_333_333; // ~one 30fps frame interval in ns
    const WBASE: u64 = WALLCLOCK_TS_MIN_NS + 1_000_000_000; // a plausible wall-clock base

    /// `n` frames captured at a steady 30fps cadence from `WBASE` (oldest-first).
    fn caps(n: usize) -> Vec<u64> {
        (0..n).map(|i| WBASE + i as u64 * NS30).collect()
    }

    #[test]
    fn release_steady_presents_one_drops_none() {
        // Steady state at matched rates: each tick drops the presented frame, so the
        // queue HEAD is the next due frame and the rest are future. Exactly one frame
        // is due → present it, drop nothing.
        let q = caps(6); // head q[0]=WBASE is due; q[1..] are future
        let present_ts = WBASE; // only the head has ts <= deadline
        let d = genlock_release(present_ts, &q);
        assert_eq!(
            d,
            GenlockRelease {
                drop_oldest: 0,
                present: true
            },
            "with only the due frame at the head, present it and drop nothing"
        );
        assert_eq!(
            q[d.drop_oldest], present_ts,
            "the presented frame is the due head"
        );
    }

    #[test]
    fn release_drops_stale_when_render_pass_lagged() {
        // The rate-leak / lagged-render case: present_ts jumped two intervals, so TWO
        // frames are now past-due. Present the NEWEST, drop the stale one — a uniform
        // controlled drop, NOT an unbounded FIFO fill (the depth-FIFO failure #136 fixes).
        let q = caps(6); // indices 0..5 due-able
        let present_ts = WBASE + 4 * NS30; // indices 0..=4 are due (5 frames)
        let d = genlock_release(present_ts, &q);
        assert_eq!(
            d,
            GenlockRelease {
                drop_oldest: 4,
                present: true
            },
            "five frames past-due → drop the four stale, present the newest"
        );
        // the presented frame (queue[drop_oldest]) is the one captured at present_ts
        assert_eq!(q[d.drop_oldest], present_ts);
    }

    #[test]
    fn release_holds_when_source_early_or_empty() {
        // Source ahead: every queued frame is in the future relative to the deadline.
        let q = caps(4); // ts >= WBASE
        let present_ts = WBASE - 1; // nothing due yet
        assert_eq!(
            genlock_release(present_ts, &q),
            GenlockRelease {
                drop_oldest: 0,
                present: false
            },
            "no frame at/before the deadline → hold (repeat last), drop nothing"
        );
        // Source empty (stalled): nothing to present.
        assert_eq!(
            genlock_release(WBASE + 100 * NS30, &[]),
            GenlockRelease {
                drop_oldest: 0,
                present: false
            }
        );
    }

    #[test]
    fn release_keeps_two_sources_in_sync_despite_different_depths() {
        // THE core property (#136). Two genlock cams capture at the SAME wall-clock
        // cadence but sit at DIFFERENT FIFO depths (the exact desync cause under the
        // old count gate: cam A shallow=5, cam B deep=20). At ONE shared presentation
        // deadline, both present the frame with the IDENTICAL capture timestamp →
        // in-sync BY CONSTRUCTION, independent of buffered depth.
        let a = caps(5);
        let b = caps(20);
        let present_ts = WBASE + 3 * NS30;
        let da = genlock_release(present_ts, &a);
        let db = genlock_release(present_ts, &b);
        assert!(da.present && db.present, "both have a due frame");
        let pres_a = a[da.drop_oldest];
        let pres_b = b[db.drop_oldest];
        assert_eq!(
            pres_a, pres_b,
            "different depths must present the SAME capture instant"
        );
        assert_eq!(
            pres_a, present_ts,
            "the presented frame is the one captured at the deadline"
        );
    }

    #[test]
    fn present_ts_subtracts_the_common_delay_with_half_interval_bias() {
        // present_ts = tick_wall - delay_frames*interval + interval/2 (saturating).
        // The +interval/2 is the #136 boundary-churn tolerance.
        let tick = WBASE + 100 * NS30;
        assert_eq!(
            genlock_present_ts(tick, 6, NS30),
            tick - 6 * NS30 + NS30 / 2
        );
        // sub saturates to 0, then the half-interval bias is added.
        assert_eq!(
            genlock_present_ts(NS30, 6, NS30),
            NS30 / 2,
            "saturates, never wraps below 0"
        );
    }

    #[test]
    fn release_no_churn_for_a_boundary_landing_frame() {
        // #136 boundary-churn regression guard. A frame captured EXACTLY on the nominal
        // deadline (wall - preload*interval) must be robustly DUE even when wall_now is a
        // hair before the boundary (render-tick slew) — otherwise it alternates hold/drop
        // tick-to-tick (the ~3 fps stream churn). With the half-interval bias it is due.
        let interval = NS30;
        let wall = WBASE + 100 * interval;
        let q = vec![wall - interval, wall]; // head captured at the nominal deadline (preload=1)
        assert!(
            genlock_release(genlock_present_ts(wall, 1, interval), &q).present,
            "boundary-landing frame must be due (no hold churn)"
        );
        // wall a hair BEFORE the boundary (−2ms slew): WITHOUT the bias this would HOLD;
        // with the half-interval bias it stays due.
        let slewed = wall - 2_000_000;
        assert!(
            genlock_release(genlock_present_ts(slewed, 1, interval), &q).present,
            "still due under −2ms render-tick slew (the fix kills the boundary jitter)"
        );
    }

    #[test]
    fn wallclock_guard_accepts_real_ts_rejects_garbage() {
        assert!(
            is_wallclock_ts(WBASE),
            "a real DanteSync wall-clock ts is accepted"
        );
        assert!(
            !is_wallclock_ts(0),
            "0 (no timecode) is rejected → count-gate fallback"
        );
        assert!(
            !is_wallclock_ts(NS30),
            "a small monotonic-style ts is rejected"
        );
        assert!(
            !is_wallclock_ts(WALLCLOCK_TS_MAX_NS),
            "upper bound is exclusive"
        );
    }

    // ---- #126: reconnect re-arm (sustained-empty → rebuild) ----------------

    #[test]
    fn rearm_unit_fires_only_after_sustained_empty_in_steady_state() {
        // Re-arm fires when the source is in STEADY state (filled) AND it has just
        // resumed after a sustained true-empty run >= the threshold.
        assert!(
            genlock_rearm_on_resume(GENLOCK_REARM_EMPTY_TICKS, true),
            "an empty run AT the threshold while filled must re-arm"
        );
        assert!(
            genlock_rearm_on_resume(GENLOCK_REARM_EMPTY_TICKS + 50, true),
            "an empty run past the threshold while filled must re-arm"
        );
    }

    #[test]
    fn rearm_unit_brief_empty_below_threshold_never_rearms() {
        // The jitter-safety guard: a brief empty (1..threshold-1) must NOT re-arm —
        // a spurious re-arm would force a ~preload-frame rebuild hold on every blip,
        // catastrophic at the shallow cam preload=1.
        for run in 0..GENLOCK_REARM_EMPTY_TICKS {
            assert!(
                !genlock_rearm_on_resume(run, true),
                "empty run {run} < threshold {GENLOCK_REARM_EMPTY_TICKS} must NOT re-arm"
            );
        }
    }

    #[test]
    fn rearm_unit_never_fires_while_building() {
        // While !filled the FIFO is already in the build phase rebuilding the reserve;
        // re-arming again is a no-op concept — never fire when not filled, at ANY run.
        for run in [
            0u32,
            1,
            GENLOCK_REARM_EMPTY_TICKS,
            GENLOCK_REARM_EMPTY_TICKS + 100,
        ] {
            assert!(
                !genlock_rearm_on_resume(run, false),
                "must never re-arm while !filled (run {run})"
            );
        }
    }

    #[test]
    fn rearm_threshold_is_safely_above_jitter() {
        // ~1 s @ 30 fps. Must be far above any realistic single-dip jitter so normal
        // operation NEVER re-arms (only a real disconnect sustains empties this long).
        assert_eq!(GENLOCK_REARM_EMPTY_TICKS, 30);
    }

    #[test]
    fn rearm_empty_run_resets_on_any_consume() {
        // The empty-run counter must reset to 0 whenever a distinct frame is consumed
        // (modelled by genlock_empty_run_next: a consume zeroes it, a true-empty +1).
        assert_eq!(genlock_empty_run_next(5, true), 0, "consume resets the run");
        assert_eq!(
            genlock_empty_run_next(5, false),
            6,
            "true-empty increments the run"
        );
        assert_eq!(
            genlock_empty_run_next(0, false),
            1,
            "first empty after a consume"
        );
        // Saturates — never wraps on a very long disconnect.
        assert_eq!(genlock_empty_run_next(u32::MAX, false), u32::MAX);
    }

    #[test]
    fn build_drain_unit_trims_to_target() {
        // At the build latch (depth > preload) drain down to target = preload+1.
        assert_eq!(genlock_build_drain(2, 1), 0); // depth == target → 0
        assert_eq!(genlock_build_drain(6, 1), 4); // deep burst → trim 4 to reach 2
        assert_eq!(genlock_build_drain(31, 30), 0); // target
        assert_eq!(genlock_build_drain(41, 30), 10);
    }

    #[test]
    fn build_drain_unit_zero_while_building_or_at_target() {
        // Below the latch (still building, depth <= preload): nothing to drain.
        for depth in 0..=30usize {
            assert_eq!(genlock_build_drain(depth, 30), 0);
        }
        // Exactly at target (preload+1): the latch instant, no trim.
        assert_eq!(genlock_build_drain(31, 30), 0);
    }

    #[test]
    fn build_drain_unit_never_underflows() {
        // A queue at or below target never produces a negative/wrapped drain.
        assert_eq!(genlock_build_drain(0, 0), 0);
        assert_eq!(genlock_build_drain(1, 0), 0); // target for preload 0 is 1
        assert_eq!(genlock_build_drain(0, 128), 0);
    }

    #[test]
    fn build_drain_unit_equals_depth_minus_target_above_latch() {
        // The exact contract: drain = depth - steady_state_depth(preload) when
        // depth > steady_state_depth, else 0.
        for preload in [0u32, 1, 2, 30, 128] {
            let target = steady_state_depth(preload) as usize;
            for depth in 0..target + 20 {
                let expected = depth.saturating_sub(target);
                assert_eq!(
                    genlock_build_drain(depth, preload),
                    expected,
                    "preload={preload} depth={depth}"
                );
            }
        }
    }

    // ---- #99 point 2: peak depth must include the PRODUCER-side high-water mark ----

    #[test]
    fn peak_update_takes_the_higher_of_prev_and_observed() {
        // The audit high-water mark only ever GROWS toward the true maximum: a freshly
        // observed depth above the running peak raises it; a lower one leaves it.
        assert_eq!(
            genlock_peak_update(0, 5),
            5,
            "first observation sets the peak"
        );
        assert_eq!(
            genlock_peak_update(5, 8),
            8,
            "a higher depth raises the peak"
        );
        assert_eq!(
            genlock_peak_update(8, 3),
            8,
            "a lower depth leaves the peak"
        );
        assert_eq!(
            genlock_peak_update(8, 8),
            8,
            "an equal depth leaves the peak"
        );
    }

    #[test]
    fn peak_captures_a_producer_burst_that_drains_before_the_next_tick() {
        // THE #99 point-2 BUG: a CONSUMER-side-only peak under-reports. Model the timeline:
        // the producer pushes the FIFO up to depth 6 between render ticks (its high-water
        // mark), then the consumer drains it back to 1 before the next render tick observes
        // the queue. If peak is folded ONLY at the consumer-observed depth (1), it records 1
        // and the operator never sees how close the queue got to the cap. Folding the
        // PRODUCER-observed depth (6) at the push site captures the true 6.
        let mut peak = 0u32;

        // --- producer pushes 6 frames between ticks (each push observes the new depth) ---
        for depth_after_push in 1..=6u32 {
            peak = genlock_peak_update(peak, depth_after_push);
        }
        // --- consumer drains to 1 by the next render tick; only THAT depth is consumer-seen ---
        let consumer_observed_depth = 1u32;
        peak = genlock_peak_update(peak, consumer_observed_depth);

        assert_eq!(
            peak, 6,
            "peak must reflect the producer-side high-water mark (6), not just the \
             consumer-observed depth (1) — the #99 point-2 under-report"
        );
    }
}
