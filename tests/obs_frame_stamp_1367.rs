//! Issue 1367 — the OBS program sender stamps the frame's RENDER TICK, never the send instant.
//!
//! Live 31.10.2026: the stream box's N==1 `NDI 2ME PGM` FIFO shed one strih program frame 3–27
//! times an hour (`n1_grows` then `dropped_due`), each after a `stamp_dup` + `stamp_gap` pair:
//! strih's DistroAV output stamped the floor of the wall clock at SEND time, so a frame the output
//! thread handed over late took the next slot and the following on-time frame took it again.
//! These cases drive `camera_box::genlock_stamp::obs_frame_stamp_100ns` on two clocks (wall in
//! 100 ns, media clock in ns, a fixed offset between them like a live box).

use camera_box::genlock_stamp::{obs_frame_stamp_100ns, OBS_FRAME_STAMP_MAX_AGE_NS};

const UNITS: i64 = 10_000_000;
/// 2026-10-31 04:15:30 UTC, the second of the matrix w-05 shed.
const SEC: i64 = 1_793_420_130;
/// wall − media clock, in ns (a box up ~11 days).
const WALL_MINUS_MONO_NS: i64 = 1_792_470_000_123_456_789;

/// Slot `k` of second `SEC` at `fps`, 100 ns units: the per-second grid the senders stamp on.
fn slot(k: i64, fps: i64) -> i64 {
    SEC * UNITS + k * UNITS / fps
}

/// The media-clock instant of a wall instant (100 ns) on this box.
fn mono_of(wall_100ns: i64) -> u64 {
    (wall_100ns * 100 - WALL_MINUS_MONO_NS) as u64
}

/// Stamp a frame whose render tick sat at wall `tick_100ns` (+ `tick_jitter_ns` on the media
/// clock) when the output sends it at wall `send_100ns`.
fn stamp(tick_100ns: i64, tick_jitter_ns: i64, send_100ns: i64, fps: i64) -> i64 {
    let frame_ts = (mono_of(tick_100ns) as i64 + tick_jitter_ns) as u64;
    obs_frame_stamp_100ns(send_100ns, mono_of(send_100ns), frame_ts, fps)
}

#[test]
fn a_frame_sent_late_across_the_slot_edge_keeps_its_tick_slot_1367() {
    // 30 fps: the tick at slot 10, the output thread hands it over 40 ms later (slot 11).
    let tick = slot(10, 30);
    let send = tick + 400_000;
    assert_eq!(stamp(tick, 0, send, 30), slot(10, 30));
    // The next frame on time 2 ms after its tick: slot 11, never a second slot-10 stamp.
    let next = slot(11, 30);
    assert_eq!(stamp(next, 0, next + 20_000, 30), slot(11, 30));
}

#[test]
fn a_late_send_run_never_duplicates_or_skips_a_slot_1367() {
    // Every frame of two seconds, each sent 0..40 ms after its tick: the stamps are the tick
    // slots one by one, so the receiver sees no stamp_dup and no stamp_gap.
    let mut prev: Option<i64> = None;
    for k in 0..60 {
        let tick = slot(k, 30);
        let late_100ns = ((k * 7_919) % 400_001) as i64;
        let s = stamp(tick, 0, tick + late_100ns, 30);
        assert_eq!(s, tick, "frame {k} sent {late_100ns} x100 ns late");
        if let Some(p) = prev {
            assert!(s > p, "frame {k}: stamp {s} not after {p}");
        }
        prev = Some(s);
    }
}

#[test]
fn a_tick_mapped_a_few_us_early_keeps_its_own_slot_1367() {
    // The tick sits ON its grid point; the media->wall mapping at send time can read it a few
    // microseconds early or late. Either way it is that slot, never the previous one.
    for fps in [30, 60] {
        for jitter_ns in [-50_000i64, -5_000, -1, 0, 1, 5_000, 50_000] {
            let tick = slot(17, fps);
            let s = stamp(tick, jitter_ns, tick + 10_000, fps);
            assert_eq!(s, slot(17, fps), "fps {fps} jitter {jitter_ns} ns");
        }
    }
}

#[test]
fn a_stamp_is_never_after_the_send_instant_1367() {
    // A frame stamped from a tick that maps AHEAD of the send instant (a media clock read ahead,
    // a wall step back) is clamped to the floor of the send instant (the #1009 rule).
    let send = slot(5, 30) + 1_000;
    let tick_ahead = slot(6, 30); // maps 1 slot after the send instant
    let frame_ts = mono_of(tick_ahead);
    let s = obs_frame_stamp_100ns(send, mono_of(send) + 1, frame_ts, 30);
    // frame_ts is in the future of mono_now -> the send-time rule.
    assert_eq!(s, slot(5, 30));
    // A tick 5 µs after the send boundary but rounded to the next slot can not pass the send floor.
    let send2 = slot(8, 30) + 10; // 1 µs after slot 8
    let tick2 = slot(9, 30) - 1_000; // 100 µs before slot 9 on the wall...
    let s2 = obs_frame_stamp_100ns(send2, mono_of(tick2) + 50, mono_of(tick2), 30);
    assert!(
        s2 <= slot(8, 30),
        "stamp {s2} future-dated past the send floor {}",
        slot(8, 30)
    );
}

#[test]
fn a_missing_future_or_stale_frame_timestamp_stamps_the_send_instant_1367() {
    let send = slot(20, 30) + 123_456;
    let mono_now = mono_of(send);
    let send_floor = slot(20, 30);
    // 0 = no timestamp.
    assert_eq!(obs_frame_stamp_100ns(send, mono_now, 0, 30), send_floor);
    // In the future of the media clock.
    assert_eq!(
        obs_frame_stamp_100ns(send, mono_now, mono_now + 1, 30),
        send_floor
    );
    // Older than the bound.
    let stale = mono_now - OBS_FRAME_STAMP_MAX_AGE_NS - 1;
    assert_eq!(obs_frame_stamp_100ns(send, mono_now, stale, 30), send_floor);
    // Exactly at the bound is still the frame's own tick: one second earlier, 12.3 ms past
    // slot 20 there, which the nearest boundary puts on slot 20 of the previous second.
    assert_eq!(
        obs_frame_stamp_100ns(send, mono_now, mono_now - OBS_FRAME_STAMP_MAX_AGE_NS, 30),
        slot(20, 30) - UNITS
    );
}

#[test]
fn fps_zero_stamps_the_send_instant_1367() {
    let send = slot(3, 30) + 777;
    assert_eq!(
        obs_frame_stamp_100ns(send, mono_of(send), mono_of(send) - 10, 0),
        send
    );
}
