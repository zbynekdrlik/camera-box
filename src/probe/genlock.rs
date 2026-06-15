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

/// Hard cap on the reserve. The steady-state queue parks at `preload + 1`, which
/// must stay STRICTLY below libobs' `MAX_ASYNC_FRAMES` (30): a `preload` of 29
/// would steady at depth 30 == the cap, force-draining every refill and FREEZING
/// the source. 28 ⇒ steady depth 29 < 30 — the highest safe reserve.
pub const GENLOCK_PRELOAD_MAX: u32 = 28;

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
    let bytes = raw.as_bytes();
    // strtol skips leading ASCII whitespace, then reads an optional sign + digits.
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        negative = bytes[i] == b'-';
        i += 1;
    }
    let digits_start = i;
    // Accumulate digits, saturating to i64::MAX like strtol saturates to LONG_MAX.
    let mut value: i64 = 0;
    let mut overflow = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        if !overflow {
            let d = (bytes[i] - b'0') as i64;
            match value.checked_mul(10).and_then(|v| v.checked_add(d)) {
                Some(v) => value = v,
                None => {
                    value = i64::MAX;
                    overflow = true;
                }
            }
        }
        i += 1;
    }
    // No digits consumed (end == env) OR a trailing non-digit (*end != '\0') ⇒ default.
    if i == digits_start || i != bytes.len() {
        return GENLOCK_PRELOAD_DEFAULT;
    }
    // Negative ⇒ default (the C `v < 0` guard). LONG_MAX from a `-` overflow is
    // still negative-intent, so honour the sign first.
    if negative {
        return GENLOCK_PRELOAD_DEFAULT;
    }
    if value > GENLOCK_PRELOAD_MAX as i64 {
        return GENLOCK_PRELOAD_MAX;
    }
    value as u32
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
