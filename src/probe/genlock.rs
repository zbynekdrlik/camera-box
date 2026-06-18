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

#[cfg(test)]
mod tests {
    use super::*;

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
}
