//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`, `differ`.
//! Hardware glue (excluded from coverage): `fb`, `painter`, `reader`, `run`, `multi_reader`.

pub mod analyzer;
pub mod differ;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod fb;
pub mod multi_reader;
pub mod painter;
pub mod reader;
pub mod run;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A timestamp on the configured clock domain — the single place that picks
/// between the wall clock and the shared monotonic clock for the whole probe.
/// `wall_clock` ⇒ CLOCK_REALTIME epoch ns (the DanteSync-disciplined wall clock,
/// strih = master), required for the #7 ABSOLUTE end-to-end latency so the
/// camera-painted `gen_ts` and the dev1 endpoint tap's `recv_ts` share one
/// origin; otherwise ns since the shared monotonic `start` (per-hop RELATIVE
/// latency, and Phase-1 single-box loopback where painter+reader share one
/// process clock). Used by both the painter (`gen_ts`) and the taps (`recv_ts`)
/// so the two domains can never silently diverge.
pub fn clock_ns(start: Instant, wall_clock: bool) -> i64 {
    if wall_clock {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock before epoch")
            .as_nanos() as i64
    } else {
        start.elapsed().as_nanos() as i64
    }
}
