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
use crate::probe::recording_verdict::BurnHopVerdict;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Default reserved per-node burn run_ids (mirrors the #111 burn filter's
/// `BURN_RUN_ID_DEFAULT_STRIH` / `…_STREAM` in `vendor/distroav/src/ndi-burn-filter.cpp`).
/// Both are far outside cam2's normal run_id range so a node-stamp QR is told apart
/// from the cam2 QR by run_id alone. The binary lets the operator override these to
/// match a non-default `OBS_BURN_RUN_ID` on the box.
pub const BURN_RUN_ID_STRIH: u32 = 911002;
/// See [`BURN_RUN_ID_STRIH`].
pub const BURN_RUN_ID_STREAM: u32 = 911004;
/// The cam1-CAPTURE burn run_id (#174) — the value `CAMERA_BOX_BURN_RUN_ID` is set to on
/// cam1 for a TEST run. cam1's burn rides through NDI into strih's program and on into
/// stream's, so the single stream recording carries the cam1 burn alongside cam2's optical
/// QR + strih's + stream's burns; the verdict pairs the cam1→strih and full-chain hops on
/// this clean digital id. Distinct from the strih/stream burn ids so all marks are told
/// apart by run_id. The binary lets the operator override it to match the cam1 env.
pub const BURN_RUN_ID_CAM1: u32 = 911001;

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

/// cam2→cam1 OPTICAL-INJECTION latency samples from the STREAM recording ALONE (#179) —
/// REAL, CO-LOCATED, and WITHOUT decoding the 7.3GB cam1 grab.
///
/// This is the stream-only replacement for [`cam2_cam1_samples`] (which needs the cam1
/// grab recording + its grab-ts sidecar). The cam1-capture burn (#174) stamps cam1's
/// CAPTURE wall-clock instant into the EMITTED YUYV frame's `Payload.gen_ts_ns`
/// (run_id = `cam1_burn_id`), and that burn rides through NDI → strih → stream. The cam2
/// optical dual-QR cam1 FILMED rides in the SAME frame, carrying cam2's PAINT instant in
/// its OWN `gen_ts_ns` (the painter stamps the wall clock at paint, exactly like
/// [`cam_strih_samples`] reads cam2's stamp from the strih recording). So a single stream
/// frame carries, for one optical instant, BOTH stamps CO-LOCATED:
///
///   `latency = cam1_burn.gen_ts_ns (capture) − cam2_qr.gen_ts_ns (paint)`
///
/// anchored at the cam2 paint instant. Both are CLOCK_REALTIME (DanteSync), so this is the
/// true optical+capture latency of cam1 filming cam2's monitor — the same number
/// [`cam2_cam1_samples`] yields from the grab, but read from the stream recording's two
/// co-located stamps so the 6GB grab is never decoded.
///
/// CO-LOCATED, NOT keyed on an external painter CSV (the earlier #179 attempt): the
/// painter's tick counter restarts every painter run, so the painter CSV's `tick` does
/// NOT correspond to the stream's cam2 `frame_id` once the painter has cycled between the
/// CSV capture and the recording — pairing on it yields no matches or a ~stale-session
/// offset (hours). The cam2 QR's gen_ts is right there IN the stream frame next to the
/// cam1 burn, so we pair the two stamps that share the frame — robust, no cross-session
/// counter mismatch, no extra file.
///
/// cam2 selection: the pinned `cam2_id` when known (recommended — the stream recording
/// also carries forwarded strih/stream burns), else any payload that is not the cam1 burn
/// nor a known foreign forwarded burn in `other_burns`. The canonical cam2 half is the
/// highest-frame_id Vernier half (both halves share one paint `gen_ts_ns`), identical to
/// [`split_payloads`]. A frame missing its cam2 QR, missing the cam1 burn, or carrying a
/// non-positive stamp on either is skipped — never a wrong number.
pub fn cam2_cam1_samples_from_burn(
    stream: &[RecordingFrame],
    cam2_id: Option<u32>,
    cam1_burn_id: u32,
    other_burns: &[u32],
) -> Vec<LatencySample> {
    let is_cam2 = |run_id: u32| match cam2_id {
        Some(c) => run_id == c,
        // No pin: cam2 = any payload that is not the cam1 burn and not a known foreign
        // forwarded burn (strih/stream) — so a forwarded burn can never hijack the
        // canonical max(frame_id) cam2 half (mirrors chain_hop_loss_from_stream).
        None => run_id != cam1_burn_id && !other_burns.contains(&run_id),
    };
    let mut out = Vec::new();
    for f in stream {
        // Co-located in ONE frame: cam1 burn (capture ts) + cam2 QR (paint ts). The
        // canonical cam2 half is the highest-frame_id Vernier half; both halves share the
        // paint gen_ts, so the chosen half's gen_ts is the paint instant.
        let mut cam2_paint: Option<i64> = None;
        let mut cam2_fid: Option<u32> = None;
        let mut cam1_cap: Option<i64> = None;
        for p in &f.payloads {
            if p.run_id == cam1_burn_id {
                if p.gen_ts_ns > 0 && cam1_cap.is_none() {
                    cam1_cap = Some(p.gen_ts_ns);
                }
            } else if is_cam2(p.run_id) {
                match cam2_fid {
                    Some(t) if t >= p.frame_id => {}
                    _ => {
                        cam2_fid = Some(p.frame_id);
                        cam2_paint = Some(p.gen_ts_ns);
                    }
                }
            }
        }
        if let (Some(cap), Some(paint)) = (cam1_cap, cam2_paint) {
            // Both wall-clock; guard the 0 sentinel so a missing stamp can't read huge.
            if cap > 0 && paint > 0 {
                out.push(LatencySample {
                    latency_ms: (cap - paint) as f64 / 1_000_000.0,
                    at_ns: paint,
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

/// strih→stream per-hop latency from the STREAM recording ALONE (#111 PART A).
///
/// The whole point of the #111 render-time burn is that the STREAM recording self-contains
/// every stamp needed for the strih→stream hop: strih burns its render timestamp into the
/// program it sends, that burn RIDES THROUGH into stream's program, and stream burns its OWN
/// render timestamp on top. So a single stream-recorded frame carries, for one optical
/// instant: cam2's QR (forwarded, for the canonical tick key) + strih's burn (forwarded) +
/// stream's burn. The hop is then `stream_burn.gen_ts_ns − strih_burn.gen_ts_ns` on the
/// shared DanteSync wall clock — NO separate strih recording, NO network record_start, NO
/// idx/30. This is the dispatch's "whole per-hop analysis from the single stream recording".
///
/// Pairing key is the canonical cam2 Vernier tick (`max_by_key(frame_id)` half), identical to
/// [`burn_by_cam2_tick`] / [`split_payloads`], so the strih-burn and stream-burn for the SAME
/// optical instant are matched even though rqrr returns the QRs in arbitrary grid order. Per
/// tick the EARLIEST (smallest `gen_ts_ns`) burn on EACH side is the deterministic
/// representative (the 60→30 oversample can place one tick on several recorded frames) — the
/// same min-keep rule [`burn_by_cam2_tick`] uses, so no capture-position bias. A frame missing
/// either burn, the cam2 key, or carrying a non-positive stamp is skipped — never a wrong
/// number. Requires the strih burn to be FORWARDED into the stream program (the #111 PROBE
/// scene chains strih's burned program into stream); when it is not present the result is
/// empty and the caller falls back to [`strih_stream_samples`] (the two-recording method).
pub fn strih_stream_samples_from_stream(
    stream: &[RecordingFrame],
    cam2_id: Option<u32>,
    strih_burn_id: u32,
    stream_burn_id: u32,
) -> Vec<LatencySample> {
    // Per canonical cam2 tick, the earliest strih-burn and earliest stream-burn seen in
    // stream's own frames. cam2 selection: pinned id if given, else any non-burn payload.
    let is_cam2 = |run_id: u32| match cam2_id {
        Some(c) => run_id == c,
        None => run_id != strih_burn_id && run_id != stream_burn_id,
    };
    let mut strih_by_tick: HashMap<u32, i64> = HashMap::new();
    let mut stream_by_tick: HashMap<u32, i64> = HashMap::new();
    for f in stream {
        // Canonical cam2 tick = the highest-frame_id cam2 half in this frame.
        let mut cam2_tick: Option<u32> = None;
        let mut strih_ts: Option<i64> = None;
        let mut stream_ts: Option<i64> = None;
        for p in &f.payloads {
            if p.run_id == strih_burn_id {
                if p.gen_ts_ns > 0 && strih_ts.is_none() {
                    strih_ts = Some(p.gen_ts_ns);
                }
            } else if p.run_id == stream_burn_id {
                if p.gen_ts_ns > 0 && stream_ts.is_none() {
                    stream_ts = Some(p.gen_ts_ns);
                }
            } else if is_cam2(p.run_id) {
                cam2_tick = Some(match cam2_tick {
                    Some(t) if t >= p.frame_id => t,
                    _ => p.frame_id,
                });
            }
        }
        let Some(tick) = cam2_tick else { continue };
        if let Some(ts) = strih_ts {
            strih_by_tick
                .entry(tick)
                .and_modify(|e| {
                    if ts < *e {
                        *e = ts;
                    }
                })
                .or_insert(ts);
        }
        if let Some(ts) = stream_ts {
            stream_by_tick
                .entry(tick)
                .and_modify(|e| {
                    if ts < *e {
                        *e = ts;
                    }
                })
                .or_insert(ts);
        }
    }

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

/// All burn `frame_id`s for `run_id` decoded across `frames`, in capture order (#174).
///
/// The render-tick id a node stamps is the clean digital key the burn-id hop loss verdict
/// ([`crate::probe::recording_verdict::burn_hop_verdict`]) pairs on. From the SINGLE stream
/// recording this extracts, per `run_id`, the sequence of burn ids that node rendered and
/// that reached the stream output — so cam1→strih and strih→stream loss are both decided
/// from one recording, on the same integer end-to-end, with no 60→30 optical-beat ambiguity.
/// A frame may carry several QRs; only payloads matching `run_id` are taken.
pub fn burn_ids_in(frames: &[RecordingFrame], run_id: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for f in frames {
        for p in &f.payloads {
            if p.run_id == run_id {
                out.push(p.frame_id);
            }
        }
    }
    out
}

/// Per-hop ABSOLUTE latency from the SINGLE stream recording, paired on the two burns
/// CO-LOCATED in one recorded frame (#174). For each stream frame carrying BOTH the
/// upstream burn (`up_run_id`) and the downstream burn (`down_run_id`) — both forwarded
/// into the stream program for the SAME optical instant — the sample is
/// `down_burn.gen_ts_ns − up_burn.gen_ts_ns` on the shared DanteSync wall clock, anchored
/// at the upstream render instant. Because the two stamps live in the SAME frame there is
/// NO cross-recording / cam2-tick pairing, so the per-cluster sampling ambiguity that blew
/// up the strih→stream p99 to 3.4 s (while p50 was ~178 ms) cannot occur. A frame missing
/// either burn, or carrying a non-positive stamp, is skipped — never a wrong number.
/// Works for cam1→strih (`up=cam1, down=strih`) and strih→stream (`up=strih, down=stream`).
pub fn chain_hop_samples_from_stream(
    stream: &[RecordingFrame],
    up_run_id: u32,
    down_run_id: u32,
) -> Vec<LatencySample> {
    let mut out = Vec::new();
    for f in stream {
        let mut up_ts: Option<i64> = None;
        let mut down_ts: Option<i64> = None;
        for p in &f.payloads {
            if p.run_id == up_run_id && p.gen_ts_ns > 0 && up_ts.is_none() {
                up_ts = Some(p.gen_ts_ns);
            } else if p.run_id == down_run_id && p.gen_ts_ns > 0 && down_ts.is_none() {
                down_ts = Some(p.gen_ts_ns);
            }
        }
        if let (Some(u), Some(d)) = (up_ts, down_ts) {
            out.push(LatencySample {
                latency_ms: (d - u) as f64 / 1_000_000.0,
                at_ns: u,
            });
        }
    }
    out
}

/// #175: a flagged cam2 tick is a single-frame BURN-DECODE MISS, NOT a real hop drop,
/// when the recorded frame for that optical instant DEMONSTRABLY EXISTED (the absent node
/// rendered it) and only its small corner burn QR failed to decode in that one 4K-scaled
/// recorded frame. Returns the subset of `flagged` ticks to EXCLUDE from the loss count.
///
/// The discriminator (run 172046073, forensically established): a flagged tick is a decode
/// miss iff, between the nearest cam2 ticks that DID decode the absent burn (`lo`/`hi`),
/// EVERY recorded cam2 tick is itself a flagged (missing-burn) tick — i.e. the gap in the
/// absent burn's decoded sequence is fully explained by recorded frames that exist but
/// whose burn didn't decode, with NO extra missing optical instant, AND the absent burn's
/// own counter is strictly monotonic across the gap (no reorder). A REAL drop is different
/// in kind: an outright dropped optical instant leaves NO recorded cam2 frame at all (the
/// cam2 tick is absent from `recorded_ticks`, so the gap contains an unflagged "hole"), or
/// it spans more than `max_run` consecutive recorded frames (a sustained skip, not a
/// momentary decode glitch). Either case fails the test and the tick is KEPT as a loss.
///
/// Why the previous absolute-counter-step test was wrong: when several ADJACENT frames all
/// miss the same burn, the nearest DECODED burns are 2-3 frames apart and the raw counter
/// step (6-9) looks like a real skip even though every frame in the gap physically exists
/// (run 172046073 ticks 3720/18324). Counting RECORDED frames in the gap — not the counter
/// step — is the rate-free, node-agnostic invariant: the cam2 tick is the shared clock, so
/// "is there a recorded frame for every optical instant in the gap?" answers "did the node
/// render it?" without needing each node's burn rate.
///
/// Inputs: `flagged` = the dropped (or phantom) ticks; `absent_decoded` = the cam2 ticks
/// where the ABSENT burn WAS decoded (with its frame_id, for the monotonicity check);
/// `recorded_ticks` = EVERY cam2 tick any recorded frame carried; `max_run` = the longest
/// run of consecutive decode-miss frames still treated as a glitch (beyond it, a sustained
/// absence is a real drop). Conservative: an edge tick (no `lo`/`hi`), a gap with an
/// unflagged hole, a non-monotonic counter, or a run longer than `max_run` is KEPT — only a
/// proven short, fully-flagged, monotonic run is excluded. Cannot hide a real drop.
pub fn decode_miss_ticks(
    flagged: &[u32],
    absent_decoded: &BTreeMap<u32, u32>,
    recorded_ticks: &BTreeSet<u32>,
    max_run: usize,
) -> BTreeSet<u32> {
    let flagged_set: BTreeSet<u32> = flagged.iter().copied().collect();
    let mut excluded = BTreeSet::new();
    for &t in flagged {
        // Nearest cam2 ticks that DID decode the absent burn, bracketing this tick.
        let lo = absent_decoded.range(..t).next_back();
        let hi = absent_decoded.range((t + 1)..).next();
        let (Some((&lo_t, &lo_c)), Some((&hi_t, &hi_c))) = (lo, hi) else {
            continue; // edge tick — cannot prove both-sided existence → KEEP as loss
        };
        if hi_c <= lo_c {
            continue; // non-monotonic absent-burn counter (reorder) → KEEP as loss
        }
        // Every recorded cam2 tick strictly between the bracketing decoded burns must be a
        // flagged (decode-miss) tick — no unflagged hole = no truly-missing optical instant.
        let between: Vec<u32> = recorded_ticks.range((lo_t + 1)..hi_t).copied().collect();
        if between.is_empty() || between.len() > max_run {
            continue; // no frames between (shouldn't happen) or a sustained run → KEEP
        }
        if between.iter().all(|c| flagged_set.contains(c)) {
            excluded.insert(t);
        }
    }
    excluded
}

/// #175: the longest run of CONSECUTIVE recorded decode-miss frames still treated as a
/// momentary 4K-decode glitch rather than a real sustained drop. A single missed burn QR
/// (run 172046073: every gap was 1 recorded frame) is the norm; `3` tolerates a brief
/// 2-3-frame soft patch (compression/scaling) without masking a real multi-frame drop.
const DECODE_MISS_MAX_RUN: usize = 3;

/// Per-hop LOSS verdict from the SINGLE stream recording, paired on the SHARED cam2
/// source tick every frame carries (#181) — NOT each node's independent burn counter.
///
/// Why the burn counter is the WRONG key: each node ([`burn_ids_in`] over `up_run_id`
/// vs `down_run_id`) stamps its OWN monotonic burn `frame_id`, started from its OWN
/// per-node default (cam1=911001-seq, strih=911002-seq, stream=911004-seq) and counting
/// only THAT node's renders. Across two nodes the two id ranges do not coincide, so a
/// set-equality overlap (`[max(first), min(last)]`) is empty ⇒ `compared_ids=0` and the
/// hop loss is uncomputable — the exact symptom of #181 (burns present, `compared_ids=0`).
///
/// The fix: key on the cam2 source tick — the SAME key the cam2-tick latency pairing
/// uses ([`strih_stream_samples_from_stream`]). (The co-located latency
/// [`chain_hop_samples_from_stream`] instead pairs the two burns WITHIN one frame; loss
/// keys on the cam2 tick across frames so the two endpoints' burns for one optical
/// instant still pair even when the 60→30 beat splits them across recorded frames.)
/// Every recorded stream frame carries cam2's optical dual-QR tick alongside the
/// forwarded burns, so the cam2 tick is the common integer present on BOTH endpoints'
/// frames. The `gen_ts_ns > 0` presence per cam2 tick is collected into two SETS over ALL
/// frames, then handed to the shared [`crate::probe::recording_verdict::overlap_set_verdict`]:
/// - a hop "survived" (in `compared_ids`) iff the cam2 tick carries BOTH burns,
/// - "dropped" = the cam2 tick carries the upstream burn but not the downstream burn,
/// - "phantom" = it carries the downstream burn but not the upstream burn,
///
/// all over the overlap span `[max(first), min(last)]` so record start/stop skew is excluded.
///
/// cam2 selection: the pinned `cam2_id` when known, else any payload that is neither the
/// upstream nor the downstream burn AND not in `other_burns`. **`other_burns` MUST list
/// every OTHER forwarded burn run_id present in the recording** (e.g. for strih→stream
/// the forwarded cam1 burn) — otherwise, when `cam2_id` is `None`, that foreign burn is
/// misread as a cam2 half and its `frame_id` hijacks the `max(frame_id)` canonical tick
/// (#181 review), corrupting the key. This mirrors [`RunIds::other_burns`] in the
/// per-frame split. Works for cam1→strih (`up=cam1_burn, down=strih_burn`,
/// `other_burns=[stream_burn]`) and strih→stream (`up=strih_burn, down=stream_burn`,
/// `other_burns=[cam1_burn]`).
///
/// LIMITATION (honest scope, #181 review): a frame a node DROPS outright loses BOTH that
/// node's burn AND every burn forwarded through it for that cam2 tick, so the tick lands
/// in NEITHER set and is invisible (not counted as dropped). The from-stream method
/// therefore detects partial-presence faults (one endpoint's burn present, the other's
/// absent on the same cam2 tick) and the overlap span; an outright dropped optical instant
/// shows up instead in the per-recording continuity [`crate::probe::recording_verdict::verdict`]
/// (its `real_gap` / backward-jump) and the cam2→cam1 / painter optical assessment — the
/// same scope boundary the from-stream latency pairing has.
pub fn chain_hop_loss_from_stream(
    hop: &str,
    stream: &[RecordingFrame],
    cam2_id: Option<u32>,
    up_run_id: u32,
    down_run_id: u32,
    other_burns: &[u32],
) -> BurnHopVerdict {
    let is_cam2 = |run_id: u32| match cam2_id {
        Some(c) => run_id == c,
        // No pin: cam2 = any payload that is not THIS hop's two burns and not a known
        // foreign forwarded burn (#181 review — keeps the forwarded cam1/stream burn out
        // of the cam2 fallback so it cannot hijack the canonical tick).
        None => run_id != up_run_id && run_id != down_run_id && !other_burns.contains(&run_id),
    };
    // Per canonical cam2 tick, did THIS recording carry the upstream / downstream burn?
    // A burn counts only when stamped (gen_ts_ns > 0) — an unstamped QR is not a render.
    // We ALSO record each node's OWN burn `frame_id` at the cam2 tick where it WAS decoded
    // (#175) so a flagged tick can be checked for burn-counter continuity: a contiguous
    // straddle proves the node rendered through the gap and the absence is a decode miss.
    let mut up_ticks: BTreeSet<u32> = BTreeSet::new();
    let mut down_ticks: BTreeSet<u32> = BTreeSet::new();
    let mut up_counter: BTreeMap<u32, u32> = BTreeMap::new();
    let mut down_counter: BTreeMap<u32, u32> = BTreeMap::new();
    // EVERY cam2 tick any recorded frame carried (#175): a real dropped optical instant
    // leaves NO recorded cam2 frame, so a gap in the absent burn's decoded sequence that is
    // NOT fully covered by recorded cam2 ticks is a real drop, not a decode miss.
    let mut recorded_ticks: BTreeSet<u32> = BTreeSet::new();
    for f in stream {
        let mut cam2_tick: Option<u32> = None;
        let mut up_fid: Option<u32> = None;
        let mut down_fid: Option<u32> = None;
        for p in &f.payloads {
            if p.run_id == up_run_id {
                if p.gen_ts_ns > 0 {
                    up_fid = Some(p.frame_id);
                }
            } else if p.run_id == down_run_id {
                if p.gen_ts_ns > 0 {
                    down_fid = Some(p.frame_id);
                }
            } else if is_cam2(p.run_id) {
                // Canonical cam2 tick = the highest-frame_id cam2 half in this frame.
                cam2_tick = Some(match cam2_tick {
                    Some(t) if t >= p.frame_id => t,
                    _ => p.frame_id,
                });
            }
        }
        let Some(tick) = cam2_tick else { continue };
        recorded_ticks.insert(tick);
        if let Some(fid) = up_fid {
            up_ticks.insert(tick);
            // Keep the earliest counter per tick (an oversampled tick repeats the frame).
            up_counter.entry(tick).or_insert(fid);
        }
        if let Some(fid) = down_fid {
            down_ticks.insert(tick);
            down_counter.entry(tick).or_insert(fid);
        }
    }

    // Shared span/drop/phantom arithmetic — identical semantics to burn_hop_verdict, so
    // the two loss paths can never diverge (#181 review).
    let mut v = crate::probe::recording_verdict::overlap_set_verdict(hop, &up_ticks, &down_ticks);

    // #175 hardening: drop the single-frame BURN-DECODE MISSES from the loss set so the
    // strict gate reflects REAL hop drops, not 4K corner-QR decode noise. A "dropped" tick
    // (down burn absent) is a decode miss when the DOWNSTREAM burn's decoded sequence skips
    // ONLY recorded-and-flagged frames across it; a "phantom" tick (up burn absent) when the
    // UPSTREAM burn's does. The recorded-frame-coverage test can only remove false positives
    // — a real drop leaves an unflagged hole (no recorded cam2 frame) and is kept.
    let drop_excl = decode_miss_ticks(
        &v.dropped_ids,
        &down_counter,
        &recorded_ticks,
        DECODE_MISS_MAX_RUN,
    );
    let phantom_excl = decode_miss_ticks(
        &v.phantom_ids,
        &up_counter,
        &recorded_ticks,
        DECODE_MISS_MAX_RUN,
    );
    v.dropped_ids.retain(|t| !drop_excl.contains(t));
    v.phantom_ids.retain(|t| !phantom_excl.contains(t));
    v.compared_ids += drop_excl.len() + phantom_excl.len();
    v.decode_miss_excluded = drop_excl.len() + phantom_excl.len();
    v
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

    /// Build a stream-recorded frame carrying all three payloads the #111 PROBE chain
    /// puts into one stream frame: cam2 (forwarded), strih burn (forwarded), stream burn.
    fn stream_frame_3(
        idx: u64,
        cam2: Option<(u32, u32)>, // (run_id, frame_id/tick)
        strih_burn: Option<i64>,  // strih render gen_ts_ns
        stream_burn: Option<i64>, // stream render gen_ts_ns
    ) -> RecordingFrame {
        let mut payloads = Vec::new();
        if let Some((r, t)) = cam2 {
            payloads.push(Payload {
                run_id: r,
                frame_id: t,
                gen_ts_ns: 1, // cam2 paint stamp; not used by the strih→stream hop
            });
        }
        if let Some(g) = strih_burn {
            payloads.push(Payload {
                run_id: BURN_RUN_ID_STRIH,
                frame_id: idx as u32,
                gen_ts_ns: g,
            });
        }
        if let Some(g) = stream_burn {
            payloads.push(Payload {
                run_id: BURN_RUN_ID_STREAM,
                frame_id: idx as u32,
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

    #[test]
    fn strih_stream_from_stream_alone_is_stream_burn_minus_strih_burn_per_cam2_tick() {
        // The #111 PART-A deliverable: the stream recording ALONE carries the forwarded
        // strih burn + stream's own burn, paired by the forwarded cam2 tick. The hop is
        // stream_burn − strih_burn on the shared wall clock — no separate strih recording.
        let stream = vec![
            // cam2 tick 200: strih rendered at t=1.000s, stream at t=1.040s → 40 ms.
            stream_frame_3(
                0,
                Some((CAM2, 200)),
                Some(1_000_000_000),
                Some(1_040_000_000),
            ),
            // cam2 tick 202: strih t=1.033s, stream t=1.066s → 33 ms.
            stream_frame_3(
                1,
                Some((CAM2, 202)),
                Some(1_033_000_000),
                Some(1_066_000_000),
            ),
        ];
        let s = strih_stream_samples_from_stream(
            &stream,
            Some(CAM2),
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
        );
        assert_eq!(s.len(), 2, "both ticks pair strih+stream burn");
        assert!((s[0].latency_ms - 40.0).abs() < 1e-6, "tick 200 = +40 ms");
        assert!((s[1].latency_ms - 33.0).abs() < 1e-6, "tick 202 = +33 ms");
        assert_eq!(
            s[0].at_ns, 1_000_000_000,
            "anchored at the strih render instant"
        );
    }

    #[test]
    fn strih_stream_from_stream_skips_frames_missing_a_burn_or_cam2() {
        // A frame without the forwarded strih burn (the chain not wired) or without a
        // cam2 key contributes nothing — never a wrong number, the caller then falls
        // back to the two-recording method.
        let stream = vec![
            stream_frame_3(0, Some((CAM2, 300)), None, Some(1_040_000_000)), // no strih burn
            stream_frame_3(1, None, Some(1_000_000_000), Some(1_050_000_000)), // no cam2 key
            stream_frame_3(
                2,
                Some((CAM2, 304)),
                Some(1_066_000_000),
                Some(1_099_000_000),
            ), // good
        ];
        let s = strih_stream_samples_from_stream(
            &stream,
            Some(CAM2),
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
        );
        assert_eq!(s.len(), 1, "only the fully-stamped frame yields a sample");
        assert!((s[0].latency_ms - 33.0).abs() < 1e-6);
    }

    // ---- #174 cam1 burn-id extraction + co-located chain latency from the stream ----

    /// A stream frame carrying any set of (run_id, frame_id, gen_ts_ns) payloads — the
    /// shape one stream-recorded frame has when cam1+strih+stream burns + cam2 are present.
    fn multi(idx: u64, ps: &[(u32, u32, i64)]) -> RecordingFrame {
        let payloads: Vec<Payload> = ps
            .iter()
            .map(|&(r, fid, g)| Payload {
                run_id: r,
                frame_id: fid,
                gen_ts_ns: g,
            })
            .collect();
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index: idx,
            payloads,
            tick,
        }
    }

    #[test]
    fn burn_ids_in_extracts_only_the_requested_run_id_in_order() {
        // Two stream frames, each with cam2 + cam1-burn + strih-burn + stream-burn.
        let frames = vec![
            multi(
                0,
                &[
                    (CAM2, 500, 10),
                    (BURN_RUN_ID_CAM1, 40, 11),
                    (BURN_RUN_ID_STRIH, 80, 12),
                    (BURN_RUN_ID_STREAM, 120, 13),
                ],
            ),
            multi(
                1,
                &[
                    (CAM2, 502, 20),
                    (BURN_RUN_ID_CAM1, 41, 21),
                    (BURN_RUN_ID_STRIH, 81, 22),
                    (BURN_RUN_ID_STREAM, 121, 23),
                ],
            ),
        ];
        assert_eq!(burn_ids_in(&frames, BURN_RUN_ID_CAM1), vec![40, 41]);
        assert_eq!(burn_ids_in(&frames, BURN_RUN_ID_STRIH), vec![80, 81]);
        assert_eq!(burn_ids_in(&frames, BURN_RUN_ID_STREAM), vec![120, 121]);
        // A run_id absent from the recording yields nothing.
        assert!(burn_ids_in(&frames, 999_999).is_empty());
    }

    #[test]
    fn chain_hop_latency_is_downstream_minus_upstream_colocated_in_one_frame() {
        // cam1→strih: per stream frame carrying BOTH the cam1 burn and the strih burn,
        // latency = strih_burn.gen_ts − cam1_burn.gen_ts. Co-located ⇒ NO cam2-tick
        // pairing, so the p99-blowup outliers cannot occur.
        let frames = vec![
            multi(
                0,
                &[
                    (BURN_RUN_ID_CAM1, 40, 1_000_000_000), // cam1 rendered at t=1.000s
                    (BURN_RUN_ID_STRIH, 80, 1_018_000_000), // strih rendered 18ms later
                ],
            ),
            multi(
                1,
                &[
                    (BURN_RUN_ID_CAM1, 41, 2_000_000_000),
                    (BURN_RUN_ID_STRIH, 81, 2_020_000_000), // 20ms later
                ],
            ),
        ];
        let s = chain_hop_samples_from_stream(&frames, BURN_RUN_ID_CAM1, BURN_RUN_ID_STRIH);
        assert_eq!(s.len(), 2);
        assert!((s[0].latency_ms - 18.0).abs() < 1e-6);
        assert!((s[1].latency_ms - 20.0).abs() < 1e-6);
        assert_eq!(
            s[0].at_ns, 1_000_000_000,
            "anchored at the upstream render instant"
        );
    }

    #[test]
    fn chain_hop_skips_frames_missing_a_burn_or_with_zero_stamp() {
        let frames = vec![
            // missing the strih burn → skipped
            multi(0, &[(BURN_RUN_ID_CAM1, 40, 1_000_000_000)]),
            // strih burn unstamped (0) → skipped
            multi(
                1,
                &[
                    (BURN_RUN_ID_CAM1, 41, 2_000_000_000),
                    (BURN_RUN_ID_STRIH, 81, 0),
                ],
            ),
            // fully stamped → the only sample
            multi(
                2,
                &[
                    (BURN_RUN_ID_CAM1, 42, 3_000_000_000),
                    (BURN_RUN_ID_STRIH, 82, 3_033_000_000),
                ],
            ),
        ];
        let s = chain_hop_samples_from_stream(&frames, BURN_RUN_ID_CAM1, BURN_RUN_ID_STRIH);
        assert_eq!(s.len(), 1, "only the fully-stamped frame yields a sample");
        assert!((s[0].latency_ms - 33.0).abs() < 1e-6);
    }

    #[test]
    fn cam2_cam1_from_burn_is_cam1_capture_minus_cam2_paint_colocated_no_grab() {
        // #179: cam2→cam1 from the STREAM recording ALONE — the cam1-capture burn (#174)
        // rides in carrying cam1's CAPTURE wall-clock ts; the cam2 optical QR cam1 FILMED
        // rides in the SAME frame carrying cam2's PAINT instant in its OWN gen_ts. latency
        // = cam1_capture − cam2_paint, CO-LOCATED in one frame (NOT an external painter
        // CSV, whose tick counter resets between sessions). The 6GB grab is never touched.
        // Each frame also carries the forwarded strih & stream burns (ignored).
        let stream = vec![
            multi(
                0,
                &[
                    (CAM2, 100, 1_000_000_000),              // cam2 QR: paint @ 1.000s
                    (BURN_RUN_ID_CAM1, 40, 1_068_000_000),   // cam1 captured 68ms after paint
                    (BURN_RUN_ID_STRIH, 80, 1_240_000_000),  // forwarded strih burn (ignore)
                    (BURN_RUN_ID_STREAM, 90, 2_300_000_000), // forwarded stream burn (ignore)
                ],
            ),
            multi(
                1,
                &[
                    (CAM2, 102, 1_033_000_000),            // paint @ 1.033s
                    (BURN_RUN_ID_CAM1, 41, 1_103_000_000), // 70ms after paint
                    (BURN_RUN_ID_STRIH, 81, 1_280_000_000),
                    (BURN_RUN_ID_STREAM, 91, 2_400_000_000),
                ],
            ),
        ];
        let s = cam2_cam1_samples_from_burn(
            &stream,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            &[BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        );
        assert_eq!(
            s.len(),
            2,
            "one sample per frame carrying cam2 QR + cam1 burn"
        );
        assert!((s[0].latency_ms - 68.0).abs() < 1e-6, "frame 0 = +68 ms");
        assert!((s[1].latency_ms - 70.0).abs() < 1e-6, "frame 1 = +70 ms");
        assert_eq!(
            s[0].at_ns, 1_000_000_000,
            "anchored at the cam2 paint instant (the cam2 QR's own gen_ts)"
        );
    }

    #[test]
    fn cam2_cam1_from_burn_skips_frame_without_cam1_burn_or_cam2_or_zero_stamp() {
        // A stream frame with no cam1 burn, no cam2 QR, or a 0-sentinel stamp on either
        // contributes NO sample (never a wrong number — the 0 sentinel would read huge).
        let stream = vec![
            multi(
                0,
                &[
                    (CAM2, 100, 1_000_000_000),
                    (BURN_RUN_ID_CAM1, 40, 1_050_000_000),
                ],
            ),
            multi(1, &[(CAM2, 102, 1_033_000_000)]), // no cam1 burn → skip
            multi(2, &[(BURN_RUN_ID_CAM1, 42, 1_120_000_000)]), // no cam2 QR → skip
            multi(3, &[(CAM2, 106, 1_099_000_000), (BURN_RUN_ID_CAM1, 43, 0)]), // 0 capture stamp → skip
            multi(4, &[(CAM2, 108, 0), (BURN_RUN_ID_CAM1, 44, 1_200_000_000)]), // 0 paint stamp → skip
        ];
        let s = cam2_cam1_samples_from_burn(&stream, Some(CAM2), BURN_RUN_ID_CAM1, &[]);
        assert_eq!(s.len(), 1, "only frame 0 yields a valid sample");
        assert!((s[0].latency_ms - 50.0).abs() < 1e-6);
    }

    #[test]
    fn cam2_cam1_from_burn_uses_highest_frame_id_cam2_vernier_half() {
        // The cam2 dual-QR has TWO halves with DIFFERENT frame_ids; the canonical half is
        // the highest frame_id (matches split_payloads). In production both halves share one
        // paint gen_ts, but here the two halves are given DIFFERENT gen_ts ON PURPOSE so the
        // test can FAIL if the selection ever picked the wrong half (min instead of max, or
        // the wrong half's gen_ts): only selecting the highest-frame_id half (101 → 1.010s)
        // gives 45 ms; selecting the older half (100 → 1.000s) would give 55 ms.
        let stream = vec![multi(
            0,
            &[
                (CAM2, 100, 1_000_000_000), // older half — gen_ts 1.000s (must NOT be chosen)
                (CAM2, 101, 1_010_000_000), // fresher half (highest frame_id) — paint 1.010s
                (BURN_RUN_ID_CAM1, 40, 1_055_000_000),
            ],
        )];
        let s = cam2_cam1_samples_from_burn(&stream, Some(CAM2), BURN_RUN_ID_CAM1, &[]);
        assert_eq!(s.len(), 1);
        assert!(
            (s[0].latency_ms - 45.0).abs() < 1e-6,
            "cam1 capture (1.055s) − the HIGHEST-frame_id cam2 half's paint (1.010s) = 45ms \
             (picking the older half would wrongly give 55ms)"
        );
        assert_eq!(
            s[0].at_ns, 1_010_000_000,
            "anchored at the chosen (highest-frame_id) half's paint instant, not the older half"
        );
    }

    /// Build a stream frame carrying a cam2 tick plus an arbitrary set of node burns
    /// (run_id, burn_frame_id, gen_ts_ns). The first tuple is cam2.
    fn loss_frame(idx: u64, cam2_tick: u32, burns: &[(u32, u32, i64)]) -> RecordingFrame {
        let mut ps = vec![(CAM2, cam2_tick, 1_000)];
        ps.extend_from_slice(burns);
        multi(idx, &ps)
    }

    /// #181 ROOT CAUSE: each node's burn counter is INDEPENDENT, so a set-equality
    /// compare of the two burn-id SEQUENCES finds zero overlap ⇒ compared_ids=0. This
    /// is exactly what the old call site (`burn_hop_verdict(cam1_ids, strih_ids)`)
    /// produced on the real run despite the burns being present. Pinned here so the
    /// regression cannot silently return.
    #[test]
    fn independent_burn_counters_give_zero_overlap_the_181_bug() {
        // cam1 burns 40,41,42 ; strih burns 80,81,82 — disjoint ranges.
        let cam1_ids = vec![40u32, 41, 42];
        let strih_ids = vec![80u32, 81, 82];
        let v =
            crate::probe::recording_verdict::burn_hop_verdict("cam1→strih", &cam1_ids, &strih_ids);
        assert_eq!(
            v.compared_ids, 0,
            "independent per-node counters never overlap — the #181 symptom"
        );
    }

    /// #181 FIX: pairing per-hop LOSS by the SHARED cam2 source tick yields the real
    /// overlap (compared_ids > 0) even though the per-node burn counters are disjoint.
    /// Every frame carries the same cam2 tick on both burns, so all ticks survive.
    #[test]
    fn loss_pairs_by_cam2_tick_clean_hop_all_survive() {
        // 3 cam2 ticks; each frame has BOTH cam1 (independent ids 40..) and strih
        // (independent ids 80..) burns, both stamped. No loss.
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_CAM1, 40, 11), (BURN_RUN_ID_STRIH, 80, 12)],
            ),
            loss_frame(
                1,
                502,
                &[(BURN_RUN_ID_CAM1, 41, 21), (BURN_RUN_ID_STRIH, 81, 22)],
            ),
            loss_frame(
                2,
                504,
                &[(BURN_RUN_ID_CAM1, 42, 31), (BURN_RUN_ID_STRIH, 82, 32)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "cam1→strih",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            &[],
        );
        assert_eq!(
            v.compared_ids, 3,
            "all 3 cam2 ticks have both burns ⇒ overlap is real, not 0"
        );
        assert!(v.dropped_ids.is_empty(), "no dropped");
        assert!(v.phantom_ids.is_empty(), "no phantom");
        assert!(v.is_pass(), "a clean hop with real overlap PASSES");
    }

    /// A cam2 tick that carries the UPSTREAM burn but not the DOWNSTREAM burn is a DROP
    /// on the hop (downstream lost that source frame). Keyed by the cam2 tick, not the
    /// burn id.
    #[test]
    fn loss_counts_a_dropped_cam2_tick_as_dropped() {
        // A REAL drop: strih is ABSENT on a SUSTAINED run of recorded frames (502,504,506,508
        // = 4 consecutive > DECODE_MISS_MAX_RUN=3). A run that long is not a momentary 4K
        // decode glitch — strih genuinely skipped renders — so #175 KEEPS all four as drops
        // (a single isolated missing-burn recorded frame would be a decode miss and excluded).
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_CAM1, 40, 11), (BURN_RUN_ID_STRIH, 80, 12)],
            ),
            loss_frame(1, 502, &[(BURN_RUN_ID_CAM1, 41, 21)]), // strih ABSENT
            loss_frame(2, 504, &[(BURN_RUN_ID_CAM1, 42, 31)]), // strih ABSENT
            loss_frame(3, 506, &[(BURN_RUN_ID_CAM1, 43, 41)]), // strih ABSENT
            loss_frame(4, 508, &[(BURN_RUN_ID_CAM1, 44, 51)]), // strih ABSENT
            loss_frame(
                5,
                510,
                &[(BURN_RUN_ID_CAM1, 45, 61), (BURN_RUN_ID_STRIH, 90, 62)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "cam1→strih",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            &[],
        );
        assert_eq!(v.compared_ids, 2, "ticks 500 and 510 survived");
        assert_eq!(
            v.dropped_ids,
            vec![502, 504, 506, 508],
            "a sustained 4-frame strih absence ⇒ real drops, not a decode-miss glitch"
        );
        assert_eq!(
            v.decode_miss_excluded, 0,
            "a sustained run exceeds DECODE_MISS_MAX_RUN ⇒ NOT a decode miss"
        );
        assert!(v.phantom_ids.is_empty());
        assert!(!v.is_pass(), "a dropped frame FAILS the hop");
    }

    /// A cam2 tick that carries the DOWNSTREAM burn but not the UPSTREAM burn is a
    /// PHANTOM (downstream rendered a source frame upstream never marked).
    #[test]
    fn loss_counts_a_phantom_cam2_tick_as_phantom() {
        // A REAL phantom: strih (the UPSTREAM node) is ABSENT on a SUSTAINED run of recorded
        // frames (502,504,506,508 = 4 > DECODE_MISS_MAX_RUN=3) — too long for a momentary
        // decode glitch, so #175 KEEPS all four as phantoms (an isolated one would be a decode
        // miss). The stream (downstream) burn rendered through them.
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_STRIH, 80, 11), (BURN_RUN_ID_STREAM, 120, 12)],
            ),
            loss_frame(1, 502, &[(BURN_RUN_ID_STREAM, 121, 21)]), // strih ABSENT
            loss_frame(2, 504, &[(BURN_RUN_ID_STREAM, 122, 31)]), // strih ABSENT
            loss_frame(3, 506, &[(BURN_RUN_ID_STREAM, 123, 41)]), // strih ABSENT
            loss_frame(4, 508, &[(BURN_RUN_ID_STREAM, 124, 51)]), // strih ABSENT
            loss_frame(
                5,
                510,
                &[(BURN_RUN_ID_STRIH, 90, 61), (BURN_RUN_ID_STREAM, 125, 62)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "strih→stream",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            &[],
        );
        assert_eq!(v.compared_ids, 2, "ticks 500 and 510 survived");
        assert!(v.dropped_ids.is_empty());
        assert_eq!(
            v.phantom_ids,
            vec![502, 504, 506, 508],
            "a sustained 4-frame strih absence ⇒ real phantoms, not a decode-miss glitch"
        );
        assert_eq!(v.decode_miss_excluded, 0);
        assert!(!v.is_pass(), "a phantom FAILS the hop");
    }

    /// Record start/stop skew: an upstream-only tick OUTSIDE the shared overlap span is
    /// NOT counted as a drop (the downstream recording simply had not started / had
    /// stopped). Mirrors the active-span handling of `burn_hop_verdict`.
    #[test]
    fn loss_excludes_out_of_overlap_ticks_as_skew_not_loss() {
        let frames = vec![
            // tick 100: only cam1 (strih recording started later) → start skew, NOT a drop.
            loss_frame(0, 100, &[(BURN_RUN_ID_CAM1, 40, 11)]),
            loss_frame(
                1,
                500,
                &[(BURN_RUN_ID_CAM1, 41, 21), (BURN_RUN_ID_STRIH, 80, 22)],
            ),
            loss_frame(
                2,
                504,
                &[(BURN_RUN_ID_CAM1, 42, 31), (BURN_RUN_ID_STRIH, 81, 32)],
            ),
            // tick 900: only cam1 (strih stopped earlier) → stop skew, NOT a drop.
            loss_frame(3, 900, &[(BURN_RUN_ID_CAM1, 43, 41)]),
        ];
        let v = chain_hop_loss_from_stream(
            "cam1→strih",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            &[],
        );
        // Overlap span = [500, 504]; 100 and 900 are outside it and excluded.
        assert_eq!(v.compared_ids, 2, "only the two in-span ticks compared");
        assert!(
            v.dropped_ids.is_empty(),
            "out-of-span cam1-only ticks are skew, not loss"
        );
        assert!(v.phantom_ids.is_empty());
    }

    /// An unstamped (gen_ts_ns = 0) burn does not count as a render — its cam2 tick has
    /// no upstream/downstream presence for that side. Here strih is unstamped/absent on a
    /// SUSTAINED run (502-508 > DECODE_MISS_MAX_RUN) so the absences are kept as REAL drops
    /// (proving the unstamped burn at 502 is treated as no-render), not #175 decode misses.
    #[test]
    fn loss_ignores_unstamped_burns() {
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_CAM1, 40, 11), (BURN_RUN_ID_STRIH, 80, 12)],
            ),
            // strih burn present but UNSTAMPED (0) → not a render ⇒ tick 502 is dropped.
            loss_frame(
                1,
                502,
                &[(BURN_RUN_ID_CAM1, 41, 21), (BURN_RUN_ID_STRIH, 81, 0)],
            ),
            loss_frame(2, 504, &[(BURN_RUN_ID_CAM1, 42, 31)]), // strih absent
            loss_frame(3, 506, &[(BURN_RUN_ID_CAM1, 43, 41)]), // strih absent
            loss_frame(4, 508, &[(BURN_RUN_ID_CAM1, 44, 51)]), // strih absent
            loss_frame(
                5,
                510,
                &[(BURN_RUN_ID_CAM1, 45, 61), (BURN_RUN_ID_STRIH, 90, 62)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "cam1→strih",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            &[],
        );
        assert_eq!(v.compared_ids, 2);
        assert_eq!(
            v.dropped_ids,
            vec![502, 504, 506, 508],
            "the unstamped strih at 502 counts as no-render; a 4-frame run ⇒ real drops"
        );
        assert_eq!(v.decode_miss_excluded, 0);
    }

    /// #181 review fix: when cam2 is NOT pinned, the OTHER forwarded burn (here the cam1
    /// burn on the strih→stream hop) must NOT be misread as a cam2 half. With its
    /// frame_id HIGHER than the real cam2 tick, the unhardened `max(frame_id)` cam2
    /// selection would hijack the canonical tick and collapse the overlap. Passing the
    /// foreign burn in `other_burns` keeps it out of cam2 selection, so the real cam2 tick
    /// keys the hop and compared_ids stays correct even with cam2_id = None.
    #[test]
    fn loss_excludes_foreign_forwarded_burn_from_cam2_when_unpinned() {
        // strih→stream hop. Each frame: real cam2 tick (500/504), strih burn, stream burn,
        // AND a forwarded cam1 burn whose frame_id (90100/90101) EXCEEDS the cam2 tick.
        let frames = vec![
            multi(
                0,
                &[
                    (CAM2, 500, 10),
                    (BURN_RUN_ID_CAM1, 90_100, 11), // foreign, higher frame_id than cam2
                    (BURN_RUN_ID_STRIH, 80, 12),
                    (BURN_RUN_ID_STREAM, 120, 13),
                ],
            ),
            multi(
                1,
                &[
                    (CAM2, 504, 20),
                    (BURN_RUN_ID_CAM1, 90_101, 21),
                    (BURN_RUN_ID_STRIH, 81, 22),
                    (BURN_RUN_ID_STREAM, 121, 23),
                ],
            ),
        ];
        // cam2 UNPINNED (None) — the real-world omitted --cam2-run-id case. Without the
        // other_burns exclusion the cam1 burn would be taken as cam2 and break the key.
        let v = chain_hop_loss_from_stream(
            "strih→stream",
            &frames,
            None,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            &[BURN_RUN_ID_CAM1],
        );
        assert_eq!(
            v.compared_ids, 2,
            "real cam2 ticks 500/504 key the hop; the forwarded cam1 burn is excluded"
        );
        assert!(v.dropped_ids.is_empty());
        assert!(v.phantom_ids.is_empty());
        assert!(v.is_pass());
    }

    /// Pins WHY the exclusion matters: WITHOUT it (other_burns empty, cam2 None), the
    /// forwarded cam1 burn — highest frame_id — is misread as the cam2 key, so the dropped
    /// id surfaced is a cam1 BURN id, not a cam2 tick. WITH the exclusion the dropped id is
    /// the real cam2 tick. Same frames, only the exclusion differs ⇒ proves the key flips.
    #[test]
    fn loss_exclusion_flips_the_key_from_foreign_burn_to_cam2_tick() {
        // Frame 0: clean (both present), cam2 tick 496 — anchors the span start below 500.
        // Frames 1-4: strih present, stream ABSENT (a SUSTAINED 4-frame run > the #175
        //   DECODE_MISS_MAX_RUN, so it is kept as a REAL drop, not a decode-miss glitch).
        //   The forwarded cam1 burn ids 90_100..90_103 ride along (> the cam2 ticks).
        // Frame 5: clean again, cam2 tick 510.
        let frames = vec![
            multi(
                0,
                &[
                    (CAM2, 496, 5),
                    (BURN_RUN_ID_CAM1, 90_099, 6),
                    (BURN_RUN_ID_STRIH, 79, 7),
                    (BURN_RUN_ID_STREAM, 119, 8),
                ],
            ),
            // stream ABSENT on 500/502/504/506 (4 consecutive) → sustained real drop run.
            multi(
                1,
                &[
                    (CAM2, 500, 10),
                    (BURN_RUN_ID_CAM1, 90_100, 11),
                    (BURN_RUN_ID_STRIH, 80, 12),
                ],
            ),
            multi(
                2,
                &[
                    (CAM2, 502, 20),
                    (BURN_RUN_ID_CAM1, 90_101, 21),
                    (BURN_RUN_ID_STRIH, 81, 22),
                ],
            ),
            multi(
                3,
                &[
                    (CAM2, 504, 30),
                    (BURN_RUN_ID_CAM1, 90_102, 31),
                    (BURN_RUN_ID_STRIH, 82, 32),
                ],
            ),
            multi(
                4,
                &[
                    (CAM2, 506, 40),
                    (BURN_RUN_ID_CAM1, 90_103, 41),
                    (BURN_RUN_ID_STRIH, 83, 42),
                ],
            ),
            multi(
                5,
                &[
                    (CAM2, 510, 50),
                    (BURN_RUN_ID_CAM1, 90_104, 51),
                    (BURN_RUN_ID_STRIH, 84, 52),
                    (BURN_RUN_ID_STREAM, 130, 53),
                ],
            ),
        ];
        // WITHOUT exclusion: the cam1 burn (90_099..90_104) is misread as cam2 and, being the
        // highest frame_id, hijacks the canonical tick — so the dropped ids are cam1 BURN ids
        // (90_100..90_103), the WRONG namespace, never the real cam2 ticks.
        let bad = chain_hop_loss_from_stream(
            "strih→stream",
            &frames,
            None,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            &[],
        );
        assert!(
            bad.dropped_ids.contains(&90_100),
            "unhardened fallback keys on the foreign cam1 burn id: {:?}",
            bad.dropped_ids
        );
        assert!(
            !bad.dropped_ids.contains(&500),
            "and NEVER on the real cam2 tick 500: {:?}",
            bad.dropped_ids
        );

        // WITH exclusion: the real cam2 tick keys the hop and the drops ARE the cam2 ticks.
        let good = chain_hop_loss_from_stream(
            "strih→stream",
            &frames,
            None,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            &[BURN_RUN_ID_CAM1],
        );
        assert_eq!(
            good.dropped_ids,
            vec![500, 502, 504, 506],
            "hardened: the dropped ids are the real cam2 ticks, not burn ids"
        );
        assert_eq!(good.compared_ids, 2, "ticks 496 and 510 survived");
    }

    // ---- #175: single-frame burn-DECODE-MISS exclusion -----------------------------

    /// #175 pure helper: a flagged tick is a DECODE MISS (excluded) when the gap between the
    /// nearest decoded-absent-burn ticks contains ONLY recorded-and-flagged frames (a short
    /// run that physically exists), and is KEPT when an unflagged hole or a too-long run
    /// proves a real drop. The cam2 tick is the shared clock; the recorded-frame COVERAGE of
    /// the gap — not the counter step — is the rate-free discriminator.
    #[test]
    fn decode_miss_ticks_excludes_covered_keeps_holed() {
        // absent burn decoded at ticks 100 and 104; recorded ticks are {100,102,104} so the
        // gap (102) is fully covered by the flagged tick 102 ⇒ decode miss ⇒ excluded.
        let mut absent = BTreeMap::new();
        absent.insert(100u32, 80u32);
        absent.insert(104u32, 82u32);
        let recorded: BTreeSet<u32> = [100u32, 102, 104].into_iter().collect();
        let excl = decode_miss_ticks(&[102], &absent, &recorded, DECODE_MISS_MAX_RUN);
        assert!(
            excl.contains(&102),
            "fully-covered single-frame gap ⇒ decode miss"
        );

        // Same flagged tick, but recorded ticks {100,102,103,104}: 103 is recorded yet NOT
        // flagged (an unflagged hole = a real missing optical instant) ⇒ KEPT as a drop.
        let recorded_hole: BTreeSet<u32> = [100u32, 102, 103, 104].into_iter().collect();
        let kept = decode_miss_ticks(&[102], &absent, &recorded_hole, DECODE_MISS_MAX_RUN);
        assert!(
            kept.is_empty(),
            "an unflagged hole in the gap ⇒ real drop, not a decode miss"
        );
    }

    /// #175 pure helper: a SUSTAINED run of consecutive flagged frames longer than `max_run`
    /// is a real drop (not a momentary glitch) and KEPT; a non-monotonic absent-burn counter
    /// (reorder) is KEPT; an edge tick (no decoded burn on one side) is KEPT.
    #[test]
    fn decode_miss_ticks_keeps_long_runs_reorders_and_edges() {
        // 4 consecutive flagged frames between decoded burns ⇒ run length 4 > max_run 3 ⇒ KEPT.
        let mut absent = BTreeMap::new();
        absent.insert(100u32, 80u32);
        absent.insert(110u32, 81u32);
        let recorded: BTreeSet<u32> = [100u32, 102, 104, 106, 108, 110].into_iter().collect();
        let flagged = [102u32, 104, 106, 108];
        let kept = decode_miss_ticks(&flagged, &absent, &recorded, DECODE_MISS_MAX_RUN);
        assert!(kept.is_empty(), "a 4-frame run exceeds max_run ⇒ real drop");

        // Non-monotonic absent-burn counter across the gap (82 → 80) ⇒ reorder ⇒ KEPT.
        let mut reorder = BTreeMap::new();
        reorder.insert(100u32, 82u32);
        reorder.insert(104u32, 80u32);
        let rec2: BTreeSet<u32> = [100u32, 102, 104].into_iter().collect();
        assert!(decode_miss_ticks(&[102], &reorder, &rec2, DECODE_MISS_MAX_RUN).is_empty());

        // Edge tick — only a decoded burn BELOW (none above) ⇒ cannot prove existence ⇒ KEPT.
        let mut only_below = BTreeMap::new();
        only_below.insert(100u32, 80u32);
        let rec3: BTreeSet<u32> = [100u32, 102].into_iter().collect();
        assert!(decode_miss_ticks(&[102], &only_below, &rec3, DECODE_MISS_MAX_RUN).is_empty());
    }

    /// #175 the real-run scenario (run 172046073): a stream frame decoded INCOMPLETELY —
    /// cam1's burn present, strih's burn NOT decoded — while strih's OWN counter is
    /// perfectly contiguous (80→83 straddling the gap). The old verdict counted this as a
    /// cam1→strih DROP; the hardened verdict reclassifies it as a decode miss (not a loss),
    /// folds it into compared_ids, and the hop PASSES (effectively zero-loss).
    #[test]
    fn chain_hop_loss_reclassifies_decode_miss_as_not_a_drop() {
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_CAM1, 40, 11), (BURN_RUN_ID_STRIH, 80, 12)],
            ),
            // tick 502: cam1 decoded, strih burn FAILED to decode in this one 4K frame.
            loss_frame(1, 502, &[(BURN_RUN_ID_CAM1, 41, 21)]),
            loss_frame(
                2,
                504,
                // strih's counter is contiguous (80 → 83) ⇒ strih RENDERED 502, the QR just
                // didn't decode there ⇒ decode miss, NOT a drop.
                &[(BURN_RUN_ID_CAM1, 42, 31), (BURN_RUN_ID_STRIH, 83, 32)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "cam1→strih",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            &[],
        );
        assert!(
            v.dropped_ids.is_empty(),
            "contiguous strih counter ⇒ the absence is a decode miss, not a drop: {:?}",
            v.dropped_ids
        );
        assert_eq!(
            v.decode_miss_excluded, 1,
            "the one decode-miss tick is recorded"
        );
        assert_eq!(
            v.compared_ids, 3,
            "the decode-miss tick folds back into compared (it WAS a real frame)"
        );
        assert!(
            v.is_pass(),
            "a chain with only decode misses is effectively zero-loss"
        );
    }

    /// #175: a PHANTOM-side decode miss is likewise reclassified — the UPSTREAM burn failed
    /// to decode on a frame the upstream node demonstrably rendered (contiguous counter).
    #[test]
    fn chain_hop_loss_reclassifies_phantom_decode_miss() {
        let frames = vec![
            loss_frame(
                0,
                500,
                &[(BURN_RUN_ID_STRIH, 80, 11), (BURN_RUN_ID_STREAM, 120, 12)],
            ),
            // tick 502: stream decoded, strih (upstream) burn failed to decode here.
            loss_frame(1, 502, &[(BURN_RUN_ID_STREAM, 121, 21)]),
            loss_frame(
                2,
                504,
                // strih (upstream) counter contiguous 80→83 ⇒ rendered ⇒ decode miss.
                &[(BURN_RUN_ID_STRIH, 83, 31), (BURN_RUN_ID_STREAM, 122, 32)],
            ),
        ];
        let v = chain_hop_loss_from_stream(
            "strih→stream",
            &frames,
            Some(CAM2),
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            &[],
        );
        assert!(
            v.phantom_ids.is_empty(),
            "phantom decode miss reclassified: {:?}",
            v.phantom_ids
        );
        assert_eq!(v.decode_miss_excluded, 1);
        assert!(v.is_pass());
    }
}
