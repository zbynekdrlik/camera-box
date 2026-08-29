//! #312 item 2 (PR A) — per-camera A/V-sync window pooling (pure decision).
//!
//! Fuses the #188 A/V-sync measurement (cam2's dual-QR video tick vs its QPSK audio marker,
//! `crate::qpsk_marker::{av_offset_candidates, cluster_offset_ms}`) into the SAME `--switch-schedule`
//! ALL-CAMBOX sweep the #186 continuity check and the #624 per-camera latency check already
//! partition (`bin/recording-verdict.rs`'s `all_cambox_continuity` / `all_cambox_latency`).
//!
//! The per-camera candidate LISTS themselves are produced window-by-window in the probe-gated
//! `bin/recording-verdict` (it needs the decoded `RecordingFrame`s to build each window's
//! `(tick, video_ts)` samples — see [`window_ticks`]). This module holds the PURE, Tier-0
//! testable part: given a camera's per-window candidate lists (already computed), POOL them
//! into one cluster (denser than clustering each window separately — a real offset piles into
//! the SAME narrow band across every window a camera contributes, while false CRC-4 decodes and
//! wrong-lap matches keep scattering) and decide the fail-closed verdict. No probe deps, so it
//! unit-tests Tier-0 (default features) — the project's CLAUDE.md "Local Build Policy" mandate
//! (a pure seam at the crate root, mirroring `switch_latency.rs` / `colour_scale.rs`).
//!
//! **PR B wires the tolerance-bounded cross-window gate on top of [`CameraAvSync`]** (#312 item 2 / #624
//! deliverable 4) — see [`av_offset_gate_pass`]. PR A only reported `all_cambox_av_sync` (offsets,
//! sample counts, any UNKNOWN cameras); this module also decides the per-camera PASS/FAIL that the
//! caller (`bin/recording-verdict.rs`) folds into the run's overall verdict — reports it.
//!
//! **#861 (2026-07-29, user decision on #856): the ±20ms bound was temporarily REPORT-ONLY, now
//! RE-ARMED (2026-08-06).** Epic #800 measured program audio drifting ~160ms/hour against video
//! (foreign Waves/Dante clock domain, sample-count timestamping) — a constant video-delay offset
//! could not hold that inside ±20ms until per-source ASRC landed (#803). ASRC is now live and
//! build-default (#912), and the offline chain converged predictably on 2026-08-06 (issue 999
//! comment 09:05 UTC) — see [`gates_overall_pass`] for the restore-path decision record.
//! [`av_offset_gate_pass`] itself is UNCHANGED throughout (still computed, still the pure
//! fail-closed decision) — only whether the CALLER folds its result into `overall_pass` changed,
//! gated on [`gates_overall_pass`] (mirrors the issue-914/915 seam, applied in reverse).

use crate::qpsk_marker::cluster_offset_ms;

/// Fail-closed floor: fewer than this many pooled candidate offsets (across every window this
/// camera contributed to) — or no dominant cluster of at least this size — and the camera's
/// verdict is [`AvSyncVerdict::Unknown`], never a fabricated number. Starting value; calibrate
/// against this PR's own live-rig data (#312 item 2 PR A live-verification step).
pub const MIN_AV_SAMPLES: usize = 8;

/// One camera's fail-closed A/V-sync verdict for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvSyncVerdict {
    /// Enough pooled candidates formed a dominant cluster — `av_offset_ms`/`mad_ms` are real.
    Measured,
    /// Zero contributing windows, too few pooled candidates, or no dominant cluster — NEVER a
    /// fabricated pass. Mirrors `colour_verify::PatchOutcome::Unsamplable` /
    /// `imag_tick_gate`'s "absent ⇒ nothing to fail on" convention: absence is reported
    /// honestly, not silently upgraded to a number.
    Unknown,
}

/// A camera's pooled-and-clustered A/V-sync measurement for one `--switch-schedule` run.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraAvSync {
    /// How many schedule windows this camera's label matched (0 ⇒ the camera never appeared in
    /// this run's sweep, e.g. a box that was down).
    pub windows: usize,
    /// Total candidate offsets pooled across every matched window, BEFORE clustering (includes
    /// the false-decode scatter — see `qpsk_marker::av_offset_candidates`'s doc comment).
    pub candidates: usize,
    /// Size of the winning (densest) cluster — 0 when [`AvSyncVerdict::Unknown`].
    pub cluster_samples: usize,
    /// `video − audio` offset in ms (>0 ⇒ video lags audio), `None` when [`AvSyncVerdict::Unknown`].
    pub av_offset_ms: Option<f64>,
    /// Median absolute deviation (ms) of the winning cluster, `None` when [`AvSyncVerdict::Unknown`].
    pub mad_ms: Option<f64>,
    pub verdict: AvSyncVerdict,
}

/// THE pure pooling + fail-closed decision: given how many schedule windows matched this camera
/// and each matched window's own candidate-offset list (already produced by
/// `qpsk_marker::av_offset_candidates` on that window's `(tick, video_ts)` samples), pool every
/// candidate into ONE list and cluster it (`qpsk_marker::cluster_offset_ms`).
///
/// `windows_matched == 0` is authoritative UNKNOWN regardless of `per_window_candidates` content
/// (a camera that never appeared in the sweep proves nothing, even if a stray candidate list were
/// passed in by mistake) — the "zero contributing windows" fail-closed case from the PR's own
/// spec. Otherwise the candidates are pooled and `cluster_offset_ms` decides: too few pooled
/// candidates OR no window of width `2 * cluster_tol_ms` containing at least `min_samples` of
/// them ⇒ [`AvSyncVerdict::Unknown`] (covers BOTH "too few pooled samples" and "no dominant
/// cluster" from the same spec, via the SAME pure kernel the whole-recording `--av-sync` mode
/// already uses — never a second decision rule that could disagree).
pub fn pool_camera_av_sync(
    windows_matched: usize,
    per_window_candidates: &[Vec<f64>],
    min_samples: usize,
    cluster_tol_ms: f64,
) -> CameraAvSync {
    let candidates: Vec<f64> = per_window_candidates.iter().flatten().copied().collect();
    let n_candidates = candidates.len();
    if windows_matched == 0 {
        return CameraAvSync {
            windows: 0,
            candidates: n_candidates,
            cluster_samples: 0,
            av_offset_ms: None,
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        };
    }
    match cluster_offset_ms(&candidates, min_samples, cluster_tol_ms) {
        Some(off) => CameraAvSync {
            windows: windows_matched,
            candidates: n_candidates,
            cluster_samples: off.matched,
            av_offset_ms: Some(off.offset_ms),
            mad_ms: Some(off.mad_ms),
            verdict: AvSyncVerdict::Measured,
        },
        None => CameraAvSync {
            windows: windows_matched,
            candidates: n_candidates,
            cluster_samples: 0,
            av_offset_ms: None,
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        },
    }
}

/// #624 deliverable 4 / #312 item 2 PR B — the per-camera A/V-offset gate tolerance: every
/// camera's measured `video − audio` offset must land within this many ms of the
/// expected/dialed value (`--av-expected-ms`, the operator's live #398 dock reading — nominally
/// ~0 since the dock is dialed to align video and audio). Issue #624's own deliverable-4 text
/// asked for ±20ms.
///
/// **#861 interim (2026-08-06): 90.0 = 20 + 2 frames @30fps (2 × 33.33, rounded up together).**
/// The deep-latency FIFO relock lands its release phase ±1-2 frames differently per lock
/// episode (issue #1003: four same-day measurements at three knob values stepped −82/−42/+56ms
/// around zero with intra-episode mad only 10-13ms; stream log shows relocks=13 in 3.8h), so a
/// ±20ms bound was a ~30% per-episode lottery that would randomly block unrelated PRs. ±90
/// still catches every gross regression class (pre-ASRC 160ms/h drift, a mis-set knob, a dead
/// marker chain, the −57ms dock-bias class of #999). Re-tightening 90 → 20 is issue #1003's
/// acceptance item 2, once the release phase is pinned to the absolute wall-clock frame grid —
/// a tracked interim, never a silent weakening.
pub const AV_OFFSET_GATE_TOLERANCE_MS: f64 = 90.0;

/// #1178 — the fixed video-leg rig offset (ms): the calibrated DEFAULT `expected_ms` the per-camera
/// A/V gate centres on, so the uniform rig constant no longer eats the whole ±90ms budget.
///
/// The measurement chain carries a fixed VIDEO-leg latency the audio path (QPSK marker → Dante →
/// mbc) does not: cam2 monitor input lag, the BMPCC sensor→HDMI delay, and the USB capture grabber.
/// The dock being "dialed to 0" nulls the SOURCE A/V, not this measurement-chain leg, so a correctly
/// aligned rig still MEASURES `av_offset_ms ≈ this constant`, not 0. Gating the raw measured offset
/// against 0 therefore fails every camera whose leg lands past ±90 (the #1178 body's exact claim).
///
/// This is a NAMED, surfaced calibration (`rig_video_leg_offset_ms` in the verdict JSON + the gate
/// log line), never a silent shift. It is the DEFAULT of `--av-expected-ms`; a mode that PHYSICALLY
/// compensates the leg (MEASUREMENT_EQ / issue 1003, whose stream-hold rebalance lands the measured
/// offset at ~0) passes its own explicit `--av-expected-ms 0`, which cleanly REPLACES this default —
/// so the leg is subtracted by default yet never DOUBLE-counted where it is already physically gone.
///
/// ## Calibration — re-derive when the physical video chain changes (grabber / monitor / camera
/// swap, e.g. the issue-1198 capture-card swap):
/// take the MEDIAN of the judged per-camera `av_offset_ms` from a full-fleet E2E gate run
/// (`--av-expected-ms 0`, no physical compensation). Median (not mean) for robustness to a single
/// per-camera outlier; sub-ms precision is noise (per-camera MAD 5–8ms, cross-camera spread ~18ms).
///
/// Current value −92.0: verdict 845554984 (run 33176192564, 2026-08-29) judged offsets cam1
/// −95.17, cam2 −91.98, cam3 −76.75, cam6 −93.92, cam7 −88.63 → median −91.98 → −92.0 (mean −89.3
/// is within noise; cam3 −76.75 is the mild outlier the median rejects). After subtraction the
/// residual median is +0.0ms, spread 18.4ms — every camera well inside ±90.
pub const RIG_VIDEO_LEG_OFFSET_MS: f64 = -92.0;

/// PASS iff `sync` is [`AvSyncVerdict::Measured`] AND its offset is within
/// [`AV_OFFSET_GATE_TOLERANCE_MS`] of `expected_ms`. [`AvSyncVerdict::Unknown`] — whether from
/// zero contributing windows (the camera never appeared in this sweep) or thin/scattered data
/// (too few pooled candidates, no dominant cluster) — NEVER passes: this is a real gate now
/// (#312 item 2 PR B), same fail-closed severity as the loss/latency-spread gates, no "advisory"
/// tier. A camera with nothing to measure proves nothing about its A/V sync — it cannot pass.
///
/// Checks `sync.verdict == Measured` EXPLICITLY, not merely `av_offset_ms.is_some()` — the two
/// currently always agree ([`pool_camera_av_sync`] is the sole real producer and keeps them in
/// lockstep), but this function does not rely on that convention holding forever: a future
/// producer that ever desyncs the pair (e.g. `Unknown` with a stray `Some(x)`, or `Measured` with
/// `None`) must still resolve to fail-closed here, not silently pass on the strength of one field
/// alone.
pub fn av_offset_gate_pass(sync: &CameraAvSync, expected_ms: f64) -> bool {
    match (sync.verdict, sync.av_offset_ms) {
        (AvSyncVerdict::Measured, Some(off)) => {
            (off - expected_ms).abs() <= AV_OFFSET_GATE_TOLERANCE_MS
        }
        _ => false,
    }
}

/// #861 (2026-08-06, user decision -- mirrors the issue-914/915 `gates_overall_pass()` seam
/// exactly, applied in the RE-BLOCKING direction): whether [`av_offset_gate_pass`]'s per-camera
/// PASS/FAIL result folds into the fused run's `overall_pass`. [`av_offset_gate_pass`] itself is
/// UNCHANGED — still fully computed, still fail-closed on thin/absent data, still reported in
/// `all_cambox_av_sync` exactly as before; only the CALLER (`bin/recording-verdict.rs`) decides
/// whether the aggregate result (`av_all_pass`) folds into `all_pass`, gated on this function.
///
/// Report-only (`false`) since 2026-07-29 (issue 861, user decision on #856): program audio
/// drifted ~160ms/hour against video in a foreign Waves/Dante clock domain (epic #800) — a
/// constant video-delay offset could not hold ±20ms until per-source ASRC landed (#803). That
/// precondition is now met: ASRC is live and build-default (#912), and the offline
/// `recording-verdict --av-sync` chain converged predictably in one measured step on 2026-08-06
/// (51.6ms -> 963 -> 913 -> 894 knob -> final av_offset_ms=-0.06ms, mad 11.8, matched 31 — issue
/// 999 comment 2026-08-06 09:05 UTC). Restore path if this ever needs softening again: flip back
/// to `false` with a fresh user decision citing live evidence the drift has returned — never a
/// silent revert (see `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861`,
/// the regression test that fails if this function's return value silently reverts).
pub fn gates_overall_pass() -> bool {
    true
}

/// #714/#689 — the ONE per-camera A/V offset a consumer (the #711 report, the merge path, a human
/// reading the raw verdict) should read, so EVERY camera under test carries a computable number,
/// not a `null` that has to be cross-read against a second field. It is:
///
/// * the genuine MEASURED `av_offset_ms` when the camera reached [`AvSyncVerdict::Measured`]
///   (cam2, whose whole-recording pool always clears the cluster floor), else
/// * the DERIVED `derived_offset_ms` when the camera was sample-starved but cam2's anchor + this
///   camera's #286 delivery delta produced a sound estimate ([`derive_camera_av_sync`]), else
/// * `None` — genuinely nothing to report (cam2 itself Unknown, or no delivery sample to
///   re-center on): a true unknown, never fabricated.
///
/// The camera's `verdict` label (`measured` / `derived` / `unknown`) still distinguishes WHICH of
/// these produced the value, so surfacing them through one field is a computability convenience,
/// never a conflation — a derived number is still plainly marked derived. This is deliberately
/// the SAME priority the #711 report already applies (measured field first, then the derived
/// field), lifted into the pure layer so the raw verdict JSON is honest by construction and the
/// gate/report never disagree on "what is this camera's A/V offset".
pub fn effective_offset_ms(sync: &CameraAvSync, derived: Option<&DerivedAvSync>) -> Option<f64> {
    match sync.verdict {
        AvSyncVerdict::Measured => sync.av_offset_ms,
        AvSyncVerdict::Unknown => derived.map(|d| d.derived_offset_ms),
    }
}

/// #1178 report-only — a camera's RESIDUAL A/V offset: its measured/effective offset with the
/// expected (calibrated video-leg) removed. ~0 for a correctly aligned camera once the fixed rig
/// leg is subtracted — the honest per-camera number a dock/report reads. Pure signed convention:
/// `measured − expected` (the SAME signed delta [`av_offset_gate_pass`] takes the magnitude of).
pub fn residual_offset_ms(measured_offset_ms: f64, expected_ms: f64) -> f64 {
    measured_offset_ms - expected_ms
}

/// #1178 report-only cross-camera residual summary — the diagnostic channel that surfaces whatever
/// cross-run / cross-camera instability REMAINS after the fixed video-leg is removed (the issue
/// 952 / issue 1004 residual finding) WITHOUT ever masking a global drift: the BLOCKING gate uses
/// the FIXED [`RIG_VIDEO_LEG_OFFSET_MS`], never this per-run median, so a whole-fleet shift still
/// moves every residual and is caught at the gate boundary. `None` when no camera produced a
/// residual (nothing to summarize) — never a fabricated 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidualSummary {
    pub median_ms: Option<f64>,
    pub spread_ms: Option<f64>,
    pub count: usize,
}

/// PURE (Tier-0): median + full spread (max − min) of the per-camera residuals. Report-only —
/// never feeds [`av_offset_gate_pass`].
pub fn residual_summary(residuals_ms: &[f64]) -> ResidualSummary {
    if residuals_ms.is_empty() {
        return ResidualSummary {
            median_ms: None,
            spread_ms: None,
            count: 0,
        };
    }
    let mut v: Vec<f64> = residuals_ms.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    let spread = v[n - 1] - v[0];
    ResidualSummary {
        median_ms: Some(median),
        spread_ms: Some(spread),
        count: n,
    }
}

/// Build `(tick, video_ts_s)` samples for `qpsk_marker::av_offset_candidates`'s third argument,
/// from a decoded window's `(frame_index, tick)` pairs — pure (no `RecordingFrame` dependency,
/// so it stays Tier-0 testable) mirroring the identical construction
/// `probe::av_sync_recording::av_sync_from_recording` already does for the whole-recording
/// `--av-sync` mode (LEFT UNTOUCHED): dedup to the FIRST occurrence of each tick, in ascending
/// tick order (`qpsk_marker::interp_video_ts`'s precondition — it binary-searches this list and
/// never extrapolates past its range, which is exactly what keeps a per-window restriction of
/// this list from ever mis-pairing a marker to a tick outside that window/lap).
///
/// `frames` need not be pre-sorted by `frame_index` — the tick VALUES are what get sorted at the
/// end, not the input order.
pub fn window_ticks(
    frames: &[(u64, Option<u32>)],
    fps: f64,
    video_start_s: f64,
) -> Vec<(u32, f64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(u32, f64)> = Vec::new();
    for &(frame_index, tick) in frames {
        if let Some(t) = tick {
            if seen.insert(t) {
                out.push((t, video_start_s + frame_index as f64 / fps));
            }
        }
    }
    out.sort_by_key(|&(t, _)| t);
    out
}

/// #714 — a per-camera A/V-sync estimate DERIVED (never fabricated) for a camera whose own
/// per-window pooling is sample-starved ([`AvSyncVerdict::Unknown`]), from cam2's own MEASURED
/// whole-recording offset plus the delta between this camera's OWN `#286` delivery-latency p50
/// (`all_cambox_delivery_latency`, `strih_burn − camera_burn`) and the mean p50 across every
/// camera that produced a delivery sample this run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerivedAvSync {
    pub derived_offset_ms: f64,
    /// The #286 delivery-latency cross-camera spread this run — the honest DIAGNOSTIC margin on
    /// the derivation (report-only field; see [`derive_camera_av_sync`]'s doc comment for why the
    /// gate itself still applies the SAME [`AV_OFFSET_GATE_TOLERANCE_MS`] tolerance as a real measurement).
    pub delivery_spread_ms: f64,
    pub gate_pass: bool,
}

/// THE pure derivation (#714).
///
/// ## Why this is sound, not fabricated — grounded in the #689 live-data finding
///
/// Every cam box receives the IDENTICAL optically-injected picture (cam2's dual-QR) via the
/// shared HDMI splitter (this project's own documented rig topology — one broadcast camera films
/// cam2's monitor, its HDMI output is physically split into every cam box's own USB capture
/// card). Live evidence (#689, 3 gate runs, 2026-07-12): the 5 non-cam2 cameras' per-window
/// candidate counts ARE non-zero and roughly proportional to their own schedule-window duration
/// (103-988 candidates vs cam2's 2013-3256 over the whole ~10x-longer recording) — the video
/// ticks genuinely decode fine inside every camera's own window. The reason none of the 5 ever
/// clears [`MIN_AV_SAMPLES`] is NOT missing signal — a single ~30-60s schedule window is simply
/// too short to accumulate enough REAL (non-false-CRC-decode) marker matches, given the QPSK
/// marker's finite occurrence rate: cam2's own whole-300s pool clears the floor at only 22-31
/// matched samples (roughly one real match every ~10-13s) — well under 8 in a single short
/// window, structurally, regardless of raw candidate count.
///
/// cam2's own whole-recording offset therefore implicitly BLENDS together whichever camera's
/// receiver-side delivery latency (`strih_burn − camera_burn`, #286's OWN Verify metric) was
/// active at each sampled instant across the whole run — cam2 has no camera-under-test delivery
/// latency of its own, so its number is an average over all 6 cameras' delivery times as they
/// rotated through program. Re-centering that blended number on THIS camera's own delivery p50,
/// relative to the mean the blend implicitly averages over, is the direct algebraic correction
/// for the one variable that genuinely differs per camera (#286's own per-source genlock hold) —
/// never a guess, and never a re-derivation of the A/V relationship itself (still cam2's own
/// measured paint-tick-vs-marker offset, only re-centered).
///
/// ## Fail-closed — `None` (never a fabricated number) when
///
/// - cam2 itself did not reach [`AvSyncVerdict::Measured`] this run (nothing to re-center), or
/// - this camera produced no #286 delivery-latency sample this run (no p50 to re-center on), or
/// - fewer than 2 cameras produced a delivery sample this run (no meaningful mean to re-center
///   against — a single-sample "mean" would just reproduce cam2's own number with zero
///   correction, silently hiding that nothing was actually derived).
pub fn derive_camera_av_sync(
    cam2_offset_ms: Option<f64>,
    camera_delivery_p50_ms: Option<f64>,
    all_delivery_p50s_ms: &[f64],
    expected_ms: f64,
) -> Option<DerivedAvSync> {
    let cam2_offset = cam2_offset_ms?;
    let cam_p50 = camera_delivery_p50_ms?;
    if all_delivery_p50s_ms.len() < 2 {
        return None;
    }
    let mean_p50 = all_delivery_p50s_ms.iter().sum::<f64>() / all_delivery_p50s_ms.len() as f64;
    let derived_offset_ms = cam2_offset + (cam_p50 - mean_p50);
    let max_p50 = all_delivery_p50s_ms
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let min_p50 = all_delivery_p50s_ms
        .iter()
        .cloned()
        .fold(f64::MAX, f64::min);
    let delivery_spread_ms = max_p50 - min_p50;
    // #714 spec: "the spread folded into the tolerance check (±20ms gate then applies to the
    // DERIVED value)" — the delivery delta is already folded INTO derived_offset_ms itself (the
    // re-centering above); the SAME tolerance the measured path uses then applies to that
    // now-per-camera-specific value. delivery_spread_ms is kept as a separate, honest DIAGNOSTIC
    // field (never silently widening or narrowing the tolerance) so a report reader can judge the
    // derivation's own confidence margin independently of the pass/fail call.
    let gate_pass = (derived_offset_ms - expected_ms).abs() <= AV_OFFSET_GATE_TOLERANCE_MS;
    Some(DerivedAvSync {
        derived_offset_ms,
        delivery_spread_ms,
        gate_pass,
    })
}

/// #748 — the discriminator for a fused A/V-sync run in which EVERY judged camera produced zero
/// candidate offsets. `candidates == 0` alone conflates two very different causes, and the
/// operator alert must not blame the wrong one: (a) a genuinely SILENT measurement chain (mbc
/// Ableton mic muted / Dante misroute) — the #748 incident, where a full cycle burned reported
/// only as a quiet `candidates: 0`; versus (b) audio PRESENT but the QPSK marker never clustered
/// (a broken emit/painter side, or a marker-decode regression) — where the mbc mute is NOT the
/// cause.
/// The QPSK demod already separates them: [`crate::qpsk_marker::DecodeStats::preamble_screens_passed`]
/// counts sample onsets whose preamble screen crossed threshold — zero means the demod never saw
/// anything resembling the marker (no/near-silent signal), so a positive count on an all-silent
/// run means the audio WAS present, the marker just did not decode. Measured from the ACTUAL
/// recorded audio, so it catches a chain that went silent mid-record too, not only pre-record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvAudioState {
    /// At least one judged camera produced candidates (a real measurement), or no camera was
    /// judged at all — the silent-vs-undecoded discriminator does not apply.
    Measured,
    /// Every judged camera had zero candidates AND the demod saw zero preamble onsets: the
    /// measurement-audio chain is SILENT (mbc mute / Dante misroute).
    Silent,
    /// Every judged camera had zero candidates BUT the demod DID see preamble onsets: the audio is
    /// present, the marker just never clustered (emit-side / QPSK decode problem, NOT a mute).
    PresentUndecoded,
}

impl AvAudioState {
    /// The machine-readable `av_audio_silent` flag emitted into the `all_cambox_av_sync` verdict
    /// block: `Some(true)` for a silent chain, `Some(false)` for present-but-undecoded audio, and
    /// `None` (JSON `null`) when the discriminator does not apply (a real measurement, or nothing
    /// judged) — a consumer that reads `null`/absent keeps the safe, loud default (blame the mbc
    /// mute) rather than suppressing the alert.
    pub fn av_audio_silent_flag(self) -> Option<bool> {
        match self {
            AvAudioState::Silent => Some(true),
            AvAudioState::PresentUndecoded => Some(false),
            AvAudioState::Measured => None,
        }
    }
}

/// #748 pure discriminator (Tier-0). `judged_cameras` is how many cameras were actually judged
/// (not operator-ack-excluded); `all_judged_candidates_zero` is whether EVERY one of those had
/// `candidates == 0`; `preamble_screens_passed` is the whole-recording QPSK preamble-onset count.
/// Fails CLOSED toward "measured/N/A" (never a false silent claim) when nothing was judged, and
/// toward SILENT only when a real all-zero run also saw zero preamble energy.
pub fn classify_av_audio_state(
    judged_cameras: usize,
    all_judged_candidates_zero: bool,
    preamble_screens_passed: u64,
) -> AvAudioState {
    if judged_cameras == 0 || !all_judged_candidates_zero {
        // Nothing judged (vacuous all-zero), or a real measurement exists -> N/A. Never accuse
        // the mbc chain of being silent on a run that actually measured something.
        return AvAudioState::Measured;
    }
    if preamble_screens_passed == 0 {
        AvAudioState::Silent
    } else {
        AvAudioState::PresentUndecoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // classify_av_audio_state — #748 silent-vs-undecoded discriminator
    // ---------------------------------------------------------------------
    #[test]
    fn av_audio_all_silent_with_zero_preamble_energy_is_silent_chain_748() {
        // Every judged camera candidates==0 AND the demod saw NO preamble onsets -> the mbc
        // measurement chain is genuinely silent (the #748 incident shape).
        assert_eq!(classify_av_audio_state(6, true, 0), AvAudioState::Silent);
        assert_eq!(
            classify_av_audio_state(6, true, 0).av_audio_silent_flag(),
            Some(true)
        );
    }

    #[test]
    fn av_audio_all_silent_but_preamble_energy_present_is_undecoded_not_a_mute_748() {
        // candidates==0 everywhere BUT the demod DID screen preamble onsets -> the audio was
        // present, the marker never decoded (emit/painter or decode problem), NOT an mbc mute.
        assert_eq!(
            classify_av_audio_state(6, true, 42),
            AvAudioState::PresentUndecoded
        );
        assert_eq!(
            classify_av_audio_state(6, true, 42).av_audio_silent_flag(),
            Some(false)
        );
    }

    #[test]
    fn av_audio_with_any_candidates_is_measured_discriminator_does_not_apply_748() {
        // A real measurement (at least one camera had candidates) -> the discriminator is N/A,
        // regardless of the preamble count.
        assert_eq!(classify_av_audio_state(6, false, 0), AvAudioState::Measured);
        assert_eq!(
            classify_av_audio_state(6, false, 99),
            AvAudioState::Measured
        );
        assert_eq!(
            classify_av_audio_state(6, false, 0).av_audio_silent_flag(),
            None
        );
    }

    #[test]
    fn av_audio_zero_judged_cameras_never_claims_silent_748() {
        // Fail closed: if no camera was judged (e.g. every box operator-ack-excluded), the
        // all-zero condition is vacuous -> never accuse the mbc chain of being silent.
        assert_eq!(classify_av_audio_state(0, true, 0), AvAudioState::Measured);
        assert_eq!(
            classify_av_audio_state(0, true, 0).av_audio_silent_flag(),
            None
        );
    }

    // ---------------------------------------------------------------------
    // gates_overall_pass (issue 861 re-arm)
    // ---------------------------------------------------------------------

    #[test]
    fn gates_overall_pass_is_blocking_again_861() {
        // ASRC (#803) is live and the offline chain converged (2026-08-06 measurement) -- the
        // 2026-07-29 report-only relaxation is reversed. A silent revert to `false` here (without
        // a fresh user decision) must fail this test loudly.
        assert!(
            gates_overall_pass(),
            "#861: the A/V-offset term must gate overall_pass again now that ASRC is live and \
             proven -- if this reverted to false, it must be a deliberate, cited user decision, \
             never a silent regression"
        );
    }

    // ---------------------------------------------------------------------
    // pool_camera_av_sync
    // ---------------------------------------------------------------------

    #[test]
    fn zero_windows_matched_is_unknown_even_with_stray_candidates() {
        // The "zero contributing windows" fail-closed case: even if a candidate list were handed
        // in (should never happen in real wiring — the caller only pushes a window's candidates
        // when it actually matched), windows_matched=0 is authoritative.
        let v = pool_camera_av_sync(0, &[vec![10.0, 11.0, 9.0, 10.5]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.windows, 0);
        assert_eq!(v.av_offset_ms, None);
        assert_eq!(v.mad_ms, None);
        assert_eq!(v.cluster_samples, 0);
    }

    #[test]
    fn too_few_pooled_candidates_is_unknown() {
        // 3 candidates pooled, need MIN_AV_SAMPLES(8) — Unknown, not a fabricated number from 3.
        let v = pool_camera_av_sync(2, &[vec![10.0, 11.0], vec![9.5]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.candidates, 3);
        assert_eq!(v.windows, 2);
        assert_eq!(v.av_offset_ms, None);
    }

    #[test]
    fn dense_cluster_pooled_across_windows_is_measured() {
        // Two windows each contributing a few real candidates (tight cluster around ~800ms) plus
        // scattered false decodes — pooling across windows is what gets to MIN_AV_SAMPLES(8) real
        // candidates; neither window alone would clear the floor.
        let win_a = vec![798.0, 801.0, 799.5, 800.5, 5000.0]; // 4 real + 1 scattered false decode
        let win_b = vec![800.0, 802.0, 797.0, 799.0, -3000.0]; // 4 real + 1 scattered false decode
        let v = pool_camera_av_sync(2, &[win_a, win_b], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Measured);
        assert_eq!(v.windows, 2);
        assert_eq!(v.candidates, 10);
        assert_eq!(
            v.cluster_samples, 8,
            "the 8 real candidates form the dense cluster"
        );
        let off = v.av_offset_ms.expect("measured");
        assert!(
            (799.0..=801.0).contains(&off),
            "offset should land in the real cluster's range: {off}"
        );
    }

    #[test]
    fn scattered_candidates_with_no_dominant_cluster_is_unknown() {
        // 8 candidates (clears the count floor) but spread out with no window containing 8 of
        // them within ±60ms of each other — no dominant cluster ⇒ Unknown, never a wrong guess.
        let scattered = vec![0.0, 200.0, 400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0];
        let v = pool_camera_av_sync(1, &[scattered], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.candidates, 8);
    }

    #[test]
    fn min_av_samples_constant_is_the_documented_starting_value() {
        // Locks the constant so a casual future edit is visible in the diff/review, not silent.
        assert_eq!(MIN_AV_SAMPLES, 8);
    }

    // ---------------------------------------------------------------------
    // window_ticks
    // ---------------------------------------------------------------------

    #[test]
    fn window_ticks_dedups_to_first_occurrence_sorted_ascending() {
        // frame_index 5 and 9 both decode tick 100 — only the FIRST (by input order) survives,
        // matching interp_video_ts's precondition of one video_ts per tick. Input order here is
        // deliberately NOT frame-index-sorted, to prove the function sorts the OUTPUT by tick,
        // not by input position.
        let frames = vec![(9, Some(100)), (5, Some(100)), (7, Some(101)), (3, None)];
        let ticks = window_ticks(&frames, 30.0, 0.0);
        assert_eq!(
            ticks,
            vec![(100, 9.0 / 30.0), (101, 7.0 / 30.0)],
            "kept the FIRST occurrence per tick (input order), sorted ascending by tick value"
        );
    }

    #[test]
    fn window_ticks_excludes_frames_with_no_decoded_tick() {
        let frames = vec![(0, None), (1, None), (2, Some(50))];
        let ticks = window_ticks(&frames, 60.0, 0.0);
        assert_eq!(ticks, vec![(50, 2.0 / 60.0)]);
    }

    #[test]
    fn window_ticks_applies_the_video_start_offset() {
        let frames = vec![(0, Some(1))];
        let ticks = window_ticks(&frames, 30.0, 12.5);
        assert_eq!(
            ticks,
            vec![(1, 12.5)],
            "frame_index 0 lands exactly at video_start_s"
        );
    }

    #[test]
    fn window_ticks_on_empty_input_is_empty() {
        assert_eq!(window_ticks(&[], 60.0, 0.0), Vec::<(u32, f64)>::new());
    }

    // ---------------------------------------------------------------------
    // av_offset_gate_pass — #624 deliverable 4 / #312 item 2 PR B
    // ---------------------------------------------------------------------

    fn measured(av_offset_ms: f64) -> CameraAvSync {
        CameraAvSync {
            windows: 2,
            candidates: 10,
            cluster_samples: 10,
            av_offset_ms: Some(av_offset_ms),
            mad_ms: Some(1.0),
            verdict: AvSyncVerdict::Measured,
        }
    }

    #[test]
    fn gate_passes_when_offset_within_tolerance_of_expected() {
        // 15ms deviation from 0 is well inside the ±20ms bound.
        assert!(av_offset_gate_pass(&measured(15.0), 0.0));
        // Negative deviation, same bound.
        assert!(av_offset_gate_pass(&measured(-15.0), 0.0));
    }

    #[test]
    fn gate_fails_when_offset_outside_tolerance_of_expected() {
        assert!(!av_offset_gate_pass(
            &measured(AV_OFFSET_GATE_TOLERANCE_MS + 5.0),
            0.0
        ));
        assert!(!av_offset_gate_pass(
            &measured(-(AV_OFFSET_GATE_TOLERANCE_MS + 5.0)),
            0.0
        ));
    }

    #[test]
    fn gate_measures_deviation_from_a_nonzero_expected_value_not_hardcoded_zero() {
        // The operator's live #398 dock may be dialed to a nonzero value — the gate must measure
        // deviation FROM THAT expected value, never from a hardcoded 0: the SAME measured value
        // passes against a nearby expected and fails against 0.
        let m = 50.0 + AV_OFFSET_GATE_TOLERANCE_MS + 5.0;
        assert!(av_offset_gate_pass(&measured(m), m - 5.0));
        assert!(!av_offset_gate_pass(&measured(m), 0.0));
    }

    /// #861 interim (2026-08-06): the tolerance is pinned at 90ms = the original ±20ms bound +
    /// 2 frames @30fps (2 × 33.33 = 66.7, rounded up together to 90) — the deep-FIFO relock
    /// lands its release phase ±1-2 frames differently per lock episode (live 4-run evidence on
    /// issue #1003; the #940 phase-pin fix reduced but did not eliminate it), so a ±20ms bound
    /// was a per-episode lottery, not a gate. Re-tightening 90 → 20 is issue #1003's acceptance
    /// item 2 — when that lands, THIS test is the one-line flip back.
    #[test]
    fn tolerance_is_the_interim_90ms_episode_quantization_bound_861() {
        assert_eq!(
            AV_OFFSET_GATE_TOLERANCE_MS, 90.0,
            "interim bound = 20 + 2 frames @30fps episode quantization (issue #1003 re-tightens)"
        );
    }

    #[test]
    fn gate_boundary_at_exactly_plus_tolerance_passes() {
        assert!(
            av_offset_gate_pass(&measured(AV_OFFSET_GATE_TOLERANCE_MS), 0.0),
            "the bound is <=, not <"
        );
    }

    #[test]
    fn gate_boundary_at_exactly_minus_tolerance_passes() {
        assert!(av_offset_gate_pass(
            &measured(-AV_OFFSET_GATE_TOLERANCE_MS),
            0.0
        ));
    }

    #[test]
    fn gate_just_outside_the_boundary_fails() {
        assert!(!av_offset_gate_pass(
            &measured(AV_OFFSET_GATE_TOLERANCE_MS + 0.01),
            0.0
        ));
    }

    #[test]
    fn gate_fails_closed_on_unknown_verdict_from_thin_data() {
        // The camera DID appear in the sweep (windows_matched=1) but too few pooled candidates to
        // clear MIN_AV_SAMPLES — Unknown. Must fail, never a fabricated pass.
        let thin = pool_camera_av_sync(1, &[vec![10.0, 11.0]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(thin.verdict, AvSyncVerdict::Unknown);
        assert!(!av_offset_gate_pass(&thin, 0.0));
    }

    #[test]
    fn gate_fails_closed_when_camera_never_appeared_in_the_sweep() {
        let absent = pool_camera_av_sync(0, &[], MIN_AV_SAMPLES, 60.0);
        assert_eq!(absent.verdict, AvSyncVerdict::Unknown);
        assert!(!av_offset_gate_pass(&absent, 0.0));
    }

    /// Code-review finding: `av_offset_gate_pass` must check `verdict == Measured` EXPLICITLY,
    /// not merely `av_offset_ms.is_some()` — the two fields currently always agree in real
    /// producers, but this locks the gate against a hand-constructed (or future-buggy) mismatch:
    /// an `Unknown` verdict carrying a stray `Some(offset)` must still fail closed.
    #[test]
    fn gate_fails_closed_on_unknown_verdict_even_with_a_stray_offset_value() {
        let mismatched = CameraAvSync {
            windows: 1,
            candidates: 3,
            cluster_samples: 0,
            av_offset_ms: Some(10.0), // stray value inconsistent with Unknown
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        };
        assert!(
            !av_offset_gate_pass(&mismatched, 0.0),
            "an Unknown verdict must never pass the gate, even if av_offset_ms happens to be Some"
        );
    }

    // ---------------------------------------------------------------------
    // derive_camera_av_sync (#714)
    // ---------------------------------------------------------------------

    #[test]
    fn derive_none_when_cam2_itself_is_not_measured() {
        let v = derive_camera_av_sync(None, Some(980.0), &[960.0, 970.0, 980.0], 0.0);
        assert_eq!(
            v, None,
            "no cam2 offset to re-center ⇒ never fabricate a number"
        );
    }

    #[test]
    fn derive_none_when_this_camera_has_no_delivery_sample() {
        let v = derive_camera_av_sync(Some(-12.0), None, &[960.0, 970.0, 980.0], 0.0);
        assert_eq!(
            v, None,
            "no delivery p50 for THIS camera ⇒ nothing to re-center against"
        );
    }

    #[test]
    fn derive_none_when_fewer_than_two_cameras_have_delivery_samples() {
        // Only 1 camera's delivery sample exists this run — a "mean" of one value would just
        // reproduce cam2's own number with zero correction, silently hiding that nothing was
        // actually derived.
        let v = derive_camera_av_sync(Some(-12.0), Some(980.0), &[980.0], 0.0);
        assert_eq!(v, None);
        let v_empty = derive_camera_av_sync(Some(-12.0), Some(980.0), &[], 0.0);
        assert_eq!(v_empty, None);
    }

    #[test]
    fn derive_recenters_cam2_offset_on_this_cameras_own_delivery_delta() {
        // mean p50 across the 6 cameras = 970ms; this camera's own p50 = 980ms (10ms ABOVE
        // the mean, i.e. this camera's frames sat in strih's queue 10ms longer than average) ⇒
        // derived = cam2_offset + (980 - 970) = cam2_offset + 10.
        let p50s = [960.0, 965.0, 970.0, 975.0, 980.0, 970.0]; // mean = 970.0
        let v = derive_camera_av_sync(Some(-12.0), Some(980.0), &p50s, 0.0)
            .expect("all inputs present ⇒ a derived estimate");
        assert!(
            (v.derived_offset_ms - (-2.0)).abs() < 1e-9,
            "expected -12.0 + (980.0 - 970.0) = -2.0, got {}",
            v.derived_offset_ms
        );
        assert!(
            (v.delivery_spread_ms - 20.0).abs() < 1e-9,
            "spread = max(980) - min(960) = 20.0, got {}",
            v.delivery_spread_ms
        );
    }

    #[test]
    fn derive_gate_pass_boundary_matches_the_measured_paths_own_tolerance() {
        let p50s = [970.0, 970.0]; // mean = 970.0, zero delta for THIS camera below
                                   // derived offset lands EXACTLY on the tolerance boundary ⇒ still PASS (<=, matching
                                   // av_offset_gate_pass's own inclusive boundary).
        let at_boundary =
            derive_camera_av_sync(Some(AV_OFFSET_GATE_TOLERANCE_MS), Some(970.0), &p50s, 0.0)
                .expect("inputs present");
        assert!(
            (at_boundary.derived_offset_ms - AV_OFFSET_GATE_TOLERANCE_MS).abs() < 1e-9,
            "zero delivery delta ⇒ derived == cam2's own offset"
        );
        assert!(
            at_boundary.gate_pass,
            "exactly at tolerance must still PASS"
        );

        let just_over = derive_camera_av_sync(
            Some(AV_OFFSET_GATE_TOLERANCE_MS + 0.1),
            Some(970.0),
            &p50s,
            0.0,
        )
        .expect("inputs present");
        assert!(!just_over.gate_pass, "just over tolerance must FAIL");
    }

    #[test]
    fn derive_gate_fails_when_the_re_centered_offset_exceeds_tolerance() {
        // cam2's own offset is safely INSIDE tolerance (tolerance − 25), but this camera's
        // delivery p50 is 30ms above the mean ⇒ the re-centered estimate (tolerance + 5) exceeds
        // the bound — the derivation must FAIL here even though cam2's own measured number would
        // have passed.
        let p50s = [940.0, 970.0, 1000.0]; // mean = 970.0
        let cam2_off = AV_OFFSET_GATE_TOLERANCE_MS - 25.0;
        let v = derive_camera_av_sync(Some(cam2_off), Some(1000.0), &p50s, 0.0)
            .expect("inputs present");
        assert!(
            (v.derived_offset_ms - (cam2_off + 30.0)).abs() < 1e-9,
            "expected cam2_off + (1000.0 - 970.0), got {}",
            v.derived_offset_ms
        );
        assert!(
            !v.gate_pass,
            "a re-centered offset outside the tolerance must FAIL, independent of cam2's own PASS"
        );
    }

    #[test]
    fn derive_respects_a_nonzero_expected_ms() {
        // expected_ms is the operator's dialed value (nominally ~0, but the gate must honor
        // whatever --av-expected-ms was actually passed).
        let p50s = [970.0, 970.0];
        let v =
            derive_camera_av_sync(Some(500.0), Some(970.0), &p50s, 500.0).expect("inputs present");
        assert!(
            v.gate_pass,
            "derived offset == expected_ms exactly ⇒ PASS regardless of the absolute value"
        );
    }

    // ---------------------------------------------------------------------
    // effective_offset_ms (#714/#689 — one computable per-camera number)
    // ---------------------------------------------------------------------
    // (reuses the `measured(av_offset_ms)` helper defined above for the
    // av_offset_gate_pass tests — a Measured CameraAvSync with that offset.)

    fn unknown() -> CameraAvSync {
        CameraAvSync {
            windows: 1,
            candidates: 500,
            cluster_samples: 0,
            av_offset_ms: None,
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        }
    }

    #[test]
    fn effective_offset_is_the_measured_value_for_a_measured_camera() {
        // cam2's own measurement — the effective value IS the measured offset (derived ignored,
        // and in practice absent for a measured camera).
        assert_eq!(effective_offset_ms(&measured(-4.2), None), Some(-4.2));
    }

    #[test]
    fn effective_offset_is_the_derived_value_for_a_starved_but_derived_camera() {
        // The #714/#689 fix: a per-window sample-starved camera (av_offset_ms=None) that HAS a
        // sound derived estimate surfaces the DERIVED number here, so the raw verdict is never a
        // bare `null` for a camera we actually have a computable A/V offset for.
        let d = DerivedAvSync {
            derived_offset_ms: -6.1,
            delivery_spread_ms: 17.0,
            gate_pass: true,
        };
        assert_eq!(effective_offset_ms(&unknown(), Some(&d)), Some(-6.1));
    }

    #[test]
    fn effective_offset_is_none_only_for_a_genuine_unknown() {
        // No measurement AND no derivation (cam2 itself Unknown, or no delivery sample) ⇒ a true
        // null — never fabricated. This is the ONLY case a consumer sees no number.
        assert_eq!(effective_offset_ms(&unknown(), None), None);
    }

    // -----------------------------------------------------------------
    // #1178 — fixed video-leg calibration + report-only residual channel
    // -----------------------------------------------------------------

    /// The fresh full-fleet cluster from verdict 845554984 (run 33176192564, 2026-08-29): every
    /// judged camera's measured A/V offset. cam1/cam2/cam6 fall 2-5ms OUTSIDE ±90 of 0 (the
    /// pre-#1178 default) and BLOCK the run; all five land well inside ±90 of the calibrated leg.
    const FRESH_CLUSTER_845554984: [f64; 5] =
        [-95.166_666, -91.979_166, -76.75, -93.916_666, -88.625];

    #[test]
    fn fresh_cluster_845554984_needs_the_video_leg_calibration_to_pass_1178() {
        // RED baseline: against the raw expected_ms=0 (the pre-#1178 default) the negative cluster
        // fails for exactly the three cameras past ±90 (cam1/cam2/cam6).
        let fails_at_zero: Vec<f64> = FRESH_CLUSTER_845554984
            .iter()
            .copied()
            .filter(|&off| !av_offset_gate_pass(&measured(off), 0.0))
            .collect();
        assert_eq!(
            fails_at_zero.len(),
            3,
            "pre-#1178: exactly cam1/cam2/cam6 (< -90) must fail vs expected_ms=0, got {fails_at_zero:?}"
        );
        // GREEN: against the calibrated video-leg default, EVERY judged camera passes with margin.
        for &off in FRESH_CLUSTER_845554984.iter() {
            assert!(
                av_offset_gate_pass(&measured(off), RIG_VIDEO_LEG_OFFSET_MS),
                "camera at {off}ms must pass vs calibrated RIG_VIDEO_LEG_OFFSET_MS={RIG_VIDEO_LEG_OFFSET_MS}"
            );
        }
    }

    #[test]
    fn video_leg_calibration_is_the_negative_cluster_median_not_zero_1178() {
        // The constant must be a real negative calibration, not the old 0.0 — pinned to the
        // verdict-845554984 5-camera median (−92.0, robust to the cam3 outlier).
        assert!(
            RIG_VIDEO_LEG_OFFSET_MS < -1.0,
            "RIG_VIDEO_LEG_OFFSET_MS must be a real negative video-leg calibration, got {RIG_VIDEO_LEG_OFFSET_MS}"
        );
        assert!(
            (RIG_VIDEO_LEG_OFFSET_MS - (-92.0)).abs() < 1e-9,
            "calibration pinned to the verdict-845554984 cluster median −92.0; a rig video-chain change (grabber/monitor/camera swap) is the only reason to re-derive it"
        );
    }

    #[test]
    fn calibration_still_catches_a_global_drift_never_masks_it_1178() {
        // A whole-fleet drift PAST the tolerance around the calibrated leg still FAILS — the fixed
        // constant (not a per-run median) is what preserves global-drift detection.
        let drifted = RIG_VIDEO_LEG_OFFSET_MS - (AV_OFFSET_GATE_TOLERANCE_MS + 10.0);
        assert!(
            !av_offset_gate_pass(&measured(drifted), RIG_VIDEO_LEG_OFFSET_MS),
            "an offset {drifted}ms (leg − (tol+10)) must still FAIL — calibration must not widen/mask the gate"
        );
        // ...and a camera exactly at the leg is a perfect residual-0 PASS.
        assert!(av_offset_gate_pass(
            &measured(RIG_VIDEO_LEG_OFFSET_MS),
            RIG_VIDEO_LEG_OFFSET_MS
        ));
    }

    #[test]
    fn residual_offset_is_measured_minus_expected_1178() {
        assert!((residual_offset_ms(-92.0, RIG_VIDEO_LEG_OFFSET_MS) - 0.0).abs() < 1e-9);
        assert!((residual_offset_ms(-76.75, -92.0) - 15.25).abs() < 1e-9);
        assert!((residual_offset_ms(0.0, 0.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn residual_summary_reports_median_and_spread_report_only_1178() {
        assert_eq!(
            residual_summary(&[]),
            ResidualSummary {
                median_ms: None,
                spread_ms: None,
                count: 0
            }
        );
        // the fresh cluster's residuals after subtracting the calibrated leg
        let residuals: Vec<f64> = FRESH_CLUSTER_845554984
            .iter()
            .map(|&off| residual_offset_ms(off, RIG_VIDEO_LEG_OFFSET_MS))
            .collect();
        let s = residual_summary(&residuals);
        assert_eq!(s.count, 5);
        // median residual ~0 (cam2), spread ~18.4ms (cam3 − cam1)
        assert!(
            s.median_ms.unwrap().abs() < 1.0,
            "median residual ~0, got {:?}",
            s.median_ms
        );
        assert!(
            (s.spread_ms.unwrap() - 18.4).abs() < 0.5,
            "spread ~18.4ms, got {:?}",
            s.spread_ms
        );
        // even n: median is the average of the two middle values (sorted [-4, 2, 8, 10])
        let even = residual_summary(&[10.0, -4.0, 2.0, 8.0]);
        assert_eq!(even.count, 4);
        assert!(
            (even.median_ms.unwrap() - 5.0).abs() < 1e-9,
            "even-n median = (2+8)/2 = 5, got {:?}",
            even.median_ms
        );
        assert!(
            (even.spread_ms.unwrap() - 14.0).abs() < 1e-9,
            "even-n spread = 10-(-4) = 14, got {:?}",
            even.spread_ms
        );
    }
}
