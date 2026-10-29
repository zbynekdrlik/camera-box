//! Issue 1387 (owner 29.10.2026: "nech aj nieco znesie a vie sa obnovit"): the RECEIVER's early
//! congestion signal, pure. Fed every finished frame, drained once per report period into the two
//! version-2 report fields the sender's controller (`crate::jxs_congestion`) reads BEFORE any loss:
//!
//! - **The queuing delay.** Per frame `d` = the first datagram's arrival (receiver clock) minus the
//!   frame's genlock timecode (sender clock). `d` = the sender's encode-to-first-slice time + the
//!   path delay + the clock offset of the two boxes. Only the path's QUEUE moves it: the offset is
//!   constant (both boxes run the dantesync-disciplined clock) and cancels against its own minimum.
//!   So the report carries the period's p50 of `d` minus the minimum over the last
//!   [`BASE_PERIODS`] periods: a growing switch queue reads as a growing delay, the WebRTC GCC /
//!   L4S principle (delay before loss), with no extra datagram and no send timestamp on the wire.
//! - **The arrival span.** The p50 of last-minus-first arrival over the period's WHOLE frames (no
//!   lost slice): a bottleneck that delivers slower than the sender sends stretches it; the sender
//!   compares it with its own send span of the same frames.
//!
//! A period whose p50 lies more than [`STEP_RESET_100NS`] above the base is a clock step (the
//! nightly dantesync date step is ~200 ms; a real switch buffer at 1 GbE holds tens of ms): the base
//! restarts from that period and the delay is not reported for it. A period with no frame reports
//! neither field.

/// Periods the minimum (the "no queue" base) is taken over: 30 s at one report a second. Longer
/// than any adaptation (the sender reacts within ~2 s), so a queue that persists while the sender
/// still adapts never becomes the base.
pub const BASE_PERIODS: usize = 30;
/// A p50 this far above the base is a clock step, not a queue (100 ms in 100 ns units).
pub const STEP_RESET_100NS: i64 = 1_000_000;

/// The meter (one camera).
#[derive(Debug, Default, Clone)]
pub struct CongestionMeter {
    /// This period's `d` values (100 ns) and whole-frame spans (100 ns).
    delays: Vec<i64>,
    spans: Vec<i64>,
    /// The per-period minima of `d`, newest last, at most [`BASE_PERIODS`].
    minima: std::collections::VecDeque<i64>,
}

/// One period's two report fields (microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CongestionSample {
    pub queue_delay_us: Option<u32>,
    pub span_p50_us: Option<u32>,
}

fn p50(v: &mut [i64]) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[(v.len() - 1) / 2])
}

fn to_us(v_100ns: i64) -> u32 {
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
        let _ = (p50(&mut []), to_us(0), &self.minima, STEP_RESET_100NS);
        self.delays.clear();
        self.spans.clear();
        CongestionSample::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TC0: u64 = 17_617_300_000_000_000;
    /// The receiver clock 3 s ahead of the sender's: the offset must cancel.
    const OFFSET: i64 = 30_000_000;

    fn period(m: &mut CongestionMeter, k0: u64, extra_100ns: i64, span: i64) -> CongestionSample {
        for k in 0..30u64 {
            let tc = TC0 + (k0 + k) * 333_333;
            let first = tc as i64 + OFFSET + 40_000 + extra_100ns + (k as i64 % 3) * 1_000;
            m.note_frame(tc, first, first + span, 0);
        }
        m.take()
    }

    #[test]
    fn a_steady_path_reads_no_queue_whatever_the_clock_offset() {
        let mut m = CongestionMeter::default();
        for p in 0..5 {
            let s = period(&mut m, p * 30, 0, 180_000);
            assert_eq!(s.span_p50_us, Some(18_000));
            assert!(s.queue_delay_us.unwrap() <= 100, "{s:?}");
        }
    }

    #[test]
    fn a_growing_queue_reads_as_delay_above_the_old_minimum() {
        let mut m = CongestionMeter::default();
        for p in 0..5 {
            period(&mut m, p * 30, 0, 180_000);
        }
        // the switch queue holds 6 ms more: the report says ~6 ms before anything is lost
        let s = period(&mut m, 150, 60_000, 230_000);
        assert_eq!(s.queue_delay_us, Some(6_100));
        assert_eq!(s.span_p50_us, Some(23_000));
        // it stays measured against the quiet minimum while the queue persists
        let s = period(&mut m, 180, 60_000, 230_000);
        assert_eq!(s.queue_delay_us, Some(6_100));
    }

    #[test]
    fn a_queue_older_than_the_base_window_becomes_the_new_base() {
        let mut m = CongestionMeter::default();
        period(&mut m, 0, 0, 180_000);
        for p in 1..=BASE_PERIODS as u64 + 1 {
            period(&mut m, p * 30, 50_000, 180_000);
        }
        let s = period(&mut m, 40 * 30, 50_000, 180_000);
        assert!(s.queue_delay_us.unwrap() <= 100, "{s:?}");
    }

    #[test]
    fn a_clock_step_restarts_the_base_and_is_never_a_queue() {
        let mut m = CongestionMeter::default();
        period(&mut m, 0, 0, 180_000);
        // the nightly date step: +200 ms on the receiver's reading
        let s = period(&mut m, 30, 2_000_000, 180_000);
        assert_eq!(s.queue_delay_us, None, "a step, not a queue");
        let s = period(&mut m, 60, 2_000_000, 180_000);
        assert!(s.queue_delay_us.unwrap() <= 100, "the new base: {s:?}");
        // a step back reads as no queue at once (a new minimum)
        let s = period(&mut m, 90, 0, 180_000);
        assert!(s.queue_delay_us.unwrap() <= 100, "{s:?}");
    }

    #[test]
    fn lossy_frames_count_for_the_delay_not_the_span_and_an_empty_period_reports_nothing() {
        let mut m = CongestionMeter::default();
        m.note_frame(TC0, TC0 as i64 + 40_000, TC0 as i64 + 90_000, 2);
        let s = m.take();
        assert_eq!(s.span_p50_us, None);
        assert_eq!(s.queue_delay_us, Some(0));
        assert_eq!(m.take(), CongestionSample::default());
        m.note_frame(TC0, 0, 0, 69);
        assert_eq!(m.take(), CongestionSample::default(), "no arrival, no data");
    }
}
