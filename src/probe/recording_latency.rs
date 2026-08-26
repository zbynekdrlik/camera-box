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
/// The cam1-CAPTURE burn run_id (#174) — the value `CAMERA_BOX_BURN_RUN_ID` is set to on
/// cam1 for a TEST run. cam1's burn rides through NDI into strih's program and on into
/// stream's, so the single stream recording carries the cam1 burn alongside cam2's optical
/// QR + strih's + stream's burns; the verdict pairs the cam1→strih and full-chain hops on
/// this clean digital id. Distinct from the strih/stream burn ids so all marks are told
/// apart by run_id. The binary lets the operator override it to match the cam1 env.
pub const BURN_RUN_ID_CAM1: u32 = 911001;
/// imag-nb's (Topology v2 IMAG box, EPIC #466) OWN digital corner burn run_id — the OBS burn
/// filter's `Corner::BottomCenterLeft` (#463, `vendor/distroav/src/burn-geom.hpp`). Distinct
/// from every other reserved id (911001 cam1 / 911002 strih / 911004 stream) so all marks are
/// told apart by run_id alone.
///
/// **Renamed from `BURN_RUN_ID_CAM3` (#463).** This constant used to be reserved for cam3's
/// `CAMERA_BOX_BURN_RUN_ID` capture-burn (the #24 mechanism, extending #174 to a THIRD source
/// camera) — but cam3 is down/deferred in Topology v2, so #463 claims the value for imag's own
/// digital burn instead. cam3's capture-burn now has its OWN fresh reserved id, [`BURN_RUN_ID_CAM3`]
/// (#24) — the two mechanisms are numerically distinct again.
pub const BURN_RUN_ID_IMAG: u32 = 911003;
/// #24 — cam4's capture-burn run_id, extending the #174 mechanism to another source camera.
/// cam1/cam3/cam4 occupy the SAME "camera under test" role (the `CAMERA_BOX_BURN_RUN_ID` capture
/// burn is the same feature on every camera-box binary — only the deployed run_id differs) and
/// are mutually exclusive in any real run: only the ONE camera actually deployed with the burn
/// enabled produces a non-empty id set. See [`BURN_RUN_ID_IMAG`]'s doc for the cam3 id note.
pub const BURN_RUN_ID_CAM4: u32 = 911007;
/// #24 — cam3's OWN capture-burn run_id, fresh + unique. Before this fix `--burn-cam3-run-id`
/// defaulted to [`BURN_RUN_ID_IMAG`] (911003), a latent collision left behind when #463 renamed
/// the old `BURN_RUN_ID_CAM3` constant and repurposed 911003 for imag-nb's own digital corner
/// burn (cam3's capture-burn was never actually deployed, so the collision was numerically
/// harmless in practice, but real). Reserved outside every other used id
/// (911001/911002/911003/911004/911007).
pub const BURN_RUN_ID_CAM3: u32 = 911008;
/// #312 — cam2's OWN capture-burn run_id. cam2 is the fixed dual-QR PAINTER (its own
/// framebuffer feeds the optical loopback every other camera-under-test box films), but since
/// #291 its camera-box daemon keeps CAPTURING + EMITTING its own NDI feed throughout a TEST-mode
/// run (a transient no-display drop-in frees only `/dev/fb0` for the separate painter process,
/// never touching `/dev/video0`/NDI). That makes cam2's OWN chain (cam2 capture → strih →
/// stream) measurable by the SAME digital render-time capture-burn mechanism as cam1/cam3/cam4 —
/// this is the id `CAMERA_BOX_BURN_RUN_ID` is set to when the ALL-CAMBOX sweep deploys the
/// probe-featured binary on cam2 (scripts/recording-e2e.sh `[2b/8]`). Reserved fresh, outside
/// every id already in use (911001/911002/911003/911004/911007/911008).
pub const BURN_RUN_ID_CAM2: u32 = 911009;
/// #312 — cam5's capture-burn run_id, extending the #24/#624 mechanism to the 5th physical
/// camera (fleet growth 4→6, #451). See [`BURN_RUN_ID_CAM4`]'s doc — same role, same mutual
/// exclusivity, fresh id outside every id already in use.
pub const BURN_RUN_ID_CAM5: u32 = 911010;
/// #312 — cam6's capture-burn run_id, extending the #24/#624 mechanism to the 6th physical
/// camera (fleet growth 4→6, #451). See [`BURN_RUN_ID_CAM4`]'s doc — same role, same mutual
/// exclusivity, fresh id outside every id already in use.
pub const BURN_RUN_ID_CAM6: u32 = 911011;
/// #755 — cam7's capture-burn run_id, extending the #24/#624 mechanism to the 7th physical
/// camera (fleet growth 6→7, #753 — the new Elgato 4K S box at 10.77.9.67). See
/// [`BURN_RUN_ID_CAM4`]'s doc — same role, same mutual exclusivity, fresh id outside every id
/// already in use (911001..911004/911007..911011).
pub const BURN_RUN_ID_CAM7: u32 = 911012;
/// issue 1196 — the aux Vernier tick pair's reserved run_id. UNLIKE every `BURN_RUN_ID_*` above,
/// this is NOT a digital burn: it is PAINTED optical content — two small QRs the cam2 painter
/// blits into the bottom burn-free gaps (`crate::aux_tick` geometry; left = latest EVEN tick,
/// right = latest ODD tick, `gen_ts_ns = 0`), giving the projection-tap tear detector the
/// vertical tick redundancy the primary single-band dual-QR lacks. It joins
/// [`crate::probe::recording::NODE_BURN_RUN_IDS`] (the tick-EXCLUSION list) because its
/// `frame_id`s must never feed `RecordingFrame::tick` / cadence / copies / latency: on a healthy
/// frame they merely duplicate the primary pair, but on a TORN frame — or when the primary band
/// is corrupted while the aux marks still decode — they carry a DIFFERENT generation, which would
/// silently shift the undecodable/continuity metrics the strict gates are calibrated on. The
/// ONLY consumer that reads these ids is the report-only tear surface (`crate::tear_detect` v2),
/// which extracts them BY this run_id explicitly. Reserved fresh, outside every id already in
/// use (911001..911004/911007..911012; 911005/911006/911099 are test-fixture-only synthetics).
pub const AUX_TICK_RUN_ID: u32 = 911013;

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

/// #286 4-camera MUTUAL phase-sync: per-camera cam→strih latency, reusing
/// [`cam_strih_samples`] UNCHANGED — only feeding it a DIFFERENT `ids.cam2` pin per call.
///
/// Each camera stamps its OWN capture-time burn onto the frame it emits during a calibration
/// window (mirrors the #174 cam1 capture burn [`BURN_RUN_ID_CAM1`], extended to a distinct
/// deploy-time `CAMERA_BOX_BURN_RUN_ID` per camera); that burn rides through NDI to strih
/// exactly like cam2's optical QR does. Pinning `ids.cam2` to a given camera's capture-burn
/// run_id therefore makes [`cam_strih_samples`] pair THAT camera's capture instant against
/// strih's own burn (`strih_burn.gen_ts_ns − camera_burn.gen_ts_ns`) — the same cam→strih
/// latency #286 needs, with zero new decode primitive.
///
/// `strih` = ONE strih recording spanning the calibration window (all requested cameras'
/// capture burns present); `strih_burn` = strih's own burn run_id; `camera_burn_ids` = each
/// camera's distinct capture-burn run_id, in the order the caller wants results back. A
/// camera's own pin is an EXACT match (`RunIds::is_cam2` with `Some`), so another requested
/// camera's burn present in the same frame can never be mistaken for this one — `other_burns`
/// is populated anyway for defensiveness/clarity, matching the [`RunIds`] contract.
///
/// Returns one `Vec<LatencySample>` per requested camera burn id, in the SAME order as
/// `camera_burn_ids` — a camera whose burn never decoded in this recording gets an empty
/// `Vec` (never a wrong number). Does NOT alter [`cam_strih_samples`] or any existing
/// cam1-only caller — purely an ADDITIVE n-camera wrapper.
pub fn n_camera_strih_samples(
    strih: &[RecordingFrame],
    strih_burn: u32,
    camera_burn_ids: &[u32],
) -> Vec<Vec<LatencySample>> {
    camera_burn_ids
        .iter()
        .map(|&cam_id| {
            let other_burns: Vec<u32> = camera_burn_ids
                .iter()
                .copied()
                .filter(|&id| id != cam_id)
                .collect();
            let ids = RunIds {
                node_burn: strih_burn,
                cam2: Some(cam_id),
                other_burns,
            };
            cam_strih_samples(strih, &ids)
        })
        .collect()
}

/// Per-camera MEDIAN cam→strih latency (ms) from [`n_camera_strih_samples`] — the #286
/// phase-sync measurement input to [`crate::phase_sync::compute_phase_sync_offsets`]. Reuses
/// [`hop_latency`]'s `p50_ms` (no parallel median implementation). `None` for a camera with
/// zero decoded samples in this recording (never a fabricated number — the caller must not
/// feed a guessed latency into the phase-sync offset kernel). Order matches `camera_burn_ids`.
pub fn n_camera_median_latency_ms(
    strih: &[RecordingFrame],
    strih_burn: u32,
    camera_burn_ids: &[u32],
) -> Vec<Option<f64>> {
    n_camera_strih_samples(strih, strih_burn, camera_burn_ids)
        .iter()
        .map(|samples| hop_latency("cam→strih", samples).map(|h| h.stats.p50_ms))
        .collect()
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
        // canonical max(frame_id) cam2 half (mirrors the burn-contiguity key selection).
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

/// #194 — cam2→cam1 OPTICAL-INJECTION latency referenced to the cam2 DISPLAY (page-flip)
/// instant, NOT the paint instant. This is the trustworthy cam2→cam1 the issue asks for.
///
/// [`cam2_cam1_samples_from_burn`] anchors on the cam2 QR's `gen_ts_ns` — the painter's
/// frame-GENERATION stamp, baked into the QR. But the camera films what is ON SCREEN, and
/// the frame reaches the screen only after the painter renders it AND the HDMI vblank
/// page-flip completes — `present()` blocks for that. So `cam1_capture − cam2_gen` includes
/// the painter's own generate→render→wait-for-vblank time (~16-30ms @ 60Hz), a TEST-RIG
/// artifact that inflates the optical hop above the true display→capture latency.
///
/// The QR cannot carry the post-flip time (it is rendered before the flip), so the painter
/// LOGS a per-frame flip-complete stamp (`flip_ts_ns`, captured after `present()` returns)
/// into its `--paint-log` CSV (`tick,gen_ts_ns,flip_ts_ns`, [`serialize_painter_log`]). The
/// caller passes that `tick → flip_ts_ns` map here. Then, for each stream frame carrying the
/// co-located cam1-capture burn (capture wall-clock ts) AND the cam2 QR (whose `frame_id` is
/// the painter tick), the true latency is:
///
///   `latency = cam1_burn.gen_ts_ns (capture) − flip_ts_ns[cam2_tick]  (display)`
///
/// anchored at the cam2 DISPLAY instant. Both are CLOCK_REALTIME (DanteSync), so this is the
/// real optical+capture latency of cam1 filming cam2's monitor, with the painter's internal
/// generate→display time REMOVED (that internal time is reported separately by
/// [`painter_internal_gen_to_flip`]).
///
/// A frame whose cam2 tick has no flip stamp in the map (a different painter session, or the
/// frame fell outside the logged window) is SKIPPED — never paired against a gen_ts fallback
/// (that would silently re-introduce the #194 inflation). The cam2 half is the canonical
/// highest-`frame_id` Vernier half (identical to [`split_payloads`]); `cam2_id`, when set,
/// pins cam2 exactly so a forwarded foreign burn cannot hijack it. Non-positive stamps are
/// guarded so a missing stamp can't read as a giant latency.
pub fn cam2_cam1_samples_from_flip(
    stream: &[RecordingFrame],
    cam2_id: Option<u32>,
    cam1_burn_id: u32,
    other_burns: &[u32],
    flip_ts_by_tick: &HashMap<u32, i64>,
) -> Vec<LatencySample> {
    let is_cam2 = |run_id: u32| match cam2_id {
        Some(c) => run_id == c,
        None => run_id != cam1_burn_id && !other_burns.contains(&run_id),
    };
    let mut out = Vec::new();
    for f in stream {
        // Co-located in ONE frame: cam1 burn (capture ts) + cam2 QR (its tick = frame_id,
        // the painter counter). The canonical cam2 half is the highest-frame_id Vernier
        // half — its frame_id is the tick to look the flip stamp up by.
        let mut cam2_tick: Option<u32> = None;
        let mut cam1_cap: Option<i64> = None;
        for p in &f.payloads {
            if p.run_id == cam1_burn_id {
                if p.gen_ts_ns > 0 && cam1_cap.is_none() {
                    cam1_cap = Some(p.gen_ts_ns);
                }
            } else if is_cam2(p.run_id) {
                match cam2_tick {
                    Some(t) if t >= p.frame_id => {}
                    _ => cam2_tick = Some(p.frame_id),
                }
            }
        }
        if let (Some(cap), Some(tick)) = (cam1_cap, cam2_tick) {
            // The cam2 DISPLAY (flip-complete) instant for this painter tick. NO gen_ts
            // fallback: a tick absent from the flip map is skipped, never inflated (#194).
            if let Some(&flip) = flip_ts_by_tick.get(&tick) {
                if cap > 0 && flip > 0 {
                    out.push(LatencySample {
                        latency_ms: (cap - flip) as f64 / 1_000_000.0,
                        at_ns: flip,
                    });
                }
            }
        }
    }
    out
}

/// #194 — the painter's INTERNAL generate→display time per painted tick: the time from
/// the QR's `gen_ts_ns` (generation) to its `flip_ts_ns` (on screen). This is the test-rig
/// artifact that [`cam2_cam1_samples_from_flip`] REMOVES from the cam2→cam1 hop; reporting
/// it separately keeps it VISIBLE (the painter's render + vblank-wait latency) rather than
/// hidden inside the optical number. Computed purely from the painter log's two stamps —
/// `flip - gen` for each tick. A tick missing either stamp, or with `flip < gen` (impossible
/// on a monotonic clock — a corrupt log), is skipped. Anchored at the gen instant.
pub fn painter_internal_gen_to_flip(
    gen_ts_by_tick: &HashMap<u32, i64>,
    flip_ts_by_tick: &HashMap<u32, i64>,
) -> Vec<LatencySample> {
    let mut out = Vec::new();
    for (tick, &gen) in gen_ts_by_tick {
        if let Some(&flip) = flip_ts_by_tick.get(tick) {
            if gen > 0 && flip > 0 && flip >= gen {
                out.push(LatencySample {
                    latency_ms: (flip - gen) as f64 / 1_000_000.0,
                    at_ns: gen,
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
/// (the burn-id contiguity check, [`crate::probe::burn_contiguity`]) keys on. From the SINGLE stream
/// recording this extracts, per `run_id`, the sequence of burn ids that node rendered and
/// that reached the stream output — so cam1→strih and strih→stream loss are both decided
/// from one recording, on the same integer end-to-end, with no 60→30 optical-beat ambiguity.
/// A frame may carry several QRs; only payloads matching `run_id` are taken.
pub fn burn_ids_in(frames: &[RecordingFrame], run_id: u32) -> Vec<u32> {
    burn_ids_with_frame_index_in(frames, run_id)
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}

/// #575 — like [`burn_ids_in`] (which now delegates to this, keeping ONE loop as the source of
/// truth), but paired with each payload's `frame_index` so a caller can apply a
/// frame-POSITION-based boundary trim (recording start/stop artifacts,
/// `crate::recording_boundary_trim`) before feeding the ids into a contiguity check. Trimming by
/// VALUE can't distinguish a real early/late drop from a boundary artifact; trimming by frame
/// POSITION can, because it never has to look at what the value IS.
pub fn burn_ids_with_frame_index_in(frames: &[RecordingFrame], run_id: u32) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    for f in frames {
        for p in &f.payloads {
            if p.run_id == run_id {
                out.push((f.frame_index, p.frame_id));
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

/// One per-frame row of the latency time-series CSV (#209) — the LITERAL points the
/// continuous-line proof graph plots.
///
/// The user's original ask is to SEE the latency as a continuous line over the whole
/// run (points forming a line, latency not fluctuating), not just summary p50/p99 stats.
/// Each row is one delivered stream frame that carries cam2's optical Vernier tick plus
/// the co-located node burns, so the plotter can draw, per hop, `latency_ms` (y) against
/// the chain-origin wall-clock instant (x). A GAP in the x-axis = a lost frame; a FLAT
/// line = stable latency. Built from the SINGLE stream recording (no cam2-tick
/// cross-recording pairing), so the per-frame outliers the cam2-tick method produced
/// cannot appear.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LatencyCsvRow {
    /// The cam2 optical Vernier tick (the canonical tick carried by a recorded frame whose
    /// cam2 QR decoded) — the per-frame optical identity. `None` (empty CSV cell) when the
    /// cam2 optical QR was UNDECODABLE for this frame (an optical-decode dropout, #216): the
    /// row is still emitted because the cam1/strih/stream burns are present, so the three
    /// burn-only hop lines stay UNBROKEN across the dropout — only this optical-identity
    /// column is empty. A monotonic gap across the `Some` ticks is a dropped OPTICAL frame.
    pub frame_id: Option<u32>,
    /// The chain ORIGIN wall-clock instant for this frame (ns since epoch) — cam1's
    /// CAPTURE burn `gen_ts_ns` when present, else strih's render `gen_ts_ns`, else the
    /// cam2 paint instant. This is the row's x-axis time anchor (the earliest stamp the
    /// frame carries), so every hop's line shares one consistent time axis.
    pub gen_ts_ns: i64,
    /// The cam2 DISPLAY (page-flip) instant for this tick (ns), when a painter flip map is
    /// supplied (`--painter` flip log). `None` (empty CSV cell) when no flip map is given.
    /// Kept so the test-rig painter display time stays visible per frame.
    pub flip_ts_ns: Option<i64>,
    /// cam1→strih per-hop latency (ms) for this frame = `strih_burn − cam1_burn`. `None`
    /// when this frame did not carry BOTH burns (empty CSV cell → a gap in that hop's line).
    pub cam1_strih_ms: Option<f64>,
    /// strih→stream per-hop latency (ms) = `stream_burn − strih_burn`. `None` if either
    /// burn is absent on this frame.
    pub strih_stream_ms: Option<f64>,
    /// cam1→stream END-TO-END latency (ms) = `stream_burn − cam1_burn`. `None` if either
    /// burn is absent on this frame.
    pub cam1_stream_ms: Option<f64>,
    /// cam2→cam1 OPTICAL-INJECTION latency (ms) for this frame = `cam1_capture_burn −
    /// cam2_display`, where `cam2_display` is the cam2 page-flip (`flip_ts`) instant when a
    /// painter flip map is supplied (#194), else the cam2 paint (`gen_ts`) instant (#179,
    /// inflated). This is the cam2 monitor → cam1 camera lens → v4l2-capture leg.
    ///
    /// #216 — HONEST GAP: `None` (empty CSV cell) when this frame had NO readable cam2 optical
    /// QR (the cam1 camera failed to OPTICALLY READ the cam2 monitor — an optical-injection
    /// READ DROPOUT, NOT a chain frame loss), or no cam1 capture burn, or (flip mode) the tick
    /// has no flip stamp. The plotter renders that empty cell as a VISIBLE GAP in the
    /// cam2→cam1 line — the optical read-failure shown honestly, NEVER drawn across. The three
    /// burn-only hops above stay UNBROKEN across the same dropout (they need only the digital
    /// burns, which ride through even when the optical read fails) — so the graph distinguishes
    /// the REAL chain (continuous) from the optical-injection read dropout (gapped).
    pub cam2_cam1_ms: Option<f64>,
}

impl LatencyCsvRow {
    /// The CSV header line (column order = struct field order). Single source of truth so
    /// the writer and any reader/plotter agree on the columns.
    pub const HEADER: &'static str =
        "frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms,cam2_cam1_ms";

    /// This row as one CSV line. `Option` fields render as an empty cell when `None` (a
    /// gap the plotter draws as a break in that hop's line); ms are fixed to 6 decimals.
    pub fn to_csv_line(&self) -> String {
        fn ms(v: Option<f64>) -> String {
            v.map(|x| format!("{x:.6}")).unwrap_or_default()
        }
        fn ns(v: Option<i64>) -> String {
            v.map(|x| x.to_string()).unwrap_or_default()
        }
        fn tick(v: Option<u32>) -> String {
            v.map(|x| x.to_string()).unwrap_or_default()
        }
        format!(
            "{},{},{},{},{},{},{}",
            tick(self.frame_id),
            self.gen_ts_ns,
            ns(self.flip_ts_ns),
            ms(self.cam1_strih_ms),
            ms(self.strih_stream_ms),
            ms(self.cam1_stream_ms),
            ms(self.cam2_cam1_ms),
        )
    }
}

/// Build the per-frame latency time-series rows (#209) from the SINGLE stream recording.
///
/// For each recorded stream frame carrying a valid x-axis time anchor, emit one
/// [`LatencyCsvRow`]: the cam2 tick (`frame_id`, `None` when the cam2 QR was undecodable —
/// #216), the chain-origin wall-clock anchor (`gen_ts_ns`), and the three co-located
/// per-hop latencies (cam1→strih, strih→stream, cam1→stream). The burns are paired WITHIN
/// the one frame (no cam2-tick cross-recording pairing), exactly as
/// [`chain_hop_samples_from_stream`] does, so the per-frame points match the summary-stat
/// hops the verdict already reports — the CSV is the per-frame expansion of the same
/// numbers, not a parallel measurement.
///
/// A hop's latency is `None` for a frame that did not carry BOTH of that hop's burns
/// (e.g. a frame with only cam2 + cam1 burn has cam1→strih = None) — an empty CSV cell,
/// which the plotter renders as a break in that hop's line. `gen_ts_ns ≤ 0` burn stamps are
/// treated as absent (never a wrong number / negative latency).
///
/// #216 — a row is emitted whenever the frame has a valid x anchor (a positive cam1/strih
/// burn or cam2 paint stamp), EVEN WHEN the cam2 optical tick is absent. The cam2 OPTICAL QR
/// (cam1 filming cam2's monitor) can go undecodable for a stretch (an optical-decode
/// dropout) while the cam1/strih/stream BURNS stay present; the three burn-only hops are
/// still computable, so emitting the row keeps those three lines UNBROKEN across the dropout
/// (without #216 the whole row was skipped, blanking ~150s of all three lines). Such a row
/// has `frame_id = None` (empty cell) and no `flip_ts_ns` (it is keyed on the cam2 tick).
/// Rows are emitted in capture order. The ONLY frame still skipped is one with NEITHER a
/// cam2 tick NOR any positive stamp — no x identity at all (a 0-anchor row would plot far
/// left of t0 and distort the continuous line; it could not be paired on a hop anyway).
///
/// `cam2_pin` (the `--cam2-run-id` the painter used): when `Some`, cam2 is matched EXACTLY
/// by this run_id — IDENTICAL to [`RunIds::is_cam2`] / [`split_payloads`], so a forwarded
/// foreign QR can never be mistaken for cam2 (the cam2-pinned summary stats the CSV mirrors
/// use the same rule). When `None`, cam2 = any payload that is not one of the three burns.
///
/// `flip_ts_by_tick` (optional, from the painter `--paint-log`): when non-empty, each
/// row's `flip_ts_ns` is the cam2 DISPLAY instant for its tick; absent ⇒ `None`.
pub fn per_frame_latency_csv_rows(
    stream: &[RecordingFrame],
    cam1_run_id: u32,
    strih_run_id: u32,
    stream_run_id: u32,
    cam2_pin: Option<u32>,
    flip_ts_by_tick: &HashMap<u32, i64>,
) -> Vec<LatencyCsvRow> {
    let is_burn =
        |run_id: u32| run_id == cam1_run_id || run_id == strih_run_id || run_id == stream_run_id;
    // cam2 selection mirrors RunIds::is_cam2: EXACT when pinned, else any non-burn QR.
    let is_cam2 = |run_id: u32| match cam2_pin {
        Some(c) => run_id == c,
        None => !is_burn(run_id),
    };
    let mut out = Vec::new();
    for f in stream {
        // Canonical cam2 Vernier tick = the highest-frame_id cam2 half.
        let mut cam2_tick: Option<u32> = None;
        let mut cam2_gen: Option<i64> = None;
        let mut cam1_ts: Option<i64> = None;
        let mut strih_ts: Option<i64> = None;
        let mut stream_ts: Option<i64> = None;
        for p in &f.payloads {
            if p.run_id == cam1_run_id {
                if p.gen_ts_ns > 0 && cam1_ts.is_none() {
                    cam1_ts = Some(p.gen_ts_ns);
                }
            } else if p.run_id == strih_run_id {
                if p.gen_ts_ns > 0 && strih_ts.is_none() {
                    strih_ts = Some(p.gen_ts_ns);
                }
            } else if p.run_id == stream_run_id {
                if p.gen_ts_ns > 0 && stream_ts.is_none() {
                    stream_ts = Some(p.gen_ts_ns);
                }
            } else if is_cam2(p.run_id) {
                // cam2 optical half — keep the freshest (max frame_id) per the canonical
                // Vernier rule, and remember its paint stamp as the last-resort x anchor.
                match cam2_tick {
                    Some(t) if t >= p.frame_id => {}
                    _ => {
                        cam2_tick = Some(p.frame_id);
                        cam2_gen = if p.gen_ts_ns > 0 {
                            Some(p.gen_ts_ns)
                        } else {
                            None
                        };
                    }
                }
            }
        }
        let ms = |down: Option<i64>, up: Option<i64>| -> Option<f64> {
            match (up, down) {
                (Some(u), Some(d)) => Some((d - u) as f64 / 1_000_000.0),
                _ => None,
            }
        };
        // x-axis anchor = the earliest POSITIVE chain stamp this frame carries: cam1
        // capture, else strih render, else cam2 paint. A frame with no positive stamp at
        // all has no valid x anchor — SKIP it (a 0 anchor would plot far left of t0 and
        // distort the line; with no stamp it can't be paired on any hop either).
        //
        // #216: the row is emitted whenever it has a valid x anchor, EVEN WHEN the cam2
        // optical tick is absent (an optical-decode dropout). The three burn-only hops
        // (cam1→strih, strih→stream, cam1→stream) need only the burns — which are present
        // across an optical dropout — so emitting the row keeps those three lines UNBROKEN
        // over the dropout (the ~150s blank band that #216 reports). Only the cam2-derived
        // fields are absent for such a row: `frame_id` is None (empty cell) and `flip_ts_ns`
        // can't be looked up (no tick). A frame with neither a cam2 tick NOR a positive
        // burn/paint stamp has no x identity at all and is the only one still skipped.
        let Some(gen_ts_ns) = cam1_ts.or(strih_ts).or(cam2_gen) else {
            continue;
        };
        // #216 — cam2→cam1 OPTICAL latency = cam1_capture − cam2_display, anchored CO-LOCATED
        // in THIS frame (no cross-recording pairing). cam2_display = the cam2 page-flip stamp
        // (flip map, #194) when this frame's tick is mapped, else the cam2 PAINT gen_ts (#179).
        // It is `None` — an empty CSV cell, the HONEST GAP the plotter shows — whenever the
        // cam1 camera did NOT optically read a cam2 QR this frame (no cam2 tick / no cam2 paint
        // stamp), or there is no cam1 capture burn, or (flip mode for this tick) no flip stamp.
        // NEVER drawn across: an optical READ DROPOUT gaps here while the three burn hops above
        // stay unbroken (the burns ride through even when the optical read fails).
        let cam2_display: Option<i64> =
            match cam2_tick.and_then(|t| flip_ts_by_tick.get(&t).copied()) {
                Some(flip) if flip > 0 => Some(flip), // #194 display (page-flip) reference
                _ => cam2_gen.filter(|&g| g > 0),     // #179 paint (gen) reference fallback
            };
        out.push(LatencyCsvRow {
            frame_id: cam2_tick,
            gen_ts_ns,
            // flip_ts is a cam2 DISPLAY instant keyed on the cam2 tick — only resolvable
            // when this frame actually carried a cam2 tick (an optical-dropout row has none).
            flip_ts_ns: cam2_tick.and_then(|t| flip_ts_by_tick.get(&t).copied()),
            cam1_strih_ms: ms(strih_ts, cam1_ts),
            strih_stream_ms: ms(stream_ts, strih_ts),
            cam1_stream_ms: ms(stream_ts, cam1_ts),
            // cam1_capture − cam2_display, both wall-clock; None on any optical-read dropout.
            cam2_cam1_ms: ms(cam1_ts, cam2_display),
        });
    }
    out
}

/// Write the per-frame latency rows (#209) as a CSV file at `path` (header + one line per
/// row). Returns the number of data rows written. The file is the literal-continuous-line
/// proof input for `scripts/latency-line-report.py`.
pub fn write_latency_csv(path: &std::path::Path, rows: &[LatencyCsvRow]) -> std::io::Result<usize> {
    use std::io::Write;
    let mut buf = String::with_capacity(rows.len() * 48 + LatencyCsvRow::HEADER.len() + 1);
    buf.push_str(LatencyCsvRow::HEADER);
    buf.push('\n');
    for r in rows {
        buf.push_str(&r.to_csv_line());
        buf.push('\n');
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(buf.as_bytes())?;
    Ok(rows.len())
}

/// One cam2→cam1 OPTICAL-READ DROPOUT window (#216): a stretch where the cam1 camera could
/// not OPTICALLY read the cam2 monitor QR (`cam2_cam1_ms` absent on consecutive frames) while
/// the burn hops kept flowing. This is a REAL readability failure on the cam2→cam1 optical-
/// injection leg — NOT a chain frame loss — reported HONESTLY (never hidden) so the verdict
/// and the graph both surface it as a labeled finding.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OpticalReadDropout {
    /// Seconds since the run's first frame where the dropout BEGAN.
    pub start_s: f64,
    /// Seconds since the run's first frame where the dropout ENDED (the read recovered).
    pub end_s: f64,
    /// Duration of the dropout (seconds) = `end_s - start_s`.
    pub dur_s: f64,
    /// Number of consecutive frames with no readable cam2 optical QR in the window.
    pub frames: usize,
}

/// Detect the cam2→cam1 OPTICAL-READ DROPOUT windows (#216) from the per-frame CSV rows.
///
/// A dropout = a run of consecutive rows whose `cam2_cam1_ms` is `None` (the cam1 camera did
/// not read a cam2 QR that frame) LONGER than `min_dur_s`. Below that floor it is normal
/// per-frame QR-decode jitter, not a dropout worth reporting (the real 30-min run's dropout
/// was ~175 s; a few-frame blink is noise). The window's start/end are taken from each row's
/// `gen_ts_ns` x-anchor (the burn/paint wall-clock the same axis the plotter uses), so the
/// reported seconds match the graph exactly.
///
/// HONEST SCOPE: this is the cam2→cam1 OPTICAL leg ONLY — a labeled readability finding, NOT a
/// chain loss. The digital burn-hop contiguity (the gate) is unaffected; a dropout here never
/// fails the zero-loss verdict, it is reported alongside it.
pub fn optical_read_dropouts(rows: &[LatencyCsvRow], min_dur_s: f64) -> Vec<OpticalReadDropout> {
    let t0 = rows
        .iter()
        .map(|r| r.gen_ts_ns)
        .filter(|&g| g > 0)
        .min()
        .unwrap_or(0);
    let to_s = |g: i64| (g - t0) as f64 / 1e9;
    let mut out = Vec::new();
    let mut run_start: Option<f64> = None;
    let mut run_frames: usize = 0;
    let mut last_t: Option<f64> = None;
    for r in rows {
        if r.gen_ts_ns <= 0 {
            continue;
        }
        let t = to_s(r.gen_ts_ns);
        if r.cam2_cam1_ms.is_none() {
            if run_start.is_none() {
                // Anchor the start at the last GOOD frame's time (the read was fine up to there),
                // falling back to this frame when the run leads the recording.
                run_start = Some(last_t.unwrap_or(t));
                run_frames = 0;
            }
            run_frames += 1;
        } else {
            if let (Some(start), Some(end)) = (run_start, last_t) {
                let dur = end - start;
                if dur >= min_dur_s {
                    out.push(OpticalReadDropout {
                        start_s: start,
                        end_s: end,
                        dur_s: dur,
                        frames: run_frames,
                    });
                }
            }
            run_start = None;
            run_frames = 0;
        }
        last_t = Some(t);
    }
    // A trailing dropout (recording ended mid-failure).
    if let (Some(start), Some(end)) = (run_start, last_t) {
        let dur = end - start;
        if dur >= min_dur_s {
            out.push(OpticalReadDropout {
                start_s: start,
                end_s: end,
                dur_s: dur,
                frames: run_frames,
            });
        }
    }
    out
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

    // ---- #286 4-camera phase-sync: n_camera_strih_samples / n_camera_median_latency_ms ----

    #[test]
    fn n_camera_strih_samples_pairs_each_camera_by_its_own_burn_id_independently() {
        // 3 cameras' capture burns ride the SAME strih frames alongside strih's own burn.
        // Each camera keeps a CONSTANT latency across both frames so the per-camera result is
        // trivially checkable, and pairing must never cross-contaminate cameras.
        const CAM_A: u32 = BURN_RUN_ID_CAM1; // 911001
        const CAM_B: u32 = 911005;
        const CAM_C: u32 = 911006;
        let base = 1_700_000_000_000_000_000i64;
        let frames: Vec<RecordingFrame> = (0..2u64)
            .map(|i| {
                let t = base + i as i64 * 1_000_000_000; // 1s apart
                multi(
                    i,
                    &[
                        (CAM_A, 100 + i as u32, t),
                        (CAM_B, 200 + i as u32, t + 10_000_000), // 10ms after CAM_A's capture
                        (CAM_C, 300 + i as u32, t + 20_000_000), // 20ms after CAM_A's capture
                        (BURN_RUN_ID_STRIH, 400 + i as u32, t + 100_000_000), // strih +100ms of t
                    ],
                )
            })
            .collect();
        // Request B then A then C — output order must follow the REQUEST order, not any
        // internal/declaration ordering.
        let out = n_camera_strih_samples(&frames, BURN_RUN_ID_STRIH, &[CAM_B, CAM_A, CAM_C]);
        assert_eq!(out.len(), 3);
        // CAM_B latency = strih(t+100ms) - CAM_B(t+10ms) = 90ms, constant across both frames.
        assert_eq!(out[0].len(), 2, "CAM_B samples {:?}", out[0]);
        for s in &out[0] {
            assert!((s.latency_ms - 90.0).abs() < 1e-6, "CAM_B {:?}", out[0]);
        }
        // CAM_A latency = 100ms constant.
        assert_eq!(out[1].len(), 2, "CAM_A samples {:?}", out[1]);
        for s in &out[1] {
            assert!((s.latency_ms - 100.0).abs() < 1e-6, "CAM_A {:?}", out[1]);
        }
        // CAM_C latency = 80ms constant.
        assert_eq!(out[2].len(), 2, "CAM_C samples {:?}", out[2]);
        for s in &out[2] {
            assert!((s.latency_ms - 80.0).abs() < 1e-6, "CAM_C {:?}", out[2]);
        }
    }

    #[test]
    fn n_camera_median_latency_ms_reports_median_per_camera_and_none_for_absent_camera() {
        const CAM_A: u32 = BURN_RUN_ID_CAM1;
        const CAM_B: u32 = 911005;
        const CAM_ABSENT: u32 = 911099; // never decoded in this recording
        let base = 1_700_000_000_000_000_000i64;
        let frames: Vec<RecordingFrame> = (0..3u64)
            .map(|i| {
                let t = base + i as i64 * 1_000_000_000;
                multi(
                    i,
                    &[
                        (CAM_A, 100 + i as u32, t),
                        (CAM_B, 200 + i as u32, t + 30_000_000),
                        (BURN_RUN_ID_STRIH, 400 + i as u32, t + 100_000_000),
                    ],
                )
            })
            .collect();
        let medians =
            n_camera_median_latency_ms(&frames, BURN_RUN_ID_STRIH, &[CAM_A, CAM_B, CAM_ABSENT]);
        assert_eq!(medians.len(), 3);
        assert!(
            (medians[0].unwrap() - 100.0).abs() < 1e-6,
            "CAM_A {:?}",
            medians[0]
        );
        assert!(
            (medians[1].unwrap() - 70.0).abs() < 1e-6,
            "CAM_B {:?}",
            medians[1]
        );
        assert_eq!(
            medians[2], None,
            "a camera that never decoded must report None, not a fabricated number"
        );
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

    #[test]
    fn cam2_cam1_from_flip_uses_flip_ts_not_gen_ts_the_194_fix() {
        // #194: the cam2→cam1 latency must reference the cam2 DISPLAY (page-flip) instant,
        // NOT the paint (gen) instant. Here the cam2 QR's gen_ts is 1.000s but the painter's
        // flip-complete for that tick is 1.018s (18ms of render + vblank-wait). cam1 captured
        // at 1.068s. The TRUE display→capture latency is 1.068 − 1.018 = 50ms. The OLD
        // gen-based number would be 1.068 − 1.000 = 68ms — INFLATED by the painter's own
        // generate→display time. This test FAILS if the function ever uses gen_ts.
        let stream = vec![
            multi(
                0,
                &[
                    (CAM2, 100, 1_000_000_000), // cam2 QR: paint(gen) @ 1.000s (in the QR)
                    (BURN_RUN_ID_CAM1, 40, 1_068_000_000), // cam1 captured @ 1.068s
                    (BURN_RUN_ID_STRIH, 80, 1_240_000_000), // forwarded (ignored)
                    (BURN_RUN_ID_STREAM, 90, 2_300_000_000), // forwarded (ignored)
                ],
            ),
            multi(
                1,
                &[
                    (CAM2, 102, 1_033_000_000),            // gen @ 1.033s
                    (BURN_RUN_ID_CAM1, 41, 1_103_000_000), // captured @ 1.103s
                ],
            ),
        ];
        // Painter flip-complete map: tick → on-screen instant (gen + ~18/20ms each).
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(100, 1_018_000_000); // tick 100 on screen 18ms after paint
        flip.insert(102, 1_053_000_000); // tick 102 on screen 20ms after paint
        let s = cam2_cam1_samples_from_flip(
            &stream,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            &[BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
            &flip,
        );
        assert_eq!(
            s.len(),
            2,
            "one sample per frame with cam1 burn + a mapped cam2 tick"
        );
        assert!(
            (s[0].latency_ms - 50.0).abs() < 1e-6,
            "frame 0: cam1 capture 1.068s − cam2 FLIP 1.018s = 50ms (NOT 68ms from gen)"
        );
        assert!(
            (s[1].latency_ms - 50.0).abs() < 1e-6,
            "frame 1: 1.103s − 1.053s = 50ms (NOT 70ms from gen)"
        );
        assert_eq!(
            s[0].at_ns, 1_018_000_000,
            "anchored at the cam2 DISPLAY (flip) instant, not the paint instant"
        );
        // Cross-check: the gen-based path on the SAME data gives the inflated 68/70ms, so the
        // flip path is provably NOT just returning the gen number.
        let g = cam2_cam1_samples_from_burn(
            &stream,
            Some(CAM2),
            BURN_RUN_ID_CAM1,
            &[BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        );
        assert!(
            (g[0].latency_ms - 68.0).abs() < 1e-6 && (s[0].latency_ms < g[0].latency_ms),
            "flip-based latency (50ms) is strictly LESS than gen-based (68ms) — the #194 \
             inflation removed"
        );
    }

    #[test]
    fn cam2_cam1_from_flip_skips_tick_with_no_flip_stamp_no_gen_fallback() {
        // A cam2 tick absent from the flip map (different painter session, or outside the
        // logged window) is SKIPPED — NEVER paired against a gen_ts fallback (that would
        // silently re-introduce the #194 inflation). Frame 1's tick 102 has no flip entry.
        let stream = vec![
            multi(
                0,
                &[
                    (CAM2, 100, 1_000_000_000),
                    (BURN_RUN_ID_CAM1, 40, 1_050_000_000),
                ],
            ),
            multi(
                1,
                &[
                    (CAM2, 102, 1_033_000_000),
                    (BURN_RUN_ID_CAM1, 41, 1_120_000_000),
                ],
            ),
        ];
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(100, 1_010_000_000); // only tick 100 mapped
        let s = cam2_cam1_samples_from_flip(&stream, Some(CAM2), BURN_RUN_ID_CAM1, &[], &flip);
        assert_eq!(
            s.len(),
            1,
            "only the mapped tick 100 yields a sample; 102 is skipped"
        );
        assert!(
            (s[0].latency_ms - 40.0).abs() < 1e-6,
            "1.050s − flip 1.010s = 40ms"
        );
    }

    #[test]
    fn cam2_cam1_from_flip_skips_zero_stamps() {
        // A 0-sentinel on the capture stamp OR a 0 flip stamp is guarded so a missing stamp
        // can never read as a giant latency.
        let stream = vec![
            multi(0, &[(CAM2, 100, 1_000_000_000), (BURN_RUN_ID_CAM1, 40, 0)]), // 0 capture
            multi(
                1,
                &[
                    (CAM2, 102, 1_033_000_000),
                    (BURN_RUN_ID_CAM1, 41, 1_120_000_000),
                ],
            ),
        ];
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(100, 1_010_000_000);
        flip.insert(102, 0); // 0 flip stamp → skip
        let s = cam2_cam1_samples_from_flip(&stream, Some(CAM2), BURN_RUN_ID_CAM1, &[], &flip);
        assert!(
            s.is_empty(),
            "0 capture and 0 flip are both guarded → no sample"
        );
    }

    #[test]
    fn painter_internal_gen_to_flip_is_flip_minus_gen() {
        // #194: the painter's INTERNAL generate→display time, reported separately so the
        // render + vblank-wait artifact removed from cam2→cam1 stays VISIBLE. flip − gen.
        let mut gen: HashMap<u32, i64> = HashMap::new();
        gen.insert(100, 1_000_000_000);
        gen.insert(102, 1_033_000_000);
        gen.insert(104, 1_066_000_000);
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(100, 1_018_000_000); // +18ms
        flip.insert(102, 1_053_000_000); // +20ms
                                         // tick 104 has NO flip entry → skipped (no half-sample)
        let mut s = painter_internal_gen_to_flip(&gen, &flip);
        s.sort_by_key(|x| x.at_ns);
        assert_eq!(s.len(), 2, "only ticks with BOTH stamps contribute");
        assert!((s[0].latency_ms - 18.0).abs() < 1e-6);
        assert!((s[1].latency_ms - 20.0).abs() < 1e-6);
        assert_eq!(s[0].at_ns, 1_000_000_000, "anchored at the gen instant");
    }

    #[test]
    fn painter_internal_gen_to_flip_skips_corrupt_flip_before_gen() {
        // flip < gen is impossible on a monotonic clock (a corrupt log) → skipped, never a
        // negative latency.
        let mut gen: HashMap<u32, i64> = HashMap::new();
        gen.insert(1, 2_000_000_000);
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(1, 1_000_000_000); // BEFORE gen → corrupt → skip
        assert!(painter_internal_gen_to_flip(&gen, &flip).is_empty());
    }

    // ===== #209: per-frame latency CSV time-series rows ==============================

    /// Build a stream-recorded frame from a cam2 optical (run_id,tick) plus any of the
    /// three co-located node burns as `(run_id, gen_ts_ns)` — the exact shape the stream
    /// recording carries (cam2 forwarded + cam1/strih/stream render burns).
    fn csv_frame(
        idx: u64,
        cam2: Option<(u32, u32)>, // (run_id, tick)
        cam1_burn: Option<i64>,   // cam1 capture gen_ts_ns
        strih_burn: Option<i64>,  // strih render gen_ts_ns
        stream_burn: Option<i64>, // stream render gen_ts_ns
    ) -> RecordingFrame {
        let mut payloads = Vec::new();
        if let Some((r, t)) = cam2 {
            // #216: cam2's paint stamp = cam1 capture − 100 ms (a deterministic 100 ms
            // optical-injection latency) when a cam1 burn is present, so the cam2→cam1 CSV
            // column reads a clean 100.0 ms for a healthy optical read; falls back to 1 ns
            // when there is no cam1 burn (the cam2_cam1 value is then unused / guarded).
            let cam2_paint = match cam1_burn {
                Some(g) if g > 100_000_000 => g - 100_000_000,
                _ => 1,
            };
            payloads.push(Payload {
                run_id: r,
                frame_id: t,
                gen_ts_ns: cam2_paint,
            });
        }
        if let Some(g) = cam1_burn {
            payloads.push(Payload {
                run_id: BURN_RUN_ID_CAM1,
                frame_id: idx as u32,
                gen_ts_ns: g,
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
    fn per_frame_csv_row_has_the_right_columns_and_all_three_hop_latencies() {
        // One stream frame carrying cam2 + all three burns: cam1=+0, strih=+10ms,
        // stream=+25ms. The row must carry the cam2 tick as frame_id, the cam1 capture
        // stamp as the x anchor, and all three per-hop latencies on the shared clock.
        let cam1 = 1_000_000_000; // chain origin
        let strih = 1_010_000_000; // +10 ms
        let stream = 1_025_000_000; // +25 ms from cam1
        let frames = vec![csv_frame(
            0,
            Some((CAM2, 500)),
            Some(cam1),
            Some(strih),
            Some(stream),
        )];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1, "one delivered optical frame ⇒ one row");
        let r = rows[0];
        assert_eq!(
            r.frame_id,
            Some(500),
            "frame_id = cam2 optical Vernier tick"
        );
        assert_eq!(
            r.gen_ts_ns, cam1,
            "x anchor = chain-origin (cam1 capture) stamp"
        );
        assert_eq!(r.flip_ts_ns, None, "no painter flip map ⇒ flip empty");
        assert!(
            (r.cam1_strih_ms.unwrap() - 10.0).abs() < 1e-6,
            "cam1→strih = +10 ms"
        );
        assert!(
            (r.strih_stream_ms.unwrap() - 15.0).abs() < 1e-6,
            "strih→stream = +15 ms"
        );
        assert!(
            (r.cam1_stream_ms.unwrap() - 25.0).abs() < 1e-6,
            "cam1→stream = +25 ms end-to-end"
        );

        // The CSV header is the contract the plotter reads — exact column order.
        assert_eq!(
            LatencyCsvRow::HEADER,
            "frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms,cam2_cam1_ms"
        );
        // cam2 paint = cam1 capture − 100 ms (csv_frame default) ⇒ cam2_cam1 = 100 ms.
        assert!(
            (r.cam2_cam1_ms.unwrap() - 100.0).abs() < 1e-6,
            "cam2→cam1 optical = +100 ms (a readable cam2 QR this frame)"
        );
        assert_eq!(
            r.to_csv_line(),
            "500,1000000000,,10.000000,15.000000,25.000000,100.000000",
            "CSV line: tick, x-anchor, empty flip, three burn hops, cam2→cam1 optical"
        );
    }

    #[test]
    fn per_frame_csv_missing_hop_burn_is_an_empty_cell_a_gap_in_that_line() {
        // A frame carrying cam2 + cam1 burn only ⇒ cam1→strih / strih→stream /
        // cam1→stream are all None (empty cells) — the plotter draws a break, not a 0.
        let frames = vec![csv_frame(
            0,
            Some((CAM2, 700)),
            Some(2_000_000_000),
            None,
            None,
        )];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        let r = rows[0];
        assert_eq!(r.frame_id, Some(700));
        assert_eq!(r.cam1_strih_ms, None);
        assert_eq!(r.strih_stream_ms, None);
        assert_eq!(r.cam1_stream_ms, None);
        // cam2 + cam1 burn ARE present ⇒ cam2→cam1 optical = +100 ms (the only filled hop).
        assert!((r.cam2_cam1_ms.unwrap() - 100.0).abs() < 1e-6);
        assert_eq!(
            r.to_csv_line(),
            "700,2000000000,,,,,100.000000",
            "empty cells for every absent burn hop = gaps; cam2→cam1 filled (read OK)"
        );
    }

    #[test]
    fn per_frame_csv_frame_with_no_cam2_but_burns_still_emits_a_row_216() {
        // #216 REVISION of the former `..._no_cam2_optical_is_skipped`: that test enshrined
        // the bug — it skipped a frame that had NO cam2 QR but DID carry all three burns,
        // blanking the burn-only hop lines across an optical-decode dropout. The correct
        // behavior is to EMIT a row (frame_id = None / empty cell) so the cam1→strih /
        // strih→stream / cam1→stream lines stay unbroken; only the cam2-optical identity is
        // absent. A frame with no cam2 AND no positive stamp is the only one still skipped
        // (covered by `per_frame_csv_frame_with_no_cam2_and_no_stamp_is_skipped`).
        let frames = vec![
            csv_frame(
                0,
                Some((CAM2, 100)),
                Some(1_000_000_000),
                Some(1_010_000_000),
                Some(1_020_000_000),
            ),
            csv_frame(
                1,
                None, // no cam2 optical QR — but all three burns present (optical dropout)
                Some(1_100_000_000),
                Some(1_110_000_000),
                Some(1_120_000_000),
            ),
            csv_frame(
                2,
                Some((CAM2, 102)),
                Some(1_200_000_000),
                Some(1_210_000_000),
                Some(1_220_000_000),
            ),
        ];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        let ticks: Vec<Option<u32>> = rows.iter().map(|r| r.frame_id).collect();
        assert_eq!(
            ticks,
            vec![Some(100), None, Some(102)],
            "#216: all three frames emit a row; the no-cam2 frame's tick is None (empty cell)"
        );
        // The no-cam2 row still carries its three burn-only hops (the chain was alive).
        assert!((rows[1].cam1_strih_ms.unwrap() - 10.0).abs() < 1e-6);
        assert!((rows[1].strih_stream_ms.unwrap() - 10.0).abs() < 1e-6);
        assert!((rows[1].cam1_stream_ms.unwrap() - 20.0).abs() < 1e-6);
        assert_eq!(
            rows[1].gen_ts_ns, 1_100_000_000,
            "x anchor = cam1 capture burn, even with no cam2 tick"
        );
    }

    #[test]
    fn per_frame_csv_frame_with_no_cam2_and_no_stamp_is_skipped() {
        // The ONLY frame still skipped after #216: no cam2 tick AND no positive burn/paint
        // stamp ⇒ no x-axis identity at all (it could not be paired on any hop either).
        let frames = vec![
            // A bare frame with zero payloads — nothing to anchor on.
            RecordingFrame {
                frame_index: 0,
                payloads: vec![],
                tick: None,
            },
            csv_frame(
                1,
                Some((CAM2, 102)),
                Some(1_000_000_000),
                Some(1_010_000_000),
                Some(1_020_000_000),
            ),
        ];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        assert_eq!(
            rows.len(),
            1,
            "the empty frame is skipped; the valid one stays"
        );
        assert_eq!(rows[0].frame_id, Some(102));
    }

    #[test]
    fn per_frame_csv_optical_decode_dropout_keeps_burn_hops_unbroken_216() {
        // #216 — the ~150s blank band: a stretch of frames whose cam2 OPTICAL QR is
        // undecodable (an optical-decode dropout, NOT a chain loss) while the cam1/strih/
        // stream BURNS are present the whole time. Before #216 such a frame was skipped
        // entirely (no cam2 tick ⇒ `continue`), so the cam1→strih / strih→stream /
        // cam1→stream lines — which need ONLY the burns, not cam2 — went blank across the
        // dropout. The fix: emit a row for every burn-carrying frame so those three lines
        // stay UNBROKEN; only the cam2-dependent `frame_id` column is empty for the gap.
        //
        // Layout: a healthy cam2 frame, then 3 burns-only frames (optical dropout), then a
        // healthy cam2 frame again. Every frame carries all three burns (chain alive).
        let frames = vec![
            csv_frame(
                0,
                Some((CAM2, 100)),
                Some(1_000_000_000),
                Some(1_010_000_000),
                Some(1_025_000_000),
            ),
            // --- optical-decode dropout: burns present, cam2 QR undecodable (no cam2) ---
            csv_frame(
                1,
                None,
                Some(1_033_000_000),
                Some(1_043_000_000),
                Some(1_058_000_000),
            ),
            csv_frame(
                2,
                None,
                Some(1_066_000_000),
                Some(1_076_000_000),
                Some(1_091_000_000),
            ),
            csv_frame(
                3,
                None,
                Some(1_099_000_000),
                Some(1_109_000_000),
                Some(1_124_000_000),
            ),
            // --- optical decode recovers ---
            csv_frame(
                4,
                Some((CAM2, 104)),
                Some(1_132_000_000),
                Some(1_142_000_000),
                Some(1_157_000_000),
            ),
        ];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        // ALL five frames must emit a row — the three dropout frames are NOT skipped, so
        // the burn-hop lines have a point at every frame across the whole window (#216).
        assert_eq!(
            rows.len(),
            5,
            "#216: every burn-carrying frame emits a row, incl. the optical-dropout stretch — \
             no blank band"
        );
        // Every row carries the three burn-only hops (computable from burns alone): +10ms,
        // +15ms, +25ms. NOT just the two cam2-bearing frames — the dropout frames too.
        for (i, r) in rows.iter().enumerate() {
            assert!(
                (r.cam1_strih_ms.unwrap() - 10.0).abs() < 1e-6,
                "row {i}: cam1→strih = +10 ms (burn-only, present across the dropout)"
            );
            assert!(
                (r.strih_stream_ms.unwrap() - 15.0).abs() < 1e-6,
                "row {i}: strih→stream = +15 ms"
            );
            assert!(
                (r.cam1_stream_ms.unwrap() - 25.0).abs() < 1e-6,
                "row {i}: cam1→stream = +25 ms end-to-end"
            );
            assert!(
                r.gen_ts_ns > 0,
                "row {i}: every row has a positive x anchor"
            );
        }
        // The three dropout rows (index 1..=3) have NO cam2 tick (empty frame_id cell) but
        // STILL carry their three burn hops — the cam1 capture burn is the x anchor. The
        // healthy frames keep their cam2 tick. (`frame_id` is `Option<u32>` after #216.)
        assert_eq!(
            rows[1].frame_id, None,
            "#216: optical-dropout frame has no cam2 tick (empty frame_id) but still a row"
        );
        assert_eq!(rows[2].frame_id, None);
        assert_eq!(rows[3].frame_id, None);
        assert_eq!(rows[0].frame_id, Some(100));
        assert_eq!(rows[4].frame_id, Some(104));

        // #216 HONEST GAP — the cam2→cam1 OPTICAL line MUST gap across the dropout (the cam1
        // camera could not read the cam2 QR), NEVER drawn across. The dropout rows (1..=3) have
        // NO cam2 optical stamp ⇒ cam2_cam1_ms is None (empty cell ⇒ visible gap). The healthy
        // rows (0, 4) DID read the cam2 QR ⇒ cam2_cam1_ms is filled (+100 ms). This is the
        // distinction: burn hops continuous (real chain), cam2→cam1 optical gapped (read failure).
        assert_eq!(
            rows[1].cam2_cam1_ms, None,
            "#216: optical-dropout frame gaps the cam2→cam1 line (no read), NOT drawn across"
        );
        assert_eq!(rows[2].cam2_cam1_ms, None);
        assert_eq!(rows[3].cam2_cam1_ms, None);
        assert!(
            (rows[0].cam2_cam1_ms.unwrap() - 100.0).abs() < 1e-6,
            "healthy optical read ⇒ cam2→cam1 filled (+100 ms)"
        );
        assert!((rows[4].cam2_cam1_ms.unwrap() - 100.0).abs() < 1e-6);
        // CSV line for a dropout row: empty frame_id cell, x anchor = cam1 burn, three burn
        // hops filled, AND an empty trailing cam2_cam1 cell (the honest optical-read gap).
        assert_eq!(
            rows[1].to_csv_line(),
            ",1033000000,,10.000000,15.000000,25.000000,",
            "#216: empty frame_id cell, three burn-hop cells filled, empty cam2_cam1 cell (gap)"
        );
        // The healthy frame fills cam2_cam1 (read OK) — proving the column gaps ONLY on dropout.
        assert_eq!(
            rows[4].to_csv_line(),
            "104,1132000000,,10.000000,15.000000,25.000000,100.000000",
            "#216: healthy frame fills cam2_cam1 — the gap is honest, not blanket-empty"
        );
    }

    // Build a CSV row carrying ONLY what optical_read_dropouts reads: the x-anchor gen_ts and
    // whether cam2_cam1 was readable this frame.
    fn drow(gen_ms: i64, cam2_cam1: Option<f64>) -> LatencyCsvRow {
        LatencyCsvRow {
            frame_id: None,
            gen_ts_ns: gen_ms * 1_000_000,
            flip_ts_ns: None,
            cam1_strih_ms: Some(10.0),
            strih_stream_ms: Some(15.0),
            cam1_stream_ms: Some(25.0),
            cam2_cam1_ms: cam2_cam1,
        }
    }

    #[test]
    fn optical_read_dropout_is_reported_as_a_labeled_window_216() {
        // #216 — a stretch where cam2_cam1 is None (the cam1 camera could not read the cam2 QR)
        // LONGER than the floor is reported as ONE optical-read dropout window with its
        // start/end/duration — a labeled finding, NOT hidden, NOT a chain loss.
        // 10 good frames (0..9 s), 6 s of dropout (10..16 s — None), then recovery.
        let mut rows = Vec::new();
        for s in 0..=9 {
            rows.push(drow(s * 1000, Some(120.0))); // readable
        }
        for s in 10..=16 {
            rows.push(drow(s * 1000, None)); // optical read FAILED
        }
        for s in 17..=20 {
            rows.push(drow(s * 1000, Some(121.0))); // recovered
        }
        let d = optical_read_dropouts(&rows, 2.0);
        assert_eq!(d.len(), 1, "exactly one dropout window: {d:?}");
        let g = d[0];
        // The window covers the unreadable stretch (~10..16 s) — well above the 2 s floor. The
        // exact boundary frame is an implementation detail; the finding's intent is: ONE window,
        // located in the gap, several seconds long, counting the unreadable frames.
        assert!(
            (8.0..=11.0).contains(&g.start_s),
            "starts at the onset of the gap (last good frame ~9 s): {g:?}"
        );
        assert!(
            (14.0..=17.0).contains(&g.end_s),
            "ends at the recovery (~16 s): {g:?}"
        );
        assert!(g.dur_s >= 6.0, "dropout lasted several seconds: {g:?}");
        assert!(g.frames >= 6, "counts the unreadable frames: {g:?}");
        assert!(
            (g.dur_s - (g.end_s - g.start_s)).abs() < 1e-6,
            "dur = end - start: {g:?}"
        );
    }

    #[test]
    fn optical_read_short_blink_below_floor_is_not_a_dropout_216() {
        // A 1-frame (sub-floor) cam2-read blink is normal QR-decode jitter, NOT an optical
        // dropout — it must NOT be reported (no false findings).
        let rows = vec![
            drow(0, Some(120.0)),
            drow(33, None), // one-frame blink (~33 ms)
            drow(66, Some(120.0)),
            drow(99, Some(120.0)),
        ];
        let d = optical_read_dropouts(&rows, 2.0);
        assert!(
            d.is_empty(),
            "a sub-floor blink is jitter, not a reported dropout: {d:?}"
        );
    }

    #[test]
    fn optical_read_all_readable_reports_no_dropout_216() {
        // A clean run (every frame read the cam2 QR) has ZERO optical dropouts.
        let rows: Vec<_> = (0..=20).map(|s| drow(s * 1000, Some(120.0))).collect();
        assert!(optical_read_dropouts(&rows, 2.0).is_empty());
    }

    #[test]
    fn per_frame_csv_flip_ts_filled_from_painter_map_when_supplied() {
        // With a painter flip map, each row's flip_ts is the cam2 DISPLAY instant for
        // its tick; a tick absent from the map stays empty (None).
        let frames = vec![
            csv_frame(0, Some((CAM2, 300)), Some(1_000_000_000), None, None),
            csv_frame(1, Some((CAM2, 301)), Some(1_033_000_000), None, None),
        ];
        let mut flip: HashMap<u32, i64> = HashMap::new();
        flip.insert(300, 999_000_000); // tick 300 has a flip stamp
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &flip,
        );
        assert_eq!(
            rows[0].flip_ts_ns,
            Some(999_000_000),
            "tick 300 flip filled"
        );
        assert_eq!(rows[1].flip_ts_ns, None, "tick 301 not in flip map ⇒ empty");
        // #216/#194: with a flip stamp, cam2→cam1 uses the DISPLAY (flip) reference:
        // cam1 capture 1.000s − flip 0.999s = 1.0 ms. Row 1's tick 301 has no flip stamp, so
        // cam2→cam1 falls back to the cam2 PAINT gen (csv_frame default = cam1 − 100 ms = 100 ms).
        assert!((rows[0].cam2_cam1_ms.unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(
            rows[0].to_csv_line(),
            "300,1000000000,999000000,,,,1.000000"
        );
    }

    #[test]
    fn per_frame_csv_pinned_cam2_is_not_hijacked_by_a_foreign_qr() {
        // A frame carries the pinned cam2 QR (tick 500) AND a STRAY non-burn QR with a
        // HIGHER frame_id (9999). With cam2 PINNED, only the cam2 run_id counts, so the
        // tick stays 500 — the stray QR must NOT hijack the canonical tick. (Unpinned,
        // the max-frame_id rule would wrongly pick 9999.)
        let stray_run_id = 4242; // not cam2, not a burn
        let frame = RecordingFrame {
            frame_index: 0,
            payloads: vec![
                Payload {
                    run_id: CAM2,
                    frame_id: 500,
                    gen_ts_ns: 1_000_000_000,
                },
                Payload {
                    run_id: stray_run_id,
                    frame_id: 9999,
                    gen_ts_ns: 1_000_000_000,
                },
                Payload {
                    run_id: BURN_RUN_ID_CAM1,
                    frame_id: 0,
                    gen_ts_ns: 1_000_000_000,
                },
                Payload {
                    run_id: BURN_RUN_ID_STRIH,
                    frame_id: 0,
                    gen_ts_ns: 1_010_000_000,
                },
                Payload {
                    run_id: BURN_RUN_ID_STREAM,
                    frame_id: 0,
                    gen_ts_ns: 1_025_000_000,
                },
            ],
            tick: Some(9999),
        };
        // PINNED to CAM2 → tick is 500, the stray QR is ignored.
        let pinned = per_frame_latency_csv_rows(
            std::slice::from_ref(&frame),
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            Some(CAM2),
            &HashMap::new(),
        );
        assert_eq!(pinned.len(), 1);
        assert_eq!(
            pinned[0].frame_id,
            Some(500),
            "pinned cam2 → tick stays 500, the higher-frame_id stray QR does NOT hijack it"
        );
        // UNPINNED → the stray QR (highest frame_id) wins, demonstrating WHY the pin matters.
        let unpinned = per_frame_latency_csv_rows(
            &[frame],
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            None,
            &HashMap::new(),
        );
        assert_eq!(
            unpinned[0].frame_id,
            Some(9999),
            "unpinned: any non-burn QR counts, so the stray 9999 wins (the bug the pin fixes)"
        );
    }

    #[test]
    fn per_frame_csv_frame_with_cam2_tick_but_no_positive_stamp_is_skipped() {
        // A frame with a cam2 tick whose paint stamp is non-positive (0 sentinel) AND no
        // positive burn ⇒ NO valid x anchor ⇒ the row is SKIPPED (a 0-anchor row would
        // plot far left of t0 and distort the continuous line; it can't pair a hop either).
        let frames = vec![
            RecordingFrame {
                frame_index: 0,
                payloads: vec![Payload {
                    run_id: CAM2,
                    frame_id: 100,
                    gen_ts_ns: 0,
                }],
                tick: Some(100),
            },
            // A valid neighbour to prove the skip is selective, not a blanket empty result.
            csv_frame(
                1,
                Some((CAM2, 102)),
                Some(1_000_000_000),
                Some(1_010_000_000),
                Some(1_020_000_000),
            ),
        ];
        let rows = per_frame_latency_csv_rows(
            &frames,
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_STRIH,
            BURN_RUN_ID_STREAM,
            Some(CAM2),
            &HashMap::new(),
        );
        let ticks: Vec<Option<u32>> = rows.iter().map(|r| r.frame_id).collect();
        assert_eq!(
            ticks,
            vec![Some(102)],
            "the 0-stamp cam2 frame is skipped; the valid one stays"
        );
        assert!(
            rows[0].gen_ts_ns > 0,
            "every emitted row has a positive x anchor"
        );
    }

    #[test]
    fn write_latency_csv_writes_header_plus_one_line_per_row() {
        let rows = vec![
            LatencyCsvRow {
                frame_id: Some(1),
                gen_ts_ns: 1_000_000_000,
                flip_ts_ns: None,
                cam1_strih_ms: Some(10.0),
                strih_stream_ms: Some(15.0),
                cam1_stream_ms: Some(25.0),
                cam2_cam1_ms: Some(120.0), // healthy optical read
            },
            // #216: a row with NO cam2 tick (optical-decode dropout) — empty frame_id cell AND
            // empty cam2_cam1_ms cell (the honest optical-read GAP), yet the burn-only hops are
            // written so those three lines stay unbroken.
            LatencyCsvRow {
                frame_id: None,
                gen_ts_ns: 1_033_000_000,
                flip_ts_ns: None,
                cam1_strih_ms: Some(11.0),
                strih_stream_ms: Some(16.0),
                cam1_stream_ms: Some(27.0),
                cam2_cam1_ms: None, // optical read failed ⇒ empty cell ⇒ visible gap
            },
        ];
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "camera-box-latency-csv-test-{}.csv",
            std::process::id()
        ));
        let n = write_latency_csv(&path, &rows).expect("write csv");
        assert_eq!(n, 2, "two data rows written");
        let body = std::fs::read_to_string(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], LatencyCsvRow::HEADER, "first line is the header");
        assert_eq!(
            lines[1],
            "1,1000000000,,10.000000,15.000000,25.000000,120.000000"
        );
        assert_eq!(
            lines[2], ",1033000000,,11.000000,16.000000,27.000000,",
            "#216: empty frame_id cell AND empty cam2_cam1 cell (optical-read gap), \
             the three burn-hop cells still filled"
        );
        assert_eq!(lines.len(), 3, "header + 2 rows");
    }
}
