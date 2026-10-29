//! Issue 1387 (owner 29.10.2026: "nech aj nieco znesie a vie sa obnovit"): the RECEIVER's half of
//! the early congestion signal, pure. Fed every finished frame, drained once per report period
//! into the version-2 report fields the sender's controller (`crate::jxs_congestion`) reads BEFORE
//! any loss:
//!
//! - **The delay p50.** Per frame `d` = the first datagram's arrival (receiver clock) minus the
//!   frame's genlock timecode (sender clock); the period's p50 in microseconds. `d` holds the
//!   sender's own delay before its first send (on a cambox the capture's PHASE in its emit slot,
//!   0..16.7 ms and drifting, plus the encode), the path delay, and the two clocks' offset. Only the
//!   SENDER knows its own part, so the sender subtracts it and keeps the "no queue" base
//!   (`crate::jxs_congestion::PathDelay`); the receiver only measures.
//! - **The delay spread.** p90 - p10 of the same `d`: a clock step (a dantesync correction on one
//!   box) moves every frame alike and leaves it small, a filling queue does not.
//! - **The arrival span.** The p50 of last-minus-first arrival over the period's WHOLE frames (no
//!   lost slice): a bottleneck that delivers slower than the sender sends stretches it.
//!
//! A period with no frame reports none of them.
//!
//! **Wall steps never read as delay.** dantesync STEPS a box's wall clock now and then (a
//! correction, the nightly date step). Both ends measure on the wall clock minus its own steps
//! ([`WallSteps`]: `CLOCK_REALTIME - CLOCK_MONOTONIC` moves only on a step, both share every rate
//! adjustment), so a step on the receiver leaves the arrival times continuous, and a step on the
//! sender shifts its timecodes and its own first-send delay alike (cancelled by the subtraction).

/// The wall clock's steps since this was made (module doc), 100 ns.
#[derive(Debug, Clone, Copy)]
pub struct WallSteps {
    offset0: i64,
}

fn realtime_minus_monotonic_100ns() -> i64 {
    let read = |id| {
        let mut t = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `t` is a valid out-pointer; both clock ids exist on every Linux.
        unsafe { libc::clock_gettime(id, &mut t) };
        t.tv_sec * 10_000_000 + t.tv_nsec / 100
    };
    read(libc::CLOCK_REALTIME) - read(libc::CLOCK_MONOTONIC)
}

impl Default for WallSteps {
    fn default() -> Self {
        Self {
            offset0: realtime_minus_monotonic_100ns(),
        }
    }
}

impl WallSteps {
    /// The wall clock's net step since [`Self::default`] (100 ns, signed).
    pub fn steps_100ns(&self) -> i64 {
        realtime_minus_monotonic_100ns() - self.offset0
    }
}

/// The meter (one camera).
#[derive(Debug, Default, Clone)]
pub struct CongestionMeter {
    /// This period's `d` values and whole-frame spans (100 ns).
    delays: Vec<i64>,
    spans: Vec<i64>,
}

/// One period's report fields (microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CongestionSample {
    pub delay_p50_us: Option<i32>,
    pub delay_spread_us: Option<u32>,
    pub span_p50_us: Option<u32>,
}

/// Nearest-rank percentile of a sorted, non-empty slice.
fn rank(sorted: &[i64], p: usize) -> i64 {
    let n = sorted.len();
    sorted[((n * p).div_ceil(100)).clamp(1, n) - 1]
}

fn to_u32_us(v_100ns: i64) -> u32 {
    (v_100ns.max(0) / 10).min(u32::MAX as i64 - 1) as u32
}

impl CongestionMeter {
    /// One finished frame: its timecode, its first / last datagram arrival (all 100 ns) and its
    /// lost slices. A frame with no arrival (`first == 0`) says nothing.
    pub fn note_frame(&mut self, timecode_100ns: u64, first: i64, last: i64, lost: u32) {
        if first <= 0 || timecode_100ns == 0 {
            return;
        }
        self.delays
            .push(first.saturating_sub(timecode_100ns.min(i64::MAX as u64) as i64));
        if lost == 0 && last >= first {
            self.spans.push(last - first);
        }
    }

    /// The period just ended (module doc), then a fresh period.
    pub fn take(&mut self) -> CongestionSample {
        let mut out = CongestionSample::default();
        if !self.spans.is_empty() {
            self.spans.sort_unstable();
            out.span_p50_us = Some(to_u32_us(rank(&self.spans, 50)));
        }
        if !self.delays.is_empty() {
            self.delays.sort_unstable();
            let p50 = rank(&self.delays, 50) / 10;
            out.delay_p50_us = Some(p50.clamp(i32::MIN as i64 + 1, i32::MAX as i64) as i32);
            out.delay_spread_us = Some(to_u32_us(rank(&self.delays, 90) - rank(&self.delays, 10)));
        }
        self.delays.clear();
        self.spans.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TC0: u64 = 17_617_300_000_000_000;

    #[test]
    fn a_period_reports_its_delay_p50_spread_and_whole_frame_span() {
        let mut m = CongestionMeter::default();
        // the receiver clock 3 s ahead: the delay carries the offset, the sender removes it
        for k in 0..30u64 {
            let tc = TC0 + k * 333_333;
            let first = tc as i64 + 30_000_000 + 40_000 + (k as i64 % 10) * 1_000;
            m.note_frame(tc, first, first + 180_000, 0);
        }
        let s = m.take();
        assert_eq!(s.span_p50_us, Some(18_000));
        // delays 40.0..40.9 ms above the offset: p50 = 3_004_400 us, p10..p90 = 0.8 ms
        assert_eq!(s.delay_p50_us, Some(3_004_400));
        assert_eq!(s.delay_spread_us, Some(800));
        assert_eq!(m.take(), CongestionSample::default(), "a fresh period");
    }

    #[test]
    fn lossy_frames_count_for_the_delay_not_the_span_and_no_arrival_says_nothing() {
        let mut m = CongestionMeter::default();
        m.note_frame(TC0, TC0 as i64 + 40_000, TC0 as i64 + 90_000, 2);
        let s = m.take();
        assert_eq!(s.span_p50_us, None);
        assert_eq!((s.delay_p50_us, s.delay_spread_us), (Some(4_000), Some(0)));
        m.note_frame(TC0, 0, 0, 69);
        assert_eq!(m.take(), CongestionSample::default());
    }

    #[test]
    fn the_wall_has_not_stepped_since_it_was_read() {
        let w = WallSteps::default();
        assert!(w.steps_100ns().abs() < 10_000, "{}", w.steps_100ns());
    }

    #[test]
    fn a_receiver_clock_behind_the_sender_gives_a_negative_delay() {
        let mut m = CongestionMeter::default();
        m.note_frame(TC0, TC0 as i64 - 20_000, TC0 as i64, 0);
        assert_eq!(m.take().delay_p50_us, Some(-2_000));
    }
}
