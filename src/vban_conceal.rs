//! Issue 1439 — the concealment of a short run of lost VBAN packets (ROZHODNUTÉ 6352253147 item
//! (b)), the Tier-0 authority of `vendor/obs-vban/src/vban-conceal.h`. RED stub: the sample math is
//! not written yet (no fade, the previous packet as it is).

/// Each crossfade lasts this much audio ...
pub const FADE_MS: u32 = 1;
/// ... and at most this fraction of the slot.
pub const FADE_DIV: u32 = 4;
/// The C header's `VBAN_CONCEAL_PI` (RED stub: used by the tests only).
#[allow(dead_code)]
const PI: f64 = std::f64::consts::PI;

/// RED stub.
pub fn fade_frames(_frames: u32, _rate: u32) -> u32 {
    1
}

/// RED stub.
pub fn cospi(_u: f64) -> f64 {
    1.0
}

/// RED stub.
pub fn weight(_i: u32, _n: u32) -> f64 {
    0.0
}

/// RED stub.
pub fn sample(
    rep: f64,
    _last: f64,
    _next: f64,
    _toward_next: bool,
    _i: u32,
    _frames: u32,
    _fade: u32,
) -> f64 {
    rep
}

#[cfg(test)]
#[path = "vban_conceal_tests.rs"]
mod tests;
