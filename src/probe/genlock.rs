//! Genlock FIFO preload-reserve decision logic (camera-box #70).
//!
//! This is the camera-box-side, pure, unit-tested MIRROR of the C decision logic
//! baked into the vendored OBS genlock FIFO
//! (`vendor/obs-studio/libobs/obs-source.c`, the `genlock_fifo` branch of
//! `ready_async_frame()` + `genlock_parse_preload()`). Keeping the contract here
//! lets CI prove the parse/clamp + consume rules without an OBS build, and the
//! `tests/genlock_preload.rs` vendored-source guard keeps the C side in lock-step.
//!
//! ## Why the reserve exists
//!
//! The original genlock FIFO (#42) consumed exactly one queued frame per
//! wall-clock render tick with ZERO slack. With the wall-clock-slaved tick the
//! producer (NDI sender) and consumer (compositor) run at the same average rate,
//! so the queue parks around depth 1 — but any NDI arrival *jitter* (one late
//! packet) leaves the queue empty at the next tick: an **underrun**, which the
//! compositor renders as a dropped/repeated frame. The #68/#69 QR instrument
//! measured ~0.38%/frame loss on each OBS hop from exactly this.
//!
//! The fix holds consumption until the queue is *deeper than* `preload`, so the
//! FIFO keeps `preload` frames of jitter buffer. `preload = 1` ⇒ one frame of
//! reserve = one frame of added latency per hop, absorbing one tick of jitter.

/// Default reserve when `OBS_GENLOCK_PRELOAD_FRAMES` is unset/invalid: one frame
/// (= one frame of latency per hop, the "1 frame per hop" the task calls for).
pub const GENLOCK_PRELOAD_DEFAULT: u32 = 1;

/// Hard cap on the reserve. Must stay strictly below libobs' `MAX_ASYNC_FRAMES`
/// (30): the steady-state queue parks at `preload + 1`, so a `preload` at/above
/// the cap could never reach steady state without triggering the force-drain.
pub const GENLOCK_PRELOAD_MAX: u32 = 29;

/// Parse the `OBS_GENLOCK_PRELOAD_FRAMES` env value into a reserve depth.
///
/// Mirrors the C `genlock_parse_preload()`:
/// * `None` / empty / all-whitespace / non-numeric / negative ⇒ default.
/// * Above the cap ⇒ clamped to [`GENLOCK_PRELOAD_MAX`] (NOT silently default).
/// * `0` is valid and reproduces the old zero-slack behavior.
pub fn parse_preload(env: Option<&str>) -> u32 {
    let Some(raw) = env else {
        return GENLOCK_PRELOAD_DEFAULT;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return GENLOCK_PRELOAD_DEFAULT;
    }
    match trimmed.parse::<i64>() {
        Ok(v) if v < 0 => GENLOCK_PRELOAD_DEFAULT,
        Ok(v) if v > GENLOCK_PRELOAD_MAX as i64 => GENLOCK_PRELOAD_MAX,
        Ok(v) => v as u32,
        Err(_) => GENLOCK_PRELOAD_DEFAULT,
    }
}

/// At a render tick, should the FIFO consume one frame?
///
/// Consume only once the queue is *deeper than* the reserve, so a `queue_depth`
/// at or below `preload` (including an empty queue) holds — repeating the last
/// frame for one tick so the reserve refills. Mirrors the C
/// `genlock_should_consume()`.
pub fn should_consume(queue_depth: usize, preload: u32) -> bool {
    queue_depth > preload as usize
}

/// The steady-state queue depth the gate parks at when producer and consumer run
/// at the same rate: one frame above the reserve, so `preload` frames of jitter
/// slack remain at the instant of consumption.
pub fn steady_state_depth(preload: u32) -> u32 {
    preload + 1
}
