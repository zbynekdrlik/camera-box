//! Issue 1387: the JPEG XS send pacing schedule, pure (no clock, no socket).
//!
//! The strih program output sends on a 10 GbE NIC to stream-snv on a 1 GbE port. Sent at line rate,
//! each slice reaches the switch as a 10 Gb/s burst. The switch buffer at the 1 GbE port drops the
//! tail of that burst: the live slice-3 run lost 1-15 slices in 10 of 21 windows, with no drop on
//! the receiving host. A pace rate spreads the sends so the slower link is never offered more than
//! it carries.
//!
//! The mechanism is in the application, not the kernel. `SO_MAX_PACING_RATE` is enforced only by
//! the `fq` qdisc (and by TCP's own pacing, which UDP lacks); strih-snv runs `fq_codel`, which
//! ignores it. `SO_TXTIME` needs an `etf` qdisc. So the sending thread holds each send until its
//! departure time from this schedule.
//!
//! - **Wire bytes.** A send is counted as it occupies the slower link: every datagram's payload plus
//!   [`WIRE_OVERHEAD_BYTES`] (UDP 8 + IPv4 20 + Ethernet 14 + FCS 4 + preamble 8 + inter-frame gap
//!   12). So `pace_mbps` is the Ethernet line rate the flow takes, comparable with the port speed.
//! - **Exact, no drift.** The schedule keeps an anchor instant and the bits sent since it; a send's
//!   departure is `anchor + ceil(bits x 1000 / mbps)` ns. Nothing is rounded per send, so a
//!   million sends land exactly where one long send would.
//! - **Late is not early.** A send that is ready after its due time (the encoder was slower than
//!   the pace, or the thread woke late) goes at once. Up to HALF a burst's wire time of lateness
//!   is kept as credit, so a thread that wakes 50 us late catches up and the rate holds; anything
//!   beyond half a burst moves the schedule later (lost, never caught up). Later than one burst
//!   ([`PaceSchedule::tolerance_ns`]) the link was idle: the schedule restarts at that instant (a
//!   new anchor).
//! - **Bounded bursts.** While pacing, one send carries at most [`PACE_BURST_SEGMENTS`] datagrams
//!   (~23.5 KB of wire, ~0.2 ms at 900 Mb/s); a longer slice is split into such groups
//!   ([`send_ranges`]). Each group still goes out at line rate, and the credit above lets at most
//!   half a burst more follow it back to back: the switch never sees more than 1.5 groups
//!   (~37 KB) arrive faster than the pace.

use super::{DATAGRAM_LEN, GSO_SEGMENTS_PER_SEND};

/// Wire bytes per datagram beyond its UDP payload: UDP 8 + IPv4 20 + Ethernet 14 + FCS 4 +
/// preamble/SFD 8 + inter-frame gap 12.
pub const WIRE_OVERHEAD_BYTES: u64 = 66;
/// Datagrams in one paced send (one GSO `sendmsg`): 16 x 1538 = 24608 wire bytes, 0.22 ms at
/// 900 Mb/s. Small enough that a 1 GbE port's buffer absorbs one group arriving at 10 Gb/s, large
/// enough that a 1080p frame needs only a few hundred sends.
pub const PACE_BURST_SEGMENTS: usize = 16;
const _: () = assert!(PACE_BURST_SEGMENTS <= GSO_SEGMENTS_PER_SEND);
/// The lowest pace a sender takes: 4 bpp at 30 fps already needs ~280 Mb/s of wire, so anything
/// lower only fills the queue.
pub const PACE_MBPS_MIN: u32 = 100;
/// The highest pace a sender takes (100 GbE).
pub const PACE_MBPS_MAX: u32 = 100_000;

/// The wire bytes of one send of `payload_len` bytes of consecutive datagrams (built by
/// `build_slice_datagrams`: every datagram [`DATAGRAM_LEN`] long except the last).
pub fn wire_bytes(payload_len: usize) -> u64 {
    payload_len as u64 + payload_len.div_ceil(DATAGRAM_LEN) as u64 * WIRE_OVERHEAD_BYTES
}

/// Nanoseconds `wire_bytes` occupy at `mbps`, rounded up (`mbps` > 0).
pub fn wire_ns(wire_bytes: u64, mbps: u32) -> u64 {
    let bits_x1000 = wire_bytes as u128 * 8 * 1000;
    bits_x1000.div_ceil(mbps.max(1) as u128) as u64
}

/// A pace setting: 0 = automatic (`super::pace_auto`), else [`PACE_MBPS_MIN`]..=[`PACE_MBPS_MAX`].
pub fn check_pace_mbps(mbps: u32) -> Result<u32, String> {
    if mbps == 0 || (PACE_MBPS_MIN..=PACE_MBPS_MAX).contains(&mbps) {
        Ok(mbps)
    } else {
        Err(format!(
            "pace {mbps} Mb/s is not 0 (automatic) or {PACE_MBPS_MIN}..={PACE_MBPS_MAX}"
        ))
    }
}

/// The byte ranges of one slice buffer for successive sends of at most `segments` datagrams
/// (each range starts on a datagram boundary, so the kernel's GSO cut lines up with the headers).
pub fn send_ranges(
    buf_len: usize,
    segments: usize,
) -> impl Iterator<Item = std::ops::Range<usize>> {
    let step = segments.max(1) * DATAGRAM_LEN;
    (0..buf_len)
        .step_by(step)
        .map(move |start| start..(start + step).min(buf_len))
}

/// Datagrams per send: [`PACE_BURST_SEGMENTS`] while pacing, else [`GSO_SEGMENTS_PER_SEND`].
pub fn segments_per_send(pacing: bool) -> usize {
    if pacing {
        PACE_BURST_SEGMENTS
    } else {
        GSO_SEGMENTS_PER_SEND
    }
}

/// The auto bpp's encode time of a frame: submit -> last packet sent (`encode_ms`) minus what the
/// pace held its sends back (`paced_ns`). The pace is a wire limit, not a codec cost; without this
/// a lower pace would quietly cap the climb (the climb needs encode p99 < 60 % of 33.3 ms, and at
/// 800 Mb/s 7 bpp alone is ~19 ms of wire). A pace too slow for the frame rate shows as
/// `dropped_busy`, which steps the bpp down by itself.
pub fn codec_ms(encode_ms: f64, paced_ns: u64) -> f64 {
    (encode_ms - paced_ns as f64 / 1e6).max(0.0)
}

/// The running send schedule of one paced link (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaceSchedule {
    mbps: u32,
    /// The instant the current run of back-to-back sends started (ns on the caller's clock).
    anchor_ns: u64,
    /// Bits scheduled since the anchor.
    bits: u128,
    started: bool,
}

impl PaceSchedule {
    /// A schedule at `mbps`; `None` for 0 (line rate: no schedule).
    pub fn new(mbps: u32) -> Option<Self> {
        (mbps > 0).then_some(Self {
            mbps,
            anchor_ns: 0,
            bits: 0,
            started: false,
        })
    }

    pub fn mbps(&self) -> u32 {
        self.mbps
    }

    /// How late a send may be and still keep the schedule: one full burst's wire time.
    pub fn tolerance_ns(&self) -> u64 {
        wire_ns(
            PACE_BURST_SEGMENTS as u64 * (DATAGRAM_LEN as u64 + WIRE_OVERHEAD_BYTES),
            self.mbps,
        )
    }

    /// When the next send is due (`None` before the first send).
    pub fn due_ns(&self) -> Option<u64> {
        self.started
            .then(|| self.anchor_ns + (self.bits * 1000).div_ceil(self.mbps as u128) as u64)
    }

    /// Schedule one send of `wire_bytes` that is ready at `ready_ns`; returns its departure time
    /// (never before `ready_ns`).
    pub fn depart(&mut self, ready_ns: u64, wire_bytes: u64) -> u64 {
        let tolerance = self.tolerance_ns();
        let depart = match self.due_ns() {
            Some(due) if ready_ns <= due => due,
            Some(due) if ready_ns - due <= tolerance => {
                // late by `lag`: keep at most half a burst of it as credit, so the sends that
                // follow catch up by no more than half a burst back to back
                let lag = ready_ns - due;
                self.anchor_ns += lag.saturating_sub(tolerance / 2);
                ready_ns
            }
            _ => {
                // the first send, or the link was idle: a new run starts now
                self.anchor_ns = ready_ns;
                self.bits = 0;
                self.started = true;
                ready_ns
            }
        };
        self.bits += wire_bytes as u128 * 8;
        depart
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jxs_link::{build_slice_datagrams, packets_per_frame, PacketHeader};

    #[test]
    fn wire_bytes_add_the_ethernet_overhead_per_datagram() {
        assert_eq!(wire_bytes(0), 0);
        assert_eq!(wire_bytes(DATAGRAM_LEN), 1538);
        assert_eq!(
            wire_bytes(DATAGRAM_LEN + 1),
            DATAGRAM_LEN as u64 + 1 + 2 * 66
        );
        assert_eq!(wire_bytes(16 * DATAGRAM_LEN), 16 * 1538);
        assert_eq!(wire_ns(1538, 1000), 12_304);
        assert_eq!(
            wire_ns(1538, 900),
            13_672,
            "12304000 / 900 = 13671.1, rounded up"
        );
        assert_eq!(wire_ns(125, 1), 1_000_000, "1000 bits at 1 Mb/s = 1 ms");
    }

    #[test]
    fn the_auto_bpp_sees_the_codec_time_without_the_paced_wait() {
        assert_eq!(codec_ms(19.5, 0), 19.5, "line rate: nothing taken off");
        assert!((codec_ms(19.5, 17_000_000) - 2.5).abs() < 1e-9);
        assert_eq!(codec_ms(1.0, 5_000_000), 0.0, "never negative");
    }

    /// Live 31.10.2026 (strih-snv program link, 8 bpp into stream's 1 GbE port): the codec
    /// finishes a frame in ~10 ms (`jxs-encode-bench` p99 10.44 ms on the box), but the sender
    /// spends ~20 ms pushing its 2.25 MB out, part of it in send calls rather than pace sleeps,
    /// and `encode_ms - paced_ns` read ~20 ms, so the auto bpp never found the 60 % headroom to
    /// climb back to 8. The codec time is the time to the first packet plus the time the sender
    /// WAITED for the encoder between packets; its own send work never counts.
    #[test]
    fn the_codec_time_counts_only_the_waits_for_the_encoder_never_the_send_work() {
        let ms = 1_000_000u64;
        let mut c = CodecClock::default();
        // first packet 1 ms after the submit, then 30 packets the encoder already holds: each
        // send takes 0.7 ms of the sender's own work, no wait in between
        let mut t = ms;
        for _ in 0..30 {
            c.enter(t);
            t += 700_000;
            c.exit(t);
        }
        let v = c.codec_ms().expect("packets seen");
        assert!((v - 1.0).abs() < 1e-9, "21 ms of sends, codec 1 ms: {v}");

        // an encoder slower than the link: 2 ms waits between packets are codec time
        let mut c = CodecClock::default();
        let mut t = ms;
        for _ in 0..10 {
            c.enter(t);
            t += 100_000;
            c.exit(t);
            t += 2 * ms;
        }
        let v = c.codec_ms().unwrap();
        assert!(
            (v - 19.0).abs() < 1e-9,
            "1 ms to the first + 9 waits of 2 ms: {v}"
        );

        assert_eq!(
            CodecClock::default().codec_ms(),
            None,
            "no packet: nothing measured"
        );
    }

    #[test]
    fn the_pace_setting_is_line_rate_or_a_bounded_rate() {
        assert_eq!(check_pace_mbps(0), Ok(0));
        assert_eq!(check_pace_mbps(900), Ok(900));
        assert_eq!(check_pace_mbps(PACE_MBPS_MIN), Ok(PACE_MBPS_MIN));
        assert_eq!(check_pace_mbps(PACE_MBPS_MAX), Ok(PACE_MBPS_MAX));
        assert!(check_pace_mbps(99).unwrap_err().contains("pace 99"));
        assert!(check_pace_mbps(100_001).is_err());
        assert!(PaceSchedule::new(0).is_none(), "0 = line rate, no schedule");
        assert_eq!(PaceSchedule::new(900).map(|p| p.mbps()), Some(900));
    }

    #[test]
    fn a_paced_send_is_cut_into_burst_groups_on_datagram_boundaries() {
        let len = 46 * DATAGRAM_LEN + 100;
        let r: Vec<_> = send_ranges(len, segments_per_send(true)).collect();
        assert_eq!(
            r,
            vec![
                0..16 * DATAGRAM_LEN,
                16 * DATAGRAM_LEN..32 * DATAGRAM_LEN,
                32 * DATAGRAM_LEN..len
            ]
        );
        let r: Vec<_> = send_ranges(len, segments_per_send(false)).collect();
        assert_eq!(r, vec![0..40 * DATAGRAM_LEN, 40 * DATAGRAM_LEN..len]);
        assert_eq!(send_ranges(0, 16).count(), 0);
    }

    #[test]
    fn sends_ready_together_leave_one_wire_time_apart() {
        let mut p = PaceSchedule::new(1000).unwrap();
        let unit = 16 * 1538;
        assert_eq!(p.due_ns(), None);
        assert_eq!(p.depart(5_000, unit), 5_000, "the first send goes at once");
        assert_eq!(p.depart(5_000, unit), 5_000 + 196_864);
        assert_eq!(p.depart(5_000, unit), 5_000 + 2 * 196_864);
        assert_eq!(p.due_ns(), Some(5_000 + 3 * 196_864));
        // a send ready after its due time (within one burst) goes when ready, and the schedule
        // holds: the next one is due where it always was
        assert_eq!(
            p.depart(5_000 + 3 * 196_864 + 40_000, unit),
            5_000 + 3 * 196_864 + 40_000
        );
        assert_eq!(p.depart(0, unit), 5_000 + 4 * 196_864);
    }

    #[test]
    fn a_send_after_an_idle_gap_starts_a_new_run_with_no_saved_credit() {
        let mut p = PaceSchedule::new(1000).unwrap();
        let unit = 16 * 1538;
        p.depart(0, unit);
        assert_eq!(p.tolerance_ns(), 196_864);
        let late = 196_864 + 196_865; // one past due + tolerance
        assert_eq!(p.depart(late, unit), late);
        assert_eq!(
            p.depart(late, unit),
            late + 196_864,
            "paced from the new anchor"
        );
        // exactly at due + tolerance: still the same run, but only half a burst of the lateness
        // is kept as credit, so the next send follows half a burst later, never back to back
        let mut q = PaceSchedule::new(1000).unwrap();
        q.depart(0, unit);
        assert_eq!(q.depart(2 * 196_864, unit), 2 * 196_864);
        assert_eq!(q.depart(0, unit), 2 * 196_864 + 98_432);
    }

    #[test]
    fn after_a_late_send_at_most_half_a_burst_follows_back_to_back() {
        // the worst case: late by the whole tolerance, then many small sends ready at once (the
        // header packet, slice tails): what leaves within one burst's wire time of the late send
        // is that send + half a burst, never two bursts
        for late in [1u64, 50_000, 98_432, 150_000, 196_864] {
            let mut p = PaceSchedule::new(1000).unwrap();
            let unit = 16 * 1538;
            p.depart(0, unit);
            let ready = 196_864 + late;
            assert_eq!(p.depart(ready, unit), ready);
            let mut back_to_back = 0u64;
            for _ in 0..200 {
                if p.depart(ready, 192) < ready + 196_864 {
                    back_to_back += 192;
                }
            }
            let credit = late.min(98_432) * 1000 / 8000; // ns of credit at 1 Gb/s -> bytes
            assert!(
                back_to_back <= credit + 192,
                "late {late}: {back_to_back} bytes followed within one burst, credit {credit}"
            );
        }
    }

    #[test]
    fn a_thread_that_wakes_late_every_time_keeps_the_rate() {
        // every send is ready 50 us after its due time (a late wake): the schedule never slips
        let mut p = PaceSchedule::new(900).unwrap();
        let mut ready = 0u64;
        for _ in 0..100_000u32 {
            p.depart(ready, 1538);
            ready = p.due_ns().unwrap() + 50_000;
        }
        let exact = (100_000u128 * 12_304_000).div_ceil(900) as u64;
        assert_eq!(p.due_ns(), Some(exact), "no drift from 100000 late wakes");
    }

    #[test]
    fn a_million_sends_land_exactly_where_one_long_send_would() {
        let mut p = PaceSchedule::new(900).unwrap();
        let mut last = 0;
        for n in 0..1_000_000u128 {
            last = p.depart(0, 1538);
            let exact = (n * 1538 * 8 * 1000).div_ceil(900) as u64;
            if n % 99_991 == 0 {
                assert_eq!(last, exact, "send {n}");
            }
        }
        assert_eq!(last, (999_999u128 * 12_304_000).div_ceil(900) as u64);
    }

    /// The datagram buffers of one 1080p frame at `bpp` (the header packet + 68 slices).
    fn frame_buffers(bpp: usize) -> Vec<Vec<u8>> {
        let count = packets_per_frame(1080);
        assert_eq!(count, 69);
        (0..count)
            .map(|i| {
                let len = if i == 0 { 110 } else { 1920 * 16 * bpp / 8 };
                let base = PacketHeader {
                    camera_id: 100,
                    timecode_100ns: 1,
                    slice_index: i as u8,
                    slice_count: count as u8,
                    offset: 0,
                    slice_len: 0,
                };
                let mut out = Vec::new();
                build_slice_datagrams(base, &vec![7u8; len], &mut out).unwrap();
                out
            })
            .collect()
    }

    /// Schedule every send of one frame ready at `ready_ns`; (first departure, last departure,
    /// the last send's end on the wire).
    fn schedule_frame(p: &mut PaceSchedule, bufs: &[Vec<u8>], ready_ns: u64) -> (u64, u64, u64) {
        let mut first = None;
        let mut last = (0, 0);
        for b in bufs {
            for r in send_ranges(b.len(), segments_per_send(true)) {
                let w = wire_bytes(r.len());
                let d = p.depart(ready_ns, w);
                first.get_or_insert(d);
                last = (d, d + wire_ns(w, p.mbps()));
            }
        }
        (first.unwrap(), last.0, last.1)
    }

    #[test]
    fn a_frame_at_900_mbps_finishes_within_the_30_fps_budget_up_to_12_bpp() {
        const BUDGET_NS: u64 = 33_333_333;
        for (bpp, want_ms) in [(4, 9.9), (8, 19.7), (12, 29.5)] {
            let bufs = frame_buffers(bpp);
            let total: u64 = bufs
                .iter()
                .flat_map(|b| send_ranges(b.len(), 16).map(|r| wire_bytes(r.len())))
                .sum();
            let mut p = PaceSchedule::new(900).unwrap();
            let (first, last, end) = schedule_frame(&mut p, &bufs, 1_000);
            assert_eq!(first, 1_000);
            // the frame ends where its whole wire size at 900 Mb/s ends: nothing lost to rounding
            assert_eq!(p.due_ns(), Some(1_000 + wire_ns(total, 900)), "{bpp} bpp");
            assert!(end - first <= wire_ns(total, 900) + 1, "{bpp} bpp");
            assert!(
                last < end && end - first < BUDGET_NS,
                "{bpp} bpp: {} ns",
                end - first
            );
            let ms = (end - first) as f64 / 1e6;
            assert!((ms - want_ms).abs() < 0.2, "{bpp} bpp: {ms} ms");
        }
    }

    #[test]
    fn frame_after_frame_every_frame_starts_on_its_own_tick_with_no_drift() {
        let bufs = frame_buffers(8);
        let mut p = PaceSchedule::new(900).unwrap();
        let mut spans = std::collections::BTreeSet::new();
        for k in 0..300u64 {
            let tick = 17_000_000_000_000 + k * 1_000_000_000 / 30;
            let (first, _, end) = schedule_frame(&mut p, &bufs, tick);
            assert_eq!(first, tick, "frame {k} leaves on its own tick");
            spans.insert(end - first);
        }
        assert_eq!(
            spans.len(),
            1,
            "every frame takes the same wire time: {spans:?}"
        );
    }

    #[test]
    fn frames_faster_than_the_pace_queue_behind_each_other_never_overlap() {
        // 8 bpp at 300 Mb/s takes ~59 ms: the second frame waits for the first one's tail
        let bufs = frame_buffers(8);
        let mut p = PaceSchedule::new(300).unwrap();
        let (_, _, end1) = schedule_frame(&mut p, &bufs, 0);
        let (first2, _, _) = schedule_frame(&mut p, &bufs, 33_333_333);
        assert!(
            end1 > 33_333_333,
            "the first frame is still on the wire at the second tick"
        );
        assert!(
            first2 + 1 >= end1 && first2 <= end1,
            "the second frame starts where the first one's last send ends: {first2} vs {end1}"
        );
    }
}
