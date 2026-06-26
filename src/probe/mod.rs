//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`, `differ`,
//! `genlock`, plus the `kms`/`presenter` decision functions. `recording`'s
//! per-frame decode is pure-tested; its ffmpeg/ffprobe glue is exercised
//! end-to-end by the `recording_decode` integration test.
//! Hardware glue (excluded from coverage): `fb`, `kms` (live DRM), `painter`,
//! `reader`, `run`, `multi_reader`.

pub mod analyzer;
pub mod burn_contiguity;
pub mod differ;
pub mod genlock;
pub mod liveness;
pub mod luma;
pub mod obs_log_audit;
pub mod payload;
pub mod qr;
pub mod recording;
pub mod recording_latency;
pub mod recording_partial;
pub mod recording_verdict;

// #193: the probe HARDWARE GLUE is Linux-only — fb (/dev/fb0 + libc ioctl), kms/presenter
// (drm page-flip), painter (fb+evdev), reader/multi_reader (v4l), run (drives all of them).
// None are needed by recording-verdict (its transitive set is recording/recording_verdict/
// recording_latency/burn_contiguity/payload/qr/luma/analyzer — all pure +
// cross-platform). Gating them on cfg(target_os="linux") lets the verdict cross-build for
// Windows (so the #193 decode runs ON stream.lan), while the Linux probe build is unchanged.
#[cfg(target_os = "linux")]
pub mod fb;
#[cfg(target_os = "linux")]
pub mod kms;
#[cfg(target_os = "linux")]
pub mod multi_reader;
#[cfg(target_os = "linux")]
pub mod painter;
#[cfg(target_os = "linux")]
pub mod presenter;
#[cfg(target_os = "linux")]
pub mod reader;
#[cfg(target_os = "linux")]
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
