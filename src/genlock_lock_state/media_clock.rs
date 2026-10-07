//! Issue 1372 part D — the MEDIA-clock (audio clock) term: the windowed wall-vs-media drift
//! ([`media_clock_window`], [`media_clock_window_ready`]) and its verdict ([`media_clock_verdict`]).
//! Mirrored in `GenlockLockState.hpp` (the `genlock_media_*` block), parity-gated by
//! `tests/genlock_lock_state_parity.rs`. Split out of `genlock_lock_state.rs` (issue 1302); every
//! item is re-exported at `crate::genlock_lock_state`.

// Issue 1372 part D — the MEDIA-clock (audio clock) term. `os_gettime_ns()` paces OBS's audio mixer,
// its video thread and every output timestamp. Once issue 1372 part A made the Windows
// `os_gettime_ns()` run at the dantesync-disciplined system-time rate (Linux's `CLOCK_MONOTONIC` is
// kernel-disciplined already), the wall-vs-media offset must stay FLAT on every box apart from wall
// steps: 0 ms over 47 min on stream after the part-A deploy, 0 on strih-lx for hours, while the
// undisciplined stream mixer walked 67 ms in 83 min (≈ 8 ms per 10 min) before it — green on the
// indicator the whole time. So the offset's RATE over a window is a real fault signal again, unlike
// the #1357-removed rate term: that one compared the rate with an instantaneous dantesync
// `f_ptp + f_phase` sample, which meant a different thing per box; this one compares with 0, which
// means the same thing everywhere.
//
// The widget samples the offset itself in µs each 1 Hz tick (the libobs `wall_qpc_drift_ms` is integer
// ms truncated toward zero). The centre rate is the MEDIAN of the per-pair rates; a pair whose offset
// change differs from what the centre rate predicts over its interval by more than
// [`GENLOCK_MEDIA_CLOCK_BAND_US`] (100 µs) is a wall STEP and is left out; the rate is the TIME-WEIGHTED
// rate of the kept pairs (Σ change / Σ interval). dantesync requests steps of ≥ 200 µs (server) /
// ≥ 500 µs (client), so they are out whatever their pattern, as long as steps touch fewer than half the
// pairs. On Windows a step lands up to one timer tick short of the request (dantesync targets the
// coarse `GetSystemTimeAsFileTime`), so some remnants fall inside the band: they are unbiased and
// ≤ 100 µs each, well under 1 ms per window. A drift present in only part of the seconds moves a ~1 s
// pair by its rate in µs, so up to ~95 ppm (with ±50 ms tick jitter) it is kept and counted at its
// true share (a pure median would drop it below half coverage). A pair more than [`GENLOCK_MEDIA_CLOCK_MAX_GAP_MS`] apart (a stalled UI) is not
// a sample, and the window counts as ready only once the counted pairs cover ≥ 90 % of it. The second input is the
// Windows discipline state libobs publishes (`os_gettime_discipline()`): a fallback to raw QPC while
// dantesync answers is DEGRADED at once. The term never makes a box UNLOCKED on its own.

/// The window (s) the media-clock rate is measured over — the design's "per 10 min".
pub const GENLOCK_MEDIA_CLOCK_WINDOW_S: i64 = 600;
/// The drift (µs per window) beyond which the media clock DEGRADES: > 2 ms per 10 min (> 3.3 ppm).
/// The undisciplined stream accrued ≈ 8 ms per 10 min; a disciplined box stays at 0.
pub const GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US: i64 = 2000;
/// A pair of samples further apart than this (ms) — a stalled UI thread — is not a rate sample.
pub const GENLOCK_MEDIA_CLOCK_MAX_GAP_MS: i64 = 5000;
/// A pair whose offset change differs from the centre rate's prediction by more than this (µs) is a
/// wall STEP. The deviation of a pair that spans a step IS the step, whatever the interval; dantesync
/// requests steps of ≥ 200 µs (server) / ≥ 500 µs (client) — a Windows step can land up to one timer
/// tick short, so a small remnant may count (unbiased, ≤ 100 µs each); ±1 µs sampling noise stays far
/// inside. A drift that differs from the centre by R ppm moves a 1 s pair by R µs, so a partial drift up
/// to ~95 ppm is kept with ±50 ms tick jitter (up to 20 ppm on a 5 s pair); a faster one present in
/// fewer than half the seconds reads as steps — a raw-QPC fallback, the fast case, is the discipline
/// outcome's.
pub const GENLOCK_MEDIA_CLOCK_BAND_US: i64 = 100;

/// What the Windows `os_gettime_ns()` last read from the system-time adjustment. Discriminants match
/// libobs `enum os_gettime_discipline_state` (`util/platform.h`) and the C mirror
/// `genlock_media_discipline` in `GenlockLockState.hpp`; `NotApplicable` is the widget's value on a
/// Linux box, whose kernel disciplines `CLOCK_MONOTONIC` together with the wall clock (macOS
/// `os_gettime_ns` is the raw `CLOCK_UPTIME_RAW`; no fleet box runs it, and the drift input still
/// covers it).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDiscipline {
    /// Not polled yet (the first ~250 ms after OBS start).
    Unknown = 0,
    /// The adjustment is read and enabled: the clock runs at the disciplined rate.
    Active = 1,
    /// The adjustment is disabled or zero: raw QPC.
    Disabled = 2,
    /// `GetSystemTimeAdjustmentPrecise` returned FALSE: raw QPC.
    ReadFailed = 3,
    /// The API is not exported (old Windows): raw QPC.
    ApiMissing = 4,
    /// Not a Windows box: there is no discipline outcome to read (the drift input still applies).
    NotApplicable = 5,
}

impl MediaDiscipline {
    /// The integer libobs and the C mirror use.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// True for the three raw-QPC fallback outcomes.
    pub fn is_raw_fallback(self) -> bool {
        matches!(
            self,
            MediaDiscipline::Disabled | MediaDiscipline::ReadFailed | MediaDiscipline::ApiMissing
        )
    }
}

/// The media-clock verdict. Discriminants match the C `genlock_media_clock` enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaClock {
    /// The media clock follows the disciplined wall clock (or there is not yet enough data).
    Ok = 0,
    /// The wall-vs-media offset grew beyond the bound over the window.
    Drift = 1,
    /// Windows: the disciplined clock fell back to raw QPC while dantesync runs.
    Undisciplined = 2,
}

impl MediaClock {
    /// The integer the C `genlock_media_clock_verdict` returns.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The media-clock rate over one window, as the widget reduces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaClockWindow {
    /// The time-weighted rate of the non-step pairs scaled to the window: µs of wall-vs-media drift per
    /// `window_s`.
    pub drift_us: i64,
    /// The total interval (ms) of the pairs that counted (gaps and non-increasing times left out);
    /// the window is ready once this covers ≥ 90 % of `window_s`.
    pub counted_ms: i64,
}

/// One pair's rate in ppb (ns of offset change per s): `change_us × 1e6 / dt_ms`, truncated toward
/// zero, saturating. `dt_ms` must be positive.
pub fn media_clock_pair_rate_ppb(change_us: i64, dt_ms: i64) -> i64 {
    change_us.saturating_mul(1_000_000) / dt_ms
}

/// The wall-vs-media rate across a window of `(t_ms, offset_us)` samples (oldest first; `t_ms` the
/// widget's monotonic ms, `offset_us` the wall-minus-media offset in µs). Each consecutive pair with
/// `0 < dt ≤ max_gap_ms` yields one rate ([`media_clock_pair_rate_ppb`]); their MEDIAN (the mean of the
/// two middle rates for an even count, `a + (b − a) / 2`) is the centre. A pair is kept when its change
/// is within `band_us` of `centre × dt / 1e6` µs (a negative band keeps none); the rate is
/// `Σ kept change × 1e6 / Σ kept dt` ppb (the centre itself when none is kept), truncated toward zero,
/// scaled to `window_s`: `rate × window_s / 1000` µs. Every step saturates. No pair, or a non-positive
/// `window_s`, gives 0 drift (`counted_ms` is still reported).
///
/// Byte-for-byte mirror of `genlock_media_clock_window_drift_us` in `GenlockLockState.hpp` — the
/// parity gate `tests/genlock_lock_state_parity.rs` keeps the two (and the python mirror) identical.
pub fn media_clock_window(
    samples: &[(i64, i64)],
    window_s: i64,
    max_gap_ms: i64,
    band_us: i64,
) -> MediaClockWindow {
    let pairs: Vec<(i64, i64)> = samples
        .windows(2)
        .map(|w| (w[1].0.saturating_sub(w[0].0), w[1].1.saturating_sub(w[0].1)))
        .filter(|&(dt, _)| dt > 0 && dt <= max_gap_ms)
        .collect();
    let counted_ms = pairs
        .iter()
        .fold(0i64, |acc, &(dt, _)| acc.saturating_add(dt));
    if pairs.is_empty() || window_s <= 0 {
        return MediaClockWindow {
            drift_us: 0,
            counted_ms,
        };
    }
    let mut rates: Vec<i64> = pairs
        .iter()
        .map(|&(dt, change)| media_clock_pair_rate_ppb(change, dt))
        .collect();
    rates.sort_unstable();
    let m = rates.len();
    let centre = if m % 2 == 1 {
        rates[m / 2]
    } else {
        let (a, b) = (rates[m / 2 - 1], rates[m / 2]);
        a.saturating_add(b.saturating_sub(a) / 2)
    };
    let (mut change_sum, mut dt_sum) = (0i64, 0i64);
    for &(dt, change) in &pairs {
        let expected = centre.saturating_mul(dt) / 1_000_000;
        if change.saturating_sub(expected).saturating_abs() <= band_us {
            change_sum = change_sum.saturating_add(change);
            dt_sum = dt_sum.saturating_add(dt);
        }
    }
    let rate = if dt_sum == 0 {
        centre
    } else {
        change_sum.saturating_mul(1_000_000) / dt_sum
    };
    MediaClockWindow {
        drift_us: rate.saturating_mul(window_s) / 1000,
        counted_ms,
    }
}

/// True once the counted pairs cover ≥ 90 % of the `window_s` window; a non-positive window is never
/// ready. Byte-for-byte mirror of `genlock_media_clock_window_ready` in `GenlockLockState.hpp`.
pub fn media_clock_window_ready(counted_ms: i64, window_s: i64) -> bool {
    window_s > 0 && counted_ms >= window_s.saturating_mul(1000) / 10 * 9
}

/// Decide the media-clock verdict. `Undisciplined` when the Windows discipline fell back to raw QPC
/// while dantesync answers (`clock_present`); else `Drift` when the window is ready (spans ≥ 90 % of
/// [`GENLOCK_MEDIA_CLOCK_WINDOW_S`]) and `|drift_us|` exceeds `drift_bound_us`; else `Ok`.
///
/// Byte-for-byte mirror of `genlock_media_clock_verdict` in `GenlockLockState.hpp` (parity-gated).
pub fn media_clock_verdict(
    window_ready: bool,
    drift_us: i64,
    drift_bound_us: i64,
    discipline: MediaDiscipline,
    clock_present: bool,
) -> MediaClock {
    if clock_present && discipline.is_raw_fallback() {
        return MediaClock::Undisciplined;
    }
    if window_ready && drift_us.saturating_abs() > drift_bound_us {
        return MediaClock::Drift;
    }
    MediaClock::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(t_ms, offset_us)` samples every `dt_ms`, offset = `f(i)`.
    fn ramp(n: i64, dt_ms: i64, f: impl Fn(i64) -> i64) -> Vec<(i64, i64)> {
        (0..=n).map(|i| (i * dt_ms, f(i))).collect()
    }

    fn window(samples: &[(i64, i64)]) -> MediaClockWindow {
        media_clock_window(
            samples,
            GENLOCK_MEDIA_CLOCK_WINDOW_S,
            GENLOCK_MEDIA_CLOCK_MAX_GAP_MS,
            GENLOCK_MEDIA_CLOCK_BAND_US,
        )
    }

    /// A deterministic ±`amp` µs noise sequence (splitmix64 of the index), for offsets that wobble
    /// but do not drift.
    fn noise(i: i64, amp: i64) -> i64 {
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z % (2 * amp as u64 + 1)) as i64 - amp
    }

    #[test]
    fn the_window_rate_leaves_steps_out_and_counts_a_partial_drift() {
        // The pre-part-A stream: 13.5 ppm = 13.5 µs per 1 Hz pair -> 8.1 ms per 600 s.
        let w = window(&ramp(600, 1000, |i| i * 27 / 2));
        assert_eq!(w.drift_us, 8100);
        assert_eq!(w.counted_ms, 600_000);
        // A disciplined box whose dantesync steps — a locked master with phase_slew off
        // (1000 µs + 2 × 23 ppm × 10 s = 1460 µs), a client (600 µs), the not-locked master's 250 µs
        // every 25 s, the −146 µs step seen on win-resolume — all one direction: 0.
        assert_eq!(window(&ramp(600, 1000, |i| (i / 60) * 1460)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 30) * 600)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 25) * 250)).drift_us, 0);
        assert_eq!(window(&ramp(600, 1000, |i| (i / 20) * -146)).drift_us, 0);
        // The same steps on top of the undisciplined rate: the rate (each step pair also carried
        // ~14 µs of rate, which leaves with it: (300 × 13 000 + 290 × 14 000) / 590 ppb).
        assert_eq!(
            window(&ramp(600, 1000, |i| i * 27 / 2 + (i / 60) * 1460)).drift_us,
            8094
        );
        // A late UI tick carrying a step, and jittered ticks: still the rate.
        let mut late: Vec<(i64, i64)> = ramp(600, 1000, |i| i * 27 / 2);
        for (k, s) in late.iter_mut().enumerate().skip(300) {
            s.0 += 500; // one 1.5 s tick at 300
            s.1 += 520 + if k >= 450 { 520 } else { 0 };
        }
        assert_eq!(window(&late).drift_us, 8098);
        // A 20 ppm drift in only 45 % of the seconds (the rest flat): a pure median would read 0; the
        // time-weighted rate reads its true share, 20 × 0.45 × 600 = 5400 µs -> DRIFT.
        let partial = ramp(600, 1000, |i| (i * 9 / 20) * 20);
        assert_eq!(window(&partial).drift_us, 5400);
        // The same at 40 ppm (a 40 µs change per 1 s pair, inside the 100 µs band): 10 800 µs, not 0.
        let partial40 = ramp(600, 1000, |i| (i * 9 / 20) * 40);
        assert_eq!(window(&partial40).drift_us, 10_800);
        // A drift sampled by short (16 ms) pairs, each change 0 or 1 µs, stays inside the band.
        let short = ramp(3000, 16, |i| i * 16 * 20 / 1000);
        assert_eq!(window(&short).drift_us, 12_000);
        // Time-weighted: 300 flat 1 s pairs + 60 stalled 4 s pairs carrying 15 ppm (60 µs each). The
        // rate is Σ change / Σ dt = 3600 µs / 540 s = 6666 ppb -> 3999 µs; an unweighted mean of the
        // per-pair rates would read 2500 ppb -> 1500 µs.
        let mut mixed: Vec<(i64, i64)> = ramp(300, 1000, |_| 0);
        for k in 1..=60i64 {
            mixed.push((300_000 + k * 4000, 60 * k));
        }
        assert_eq!(window(&mixed).drift_us, 3999);
        // The band edge: a change exactly `band` µs off the centre's prediction is kept, 1 µs more is not.
        let edge = |far: i64| {
            let mut v = vec![(0i64, 0i64)];
            for k in 1..=5i64 {
                v.push((k * 1000, 0));
            }
            v.push((6000, far));
            window(&v).drift_us
        };
        // 100 µs is kept (100 µs / 6 s = 16 666 ppb × 600 / 1000); 101 µs is dropped.
        assert_eq!(edge(100), 9999);
        assert_eq!(edge(101), 0);
        // Sampling wobble (5× the ±1 µs measured on dev1) that does not drift stays far below 2 ms.
        let wobble = window(&ramp(600, 1000, |i| noise(i, 5)));
        assert!(wobble.drift_us.abs() < 200, "{wobble:?}");
        // A pair more than 5 s apart (a stalled UI) is not a sample, and does not count as covered.
        let gap = window(&[(0, 0), (1000, 20), (121_000, -14_000), (122_000, -13_980)]);
        assert_eq!(
            gap,
            MediaClockWindow {
                drift_us: 12_000,
                counted_ms: 2000
            }
        );
        assert!(!media_clock_window_ready(gap.counted_ms, 600));
        // Negative drift; an even count averages the two middle rates, and an odd, negative middle
        // gap rounds toward the lower rate exactly like the C (a + (b - a) / 2).
        assert_eq!(window(&ramp(3, 1000, |i| -20 * i)).drift_us, -12_000);
        assert_eq!(window(&[(0, 0), (1000, 10), (2000, 30)]).drift_us, 9000);
        // (a negative band keeps no pair, so the centre itself is the result)
        // centre -333 333 + 333 333 / 2 = -166 667 ppb:
        let odd_neg = media_clock_window(&[(0, 0), (3, -1), (6, -1)], 600, 5000, -1);
        assert_eq!(odd_neg.drift_us, -100_000);
        // centre 333 333 + 333 333 / 2 = 499 999 ppb:
        let odd_pos = media_clock_window(&[(0, 0), (3, 1), (6, 3)], 600, 5000, -1);
        assert_eq!(odd_pos.drift_us, 299_999);
        // ...and kept, the time-weighted rate: 3 µs / 6 ms = 500 000 ppb.
        let kept = media_clock_window(&[(0, 0), (3, 1), (6, 3)], 600, 5000, 1);
        assert_eq!(kept.drift_us, 300_000);

        // No pair, a non-increasing time, a non-positive window.
        assert_eq!(
            window(&[(0, 7)]),
            MediaClockWindow {
                drift_us: 0,
                counted_ms: 0
            }
        );
        assert_eq!(window(&[]).drift_us, 0);
        assert_eq!(window(&[(1000, 0), (1000, 50)]).counted_ms, 0);
        assert_eq!(
            media_clock_window(&[(0, 0), (1000, 5)], 0, 5000, GENLOCK_MEDIA_CLOCK_BAND_US).drift_us,
            0
        );
        // Saturating at the extremes, never a panic.
        assert_eq!(
            media_clock_window(
                &[(0, i64::MIN), (1, i64::MAX)],
                i64::MAX,
                i64::MAX,
                i64::MAX
            )
            .drift_us,
            i64::MAX / 1000
        );
        // an even count of MIN and MAX: the centre MIN + MAX / 2 < 0 keeps neither (band 100 µs), so
        // the centre is the rate; times a huge window it saturates to MIN
        assert_eq!(
            media_clock_window(
                &[(0, 0), (1, i64::MIN), (2, i64::MAX)],
                i64::MAX,
                i64::MAX,
                GENLOCK_MEDIA_CLOCK_BAND_US
            )
            .drift_us,
            i64::MIN / 1000
        );
        // two saturated (MAX) rates kept: the change sum × 1e6 saturates, / 2 ms = MAX / 2, and the
        // window scale saturates again
        assert_eq!(
            media_clock_window(&[(0, 0), (1, 1 << 62), (2, i64::MAX)], 1000, 5000, i64::MAX)
                .drift_us,
            i64::MAX / 1000
        );
    }

    #[test]
    fn the_window_is_ready_at_ninety_percent_coverage() {
        assert!(!media_clock_window_ready(539_999, 600));
        assert!(media_clock_window_ready(540_000, 600));
        assert!(media_clock_window_ready(600_000, 600));
        assert!(!media_clock_window_ready(0, 600));
        assert!(!media_clock_window_ready(0, 0));
        assert!(!media_clock_window_ready(i64::MAX, -1));
        assert!(media_clock_window_ready(i64::MAX, i64::MAX));
    }

    #[test]
    fn drift_verdict_follows_the_bound_and_waits_for_the_window() {
        let b = GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US;
        let a = MediaDiscipline::Active;
        // Pre-part-A stream (≈ 8 ms / 10 min) -> DRIFT; a disciplined box (0) -> OK.
        assert_eq!(
            media_clock_verdict(true, 8100, b, a, true),
            MediaClock::Drift
        );
        assert_eq!(
            media_clock_verdict(true, -2001, b, a, true),
            MediaClock::Drift
        );
        for d in [-2000, -1, 0, 1, 2000] {
            assert_eq!(media_clock_verdict(true, d, b, a, true), MediaClock::Ok);
        }
        // Not ready (the window is not yet ~full) -> never judged on drift.
        assert_eq!(
            media_clock_verdict(false, 99_000, b, a, true),
            MediaClock::Ok
        );
        // Linux: not applicable, judged on drift only.
        let na = MediaDiscipline::NotApplicable;
        assert_eq!(media_clock_verdict(true, 0, b, na, true), MediaClock::Ok);
        assert_eq!(
            media_clock_verdict(true, 5000, b, na, true),
            MediaClock::Drift
        );
    }

    #[test]
    fn a_raw_qpc_fallback_while_dantesync_runs_is_undisciplined() {
        let b = GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US;
        for d in [
            MediaDiscipline::Disabled,
            MediaDiscipline::ReadFailed,
            MediaDiscipline::ApiMissing,
        ] {
            assert!(d.is_raw_fallback());
            // Immediately, before any drift accrues and before the window is ready.
            assert_eq!(
                media_clock_verdict(false, 0, b, d, true),
                MediaClock::Undisciplined
            );
            // No dantesync answering: raw QPC is the right clock, judged on drift only.
            assert_eq!(media_clock_verdict(false, 0, b, d, false), MediaClock::Ok);
            assert_eq!(
                media_clock_verdict(true, 9000, b, d, false),
                MediaClock::Drift
            );
        }
        for d in [
            MediaDiscipline::Unknown,
            MediaDiscipline::Active,
            MediaDiscipline::NotApplicable,
        ] {
            assert!(!d.is_raw_fallback());
            assert_eq!(media_clock_verdict(false, 0, b, d, true), MediaClock::Ok);
        }
    }

    #[test]
    fn media_codes_match_the_c_and_libobs_values() {
        assert_eq!(MediaClock::Ok.code(), 0);
        assert_eq!(MediaClock::Drift.code(), 1);
        assert_eq!(MediaClock::Undisciplined.code(), 2);
        assert_eq!(MediaDiscipline::Unknown.code(), 0);
        assert_eq!(MediaDiscipline::Active.code(), 1);
        assert_eq!(MediaDiscipline::Disabled.code(), 2);
        assert_eq!(MediaDiscipline::ReadFailed.code(), 3);
        assert_eq!(MediaDiscipline::ApiMissing.code(), 4);
        assert_eq!(MediaDiscipline::NotApplicable.code(), 5);
    }
}
