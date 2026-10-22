//! Issue 1439 — the concealment of a short run of lost VBAN packets in the vendored obs-vban
//! receive source (ROZHODNUTÉ 6352253147 item (b), the run limit raised to 3 slots on 23.10.2026),
//! the Tier-0 authority of `vendor/obs-vban/src/vban-conceal.h`.
//!
//! The receive timeline ([`crate::vban_receive_timeline`]) decides WHICH missing slot is concealed:
//! one of a run of at most `CONCEAL_MAX_SLOTS` missing slots that sits between two received
//! packets, decided at the slot's deadline from what is stored then, never waiting for a later
//! packet. This module is the sample math of a concealed slot: the PREVIOUS packet's samples played
//! again in the missing slot, with two short raised-cosine crossfades so neither boundary steps:
//!
//! * **In.** Over the first `fade` frames the slot fades from the last sample handed to libobs on
//!   that channel (held) into the repeated samples. The previous output already went out and cannot
//!   change; holding its last value keeps the join continuous.
//! * **Out** (only when the next slot is the stored packet that ends the run, `toward_next`). Over
//!   the last `fade` frames the repeated samples fade toward that packet's first sample, so the
//!   packet starts where the concealment ends. A slot whose next slot is missing too ends on the
//!   repeat; the next concealed slot fades in from it.
//!
//! `fade` is 1 ms of audio ([`FADE_MS`]), at most a quarter of the slot, at least one frame. The
//! weight of fade frame `i` of `n` is `0.5 − 0.5 cos(π (i + 1) / (n + 1))`: rising, strictly inside
//! (0, 1), and the largest step between two weights is under `π / (2 (n + 1))` -- so a concealed
//! slot never steps more than the signal's own step plus that slope times the gap a fade bridges.
//!
//! Timing is not touched: a concealed slot has exactly the slot's frames and the slot's stamp; only
//! its CONTENT differs from silence.
//!
//! No libm in the C twin: the cosine is a Taylor series of cos on [0, π/4] and of sin on
//! [π/4, π/2] (`cos x = sin(π/2 − x)`), each to 8 terms (error under 1e-14, exactly 0 at π/2),
//! written here in the C header's operation order; `tests/vban_conceal_parity_1439.rs` compiles the
//! shipped header and requires identical results bit for bit.

/// Each crossfade lasts this much audio ...
pub const FADE_MS: u32 = 1;
/// ... and at most this fraction of the slot.
pub const FADE_DIV: u32 = 4;
/// The C header's `VBAN_CONCEAL_PI`.
const PI: f64 = std::f64::consts::PI;

/// The crossfade length in frames for a slot of `frames` frames at `rate`: 1 ms of audio, at most a
/// quarter of the slot, at least one frame (`vban_conceal_fade_frames`).
pub fn fade_frames(frames: u32, rate: u32) -> u32 {
    let n = (rate / 1000 * FADE_MS).min(frames / FADE_DIV);
    n.max(1)
}

/// `cos(π u)` for `u` in [0, 1] (`vban_conceal_cospi`): mirrored above 1/2
/// (`cos(π u) = −cos(π (1 − u))`), then a Taylor series in Horner form of cos on [0, π/4] and of
/// `sin(π/2 − x)` on (π/4, π/2], 8 terms each.
pub fn cospi(u: f64) -> f64 {
    let upper = u > 0.5;
    let v = if upper { 1.0 - u } else { u };
    let c = if v <= 0.25 {
        let x = v * PI;
        let x2 = x * x;
        1.0 - x2 / 2.0
            * (1.0
                - x2 / 12.0
                    * (1.0
                        - x2 / 30.0
                            * (1.0
                                - x2 / 56.0
                                    * (1.0 - x2 / 90.0 * (1.0 - x2 / 132.0 * (1.0 - x2 / 182.0))))))
    } else {
        let x = (0.5 - v) * PI;
        let x2 = x * x;
        x * (1.0
            - x2 / 6.0
                * (1.0
                    - x2 / 20.0
                        * (1.0
                            - x2 / 42.0
                                * (1.0
                                    - x2 / 72.0
                                        * (1.0
                                            - x2 / 110.0
                                                * (1.0 - x2 / 156.0 * (1.0 - x2 / 210.0)))))))
    };
    if upper {
        -c
    } else {
        c
    }
}

/// The raised-cosine weight of fade frame `i` (0 .. n − 1) of `n` (`vban_conceal_weight`):
/// `0.5 − 0.5 cos(π (i + 1) / (n + 1))`, rising, strictly inside (0, 1).
pub fn weight(i: u32, n: u32) -> f64 {
    0.5 - 0.5 * cospi(f64::from(i + 1) / f64::from(n + 1))
}

/// One concealed sample (`vban_conceal_sample`): frame `i` of a slot of `frames` frames, `rep` the
/// previous packet's sample at `i` on this channel. The first `fade` frames cross-fade in from
/// `last` (the last sample handed to libobs on this channel); with `toward_next` the last `fade`
/// frames cross-fade out to `next` (the first sample of the packet that follows the slot).
pub fn sample(
    rep: f64,
    last: f64,
    next: f64,
    toward_next: bool,
    i: u32,
    frames: u32,
    fade: u32,
) -> f64 {
    let mut x = rep;
    if i < fade {
        x = last + weight(i, fade) * (x - last);
    }
    if toward_next && frames >= fade && i >= frames - fade {
        x += weight(i - (frames - fade), fade) * (next - x);
    }
    x
}

#[cfg(test)]
#[path = "vban_conceal_tests.rs"]
mod tests;
