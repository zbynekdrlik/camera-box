//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`, `differ`,
//! `genlock`, plus the `kms`/`presenter` decision functions. `recording`'s
//! per-frame decode is pure-tested; its ffmpeg/ffprobe glue is exercised
//! end-to-end by the `tests/recording_decode.rs` integration test (not the `recording_decode`
//! module, which is the per-frame decode core split out of `qr`, issue 1374).
//! Hardware glue (excluded from coverage): `fb`, `kms` (live DRM), `painter`,
//! `reader`, `run`, `multi_reader`.

pub mod analyzer;
pub mod burn_contiguity;
// issue 1367 — the recording decode's node-burn ECHO gate: a node burn counts only when read
// inside its own `crate::burn_regions` slot; any other read of it is an optical echo.
pub mod burn_echo;
// issue 1370 — the burn-isolated recovery pass of the recording decode: an expected node burn the
// plain + #202 tile passes still miss is decoded from its own `crate::burn_regions` slot crop.
pub mod burn_region_decode;
pub mod differ;
pub mod genlock;
pub mod liveness;
pub mod luma;
pub mod obs_log_audit;
pub mod payload;
pub mod qr;
pub mod recording;
// issue 1374 — the recording decode core (fast-then-robust gate, #202 tiles, #754 top band, the
// issue-1367 echo-gated core), split out of `qr`, which keeps the QR primitives and re-exports it.
pub mod recording_decode;
pub mod recording_latency;
pub mod recording_partial;
pub mod recording_segments;
pub mod recording_verdict;
// #364 — colour-sampling glue for the per-camera colour gate (RgbImage adapter + burn-exclusion
// geometry + ffmpeg colour pass). Cross-platform (Command + image + the pure colour_verify), like
// the recording set above; the JUDGEMENT is the Tier-0 `colour_verify` module.
pub mod colour_sample;

// #188 — A/V-sync offset from a recording (ffmpeg audio extract + cam2 dual-QR video + the pure
// qpsk_marker decode/pair/offset). Cross-platform like the recording set, so it runs on stream.lan.
pub mod av_sync_recording;

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
// #188: continuous-feed QPSK A/V-sync marker emitter (norihiro-compatible; replaced the chirp).
#[cfg(target_os = "linux")]
pub mod qpsk_emit;
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
