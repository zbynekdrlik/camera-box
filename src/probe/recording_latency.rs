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
//! Both stamps are the RAW wall-clock read at paint/render — the cam2 painter stamps
//! `clock_ns()` (raw `SystemTime::now`, NOT the pacing boundary it sleeps to), and the
//! #111 burn's `burn_clock::gen_ts_ns()` returns the raw `wall_now_ns()` (NOT boundary-
//! snapped). Sharing the RAW basis is what makes cam→strih genuinely bias-free: a snapped burn
//! against a raw cam2 stamp would inject a systematic ~½-frame (~16.7 ms @ 30 fps) offset
//! plus up to a full-frame of quantization jitter (finding #2). The EMIT timecode the
//! genlock fork puts on the outgoing NDI frame (`ndi-output.cpp`) stays boundary-snapped —
//! that path is unrelated to the QR burn stamp and is unchanged.
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

use crate::probe::analyzer::LatencyStats;
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

/// How a recorded frame's QR payloads are classified into the cam2 (optical
/// content) stamp and THIS node's burn stamp.
///
/// CRITICAL for strih→stream: the STREAM recording carries THREE QRs — cam2's
/// (center, forwarded from strih's program), strih's burn (bottom, also forwarded),
/// and stream's own burn (bottom). The classifier MUST tell cam2 apart from the
/// FOREIGN strih burn, or strih's burn would be misread as the "cam2 tick" and the
/// strih→stream pairing would be nonsense. So cam2 is identified by a POSITIVE
/// allowlist (`cam2_run_id`) whenever known, and the foreign burn run_ids are
/// excluded; only when `cam2_run_id` is `None` (operator didn't pin it) does it fall
/// back to "any non-burn payload is cam2", which is safe ONLY when no foreign burn is
/// present (e.g. the strih recording, which has no stream burn).
#[derive(Debug, Clone)]
pub struct RunIds {
    /// THIS node's burn run_id (the stamp whose gen_ts_ns is this node's render time).
    pub node_burn: u32,
    /// cam2's painter run_id, if the operator pinned it. `Some` ⇒ cam2 is matched
    /// EXACTLY by this run_id (foreign burns can never be mistaken for cam2). `None`
    /// ⇒ cam2 = the first payload that is neither `node_burn` nor in `other_burns`.
    pub cam2: Option<u32>,
    /// Other (foreign) burn run_ids present in the recording that are NOT cam2 — e.g.
    /// in the stream recording, strih's burn run_id. Excluded from the cam2 fallback
    /// so a forwarded foreign burn is never misread as cam2.
    pub other_burns: Vec<u32>,
}

impl RunIds {
    /// Is `run_id` a known burn stamp (this node's or a foreign node's)?
    fn is_any_burn(&self, run_id: u32) -> bool {
        run_id == self.node_burn || self.other_burns.contains(&run_id)
    }
    /// Does `run_id` qualify as cam2? Exact match when pinned; otherwise any non-burn.
    fn is_cam2(&self, run_id: u32) -> bool {
        match self.cam2 {
            Some(c) => run_id == c,
            None => !self.is_any_burn(run_id),
        }
    }
}

/// Split a recorded frame's payloads into `(cam2 payload, this-node burn payload)`,
/// classified per [`RunIds`].
///
/// cam2 selection is the **canonical Vernier tick**: the cam2 painter emits a DUAL-QR
/// Vernier — TWO cam2 QRs per frame, same `run_id`, DIFFERENT `frame_id` (left = latest
/// even tick, right = latest odd tick). The two halves share one `gen_ts_ns` (the paint
/// instant), but they do NOT carry the same `frame_id`, and across a refresh straddle a
/// recorded frame can show one fresh half + one settled half. The canonical tick is
/// therefore `max_by_key(frame_id)` — IDENTICAL to [`crate::probe::qr::decode_capture_dual`]
/// and [`crate::probe::recording::RecordingFrame::tick`]. Taking the FIRST cam2 payload
/// rqrr happens to return is WRONG: rqrr grid order is not stable across two independent
/// MKVs, so the strih and stream recordings could key on different halves of the SAME
/// optical instant and never pair (finding #1). Selecting the max-frame_id half makes both
/// sides agree on one tick per optical instant.
///
/// The node burn is the FIRST payload matching `ids.node_burn` (the burn filter emits
/// exactly one burn QR per render, so there is only ever one to find).
pub fn split_payloads(frame: &RecordingFrame, ids: &RunIds) -> (Option<Payload>, Option<Payload>) {
    let mut node: Option<Payload> = None;
    let mut cam2: Option<Payload> = None;
    for p in &frame.payloads {
        if p.run_id == ids.node_burn {
            if node.is_none() {
                node = Some(*p);
            }
        } else if ids.is_cam2(p.run_id) {
            // Canonical Vernier tick: keep the cam2 half with the HIGHEST frame_id
            // (the freshly-painted half), matching the dual-decode path everywhere.
            match cam2 {
                Some(c) if c.frame_id >= p.frame_id => {}
                _ => cam2 = Some(*p),
            }
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
/// never a wrong number. `ids.node_burn` is strih's burn run_id; the strih recording
/// has no foreign burn, so `ids.cam2`/`other_burns` may be left default.
pub fn cam_strih_samples(strih: &[RecordingFrame], ids: &RunIds) -> Vec<LatencySample> {
    let mut out = Vec::new();
    for f in strih {
        let (cam2, node) = split_payloads(f, ids);
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

/// cam2→cam1 OPTICAL+GRAB latency samples (#105 node 2) — REAL, available WITHOUT
/// the #111 burn.
///
/// cam1's grab recording carries ONLY the cam2 painter QR (cam1 IS the camera —
/// there is no node-burn in cam1's grab). Its `gen_ts_ns` is the cam2 PAINT instant
/// (wall clock). cam1's OWN grab instant is in the grab-timestamp SIDECAR the
/// `--record-grab` mode writes (`frame_index → grab_ts_ns`, same wall clock). So for
/// each decoded cam1 grab frame:
///
///   `latency = cam1_grab_ts_ns[frame_index] − cam2_paint.gen_ts_ns`
///
/// anchored at the cam2 paint instant. Both stamps are CLOCK_REALTIME (DanteSync),
/// so this is a true absolute number — the optical + capture latency of the camera
/// filming cam2's monitor. NO #111 burn is needed (cam1 is not an OBS node).
///
/// A frame missing its cam2 QR, missing a sidecar grab_ts, or with a non-positive
/// stamp is skipped (never a wrong number). `cam2_run_id` pins cam2 exactly; on the
/// cam1 grab there is no foreign burn, so `None` (any non-burn = cam2) is also safe.
pub fn cam2_cam1_samples(
    cam1_frames: &[RecordingFrame],
    grab_ts_by_index: &HashMap<u64, i64>,
    cam2_run_id: Option<u32>,
) -> Vec<LatencySample> {
    let ids = RunIds {
        node_burn: 0, // cam1 grab has no node burn; 0 matches nothing real
        cam2: cam2_run_id,
        other_burns: vec![],
    };
    let mut out = Vec::new();
    for f in cam1_frames {
        let (cam2, _node) = split_payloads(f, &ids);
        if let Some(c) = cam2 {
            if let Some(&grab) = grab_ts_by_index.get(&f.frame_index) {
                // Both wall-clock; guard the unstamped 0 sentinel on either side so a
                // missing stamp can't read as a giant latency.
                if c.gen_ts_ns > 0 && grab > 0 {
                    out.push(LatencySample {
                        latency_ms: (grab - c.gen_ts_ns) as f64 / 1_000_000.0,
                        at_ns: c.gen_ts_ns,
                    });
                }
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
/// Pairing by the CANONICAL cam2 tick (NOT by capture position) makes this offset-immune:
/// the two independent recordings never start on the same camera frame and rqrr returns the
/// two dual-QR halves in a different order across the two MKVs, but the canonical tick
/// (`max_by_key(frame_id)`, per [`split_payloads`]) is identical at both outputs for one
/// optical instant. A tick present in only one recording is tap start/stop skew (or a real
/// drop, already caught by the #107 loss verdict) and contributes no latency sample.
///
/// Honesty (finding #4): each side reduces a canonical tick to ONE node-render stamp via the
/// deterministic earliest-render rule in [`burn_by_cam2_tick`] (same rule on both sides, so
/// no capture-position bias). The 60→30 camera oversample can still place the two outputs'
/// captures on different members of a tick's oversampled cluster; any residual beat that the
/// recordings alone cannot resolve is the SAME oversample #107 isolates — it is surfaced as
/// jitter/drift on the hop, never absorbed into the p50 nor claimed as sub-frame precision
/// the data does not back.
pub fn strih_stream_samples(
    strih: &[RecordingFrame],
    stream: &[RecordingFrame],
    strih_ids: &RunIds,
    stream_ids: &RunIds,
) -> Vec<LatencySample> {
    // Map canonical cam2 tick → this node's EARLIEST burn gen_ts for that tick (the
    // deterministic per-tick representative, same rule both sides — see
    // burn_by_cam2_tick). The stream side uses stream_ids, which MUST list strih's burn
    // in `other_burns` so the forwarded strih burn in stream's frames is never misread
    // as cam2.
    let strih_by_tick: HashMap<u32, i64> = burn_by_cam2_tick(strih, strih_ids);
    let stream_by_tick: HashMap<u32, i64> = burn_by_cam2_tick(stream, stream_ids);

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

/// Build `canonical cam2 tick → this node's burn gen_ts_ns` for one recording.
///
/// The key is the canonical Vernier tick from [`split_payloads`] (`max_by_key(frame_id)`),
/// so strih and stream key on the SAME tick for the SAME optical instant even though rqrr
/// returns the two dual-QR halves in a different order across the two MKVs (finding #1).
///
/// Per-tick representative (finding #4 — the 60→30 oversample): the camera paints at the
/// genlock 30 fps grid but a single optical tick can be captured by more than one recorded
/// frame on each side (the 60→30 beat #107 separates). To keep the per-tick choice
/// DETERMINISTIC and the strih/stream pairing honest, we take the EARLIEST node render
/// (smallest `gen_ts_ns`) for each canonical tick on each side — the first instant this
/// node put that optical content on the wire — using an explicit `min` keep, NOT
/// "whichever frame rqrr decoded first". Choosing the same representative rule on both
/// sides removes capture-position bias; any residual beat that cannot be resolved from the
/// recordings alone is the same oversample #107 isolates and is reported honestly, never
/// hidden inside a percentile. Skips frames missing either stamp or with a non-positive
/// burn stamp.
fn burn_by_cam2_tick(frames: &[RecordingFrame], ids: &RunIds) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for f in frames {
        let (cam2, node) = split_payloads(f, ids);
        if let (Some(c), Some(n)) = (cam2, node) {
            if n.gen_ts_ns > 0 {
                m.entry(c.frame_id)
                    .and_modify(|ts| {
                        if n.gen_ts_ns < *ts {
                            *ts = n.gen_ts_ns;
                        }
                    })
                    .or_insert(n.gen_ts_ns);
            }
        }
    }
    m
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

    /// strih-side ids: strih burn is this node; cam2 pinned; no foreign burn.
    fn strih_ids() -> RunIds {
        RunIds {
            node_burn: BURN_RUN_ID_STRIH,
            cam2: Some(CAM2),
            other_burns: vec![],
        }
    }
    /// stream-side ids: stream burn is this node; cam2 pinned; strih burn is FOREIGN
    /// (forwarded into stream's frames) and must be excluded from the cam2 match.
    fn stream_ids() -> RunIds {
        RunIds {
            node_burn: BURN_RUN_ID_STREAM,
            cam2: Some(CAM2),
            other_burns: vec![BURN_RUN_ID_STRIH],
        }
    }

    #[test]
    fn cam2_cam1_latency_is_grab_minus_paint_both_wall_clock() {
        // cam1 grab frames carry ONLY the cam2 painter QR (gen_ts = PAINT instant).
        // cam1's grab instant comes from the sidecar (frame_index → grab_ts). The
        // optical+grab latency is grab_ts − paint_gen_ts, both wall clock. No #111 burn.
        let cam1 = vec![
            frame(0, Some((CAM2, 100, 1_000_000_000)), None),
            frame(1, Some((CAM2, 102, 1_033_000_000)), None),
        ];
        let mut grab: HashMap<u64, i64> = HashMap::new();
        grab.insert(0, 1_050_000_000); // +50 ms after paint
        grab.insert(1, 1_073_000_000); // +40 ms after paint
        let s = cam2_cam1_samples(&cam1, &grab, Some(CAM2));
        assert_eq!(s.len(), 2);
        assert!((s[0].latency_ms - 50.0).abs() < 1e-6, "frame 0 = +50 ms");
        assert!((s[1].latency_ms - 40.0).abs() < 1e-6, "frame 1 = +40 ms");
        assert_eq!(s[0].at_ns, 1_000_000_000, "anchored at the paint instant");
    }

    #[test]
    fn cam2_cam1_skips_frame_with_no_sidecar_grab_ts_or_no_cam2() {
        // A cam1 frame with no decoded cam2 QR, or with no sidecar grab_ts for its
        // index, or a 0 sentinel stamp, contributes NO sample (never a wrong number).
        let cam1 = vec![
            frame(0, Some((CAM2, 100, 1_000_000_000)), None), // has grab below
            frame(1, None, None),                             // no cam2 QR → skip
            frame(2, Some((CAM2, 104, 1_066_000_000)), None), // no sidecar entry → skip
            frame(3, Some((CAM2, 106, 0)), None),             // 0 paint sentinel → skip
        ];
        let mut grab: HashMap<u64, i64> = HashMap::new();
        grab.insert(0, 1_050_000_000);
        grab.insert(1, 9); // present but frame 1 has no cam2
        grab.insert(3, 1_100_000_000);
        let s = cam2_cam1_samples(&cam1, &grab, Some(CAM2));
        assert_eq!(s.len(), 1, "only frame 0 yields a valid sample");
        assert!((s[0].latency_ms - 50.0).abs() < 1e-6);
    }

    #[test]
    fn split_separates_node_from_cam2_by_run_id() {
        let f = frame(
            0,
            Some((CAM2, 100, 1_000)),
            Some((BURN_RUN_ID_STRIH, 7, 2_000)),
        );
        let (cam2, node) = split_payloads(&f, &strih_ids());
        assert_eq!(cam2.unwrap().run_id, CAM2);
        assert_eq!(node.unwrap().run_id, BURN_RUN_ID_STRIH);
        assert_eq!(node.unwrap().gen_ts_ns, 2_000);
    }

    #[test]
    fn split_node_absent_when_no_burn_run_id() {
        // A production (non-probe) frame: only cam2, no burn.
        let f = frame(0, Some((CAM2, 100, 1_000)), None);
        let (cam2, node) = split_payloads(&f, &strih_ids());
        assert!(cam2.is_some());
        assert!(node.is_none());
    }

    #[test]
    fn split_excludes_foreign_burn_from_cam2_in_stream_frame() {
        // CRITICAL: a STREAM frame carries cam2 (center) + strih's FOREIGN burn
        // (911002, forwarded) + stream's own burn (911004). With cam2 pinned and
        // strih's burn in other_burns, split must pick the REAL cam2 — never strih's
        // burn — as cam2, and stream's burn as the node stamp.
        let mut payloads = vec![
            Payload {
                run_id: BURN_RUN_ID_STRIH, // foreign strih burn forwarded into stream
                frame_id: 11,
                gen_ts_ns: 50,
            },
            Payload {
                run_id: CAM2, // the real cam2 content
                frame_id: 777,
                gen_ts_ns: 10,
            },
            Payload {
                run_id: BURN_RUN_ID_STREAM, // stream's own burn
                frame_id: 22,
                gen_ts_ns: 90,
            },
        ];
        // Order it so the foreign burn comes FIRST (the old "first non-burn" logic
        // would have wrongly returned it before the cam2 if it weren't excluded).
        payloads.rotate_left(0);
        let f = RecordingFrame {
            frame_index: 0,
            payloads,
            tick: Some(777),
        };
        let (cam2, node) = split_payloads(&f, &stream_ids());
        assert_eq!(
            cam2.unwrap().run_id,
            CAM2,
            "must pick real cam2, not strih burn"
        );
        assert_eq!(cam2.unwrap().frame_id, 777);
        assert_eq!(node.unwrap().run_id, BURN_RUN_ID_STREAM);
    }

    #[test]
    fn split_fallback_excludes_burns_when_cam2_unpinned() {
        // With cam2 = None, the foreign strih burn must STILL be excluded (it's in
        // other_burns) so the fallback "first non-burn" picks cam2, not strih's burn.
        let f = RecordingFrame {
            frame_index: 0,
            payloads: vec![
                Payload {
                    run_id: BURN_RUN_ID_STRIH,
                    frame_id: 11,
                    gen_ts_ns: 50,
                },
                Payload {
                    run_id: CAM2,
                    frame_id: 777,
                    gen_ts_ns: 10,
                },
                Payload {
                    run_id: BURN_RUN_ID_STREAM,
                    frame_id: 22,
                    gen_ts_ns: 90,
                },
            ],
            tick: Some(777),
        };
        let ids = RunIds {
            node_burn: BURN_RUN_ID_STREAM,
            cam2: None,
            other_burns: vec![BURN_RUN_ID_STRIH],
        };
        let (cam2, node) = split_payloads(&f, &ids);
        assert_eq!(cam2.unwrap().run_id, CAM2);
        assert_eq!(node.unwrap().run_id, BURN_RUN_ID_STREAM);
    }

    /// Build a frame with BOTH cam2 Vernier halves (same run_id + gen_ts_ns, DIFFERENT
    /// frame_id) plus a node burn, in the rqrr payload order given.
    /// `cam2 = (run_id, even_tick, odd_tick, gen_ts_ns)`, `node = (run_id, frame_id, gen_ts_ns)`.
    /// `halves_first`: true → cam2 halves before node; false → node first (swapped order).
    fn dual_cam2_frame(
        idx: u64,
        cam2: (u32, u32, u32, i64),
        node: (u32, u32, i64),
        halves_first: bool,
    ) -> RecordingFrame {
        let (cam2_run, even_tick, odd_tick, cam2_gen) = cam2;
        let (node_run, node_fid, node_gen) = node;
        let even = Payload {
            run_id: cam2_run,
            frame_id: even_tick,
            gen_ts_ns: cam2_gen,
        };
        let odd = Payload {
            run_id: cam2_run,
            frame_id: odd_tick,
            gen_ts_ns: cam2_gen,
        };
        let node = Payload {
            run_id: node_run,
            frame_id: node_fid,
            gen_ts_ns: node_gen,
        };
        let payloads = if halves_first {
            vec![even, odd, node]
        } else {
            // node first, then odd half, then even half — rqrr grid order varies per MKV.
            vec![node, odd, even]
        };
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index: idx,
            payloads,
            tick,
        }
    }

    #[test]
    fn split_picks_canonical_max_frame_id_cam2_half_both_orderings() {
        // The cam2 painter emits a dual Vernier: two QRs, same run_id+gen_ts_ns, DIFFERENT
        // frame_id (even tick vs odd tick). The canonical tick is max_by_key(frame_id) —
        // matching decode_capture_dual / RecordingFrame.tick. split_payloads must return
        // that half regardless of rqrr's payload order (finding #1).
        for halves_first in [true, false] {
            let f = dual_cam2_frame(
                0,
                (CAM2, 100, 101, 1_000),
                (BURN_RUN_ID_STRIH, 7, 2_000),
                halves_first,
            );
            let (cam2, node) = split_payloads(&f, &strih_ids());
            assert_eq!(
                cam2.unwrap().frame_id,
                101,
                "canonical cam2 tick must be max(frame_id)=101 (ordering halves_first={halves_first})"
            );
            assert_eq!(node.unwrap().run_id, BURN_RUN_ID_STRIH);
        }
    }

    #[test]
    fn cam_strih_latency_with_dual_cam2_halves_uses_canonical_tick() {
        // cam→strih over frames that each carry BOTH cam2 halves. gen_ts_ns is shared
        // across halves, so the latency is correct AND the anchor tick is the canonical
        // max-frame_id half.
        let off = 150_000_000i64; // 150 ms
        let base = 1_700_000_000_000_000_000i64;
        let frames: Vec<RecordingFrame> = (0..4u64)
            .map(|i| {
                let g = base + i as i64 * 33_333_333;
                // even tick 200+2i, odd tick 200+2i+1 -> canonical = odd.
                dual_cam2_frame(
                    i,
                    (CAM2, 200 + 2 * i as u32, 201 + 2 * i as u32, g),
                    (BURN_RUN_ID_STRIH, 5000 + i as u32, g + off),
                    i % 2 == 0, // alternate rqrr ordering frame to frame
                )
            })
            .collect();
        let samples = cam_strih_samples(&frames, &strih_ids());
        assert_eq!(samples.len(), 4);
        let h = hop_latency("cam→strih", &samples).unwrap();
        assert!(
            (h.stats.p50_ms - 150.0).abs() < 1e-6,
            "p50 {}",
            h.stats.p50_ms
        );
        assert!(h.jitter_ms.abs() < 1e-6);
    }

    #[test]
    fn strih_stream_pairs_same_optical_instant_despite_swapped_decode_order() {
        // CRITICAL (finding #1): the two independent MKVs decode the cam2 dual halves in
        // DIFFERENT rqrr order. If split took the FIRST cam2 payload, strih could key on
        // the even half while stream keyed on the odd half of the SAME optical instant —
        // and they would never pair. Selecting the canonical max-frame_id half makes both
        // sides key on the SAME tick, so the pair survives.
        let base = 1_700_000_000_000_000_000i64;
        let off = 40_000_000i64; // stream renders 40 ms after strih
                                 // Shared optical instants: even ticks 300,302,304 ; odd ticks 301,303,305.
                                 // Canonical (max) ticks = 301,303,305.
        let strih: Vec<RecordingFrame> = (0..3u64)
            .map(|i| {
                let g = base + i as i64 * 33_000_000;
                dual_cam2_frame(
                    i,
                    (CAM2, 300 + 2 * i as u32, 301 + 2 * i as u32, base - 1),
                    (BURN_RUN_ID_STRIH, 70 + i as u32, g),
                    true, // strih: cam2 halves first (even before odd)
                )
            })
            .collect();
        let stream: Vec<RecordingFrame> = (0..3u64)
            .map(|i| {
                let g = base + i as i64 * 33_000_000 + off;
                dual_cam2_frame(
                    i + 5, // different capture position
                    (CAM2, 300 + 2 * i as u32, 301 + 2 * i as u32, base - 1),
                    (BURN_RUN_ID_STREAM, 90 + i as u32, g),
                    false, // stream: SWAPPED rqrr order (node first, odd, even)
                )
            })
            .collect();
        let samples = strih_stream_samples(&strih, &stream, &strih_ids(), &stream_ids());
        assert_eq!(
            samples.len(),
            3,
            "all three shared canonical ticks must pair despite swapped decode order"
        );
        let h = hop_latency("strih→stream", &samples).unwrap();
        assert!(
            (h.stats.p50_ms - 40.0).abs() < 1e-6,
            "p50 {}",
            h.stats.p50_ms
        );
        assert!(h.jitter_ms.abs() < 1e-6);
    }

    #[test]
    fn burn_by_cam2_tick_takes_earliest_render_for_oversampled_tick() {
        // finding #4: a single canonical tick captured by TWO recorded frames (the 60→30
        // oversample) must reduce DETERMINISTICALLY to the EARLIEST node render, not
        // "whichever rqrr decoded first". Feed the later render FIRST and assert the map
        // keeps the earlier one.
        let base = 1_700_000_000_000_000_000i64;
        // SAME rqrr ordering on both frames so the canonical tick key is constant (this
        // isolates the earliest-render REDUCTION, independent of the canonical-tick fix):
        // both frames carry cam2 ticks 400/401 → canonical 401. Capture order feeds the
        // LATER render (base+20ms) FIRST; the deterministic min-keep must keep base+5ms.
        let frames = vec![
            dual_cam2_frame(
                0,
                (CAM2, 400, 401, base),
                (BURN_RUN_ID_STRIH, 10, base + 20_000_000),
                true,
            ),
            dual_cam2_frame(
                1,
                (CAM2, 400, 401, base),
                (BURN_RUN_ID_STRIH, 11, base + 5_000_000),
                true,
            ),
        ];
        let m = burn_by_cam2_tick(&frames, &strih_ids());
        assert_eq!(
            m.get(&401),
            Some(&(base + 5_000_000)),
            "per-tick representative must be the earliest node render, deterministically"
        );
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
        let samples = cam_strih_samples(&frames, &strih_ids());
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
        let samples = cam_strih_samples(&frames, &strih_ids());
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
        let samples = cam_strih_samples(&frames, &strih_ids());
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
        let samples = strih_stream_samples(&strih, &stream, &strih_ids(), &stream_ids());
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
        let samples = strih_stream_samples(&strih, &stream, &strih_ids(), &stream_ids());
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
