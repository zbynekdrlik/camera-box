//! Issue 1372 part A — the Windows OBS media clock runs at the dantesync-disciplined rate.
//!
//! ## Why this module exists
//!
//! libobs paces its audio thread, video thread, ASRC servo and every output timestamp off
//! `os_gettime_ns()`. On Windows that was raw QPC. dantesync disciplines the SYSTEM time with
//! `SetSystemTimeAdjustmentPrecise` (PTP frequency plus the NTP phase slew) and never touches
//! QPC. So Windows OBS ran up to ~20 ppm off every other disciplined box (live, win-resolume
//! 25.9.2026), and VBAN streams between the PCs slipped a packet every few minutes. The result
//! follows dantesync's SYSTEM time; a peer clocked by Dante itself (ASIO/DVS, a Dante-clocked
//! VB-Matrix) still differs by dantesync's steady `f_phase` (the Dante-GM-vs-UTC offset).
//!
//! The vendored `os_gettime_ns()` in `vendor/obs-studio/libobs/util/platform-windows.c` now
//! integrates QPC deltas scaled by the rate the OS currently applies to system time. This
//! module is the Tier-0 authority for that arithmetic. The committed gate
//! `tests/os_clock_discipline_parity_1372.rs` lifts the C block verbatim, compiles it against a
//! fake Win32 layer and requires identical output from the functions here.
//!
//! ## The rate
//!
//! `GetSystemTimeAdjustmentPrecise(&adj, &inc, &disabled)` returns the "adjusted clock update
//! frequency" (`adj`) and the "clock update frequency" (`inc`), both in QPC-count units for the
//! Precise API (Microsoft's SetSystemTimeAdjustmentPrecise sample adjusts by
//! `ppm * QPCfreq / 1e6` units). A LARGER `adj` SLOWS the time-of-day clock. This was measured
//! 1:1 live on win-resolume (issue 1372 comment 5832932907), and dantesync steers with
//! `new_adj = inc - ppm * freq / 1e6`. So:
//!
//! ```text
//! system-time rate / QPC rate = inc / adj
//! ```
//!
//! Disabled adjustment, a zero value or a failed read → rate `1 / 1` (stock QPC behaviour).
//! `|adj - inc|` is clamped to `inc / RATE_CLAMP_DIV` (1000 ppm), above dantesync's 500 ppm
//! drift ceiling plus its phase slew.
//!
//! ## The integrator
//!
//! A [`Segment`] is `(base_qpc, base_ns, rate)`:
//!
//! ```text
//! now = base_ns + floor(floor((qpc - base_qpc) * 1e9 / freq) * num / den)
//! ```
//!
//! - Integer only, no floating accumulation.
//! - A rate CHANGE rebases the segment at the current value. Time is therefore continuous and
//!   monotonic across the change, and the floor loses < 1 ns per dantesync update.
//! - The rate is re-read when `qpc - last_poll >= freq / POLL_DIV` (250 ms).
//! - An NTP date step changes system TIME, never the adjustment rate, so it never reaches here.
//! - The first value equals the old raw-QPC nanoseconds, so startup is unchanged.
//!
//! Pure `std`, no `crate::` imports — standalone-rustc Tier-0 testable.

/// Nanoseconds per second.
pub const NS_PER_SEC: u64 = 1_000_000_000;

/// The adjustment is re-read at most every `freq / POLL_DIV` QPC counts (250 ms).
/// Mirrors `OS_CLK_POLL_DIV` in platform-windows.c.
pub const POLL_DIV: u64 = 4;

/// `|adj - inc|` is clamped to `inc / RATE_CLAMP_DIV` (1000 ppm).
/// Mirrors `OS_CLK_RATE_CLAMP_DIV` in platform-windows.c.
pub const RATE_CLAMP_DIV: u64 = 1000;

/// `floor(num * mul / div)`: what libobs `util_mul_div64` computes (exact 128-bit on MSVC x64;
/// the portable `(num / div) * mul + (rem * mul) / div` is the same floor while nothing
/// overflows). Truncated to 64 bits like the C.
pub fn mul_div64(num: u64, mul: u64, div: u64) -> u64 {
    ((num as u128 * mul as u128) / div as u128) as u64
}

/// The system-time rate relative to QPC as `(num, den)` = `(inc, clamped adj)`, or `(1, 1)`
/// when the adjustment is disabled, unreadable (both passed as 0) or degenerate.
pub fn rate_from_adjustment(adj: u64, inc: u64, disabled: bool) -> (u64, u64) {
    if disabled || adj == 0 || inc == 0 {
        return (1, 1);
    }
    let bound = inc / RATE_CLAMP_DIV;
    let adj = adj.clamp(inc - bound, inc + bound);
    (inc, adj)
}

/// One constant-rate stretch of the disciplined clock. `rate_den == 0` = not initialised yet
/// (the C zero-initialised static).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub base_qpc: u64,
    pub base_ns: u64,
    pub rate_num: u64,
    pub rate_den: u64,
    pub last_poll_qpc: u64,
}

impl Segment {
    /// The disciplined nanoseconds at `qpc`. A `qpc` older than the base reads the base value
    /// (never earlier), so a stale counter read can never step the clock backwards.
    pub fn now(&self, qpc: u64, freq: u64) -> u64 {
        let dqpc = qpc.saturating_sub(self.base_qpc);
        let raw_ns = mul_div64(dqpc, NS_PER_SEC, freq);
        self.base_ns + mul_div64(raw_ns, self.rate_num, self.rate_den)
    }

    /// True when the adjustment must be (re-)read: never initialised, or at least
    /// `freq / POLL_DIV` counts since the last poll.
    pub fn poll_due(&self, qpc: u64, freq: u64) -> bool {
        if self.rate_den == 0 {
            return true;
        }
        qpc > self.last_poll_qpc && qpc - self.last_poll_qpc >= freq / POLL_DIV
    }

    /// Apply a freshly read rate at `qpc`. The first call initialises the segment at the raw
    /// QPC nanoseconds. A changed rate rebases at the current value. An unchanged rate only
    /// records the poll.
    pub fn update(&mut self, qpc: u64, freq: u64, num: u64, den: u64) {
        if self.rate_den == 0 {
            self.base_qpc = qpc;
            self.base_ns = mul_div64(qpc, NS_PER_SEC, freq);
            self.rate_num = num;
            self.rate_den = den;
        } else if num != self.rate_num || den != self.rate_den {
            self.base_ns = self.now(qpc, freq);
            if qpc > self.base_qpc {
                self.base_qpc = qpc;
            }
            self.rate_num = num;
            self.rate_den = den;
        }
        if qpc > self.last_poll_qpc {
            self.last_poll_qpc = qpc;
        }
    }
}

/// Map a RAW-QPC timestamp (ns on the raw QPC timeline, e.g. WASAPI `qpcPosition * 100`) onto the
/// disciplined clock: measure its age on raw QPC (`raw_now_ns - raw_ts_ns`) and subtract it from the
/// disciplined `now_ns` (a future stamp adds). Over an age of milliseconds the rate difference is
/// below a microsecond. Mirrors `os_qpc_ns_map_to_gettime_ns` in
/// `vendor/obs-studio/libobs/util/windows/qpc-timestamp.h`.
pub fn map_raw_qpc_ns(raw_ts_ns: u64, raw_now_ns: u64, now_ns: u64) -> u64 {
    if raw_ts_ns >= raw_now_ns {
        return now_ns + (raw_ts_ns - raw_now_ns);
    }
    let age_ns = raw_now_ns - raw_ts_ns;
    now_ns.saturating_sub(age_ns)
}

/// The single-thread model of the C `os_gettime_ns()`: read the segment, poll the adjustment
/// only when due, return the disciplined time. `adjustment` is what
/// `GetSystemTimeAdjustmentPrecise` would return right now, `(adj, inc, disabled)`, and
/// `None` models a missing or failing API.
#[derive(Clone, Copy, Debug)]
pub struct DisciplinedClock {
    pub freq: u64,
    pub seg: Segment,
}

impl DisciplinedClock {
    pub fn new(freq: u64) -> Self {
        Self {
            freq,
            seg: Segment::default(),
        }
    }

    pub fn now(&mut self, qpc: u64, adjustment: Option<(u64, u64, bool)>) -> u64 {
        if !self.seg.poll_due(qpc, self.freq) {
            return self.seg.now(qpc, self.freq);
        }
        let (adj, inc, disabled) = adjustment.unwrap_or((0, 0, true));
        let (num, den) = rate_from_adjustment(adj, inc, disabled);
        self.seg.update(qpc, self.freq, num, den);
        self.seg.now(qpc, self.freq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: u64 = 10_000_000; // the Windows 10+ QPC frequency (win-resolume)

    #[test]
    fn a_larger_adjustment_slows_the_clock() {
        // Live win-resolume: adj 9_999_809 against inc 10_000_000 → system time ran +19.1 ppm.
        let (num, den) = rate_from_adjustment(9_999_809, 10_000_000, false);
        assert_eq!((num, den), (10_000_000, 9_999_809));
        let mut c = DisciplinedClock::new(F);
        let a = c.now(0, Some((9_999_809, F, false)));
        let b = c.now(60 * F, Some((9_999_809, F, false)));
        let ppm = ((b - a) as f64 - 60e9) / 60e9 * 1e6;
        assert!((ppm - 19.1).abs() < 0.01, "expected ≈ +19.1 ppm, got {ppm}");
    }

    #[test]
    fn disabled_missing_or_degenerate_adjustment_is_stock_qpc() {
        assert_eq!(rate_from_adjustment(9_999_000, F, true), (1, 1));
        assert_eq!(rate_from_adjustment(0, F, false), (1, 1));
        assert_eq!(rate_from_adjustment(F, 0, false), (1, 1));
        let mut c = DisciplinedClock::new(F);
        for q in [5 * F, 5 * F + 1, 3600 * F + 7] {
            assert_eq!(c.now(q, None), mul_div64(q, NS_PER_SEC, F));
        }
    }

    #[test]
    fn an_insane_adjustment_is_clamped_to_1000_ppm() {
        assert_eq!(rate_from_adjustment(1, F, false), (F, F - F / 1000));
        assert_eq!(rate_from_adjustment(u64::MAX, F, false), (F, F + F / 1000));
        assert_eq!(
            rate_from_adjustment(F + F / 1000, F, false),
            (F, F + F / 1000)
        );
    }

    #[test]
    fn first_value_equals_the_old_raw_qpc_nanoseconds() {
        let mut c = DisciplinedClock::new(F);
        let q = 123_456_789_012;
        assert_eq!(
            c.now(q, Some((9_999_900, F, false))),
            mul_div64(q, NS_PER_SEC, F)
        );
    }

    #[test]
    fn a_rate_change_mid_interval_is_continuous_and_monotonic() {
        let mut c = DisciplinedClock::new(F);
        let adj_a = Some((9_999_900, F, false)); // +10 ppm
        let adj_b = Some((10_000_100, F, false)); // -10 ppm
        let mut prev = c.now(1_000 * F, adj_a);
        let mut q = 1_000 * F;
        let mut rebases = 0;
        // Read every 1 ms for 3 s; the rate flips every 0.6 s, i.e. between polls.
        for i in 1..=3000u64 {
            q += F / 1000;
            let adj = if (i / 600) % 2 == 0 { adj_a } else { adj_b };
            let before = c.seg;
            let t = c.now(q, adj);
            assert!(t >= prev, "clock stepped back at read {i}: {prev} -> {t}");
            let rebased = before.rate_den != 0
                && (c.seg.rate_num != before.rate_num || c.seg.rate_den != before.rate_den);
            if rebased {
                // At the rebase instant the value is exactly the old segment's value.
                assert_eq!(t, before.now(q, F), "rebase jumped at read {i}");
                rebases += 1;
            }
            prev = t;
        }
        assert!(
            rebases >= 4,
            "the scenario must exercise rebases, saw {rebases}"
        );
    }

    #[test]
    fn the_rate_is_only_re_read_every_250_ms() {
        let mut c = DisciplinedClock::new(F);
        c.now(0, Some((F, F, false)));
        // A new adjustment appears at once, but is not applied before freq/4 counts.
        c.now(F / 4 - 1, Some((9_999_000, F, false)));
        assert_eq!((c.seg.rate_num, c.seg.rate_den), (F, F));
        c.now(F / 4, Some((9_999_000, F, false)));
        assert_eq!((c.seg.rate_num, c.seg.rate_den), (F, 9_999_000));
        assert_eq!(c.seg.base_qpc, F / 4);
    }

    #[test]
    fn a_stale_qpc_never_steps_back() {
        let mut c = DisciplinedClock::new(F);
        c.now(10 * F, Some((9_999_000, F, false)));
        c.now(11 * F, Some((10_001_000, F, false))); // rebase at 11 s
        let base = c.seg.base_ns;
        assert_eq!(c.seg.now(11 * F - 5, F), base);
    }

    #[test]
    fn a_raw_qpc_timestamp_maps_by_its_age() {
        // The disciplined clock is 100 ms ahead of raw QPC; a stamp 10 ms old on raw QPC is 10 ms
        // before the disciplined now, not 110 ms before it.
        let raw_now = 1_000_000_000_000;
        let now = raw_now + 100_000_000;
        assert_eq!(
            map_raw_qpc_ns(raw_now - 10_000_000, raw_now, now),
            now - 10_000_000
        );
        assert_eq!(map_raw_qpc_ns(raw_now + 5, raw_now, now), now + 5);
        assert_eq!(map_raw_qpc_ns(0, raw_now, 7), 0);
    }

    #[test]
    fn integer_math_holds_over_a_long_segment() {
        // One segment for ten days at -7 ppm: the result is the exact floor, no accumulation.
        let mut c = DisciplinedClock::new(F);
        c.now(0, Some((10_000_070, F, false)));
        let q = 10 * 86_400 * F;
        let got = c.seg.now(q, F);
        let exact = (q as u128 * 1_000_000_000 / F as u128) * F as u128 / 10_000_070;
        assert_eq!(got as u128, exact);
    }
}
