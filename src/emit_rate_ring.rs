//! camera-box #707 B1 — per-second emit/capture rate ring.
//!
//! The existing 5s `Streaming:` report (src/main.rs) averages emit/capture fps over a whole 5s
//! window, so a sub-5s EMIT PAUSE — the #707 B1 FREEZE (strih shows ~2.5s of frozen frames then a
//! +170-id JUMP while the box's own capture reads clean) — can average out and hide. That was the
//! last remaining BOX-SIDE blind spot named in #707's 2026-07-14 CORRECTION comment: "the box's
//! Streaming report is a 5s bucket — one remaining box-side blind spot (a sub-5s emit pause could
//! hide)".
//!
//! This pure ring keeps the last N completed 1-SECOND buckets of (emit, capture) counts so the
//! report can print a compact `emit-1s: [60,60,59,60,60]` line AND WARN the instant any single
//! second's emit count dips below the configured send floor. It is the FIRST prong of #707 B1's
//! freeze discriminator: on the next freeze, if this WARN fires the box emit path dipped; if the
//! box stays clean (buckets ~60) while strih freezes, the loss is downstream (link / NDI SDK), to
//! be read off the transport sampler (the second prong).
//!
//! Pure Tier-0 (default features, no probe): the caller feeds a MONOTONIC `now_ns` (never the wall
//! clock — this whole ticket is about DanteSync NTP/PTP wall-clock seams) plus per-tick emit/
//! capture deltas; the ring rolls completed 1-second buckets and returns them so the caller can
//! WARN immediately. Unit-tested off-rig.

use std::collections::VecDeque;

/// One completed 1-second bucket: frames EMITTED and frames CAPTURED in that wall-clock second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondBucket {
    pub emit: u32,
    pub capture: u32,
}

/// Default ring depth = the 5s Streaming report window, so the printed `emit-1s:` line covers the
/// same window the averaged fps line does.
pub const DEFAULT_RING_SECONDS: usize = 5;

/// A completed 1-second bucket's emit count is a real dip when it falls below this fraction of the
/// configured per-box send rate. 0.9 → floor 54 at a 60fps send rate (the dispatch's "< 55"
/// example), 27 at a 30fps send rate — keyed on the box's real `send_fps` so a genuinely
/// slower-send box never false-warns.
pub const BUCKET_FLOOR_FRACTION: f64 = 0.9;

const NS_PER_SEC: u64 = 1_000_000_000;

/// Rolling ring of the last `capacity` completed 1-second (emit, capture) buckets.
pub struct EmitRateRing {
    capacity: usize,
    window_start_ns: Option<u64>,
    cur_emit: u32,
    cur_capture: u32,
    completed: VecDeque<SecondBucket>,
}

impl EmitRateRing {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            window_start_ns: None,
            cur_emit: 0,
            cur_capture: 0,
            completed: VecDeque::with_capacity(capacity),
        }
    }

    /// Record one frame-tick's `emitted`/`captured` deltas at MONOTONIC `now_ns`. Rolls any
    /// completed 1-second buckets into the ring and RETURNS them (oldest first) so the caller can
    /// WARN on a low one immediately. Usually returns `[]`; returns one bucket when a second
    /// boundary was crossed, or several — the FROZEN seconds as `emit: 0` buckets — when a
    /// multi-second stall resumes (exactly the #707 freeze the ring must surface, never hide).
    pub fn observe(&mut self, now_ns: u64, emitted: u32, captured: u32) -> Vec<SecondBucket> {
        let Some(mut start) = self.window_start_ns else {
            // First observation seeds the window; nothing completes yet.
            self.window_start_ns = Some(now_ns);
            self.cur_emit = emitted;
            self.cur_capture = captured;
            return Vec::new();
        };
        let mut rolled = Vec::new();
        // A monotonic clock never regresses, but saturating_add keeps this total even if a caller
        // ever feeds a stale timestamp (it simply completes no bucket).
        while now_ns >= start.saturating_add(NS_PER_SEC) {
            let bucket = SecondBucket {
                emit: self.cur_emit,
                capture: self.cur_capture,
            };
            self.push_completed(bucket);
            rolled.push(bucket);
            self.cur_emit = 0;
            self.cur_capture = 0;
            start = start.saturating_add(NS_PER_SEC);
        }
        self.window_start_ns = Some(start);
        self.cur_emit = self.cur_emit.saturating_add(emitted);
        self.cur_capture = self.cur_capture.saturating_add(captured);
        rolled
    }

    fn push_completed(&mut self, b: SecondBucket) {
        if self.completed.len() == self.capacity {
            self.completed.pop_front();
        }
        self.completed.push_back(b);
    }

    /// The last completed 1-second EMIT counts, oldest first (for the `emit-1s: [..]` report line).
    pub fn emit_buckets(&self) -> Vec<u32> {
        self.completed.iter().map(|b| b.emit).collect()
    }

    /// The last completed 1-second CAPTURE counts, oldest first.
    pub fn capture_buckets(&self) -> Vec<u32> {
        self.completed.iter().map(|b| b.capture).collect()
    }
}

/// Is this completed 1-second bucket's emit count a real dip — below `floor_fraction` of the
/// configured per-box `send_fps`? Returns `false` when `send_fps <= 0` (no configured send rate to
/// compare — e.g. a non-genlock box) so the WARN never false-fires.
pub fn emit_bucket_below_floor(emit: u32, send_fps: f64, floor_fraction: f64) -> bool {
    if send_fps <= 0.0 {
        return false;
    }
    (emit as f64) < send_fps * floor_fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_then_rolls_one_bucket_per_elapsed_second() {
        let mut r = EmitRateRing::new(5);
        assert!(
            r.observe(0, 1, 1).is_empty(),
            "the first observe only seeds the window — nothing completes yet"
        );
        assert!(
            r.observe(500_000_000, 1, 1).is_empty(),
            "still within the first second — no bucket completes"
        );
        // Cross into second 1 → completes second 0 (seed 1 + the 500ms 1 = 2).
        assert_eq!(
            r.observe(1_000_000_000, 1, 1),
            vec![SecondBucket {
                emit: 2,
                capture: 2
            }]
        );
        assert_eq!(
            r.observe(2_000_000_000, 5, 5),
            vec![SecondBucket {
                emit: 1,
                capture: 1
            }],
            "second 1 carried only the single frame observed at the 1s boundary"
        );
        assert_eq!(r.emit_buckets(), vec![2, 1]);
        assert_eq!(r.capture_buckets(), vec![2, 1]);
    }

    #[test]
    fn multi_second_gap_emits_zero_buckets_for_the_frozen_seconds() {
        // The #707 freeze: no observe calls for ~2.5s, then a resume — the frozen seconds MUST
        // surface as emit:0 buckets, never be swallowed.
        let mut r = EmitRateRing::new(5);
        r.observe(0, 1, 1); // seed
        assert_eq!(
            r.observe(1_000_000_000, 0, 0),
            vec![SecondBucket {
                emit: 1,
                capture: 1
            }]
        );
        // ~2.5s freeze (no observe), resume at 3.5s with one frame.
        assert_eq!(
            r.observe(3_500_000_000, 1, 1),
            vec![
                SecondBucket {
                    emit: 0,
                    capture: 0
                },
                SecondBucket {
                    emit: 0,
                    capture: 0
                },
            ],
            "the two whole frozen seconds must each surface as an emit:0 bucket"
        );
    }

    #[test]
    fn ring_is_bounded_to_capacity_oldest_evicted() {
        let mut r = EmitRateRing::new(3);
        r.observe(0, 0, 0); // seed second 0
        for s in 1..=5u64 {
            r.observe(s * 1_000_000_000, s as u32, s as u32);
        }
        // Completed: sec0=0, sec1=1, sec2=2, sec3=3, sec4=4 → capacity 3 keeps the newest 3.
        assert_eq!(r.emit_buckets(), vec![2, 3, 4]);
    }

    #[test]
    fn floor_is_keyed_on_send_fps_and_never_false_warns() {
        // 60fps send: floor 54. A 53-frame second dips; 54 (== floor) and 60 do not.
        assert!(emit_bucket_below_floor(53, 60.0, BUCKET_FLOOR_FRACTION));
        assert!(!emit_bucket_below_floor(54, 60.0, BUCKET_FLOOR_FRACTION));
        assert!(!emit_bucket_below_floor(60, 60.0, BUCKET_FLOOR_FRACTION));
        // 30fps send: floor 27. 30 is healthy; 26 dips — proves the floor scales with send_fps and
        // a genuinely slower-send box is never false-warned.
        assert!(!emit_bucket_below_floor(30, 30.0, BUCKET_FLOOR_FRACTION));
        assert!(emit_bucket_below_floor(26, 30.0, BUCKET_FLOOR_FRACTION));
        // No configured send rate → never warns.
        assert!(!emit_bucket_below_floor(0, 0.0, BUCKET_FLOOR_FRACTION));
    }
}
