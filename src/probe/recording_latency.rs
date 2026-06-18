//! #108 — Bias-free per-hop ABSOLUTE latency from the recorded OBS program files.
//!
//! Part of #105 Step 3. Computes cam→strih and strih→stream per-hop latency with
//! NO networked `record_start` and NO `idx/30` assumption — every number is a
//! difference of two in-frame `gen_ts_ns` stamps that already share ONE timebase
//! (the DanteSync-disciplined wall clock; strih = master, verified via the
//! dantesync log per `dantesync-ops`, NEVER `timedatectl`).
//!
//! ## The two QR stamps per recorded frame
//!
//! A recorded **strih** (or **stream**) program frame on the dedicated #108 PROBE
//! scene carries TWO QR payloads (read in one rqrr pass by [`crate::probe::recording`]):
//!
//! - **cam2's QR** — the optical content cam2 painted and the broadcast camera
//!   filmed: `Payload { run_id = <cam2 run_id>, frame_id = <logical tick>,
//!   gen_ts_ns = <cam2 PAINT instant on the wall clock> }`.
//! - **this node's burn QR** — stamped by the #111 DistroAV burn filter at THIS
//!   node's render time: `Payload { run_id = 911002 (strih) | 911004 (stream),
//!   frame_id = <node's own monotonic counter>, gen_ts_ns = <node RENDER instant
//!   on the SAME wall clock> }`.
//!
//! The two run_ids are disjoint by construction (the burn defaults 911002/911004
//! sit far outside cam2's range), so [`split_payloads`] separates them with no
//! ambiguity.
//!
//! ## The per-hop math — co-located, no cross-tap pairing for cam→strih
//!
//! Because both QRs live in the SAME recorded frame, the cam→strih hop needs no
//! pairing across recordings:
//!
//! - **cam→strih** = `strih_burn.gen_ts_ns − cam2.gen_ts_ns` — the time from cam2's
//!   paint instant to strih's render of that same captured frame. Computed PER strih
//!   frame that decoded both a cam2 QR and a strih burn QR.
//! - **strih→stream** = `stream_burn.gen_ts_ns − strih_burn.gen_ts_ns` — paired by
//!   the **cam2 logical tick** (`frame_id` of cam2's QR), which is common to both
//!   recordings (it is the upstream optical content, identical at both outputs).
//!   For each cam2 tick present in BOTH recordings, subtract the strih render stamp
//!   from the stream render stamp.
//!
//! Both anchor on a value (cam2's `gen_ts_ns` / the cam2 tick) carried inside the
//! frame, so there is no first-occurrence / oversample ambiguity and no network
//! round-trip bias.
//!
//! ## What this module reports
//!
//! Per hop: p50, p99, **jitter** (`p99 − p50`), and **drift over the window**
//! (linear slope of the per-sample latency vs sample index, in ms per minute) — so
//! a stable, defined latency can be shown, and a creeping clock breach surfaces as
//! non-zero drift rather than hiding inside a percentile. cam→strih is honestly
//! labelled as derived from the #111 render-ts burn (the issue's Step-6 dependency
//! is satisfied by the burn), NEVER faked.
//!
//! The engine is PURE (decoupled from ffmpeg/image via [`crate::probe::recording::RecordingFrame`])
//! and fully unit-tested on synthetic streams with KNOWN offsets; the binary
//! (`recording-verdict --burn-strih …`) is the I/O glue that runs it on the real
//! recordings.

use crate::probe::analyzer::{percentile, LatencyStats};
use crate::probe::payload::Payload;
use crate::probe::recording::RecordingFrame;
use serde::Serialize;
use std::collections::HashMap;

/// Default reserved per-node burn run_ids (mirrors the #111 burn filter's
/// `BURN_RUN_ID_DEFAULT_STRIH` / `…_STREAM` in `vendor/distroav/src/ndi-burn-filter.cpp`).
/// Both are far outside cam2's normal run_id range so a node-stamp QR is told apart
/// from the cam2 QR by run_id alone. The binary lets the operator override these to
/// match a non-default `OBS_BURN_RUN_ID` on the box.
pub const BURN_RUN_ID_STRIH: u32 = 911002;
/// See [`BURN_RUN_ID_STRIH`].
pub const BURN_RUN_ID_STREAM: u32 = 911004;

/// Per-hop latency over the analyzed window, with the #108 stability dimensions
/// (jitter + drift) on top of the reused [`LatencyStats`] percentiles.
#[derive(Debug, Clone, Serialize)]
pub struct HopLatency {
    /// Human label for the hop (`cam→strih` / `strih→stream`).
    pub hop: String,
    /// p50/p95/p99/min/mean/max over the per-sample latency (ms).
    pub stats: LatencyStats,
    /// Jitter = `p99 − p50` (ms): the spread of the upper tail above the median.
    /// The #108 "defined, stable" criterion is a small jitter relative to p50.
    pub jitter_ms: f64,
    /// Drift over the analyzed window: linear least-squares slope of per-sample
    /// latency (ms) vs sample WALL-CLOCK time (minutes), reported in **ms/min**. A
    /// near-zero slope = a STABLE hop (no creeping clock breach); a large magnitude
    /// = the two nodes' clocks are diverging over the window (a real fault, NOT
    /// hidden inside a percentile). 0.0 when there are fewer than 2 samples or the
    /// samples span zero time.
    pub drift_ms_per_min: f64,
    /// Number of paired samples the hop latency was computed from.
    pub samples: usize,
}

/// One paired latency sample: the latency (ms) and the wall-clock instant it
/// belongs to (the upstream `gen_ts_ns`, used as the x-axis for drift). Kept as an
/// explicit type so the drift regression has a defined, testable time axis rather
/// than the capture index (which is non-uniform when frames are undecodable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencySample {
    /// The per-hop latency for this sample, in milliseconds.
    pub latency_ms: f64,
    /// The wall-clock instant (ns since epoch) this sample is anchored at — the
    /// upstream stamp. Used as the drift regression's time axis.
    pub at_ns: i64,
}

/// Split a recorded frame's payloads into `(cam2 payloads, node-burn payloads)` by
/// run_id. A payload whose run_id equals `burn_run_id` is THIS node's burn stamp;
/// everything else is treated as cam2 content. Returns the FIRST of each kind
/// (a frame carries at most one of each on the probe scene; the dual-QR cam2 halves
/// share one `gen_ts_ns`, so either half gives the same cam2 paint instant).
pub fn split_payloads(
    frame: &RecordingFrame,
    burn_run_id: u32,
) -> (Option<Payload>, Option<Payload>) {
    let mut cam2: Option<Payload> = None;
    let mut node: Option<Payload> = None;
    for p in &frame.payloads {
        if p.run_id == burn_run_id {
            if node.is_none() {
                node = Some(*p);
            }
        } else if cam2.is_none() {
            cam2 = Some(*p);
        }
    }
    (cam2, node)
}

/// Least-squares slope of `y` vs `x` (the drift regression). Returns 0.0 when there
/// are fewer than 2 points or `x` has zero variance (all samples at one instant).
/// Pure + total (no panics, no NaN leak: a zero denominator short-circuits to 0.0).
fn lsq_slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let sum_x: f64 = points.iter().map(|(x, _)| *x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| *y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let denom = nf * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return 0.0;
    }
    (nf * sum_xy - sum_x * sum_y) / denom
}

/// Reduce a set of paired latency samples to a [`HopLatency`] (None when empty).
///
/// `stats` reuses the project's `latency_stats` percentiles (no parallel impl);
/// `jitter_ms = p99 − p50`; `drift_ms_per_min` is the least-squares slope of
/// `latency_ms` vs `at_ns` converted to ms per minute. The samples are taken in the
/// order given (capture order) but the drift uses the WALL-CLOCK `at_ns` axis, so a
/// gap from undecodable frames does not distort the slope.
pub fn hop_latency(hop: &str, samples: &[LatencySample]) -> Option<HopLatency> {
    if samples.is_empty() {
        return None;
    }
    let latencies: Vec<f64> = samples.iter().map(|s| s.latency_ms).collect();
    let stats = crate::probe::analyzer::latency_stats(&latencies)?;
    let jitter_ms = stats.p99_ms - stats.p50_ms;

    // Drift: slope of latency_ms vs time-in-minutes. x in minutes keeps the slope
    // in the reported ms/min unit directly.
    let t0 = samples.iter().map(|s| s.at_ns).min().unwrap_or(0);
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|s| {
            let minutes = (s.at_ns - t0) as f64 / 60_000_000_000.0; // ns → minutes
            (minutes, s.latency_ms)
        })
        .collect();
    let drift_ms_per_min = lsq_slope(&points);

    Some(HopLatency {
        hop: hop.to_string(),
        stats,
        jitter_ms,
        drift_ms_per_min,
        samples: samples.len(),
    })
}

/// cam→strih per-frame samples: for each strih frame that decoded BOTH a cam2 QR
/// and a strih burn QR, `latency = strih_burn.gen_ts_ns − cam2.gen_ts_ns`, anchored
/// at the cam2 paint instant (`cam2.gen_ts_ns`). A frame missing either stamp, or
/// with a non-positive cam2 stamp (an unstamped / monotonic sentinel), is skipped —
/// never a wrong number.
pub fn cam_strih_samples(strih: &[RecordingFrame], strih_burn_run_id: u32) -> Vec<LatencySample> {
    let mut out = Vec::new();
    for f in strih {
        let (cam2, node) = split_payloads(f, strih_burn_run_id);
        if let (Some(c), Some(n)) = (cam2, node) {
            // gen_ts_ns must be the wall-clock domain (huge epoch ns). A 0 is the
            // unstamped sentinel; guard it so a missing stamp can't read as a giant
            // negative/positive latency.
            if c.gen_ts_ns > 0 && n.gen_ts_ns > 0 {
                out.push(LatencySample {
                    latency_ms: (n.gen_ts_ns - c.gen_ts_ns) as f64 / 1_000_000.0,
                    at_ns: c.gen_ts_ns,
                });
            }
        }
    }
    out
}

/// strih→stream per-cam2-tick samples: pair the strih and stream recordings by the
/// cam2 logical tick (`frame_id` of the cam2 QR, common to both outputs), then
/// `latency = stream_burn.gen_ts_ns − strih_burn.gen_ts_ns`, anchored at the strih
/// render instant (`strih_burn.gen_ts_ns`).
///
/// Pairing by the cam2 tick (NOT by capture position) makes this offset-immune: the
/// two independent recordings never start on the same camera frame, but the cam2
/// optical tick is identical at both outputs. A tick present in only one recording
/// is tap start/stop skew (or a real drop, already caught by the #107 loss verdict)
/// and contributes no latency sample.
pub fn strih_stream_samples(
    strih: &[RecordingFrame],
    stream: &[RecordingFrame],
    strih_burn_run_id: u32,
    stream_burn_run_id: u32,
) -> Vec<LatencySample> {
    // Map cam2 tick → strih burn gen_ts (first occurrence; the burn is 1:1 with the
    // rendered frame, and the first time strih renders a given cam2 tick is its
    // render instant for that optical content).
    let strih_by_tick: HashMap<u32, i64> = burn_by_cam2_tick(strih, strih_burn_run_id);
    let stream_by_tick: HashMap<u32, i64> = burn_by_cam2_tick(stream, stream_burn_run_id);

    let mut ticks: Vec<u32> = stream_by_tick.keys().copied().collect();
    ticks.sort_unstable();
    let mut out = Vec::new();
    for tick in ticks {
        if let (Some(&strih_ts), Some(&stream_ts)) =
            (strih_by_tick.get(&tick), stream_by_tick.get(&tick))
        {
            out.push(LatencySample {
                latency_ms: (stream_ts - strih_ts) as f64 / 1_000_000.0,
                at_ns: strih_ts,
            });
        }
    }
    out
}

/// Build `cam2 tick → this node's burn gen_ts_ns` for one recording. Uses the FIRST
/// frame that carries both a cam2 QR and a node burn QR for each cam2 tick (a tick
/// may be sampled by several camera frames; the first render is the canonical
/// instant for that optical content). Skips frames missing either stamp or with a
/// non-positive burn stamp.
fn burn_by_cam2_tick(frames: &[RecordingFrame], burn_run_id: u32) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for f in frames {
        let (cam2, node) = split_payloads(f, burn_run_id);
        if let (Some(c), Some(n)) = (cam2, node) {
            if n.gen_ts_ns > 0 {
                m.entry(c.frame_id).or_insert(n.gen_ts_ns);
            }
        }
    }
    m
}

/// Percentile re-export so the binary can render an extra cut without importing the
/// analyzer directly (keeps the #108 surface in one module).
pub fn pctl(sorted: &[f64], q: f64) -> f64 {
    percentile(sorted, q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::payload::Payload;

    /// Build a recorded frame carrying an optional cam2 QR and an optional node burn
    /// QR, the exact shape the rqrr recorded-file decoder produces.
    fn frame(
        idx: u64,
        cam2: Option<(u32, u32, i64)>, // (run_id, tick, gen_ts_ns)
        node: Option<(u32, u32, i64)>, // (run_id, frame_id, gen_ts_ns)
    ) -> RecordingFrame {
        let mut payloads = Vec::new();
        if let Some((r, t, g)) = cam2 {
            payloads.push(Payload {
                run_id: r,
                frame_id: t,
                gen_ts_ns: g,
            });
        }
        if let Some((r, fid, g)) = node {
            payloads.push(Payload {
                run_id: r,
                frame_id: fid,
                gen_ts_ns: g,
            });
        }
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index: idx,
            payloads,
            tick,
        }
    }

    const CAM2: u32 = 6519; // a representative cam2 run_id (outside the burn range)

    #[test]
    fn split_separates_node_from_cam2_by_run_id() {
        let f = frame(
            0,
            Some((CAM2, 100, 1_000)),
            Some((BURN_RUN_ID_STRIH, 7, 2_000)),
        );
        let (cam2, node) = split_payloads(&f, BURN_RUN_ID_STRIH);
        assert_eq!(cam2.unwrap().run_id, CAM2);
        assert_eq!(node.unwrap().run_id, BURN_RUN_ID_STRIH);
        assert_eq!(node.unwrap().gen_ts_ns, 2_000);
    }

    #[test]
    fn split_node_absent_when_no_burn_run_id() {
        // A production (non-probe) frame: only cam2, no burn.
        let f = frame(0, Some((CAM2, 100, 1_000)), None);
        let (cam2, node) = split_payloads(&f, BURN_RUN_ID_STRIH);
        assert!(cam2.is_some());
        assert!(node.is_none());
    }

    #[test]
    fn cam_strih_latency_is_node_minus_cam2_in_ms() {
        // KNOWN offset: strih renders 200 ms (200_000_000 ns) after cam2 painted,
        // every frame. The computed cam→strih latency must be exactly 200.0 ms.
        let off_ns = 200_000_000i64;
        let base = 1_700_000_000_000_000_000i64; // ~2023 epoch ns
        let frames: Vec<RecordingFrame> = (0..5u64)
            .map(|i| {
                let cam_g = base + i as i64 * 33_333_333; // ~30 fps cam paints
                frame(
                    i,
                    Some((CAM2, 100 + i as u32, cam_g)),
                    Some((BURN_RUN_ID_STRIH, 1000 + i as u32, cam_g + off_ns)),
                )
            })
            .collect();
        let samples = cam_strih_samples(&frames, BURN_RUN_ID_STRIH);
        assert_eq!(samples.len(), 5);
        let h = hop_latency("cam→strih", &samples).unwrap();
        assert!(
            (h.stats.p50_ms - 200.0).abs() < 1e-6,
            "p50 {}",
            h.stats.p50_ms
        );
        assert!((h.stats.p99_ms - 200.0).abs() < 1e-6);
        assert!((h.jitter_ms).abs() < 1e-6, "no jitter on a constant offset");
        assert!(
            h.drift_ms_per_min.abs() < 1e-6,
            "no drift on a constant offset"
        );
    }

    #[test]
    fn cam_strih_skips_frames_missing_either_stamp() {
        let base = 1_700_000_000_000_000_000i64;
        let frames = vec![
            frame(
                0,
                Some((CAM2, 1, base)),
                Some((BURN_RUN_ID_STRIH, 10, base + 100_000_000)),
            ),
            frame(1, Some((CAM2, 2, base)), None), // no burn → skip
            frame(2, None, Some((BURN_RUN_ID_STRIH, 11, base))), // no cam2 → skip
            frame(
                3,
                Some((CAM2, 3, base + 1)),
                Some((BURN_RUN_ID_STRIH, 12, base + 1 + 100_000_000)),
            ),
        ];
        let samples = cam_strih_samples(&frames, BURN_RUN_ID_STRIH);
        assert_eq!(
            samples.len(),
            2,
            "only the two complete frames yield samples"
        );
        let h = hop_latency("cam→strih", &samples).unwrap();
        assert!((h.stats.p50_ms - 100.0).abs() < 1e-6);
    }

    #[test]
    fn cam_strih_skips_unstamped_zero_gen_ts() {
        // gen_ts_ns == 0 is the unstamped sentinel; a frame with it must NOT produce
        // a giant false latency.
        let frames = vec![
            frame(
                0,
                Some((CAM2, 1, 0)),
                Some((BURN_RUN_ID_STRIH, 10, 200_000_000)),
            ),
            frame(
                1,
                Some((CAM2, 2, 1_700_000_000_000_000_000)),
                Some((BURN_RUN_ID_STRIH, 11, 0)),
            ),
        ];
        let samples = cam_strih_samples(&frames, BURN_RUN_ID_STRIH);
        assert!(samples.is_empty(), "unstamped frames produce no sample");
        assert!(hop_latency("cam→strih", &samples).is_none());
    }

    #[test]
    fn strih_stream_paired_by_cam2_tick_is_stream_minus_strih() {
        // KNOWN offset: stream renders 40 ms after strih for the SAME cam2 tick.
        // The two recordings start at DIFFERENT capture positions (offset-immune):
        // stream's frames carry a +3 capture-index shift but the SAME cam2 ticks.
        let base = 1_700_000_000_000_000_000i64;
        let strih: Vec<RecordingFrame> = (0..6u64)
            .map(|i| {
                let strih_g = base + i as i64 * 33_000_000;
                frame(
                    i,
                    Some((CAM2, 500 + i as u32, base - 1)),
                    Some((BURN_RUN_ID_STRIH, 70 + i as u32, strih_g)),
                )
            })
            .collect();
        // stream: same cam2 ticks (500..505) but shifted capture position, +40 ms render.
        let stream: Vec<RecordingFrame> = (0..6u64)
            .map(|i| {
                let strih_g = base + i as i64 * 33_000_000;
                let stream_g = strih_g + 40_000_000; // +40 ms
                frame(
                    i + 3,
                    Some((CAM2, 500 + i as u32, base - 1)),
                    Some((BURN_RUN_ID_STREAM, 90 + i as u32, stream_g)),
                )
            })
            .collect();
        let samples = strih_stream_samples(&strih, &stream, BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM);
        assert_eq!(samples.len(), 6, "all six shared cam2 ticks pair");
        let h = hop_latency("strih→stream", &samples).unwrap();
        assert!(
            (h.stats.p50_ms - 40.0).abs() < 1e-6,
            "p50 {}",
            h.stats.p50_ms
        );
        assert!(h.jitter_ms.abs() < 1e-6);
    }

    #[test]
    fn strih_stream_only_shared_ticks_pair() {
        let base = 1_700_000_000_000_000_000i64;
        // strih has ticks 1,2,3; stream has ticks 2,3,4 — only 2,3 are shared.
        let strih = vec![
            frame(
                0,
                Some((CAM2, 1, base)),
                Some((BURN_RUN_ID_STRIH, 10, base + 1_000_000)),
            ),
            frame(
                1,
                Some((CAM2, 2, base)),
                Some((BURN_RUN_ID_STRIH, 11, base + 2_000_000)),
            ),
            frame(
                2,
                Some((CAM2, 3, base)),
                Some((BURN_RUN_ID_STRIH, 12, base + 3_000_000)),
            ),
        ];
        let stream = vec![
            frame(
                0,
                Some((CAM2, 2, base)),
                Some((BURN_RUN_ID_STREAM, 20, base + 12_000_000)),
            ),
            frame(
                1,
                Some((CAM2, 3, base)),
                Some((BURN_RUN_ID_STREAM, 21, base + 13_000_000)),
            ),
            frame(
                2,
                Some((CAM2, 4, base)),
                Some((BURN_RUN_ID_STREAM, 22, base + 14_000_000)),
            ),
        ];
        let samples = strih_stream_samples(&strih, &stream, BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM);
        // tick 2: 12-2=10 ms ; tick 3: 13-3=10 ms ; tick 1 & 4 unshared.
        assert_eq!(samples.len(), 2);
        let h = hop_latency("strih→stream", &samples).unwrap();
        assert!((h.stats.p50_ms - 10.0).abs() < 1e-6);
    }

    #[test]
    fn jitter_is_p99_minus_p50() {
        // Latencies 100×98 then 300×2 → p50≈100, p99≈300, jitter≈200.
        let base = 1_700_000_000_000_000_000i64;
        let mut samples: Vec<LatencySample> = (0..98)
            .map(|i| LatencySample {
                latency_ms: 100.0,
                at_ns: base + i,
            })
            .collect();
        samples.extend((0..2).map(|i| LatencySample {
            latency_ms: 300.0,
            at_ns: base + 98 + i,
        }));
        let h = hop_latency("x", &samples).unwrap();
        assert!((h.stats.p50_ms - 100.0).abs() < 1e-9);
        assert!((h.stats.p99_ms - 300.0).abs() < 1e-9);
        assert!((h.jitter_ms - 200.0).abs() < 1e-9);
    }

    #[test]
    fn drift_detects_a_linear_creep() {
        // Latency creeps +1 ms per minute over a 10-minute window: drift ≈ +1 ms/min.
        let base = 1_700_000_000_000_000_000i64;
        let samples: Vec<LatencySample> = (0..=10)
            .map(|m| LatencySample {
                latency_ms: 200.0 + m as f64,            // +1 ms each minute
                at_ns: base + m as i64 * 60_000_000_000, // one sample per minute
            })
            .collect();
        let h = hop_latency("x", &samples).unwrap();
        assert!(
            (h.drift_ms_per_min - 1.0).abs() < 1e-6,
            "expected +1 ms/min drift, got {}",
            h.drift_ms_per_min
        );
    }

    #[test]
    fn drift_zero_on_constant_latency() {
        let base = 1_700_000_000_000_000_000i64;
        let samples: Vec<LatencySample> = (0..100)
            .map(|i| LatencySample {
                latency_ms: 216.0,
                at_ns: base + i as i64 * 33_000_000,
            })
            .collect();
        let h = hop_latency("x", &samples).unwrap();
        assert!(
            h.drift_ms_per_min.abs() < 1e-6,
            "constant latency = zero drift"
        );
        assert!(h.jitter_ms.abs() < 1e-9);
    }

    #[test]
    fn lsq_slope_handles_degenerate_inputs() {
        assert_eq!(lsq_slope(&[]), 0.0);
        assert_eq!(lsq_slope(&[(1.0, 5.0)]), 0.0);
        // All x equal (zero variance): slope is undefined → 0.0, no NaN.
        assert_eq!(lsq_slope(&[(2.0, 1.0), (2.0, 9.0)]), 0.0);
    }

    #[test]
    fn empty_samples_yield_no_hop() {
        assert!(hop_latency("x", &[]).is_none());
    }
}
