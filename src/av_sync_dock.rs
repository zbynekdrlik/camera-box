//! #398 — pure Tier-0 decision logic for the LIVE OBS A/V-sync dock.
//!
//! The vendored norihiro dock (`vendor/av-sync-dock/`) is the LIVE receiver: it captures the stream
//! program video+audio, decodes camera-box's dual-QR (frame identity) from the video and the QPSK
//! marker from the audio, pairs them, and displays the video↔audio offset ("Latency"). Everything
//! that DECIDES a value lives here, in Rust that compiles on DEFAULT features and is unit-tested at
//! Tier-0; the dock's C++ (`camera-box-audio.hpp` / `camera-box-video.hpp`) MIRRORS these functions
//! byte-for-byte and a committed C++ self-test (`vendor/av-sync-dock/test/camera-box-selftest.cpp`)
//! cross-checks the mirror against this module's results. The OBS/quirc GLUE (frame gather, ALSA
//! callback, ring, Qt labels) is the only C++-only part.
//!
//! WHY this module exists (the #398 bug it fixes): the deployed dock never locks the audio index or
//! the offset. Two root causes, both here-fixable by mirroring proven camera-box logic instead of
//! norihiro's:
//!   1. AUDIO — norihiro's `st_raw_audio_test_preamble` computes its half-symbol resolution as
//!      `c1 = c/2`, which is 0 at the rig's `c = auto_c(2,442,60/1) = 1` (60 fps), collapsing the
//!      preamble finder (`buffer_length`/`symbol_ns` → 0) so no marker is EVER detected; and its
//!      decode reads only 6 symbols with a bit-mapping that can't recover the full 8-bit index. The
//!      "norihiro's demod applies with no code change" assumption is FALSE at c=1. Fix: drive the
//!      dock's audio decode from [`crate::qpsk_marker::decode_markers`] — camera-box's OWN QPSK demod
//!      that is round-trip tested for all 256 indices AT c=1 — via the streaming wrapper here.
//!   2. VIDEO — the dock downscales the WHOLE (4K-rescaled) frame by `qr_step` (8 at 4K) with nearest
//!      sampling, shrinking each ~700 px dual-QR half to ~87 px → ~98 % miss. Fix: decode only the
//!      TOP band (where the top-anchored dual-QR lives), AREA-averaged to a scale that keeps each QR
//!      large, with an Otsu-binarized retry — the same techniques `src/probe/qr.rs` proved on the
//!      real soft optical frames (#202/#363). The geometry is [`top_band_decode_plan`].

use crate::qpsk_marker::{
    cluster_offset_ms, decode_markers_with_stats, AudioParams, AvOffset, DecodeStats,
};

/// QPSK preamble-screen threshold for the live decode — MATCHES the proven offline
/// `recording-verdict --av-sync` default (`av_threshold`). Low enough to catch a marker buried in
/// the music-laden mbc mix; the CRC-4 false decodes it lets through are rejected downstream by the
/// densest-cluster estimator, exactly as offline.
pub const DOCK_QPSK_THRESHOLD: f64 = 0.35;

/// Half-width (ms) of the offset cluster window. Candidate `video − audio` offsets within ±this
/// of the densest band are the real markers.
///
/// #733 (2026-07-13) tightened the OFFLINE `av_cluster_tol_ms` default from 60 to 25ms (a
/// real-data audit found the wider window occasionally blending two nearby sub-clusters into one
/// noisier band). This constant — the LIVE dock's own window — was deliberately left AT 60ms
/// rather than changed to match: the live dock operates very differently from the offline
/// one-shot per-recording decode (a continuous [`DOCK_CLUSTER_WINDOW_NS`]-rolling estimate, with
/// its own separate honesty gate [`DOCK_CLUSTER_MAX_MAD_MS`] already rejecting a diffuse/false-only
/// band), and this constant is mirrored BYTE-FOR-BYTE into the vendored C++ dock — changing it
/// safely needs the ~150min genlock vendored-OBS build cycle to verify, out of scope for #733's
/// pure-Rust audit. Filed #735 to evaluate tightening this one too — do NOT assume the two
/// values are in sync; check #735's status before relying on "matches the offline default" again.
pub const DOCK_CLUSTER_TOL_MS: f64 = 60.0;

/// Minimum tightly-clustered offset candidates before the dock trusts (and displays) a Latency.
/// Higher than the offline `av_min_matched` (4) because the LIVE dock must not flash a number from a
/// transient false-only band: it waits until enough real markers pile into one tight cluster.
pub const DOCK_CLUSTER_MIN_MATCHED: usize = 8;

/// Maximum MAD (ms) the densest cluster may have to be TRUSTED. Real markers share one near-constant
/// pipeline delay → a tight cluster (MAD ≈ 5–15 ms). A band that is merely the densest slice of the
/// uniformly-scattered CRC-4 false decodes is DIFFUSE (MAD → ~half the window). Gating on a small MAD
/// is what prevents the dock from ever locking onto a false-only band — it shows "measuring" until a
/// genuinely tight real cluster forms. This is the LIVE analog of the offline whole-recording
/// clustering: same estimator, plus an honesty gate so no wrong number is ever displayed.
pub const DOCK_CLUSTER_MAX_MAD_MS: f64 = 25.0;

/// Rolling window (ns) of recent offset candidates the cluster is computed over. ~3 min: long enough
/// to accumulate many real markers (≥ one per ~3 s cadence) so the real cluster dominates the
/// scattered false decodes, short enough that a genuine A/V-offset CHANGE (e.g. an OBS restart, #137)
/// re-converges within a few minutes.
pub const DOCK_CLUSTER_WINDOW_NS: u64 = 180 * 1_000_000_000;

/// Fraction of the frame HEIGHT (from the top) the video-QR decode band covers. The camera-box
/// dual-QR is TOP-anchored (`render_qr_dual_bgra` → `VAnchor::Top`): its top row sits ~24 px below
/// the frame top and it is ≤ ~700 px tall in a 1080-native frame (≈ 0.67 h), the SAME fraction after
/// any uniform rescale to 4K. 0.72 covers the whole dual-QR with margin at ANY output resolution
/// while excluding the bottom-corner burns — so the band is tall enough to contain each SQUARE QR
/// whole (a full-width wide band downscaled to a wide-aspect strip would clip the square QR).
pub const TOP_BAND_FRAC_NUM: u32 = 72;
pub const TOP_BAND_FRAC_DEN: u32 = 100;

/// Long-side cap (px) the top band is AREA-downscaled to before quirc. Chosen so each ~700-px-native
/// (≈1400 px at 4K) dual-QR half stays ≈ 500 px after downscale — comfortably ≥ the ~3 px/module
/// quirc needs even on a soft, NDI-recompressed optical capture — while keeping the quirc image small
/// enough (≤ ~760 px long side) that a 30 fps monitoring decode stays real-time.
pub const TOP_BAND_DECODE_CAP: u32 = 760;

/// The pixel plan for the live video-QR top-band decode: crop `[0, band_h)` rows of a `frame_w ×
/// frame_h` luma frame, then AREA-average-downscale that crop to `dst_w × dst_h` before quirc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopBandPlan {
    /// Height (px) of the top crop taken from the source frame.
    pub band_h: u32,
    /// Downscaled width fed to quirc.
    pub dst_w: u32,
    /// Downscaled height fed to quirc.
    pub dst_h: u32,
}

/// Compute the [`TopBandPlan`] for a `frame_w × frame_h` frame. The crop is the top
/// [`TOP_BAND_FRAC_NUM`]/[`TOP_BAND_FRAC_DEN`] of the height; it is then downscaled preserving aspect
/// so its LONG side is at most [`TOP_BAND_DECODE_CAP`] (never UPscaled — a small source stays 1:1).
/// Every dimension is clamped ≥ 1 so a degenerate tiny frame never yields a zero-sized quirc image.
/// Pure geometry, so the C++ mirror's dimension math is Tier-0 locked.
pub fn top_band_decode_plan(frame_w: u32, frame_h: u32) -> TopBandPlan {
    let band_h = (frame_h * TOP_BAND_FRAC_NUM / TOP_BAND_FRAC_DEN).clamp(1, frame_h.max(1));
    let src_w = frame_w.max(1);
    let long = src_w.max(band_h);
    let (dst_w, dst_h) = if long > TOP_BAND_DECODE_CAP {
        (
            (src_w * TOP_BAND_DECODE_CAP / long).max(1),
            (band_h * TOP_BAND_DECODE_CAP / long).max(1),
        )
    } else {
        (src_w, band_h)
    };
    TopBandPlan {
        band_h,
        dst_w,
        dst_h,
    }
}

/// Otsu's global threshold (0..=255) maximizing between-class variance of a gray histogram, with the
/// standard plateau-midpoint refinement (a clean black/white capture cuts BETWEEN the two peaks, not
/// at the dark peak). PURE + total: an empty/flat histogram returns 128 (a neutral mid-gray cut).
/// The C++ dual-QR decode binarizes the downscaled band at this threshold as a retry when quirc's own
/// adaptive prepare misses the SOFT optical capture — the exact recovery `src/probe/qr.rs` (#363)
/// proved essential on the real stream frames. Mirrored in `camera-box-video.hpp`.
pub fn otsu_threshold(hist: &[u64; 256]) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut w_bg, mut sum_bg) = (0u64, 0.0f64);
    let (mut best_var, mut plateau_lo, mut plateau_hi) = (-1.0f64, 128usize, 128usize);
    for (t, &count) in hist.iter().enumerate() {
        w_bg += count;
        if w_bg == 0 {
            continue;
        }
        let w_fg = total - w_bg;
        if w_fg == 0 {
            break;
        }
        sum_bg += t as f64 * count as f64;
        let m_bg = sum_bg / w_bg as f64;
        let m_fg = (sum_all - sum_bg) / w_fg as f64;
        let between = w_bg as f64 * w_fg as f64 * (m_bg - m_fg) * (m_bg - m_fg);
        if between > best_var + f64::EPSILON {
            best_var = between;
            plateau_lo = t;
            plateau_hi = t;
        } else if (between - best_var).abs() <= f64::EPSILON {
            plateau_hi = t;
        }
    }
    ((plateau_lo + plateau_hi) / 2) as u8
}

/// Streaming QPSK marker detector for the LIVE audio path: keeps a rolling window of the most recent
/// raw mono samples, runs the proven [`crate::qpsk_marker::decode_markers`] over it each `push`, and
/// returns each NEWLY detected marker as `(absolute_sample_index_from_stream_start, index)`.
///
/// WHY a rolling window over the batched `decode_markers` (rather than a bespoke incremental demod):
/// `decode_markers` is round-trip tested for every one of the 256 indices AT the rig's c=1 and under
/// noise+gain — it is the ONE audio demod known to decode exactly what cam2 emits. Re-running it over
/// a small window each audio callback reuses that tested code verbatim. The window is a few marker
/// lengths so any marker is wholly present in some call; dedup by absolute position (a marker seen in
/// two overlapping windows lands at the SAME absolute index) reports each marker exactly once.
///
/// The absolute sample index is stream-relative and monotone; the caller maps it to an OBS timestamp
/// using the callback clock (kept in the C++ glue, drift-anchored per callback). The false CRC-4
/// decodes this admits — inescapable on a music-laden mix, CRC-4 is 4 bits — are NOT filtered here;
/// they are rejected downstream by [`RollingOffsetCluster`], exactly as the offline path clusters.
pub struct StreamingMarkerDecoder {
    params: AudioParams,
    threshold: f64,
    /// Rolling raw mono samples (most recent `capacity` at most).
    buf: Vec<f32>,
    /// Max samples retained — must exceed one marker's `signal_len` so a marker is wholly present.
    capacity: usize,
    /// Absolute index (from stream start) of `buf[0]`; grows as the front is trimmed.
    origin: u64,
    /// Absolute index of the last reported marker's start (dedup anchor), or `None`.
    last_reported: Option<u64>,
    /// Minimum absolute-index gap for a detection to count as a NEW marker (dedup width).
    min_gap: u64,
    /// #690 — cumulative [`DecodeStats`] across every `push()` call, for the live dock's periodic
    /// audio diagnostic (`sync-test-output.cpp`'s rate-limited INFO log). Counts are a DELIBERATE
    /// over-count, not a per-marker tally: each `push()` re-decodes the WHOLE rolling window, so a
    /// real onset near the front of the window gets re-screened/re-counted on every subsequent
    /// `push()` until it ages out of `capacity` — the same reason `push()`'s own dedup (`last_reported`
    /// / `min_gap`) exists for the returned markers. That's fine for this counter's purpose: telling
    /// "zero vs nonzero" (does the demod see anything at all / does anything ever decode) and rough
    /// relative magnitude (`crc_fail` swamping `crc_ok` means mostly noise) — never an exact count of
    /// distinct real markers (use the deduped `push()` return value / [`RollingOffsetCluster`] for that).
    stats: DecodeStats,
}

impl StreamingMarkerDecoder {
    /// `capacity` samples retained (≥ `2 × signal_len` recommended so every marker is wholly present
    /// in some window); `min_gap` samples of separation for a detection to count as new (a fraction
    /// of the marker cadence — one `signal_len` is ample since real markers are seconds apart).
    pub fn new(params: AudioParams, threshold: f64, capacity: usize, min_gap: u64) -> Self {
        Self {
            params,
            threshold,
            buf: Vec::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            origin: 0,
            last_reported: None,
            min_gap: min_gap.max(1),
            stats: DecodeStats::default(),
        }
    }

    /// Append `samples`, trim to `capacity`, decode the window, and return the ABSOLUTE start index
    /// and `index` of each newly detected marker (in ascending absolute order). Also accumulates
    /// [`Self::stats`] — see its field doc for the over-counting caveat.
    pub fn push(&mut self, samples: &[f32]) -> Vec<(u64, u8)> {
        self.buf.extend_from_slice(samples);
        if self.buf.len() > self.capacity {
            let drop = self.buf.len() - self.capacity;
            self.buf.drain(0..drop);
            self.origin += drop as u64;
        }
        let mut out = Vec::new();
        let sr = self.params.sample_rate as f64;
        let (markers, batch_stats) =
            decode_markers_with_stats(&self.buf, &self.params, self.threshold);
        self.stats.preamble_screens_passed += batch_stats.preamble_screens_passed;
        self.stats.crc_ok += batch_stats.crc_ok;
        self.stats.crc_fail += batch_stats.crc_fail;
        for (ts_s, idx) in markers {
            // decode_markers reports the marker start in seconds within the window; convert to an
            // absolute stream index. Round to the nearest sample so the same marker seen in two
            // overlapping windows maps to an identical absolute index (stable dedup).
            let abs = self.origin + (ts_s * sr).round() as u64;
            let is_new = match self.last_reported {
                None => true,
                Some(prev) => abs > prev.saturating_add(self.min_gap),
            };
            if is_new {
                self.last_reported = Some(abs);
                out.push((abs, idx));
            }
        }
        out
    }

    /// Cumulative decode diagnostics since construction — see [`Self::stats`]'s field doc for what
    /// these counts mean (and why they over-count relative to distinct real markers).
    pub fn stats(&self) -> DecodeStats {
        self.stats
    }
}

/// Rolling densest-cluster A/V-offset estimator for the LIVE dock — the robust replacement for the
/// deployed dock's 1 s median (`cb_smooth_offset_ns`), which a burst of CRC-4 false decodes on the
/// music mix would drag off the real value.
///
/// Each real marker contributes one `video − audio` offset near a constant pipeline delay; each false
/// CRC-4 decode contributes a random index → a random ring slot → an offset scattered ~uniformly over
/// ±half the ring cycle. So the real markers form ONE tight cluster while the false decodes spread
/// thinly. [`push`](Self::push) keeps the last [`window_ns`](Self::window_ns) of `(ts, offset)`
/// samples and returns [`cluster_offset_ms`] over them — BUT only when the densest cluster is both big
/// enough (`min_matched`) AND tight enough (`max_mad_ms`); a diffuse, false-only band fails the MAD
/// gate, so the dock shows "measuring" rather than ever locking a wrong number. This mirrors the
/// offline whole-recording cluster, sized to a rolling window and honesty-gated for a live display.
pub struct RollingOffsetCluster {
    window_ns: u64,
    tol_ms: f64,
    min_matched: usize,
    max_mad_ms: f64,
    samples: std::collections::VecDeque<(u64, f64)>,
}

impl RollingOffsetCluster {
    pub fn new(window_ns: u64, tol_ms: f64, min_matched: usize, max_mad_ms: f64) -> Self {
        Self {
            window_ns,
            tol_ms,
            min_matched,
            max_mad_ms,
            samples: std::collections::VecDeque::new(),
        }
    }

    /// The dock's standing configuration (all the `DOCK_*` constants).
    pub fn dock() -> Self {
        Self::new(
            DOCK_CLUSTER_WINDOW_NS,
            DOCK_CLUSTER_TOL_MS,
            DOCK_CLUSTER_MIN_MATCHED,
            DOCK_CLUSTER_MAX_MAD_MS,
        )
    }

    /// Add one `(sample_ts_ns, offset_ms)` candidate (offset already lap-resolved), prune samples
    /// older than the window, and return the TRUSTED cluster offset if the densest band now clears
    /// both the size and MAD gates — else `None` ("still measuring / no trustworthy lock").
    pub fn push(&mut self, sample_ts_ns: u64, offset_ms: f64) -> Option<AvOffset> {
        self.samples.push_back((sample_ts_ns, offset_ms));
        while let Some(&(ts, _)) = self.samples.front() {
            if sample_ts_ns.saturating_sub(ts) > self.window_ns {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let offsets: Vec<f64> = self.samples.iter().map(|&(_, o)| o).collect();
        match cluster_offset_ms(&offsets, self.min_matched, self.tol_ms) {
            Some(est) if est.matched >= self.min_matched && est.mad_ms <= self.max_mad_ms => {
                Some(est)
            }
            _ => None,
        }
    }

    /// #926 fix-up (review finding 1/7) — shift every RETAINED sample's offset by `-delta_ms`.
    /// Call this the instant [`DockLockCorrector::decide`] returns an [`DockLockAction::Apply`]
    /// that actually changed `genlock_latency_ms_src` by `delta_ms` (`new_delay - current_delay`).
    ///
    /// WHY: every retained sample was measured under the OLD delay. Left alone, the window keeps
    /// reporting close to the PRE-correction offset for up to [`DOCK_CLUSTER_WINDOW_NS`] after the
    /// actuator already moved — far longer than [`DOCK_LOCK_MIN_REAPPLY_S`], so several more
    /// cooldown ticks can fire "correcting" an error that (from the actuator's point of view) no
    /// longer exists, over-shooting the target and — for a residual that started above the
    /// target — potentially crossing straight through it into the forbidden audio-early zone.
    /// Re-basing means the SAME closed-form relation [`DockLockCorrector::decide`] uses for a
    /// single correction (`ts_new = ts_old - delta_applied`) also holds for every already-retained
    /// sample, so the window's own median/MAD are correct for the NEW delay immediately — no need
    /// to wait for fresh markers to dilute the stale ones.
    pub fn rebase(&mut self, delta_ms: f64) {
        for (_, offset) in self.samples.iter_mut() {
            *offset -= delta_ms;
        }
    }
}

/// #926 — max ms a single [`DockLockCorrector::decide`] call may move `genlock_latency_ms_src`
/// away from its current value. Deliberately much smaller than the offline per-run
/// `AV_SYNC_MAX_STEP_MS` (50, `av_sync_calibrate.py`/`qpsk_marker::required_delay_ms`): the LIVE
/// corrector fires on every `Locked`/`Updated` lock-audit transition (potentially every few
/// seconds while converging), so each individual nudge must stay small — mirrors ASRC's #803
/// "inaudible, never one abrupt jump" philosophy applied to the video-delay knob instead of a
/// resample ratio. A large initial error (e.g. the ticket's own -52ms) converges over several
/// Locked/Updated events rather than jumping in one step.
pub const DOCK_LOCK_MAX_STEP_MS: i32 = 5;

/// #926 — minimum wall-clock time (via the caller's own monotonic `now_ns`) between two actuator
/// writes. Gives the rolling offset cluster time to reflect the PREVIOUS correction (the samples
/// already in its 180s window were measured under the OLD delay) before nudging again, and keeps
/// a converged, healthy lock from re-writing the OBS source setting on every marker.
pub const DOCK_LOCK_MIN_REAPPLY_S: f64 = 30.0;

/// DistroAV hardware range for `genlock_latency_ms_src` (mirrors `av_sync_calibrate.py`'s
/// `LATENCY_MIN`/`LATENCY_MAX` and `qpsk_marker::required_delay_ms`'s own `.clamp(3, 2000)`).
pub const DOCK_LOCK_LATENCY_MIN_MS: i32 = 3;
pub const DOCK_LOCK_LATENCY_MAX_MS: i32 = 2000;

/// #926 fix-up (review finding 3) — the MINIMUM safety margin (ms) the corrector targets above
/// zero, regardless of how tight the current cluster measurement claims to be. Targeting exactly
/// `[0, 1)` claims sub-millisecond precision that is three orders of magnitude below the
/// estimator's own accepted noise floor ([`DOCK_CLUSTER_MAX_MAD_MS`], 25ms) — a false precision
/// that can itself swing back negative on ordinary measurement jitter. The ACTUAL target margin is
/// `mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS)`: scaled to the cluster's own
/// observed dispersion (a wide/noisy lock gets a bigger, honest margin), but never below this
/// floor even when a cluster reports a suspiciously tiny/zero MAD (few samples, or a lucky run).
pub const DOCK_LOCK_MIN_MARGIN_MS: f64 = 1.0;

/// The decision [`DockLockCorrector::decide`] returns: either leave the actuator alone, or set it
/// to a new absolute `genlock_latency_ms_src` value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DockLockAction {
    /// No test-signal lock right now, or the current value already satisfies the "audio never
    /// early" target, or the cooldown has not yet elapsed — do not touch the actuator.
    Hold,
    /// Apply this NEW absolute `genlock_latency_ms_src` value.
    Apply(i32),
}

/// #926 — the LIVE, in-process A/V-sync dock corrector. Holds `genlock_latency_ms_src` (the SAME
/// per-source video-delay knob the offline `av_sync_calibrate.py` path already uses) at a target
/// where the measured dock-convention offset (`ts_ms = audio_ts - video_ts`, see
/// `sync-test-output.cpp`) is NEVER negative ("audio early" — a forbidden steady state per the
/// issue's own directive: sound is always physically slower than light, so a resting audio-ahead
/// state can only be a rig defect) — landing at a deliberate, noise-scaled safety MARGIN above
/// zero (see [`DOCK_LOCK_MIN_MARGIN_MS`]'s own doc comment for why exactly `[0, 1)` is a false
/// precision the estimator's ~25ms noise floor cannot actually back up).
///
/// Closed-form proof of the "never audio early" invariant (ignoring the safety clamps below, which
/// only ever slow convergence, never violate the invariant once it IS reached): let `margin =
/// mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS)` and `g =
/// floor(ts_ms - margin)`. Setting `new_delay = current_delay + g` changes `ts` by exactly `-g`
/// (increasing the video source's own added delay by `g` ms delays the video `g` ms further,
/// which — since `ts = audio_ts - video_ts` — REDUCES `ts` by `g`). So the resulting `ts_new =
/// ts_ms - g = margin + ((ts_ms - margin) - floor(ts_ms - margin))`, `margin` plus the fractional
/// part of `(ts_ms - margin)`, which is always in `[margin, margin + 1)` by definition of `floor`
/// — and since `margin >= DOCK_LOCK_MIN_MARGIN_MS > 0`, `ts_new` is always strictly positive, never
/// merely non-negative. `g` positive means the video is currently arriving too early relative to
/// the target (bring it later, increase delay); `g` negative means video is lagging (audio is
/// early), so the delay is REDUCED — same physical direction the offline `required_delay_ms`
/// already uses, just floor-biased instead of round-to-nearest so the residual can only land on
/// the audio-late-or-margin side.
///
/// Only ever acts on a genuine trusted measurement (the caller passes `locked = true` only when
/// the rolling cluster currently reports `est.ok`) — `locked = false` (no test signal: real event,
/// no QR, no marker) never touches the actuator, which is what implements the ticket's
/// requirement 5 (measure-only, permanent lock, no drift-chasing on program material) with no
/// separate timeout/heartbeat. #926 fix-up (review finding 2): the caller drives this from EVERY
/// trusted (`est.ok`) measurement, not only a `CbLockAuditTracker` `Locked`/`Updated` classifier
/// transition — the classifier's `Updated` needs a >5ms MOVE of the (window-smoothed) median,
/// which stalls convergence once the window itself lags a landed correction; this function's own
/// cooldown ([`DOCK_LOCK_MIN_REAPPLY_S`]) and dead-zone (`g == 0`) checks are what make calling it
/// on every trusted measurement safe.
pub struct DockLockCorrector {
    max_step_ms: i32,
    min_delay_ms: i32,
    max_delay_ms: i32,
    min_reapply_s: f64,
    last_applied_ns: Option<u64>,
}

impl DockLockCorrector {
    pub fn new(max_step_ms: i32, min_reapply_s: f64) -> Self {
        Self {
            max_step_ms,
            min_delay_ms: DOCK_LOCK_LATENCY_MIN_MS,
            max_delay_ms: DOCK_LOCK_LATENCY_MAX_MS,
            min_reapply_s,
            last_applied_ns: None,
        }
    }

    /// The dock's standing configuration ([`DOCK_LOCK_MAX_STEP_MS`]/[`DOCK_LOCK_MIN_REAPPLY_S`]).
    pub fn dock() -> Self {
        Self::new(DOCK_LOCK_MAX_STEP_MS, DOCK_LOCK_MIN_REAPPLY_S)
    }

    /// Decide what (if anything) to do with the actuator right now.
    ///
    /// `locked`: true only when the caller's rolling cluster currently reports a trusted
    /// (`est.ok`) measurement; false otherwise (no test signal — real event, no QR, no marker) —
    /// see the struct doc for why `false` always yields `Hold`. #926 fix-up (review finding 2):
    /// the caller must pass `true` on EVERY trusted measurement, not only a lock-audit
    /// `Locked`/`Updated` classifier transition — this function's own cooldown/dead-zone gates
    /// make that safe. `offset_ms`: the locked cluster offset in DOCK convention (`audio_ts -
    /// video_ts`), only meaningful when `locked`. `mad_ms`: the SAME cluster's median absolute
    /// deviation (ms), used to size the safety margin (see [`DOCK_LOCK_MIN_MARGIN_MS`]) — also
    /// only meaningful when `locked`. `current_delay_ms`: the actuator's CURRENT
    /// `genlock_latency_ms_src`, read fresh by the caller (never cached) so a concurrent manual/
    /// scripted change is respected. `now_ns`: the caller's own monotonic clock (e.g. the OBS
    /// pipeline timestamp already in hand) for the cooldown.
    ///
    /// #926 fix-up (review finding 5): a non-finite `offset_ms` (NaN/±inf — should never happen
    /// from a valid cluster estimate, but never trust an external input blindly) always yields
    /// `Hold` rather than risking UB/a panic on the later float→int conversions.
    pub fn decide(
        &mut self,
        locked: bool,
        offset_ms: f64,
        mad_ms: f64,
        current_delay_ms: i32,
        now_ns: u64,
    ) -> DockLockAction {
        if !locked {
            return DockLockAction::Hold;
        }
        if !offset_ms.is_finite() {
            return DockLockAction::Hold;
        }
        let margin = if mad_ms.is_finite() {
            mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS)
        } else {
            DOCK_LOCK_MIN_MARGIN_MS
        };
        // Clamp BEFORE the later `as i64` casts (finding 5): offset_ms is finite but could still
        // be astronomically large, which would otherwise risk an overflowing add below.
        let g = (offset_ms - margin).floor().clamp(-1_000_000.0, 1_000_000.0);
        if g == 0.0 {
            return DockLockAction::Hold; // already ts_ms in [margin, margin + 1) -- nothing to do
        }
        if let Some(last) = self.last_applied_ns {
            let elapsed_s = now_ns.saturating_sub(last) as f64 / 1_000_000_000.0;
            if elapsed_s < self.min_reapply_s {
                return DockLockAction::Hold; // cooldown -- let the last correction take effect first
            }
        }
        let raw = current_delay_ms as i64 + g as i64;
        let lo = (current_delay_ms - self.max_step_ms) as i64;
        let hi = (current_delay_ms + self.max_step_ms) as i64;
        let stepped = raw.clamp(lo, hi);
        let clamped = stepped.clamp(self.min_delay_ms as i64, self.max_delay_ms as i64) as i32;
        if clamped == current_delay_ms {
            return DockLockAction::Hold;
        }
        self.last_applied_ns = Some(now_ns);
        DockLockAction::Apply(clamped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpsk_marker::{frame_id_to_index, marker_signal, signal_len, AV_SYNC_RING_CYCLE_NS};

    #[test]
    fn top_band_plan_4k_keeps_qr_large_and_within_cap() {
        // A 4K program frame: the top 72 % (1555 px) crop, downscaled so the long side (3840) hits
        // the 760 cap. The ~1400 px dual-QR half then lands at 1400*760/3840 ≈ 277 px... but the
        // decode band is only the TOP portion; what matters is the long-side cap holds and the band
        // stays tall enough to contain a square QR whole.
        let p = top_band_decode_plan(3840, 2160);
        assert_eq!(p.band_h, 2160 * 72 / 100); // 1555
        assert_eq!(p.dst_w, 760); // long side capped
        assert_eq!(p.dst_h, 1555 * 760 / 3840); // aspect preserved, ~307
        assert!(
            p.dst_h >= 256,
            "band must stay tall enough for a square QR: {p:?}"
        );
    }

    #[test]
    fn top_band_plan_1080p_downscales_modestly() {
        let p = top_band_decode_plan(1920, 1080);
        assert_eq!(p.band_h, 1080 * 72 / 100); // 777
        assert_eq!(p.dst_w, 760);
        assert_eq!(p.dst_h, 777 * 760 / 1920);
    }

    #[test]
    fn top_band_plan_small_frame_is_not_upscaled_and_never_zero() {
        // A tiny frame stays 1:1 (no upscale) and every dimension is ≥ 1 (no zero-sized quirc image).
        let p = top_band_decode_plan(320, 200);
        assert_eq!(p.band_h, 200 * 72 / 100); // 144
        assert_eq!(p.dst_w, 320);
        assert_eq!(p.dst_h, 144);
        let d = top_band_decode_plan(1, 1);
        assert!(d.band_h >= 1 && d.dst_w >= 1 && d.dst_h >= 1, "{d:?}");
    }

    #[test]
    fn otsu_matches_the_probe_reference_behaviour() {
        // Same contract as src/probe/qr.rs::otsu_threshold (the mirror source): bimodal cuts between
        // the peaks; empty → 128.
        let mut hist = [0u64; 256];
        hist[20] = 1000;
        hist[230] = 1000;
        let t = otsu_threshold(&hist);
        assert!(t > 20 && t < 230, "cut between peaks, got {t}");
        assert_eq!(otsu_threshold(&[0u64; 256]), 128);
    }

    #[test]
    fn streaming_decoder_reports_each_marker_once_across_overlapping_windows() {
        // Feed a long silence with two markers embedded, in SMALL chunks (so each marker straddles
        // several push() windows), and assert each is reported EXACTLY once with the right index.
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let sig = signal_len(&p);
        let mut stream = vec![0.0f32; sr * 8]; // 8 s
                                               // frame_ids → indices via the emitter's own mapping (index = frame_id & 0xFF).
        let m0 = (sr, 2000u32); // marker at 1.0 s, frame_id 2000 → idx 208
        let m1 = (sr * 5, 2431u32); // marker at 5.0 s, frame_id 2431 → idx 127
        for &(start, fid) in &[m0, m1] {
            let s = marker_signal(frame_id_to_index(fid), &p);
            stream[start..start + s.len()].copy_from_slice(&s);
        }
        // capacity 2 markers wide; min_gap one signal_len; feed in 480-sample chunks.
        let mut dec = StreamingMarkerDecoder::new(p, DOCK_QPSK_THRESHOLD, sig * 3, sig as u64);
        let mut found: Vec<(u64, u8)> = Vec::new();
        for chunk in stream.chunks(480) {
            found.extend(dec.push(chunk));
        }
        assert_eq!(found.len(), 2, "each marker exactly once: {found:?}");
        // Absolute indices ≈ the true starts; indices == frame_id & 0xFF.
        assert!(
            (found[0].0 as i64 - sr as i64).abs() < 8,
            "m0 pos {found:?}"
        );
        assert_eq!(found[0].1, frame_id_to_index(2000));
        assert!(
            (found[1].0 as i64 - 5 * sr as i64).abs() < 8,
            "m1 pos {found:?}"
        );
        assert_eq!(found[1].1, frame_id_to_index(2431));
    }

    #[test]
    fn streaming_decoder_stats_are_zero_on_silence_and_nonzero_once_a_marker_streams_through() {
        // #690: the live-dock audio diagnostic reads `stats()` — pin that it starts at all-zero
        // (silence) and that a real marker fed through `push()` (even split across chunks, the
        // production shape) drives `crc_ok` above zero with no `crc_fail` needed for a clean signal.
        let p = AudioParams::rig60();
        let mut dec = StreamingMarkerDecoder::new(
            p,
            DOCK_QPSK_THRESHOLD,
            signal_len(&p) * 3,
            signal_len(&p) as u64,
        );
        let s0 = dec.stats();
        assert_eq!(s0.preamble_screens_passed, 0);
        assert_eq!(s0.crc_ok, 0);
        assert_eq!(s0.crc_fail, 0);

        let sr = p.sample_rate as usize;
        let mut stream = vec![0.0f32; sr]; // 1 s
        let s = marker_signal(42, &p);
        stream[sr / 4..sr / 4 + s.len()].copy_from_slice(&s);
        let mut found: Vec<(u64, u8)> = Vec::new();
        for chunk in stream.chunks(480) {
            found.extend(dec.push(chunk));
        }
        assert_eq!(
            found.len(),
            1,
            "sanity: the marker itself decoded: {found:?}"
        );
        let s1 = dec.stats();
        assert!(
            s1.crc_ok >= 1,
            "a decoded marker must count as at least one crc_ok: {s1:?}"
        );
        assert!(
            s1.preamble_screens_passed >= s1.crc_ok,
            "every crc_ok started as a passed screen: {s1:?}"
        );
    }

    #[test]
    fn rolling_cluster_locks_the_real_offset_and_rejects_a_false_only_burst() {
        // The live-dock analog of the offline false-flood test: ~12 real markers at a constant
        // +40 ms offset, plus a heavy scatter of false decodes spread across ±half the ring cycle.
        // The rolling cluster must LOCK +40 ms (tight), not a false-only band.
        let cycle_ms = AV_SYNC_RING_CYCLE_NS as f64 / 1_000_000.0; // ~4266 ms
        let mut c = RollingOffsetCluster::dock();
        let mut locked: Option<AvOffset> = None;
        // False decodes arrive ~10×/s scattered across the whole ±cycle/2 range; real markers every
        // ~3 s at +40 ms. Interleave over 60 s.
        let mut t_ns: u64 = 0;
        let step_ns: u64 = 100_000_000; // 0.1 s between false decodes
        let mut n_real = 0;
        for k in 0..600u64 {
            t_ns += step_ns;
            // deterministic pseudo-random false offset in (-cycle/2, +cycle/2]
            let r = ((k.wrapping_mul(2_654_435_761) >> 8) % 100_000) as f64 / 100_000.0; // 0..1
            let false_off = (r - 0.5) * cycle_ms;
            if let Some(est) = c.push(t_ns, false_off) {
                locked = Some(est);
            }
            // a real marker every 3 s (every 30 false steps), tight around +40 ms
            if k % 30 == 0 {
                n_real += 1;
                let jitter = ((k % 7) as f64 - 3.0) * 2.0; // ±6 ms
                if let Some(est) = c.push(t_ns, 40.0 + jitter) {
                    locked = Some(est);
                }
            }
        }
        assert!(
            n_real >= DOCK_CLUSTER_MIN_MATCHED,
            "test needs enough real markers: {n_real}"
        );
        let est = locked.expect("the rolling cluster must eventually LOCK a trustworthy offset");
        assert!(
            (est.offset_ms - 40.0).abs() < DOCK_CLUSTER_TOL_MS,
            "locked offset {} should be the real +40 ms cluster (matched {}, mad {})",
            est.offset_ms,
            est.matched,
            est.mad_ms
        );
        assert!(
            est.mad_ms <= DOCK_CLUSTER_MAX_MAD_MS,
            "locked cluster must be tight: {est:?}"
        );
    }

    #[test]
    fn rolling_cluster_shows_nothing_until_enough_tight_markers() {
        // Below min_matched real markers → no lock (None), even with some scattered false decodes:
        // the dock must display "measuring", never a premature/false number.
        let mut c = RollingOffsetCluster::dock();
        let mut any_lock = false;
        for k in 0..(DOCK_CLUSTER_MIN_MATCHED as u64 - 1) {
            // a few tight real markers, but fewer than min_matched
            if c.push(k * 100_000_000, 40.0).is_some() {
                any_lock = true;
            }
        }
        assert!(
            !any_lock,
            "must not lock before min_matched tight markers arrive"
        );
    }

    #[test]
    fn rolling_cluster_rejects_a_wide_band_via_the_mad_gate() {
        // The MAD gate's job: even when the cluster has ENOUGH members (≥ min_matched), a band that
        // is WIDE (spread across the whole tol window, MAD ≫ 25 ms — the signature of a loose/
        // false-only band, never a real constant-delay cluster) must NOT lock. Feed exactly
        // min_matched candidates evenly spread across the full ±tol window: matched == min_matched
        // but MAD ≈ 30 ms > 25 ms → no lock. (Because there are only min_matched points total, no
        // TIGHT sub-cluster of min_matched can form — this isolates the MAD gate cleanly.)
        let mut c = RollingOffsetCluster::dock();
        let n = DOCK_CLUSTER_MIN_MATCHED as u64;
        let mut locked = false;
        for k in 0..n {
            let off =
                -DOCK_CLUSTER_TOL_MS + (k as f64) * (2.0 * DOCK_CLUSTER_TOL_MS / (n - 1) as f64);
            if c.push(k * 100_000_000, off).is_some() {
                locked = true;
            }
        }
        assert!(
            !locked,
            "a wide (high-MAD) band must be rejected by the MAD gate"
        );
    }

    // ---- #926 DockLockCorrector ----

    #[test]
    fn corrector_holds_when_not_locked() {
        // Unlocked (real event, no test signal) must NEVER touch the actuator, regardless of
        // however wrong the last-known offset was.
        let mut c = DockLockCorrector::new(5, 30.0);
        assert_eq!(
            c.decide(false, -52.2, 5.0, 950, 1_000_000_000),
            DockLockAction::Hold
        );
    }

    #[test]
    fn corrector_holds_once_already_in_the_safety_margin_zone() {
        // With mad_ms=5.0 (a typical tight cluster) the target zone is [5, 6)ms, not [0,1) --
        // #926 fix-up finding 3: a bare [0,1) target is false precision the estimator's own noise
        // floor can't back up. Both boundary-ish values within the margin zone must Hold.
        let mut c = DockLockCorrector::new(5, 30.0);
        assert_eq!(
            c.decide(true, 5.0, 5.0, 950, 1_000_000_000),
            DockLockAction::Hold
        );
        let mut c2 = DockLockCorrector::new(5, 30.0);
        assert_eq!(
            c2.decide(true, 5.9, 5.0, 950, 1_000_000_000),
            DockLockAction::Hold
        );
    }

    #[test]
    fn corrector_never_lands_below_the_safety_margin_across_many_offsets_mads_and_deploys() {
        // The closed-form invariant, swept over a wide range of measured offsets, cluster MADs,
        // and current delays, WITHOUT the step clamp interfering (max_step huge) so the single
        // application fully converges: the resulting ts (offset_ms - applied_delta) must land in
        // [margin, margin+1), where margin = mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS,
        // DOCK_CLUSTER_MAX_MAD_MS) -- covering the margin CLAMPING at both ends (0.1/50.0 fall
        // outside [1,25] and must clamp).
        for &offset_ms in &[
            -523.7, -100.0, -52.2, -10.4, -1.0, -0.1, 0.0, 0.5, 3.3, 10.0, 42.9, 100.0, 900.0,
        ] {
            for &current in &[3, 50, 500, 950, 1000, 1500, 1999, 2000] {
                for &mad_ms in &[0.1, 1.0, 5.0, 25.0, 50.0] {
                    let mut c = DockLockCorrector::new(100_000, 30.0); // effectively unclamped step
                    let action = c.decide(true, offset_ms, mad_ms, current, 1_000_000_000);
                    let new_delay = match action {
                        DockLockAction::Apply(v) => v,
                        DockLockAction::Hold => current, // already within the margin zone, or a no-op
                    };
                    let delta_applied = (new_delay - current) as f64;
                    let ts_new = offset_ms - delta_applied;
                    let hit_rail = new_delay == 3 || new_delay == 2000;
                    if !hit_rail {
                        let margin = mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS);
                        assert!(
                            (margin - 1e-9..margin + 1.0 + 1e-9).contains(&ts_new),
                            "offset={offset_ms} mad={mad_ms} current={current} new_delay={new_delay} \
                             ts_new={ts_new} margin={margin} must land in [margin,margin+1) -- audio \
                             must never end up early"
                        );
                    } else {
                        // #926 fix-up finding 9: pinned at a hardware rail is a genuine, explicitly
                        // acknowledged case (never a silently-skipped one) -- ts_new MAY remain
                        // outside the margin zone (including negative/audio-early) by hardware
                        // necessity. See corrector_at_floor_rail_can_leave_audio_early_by_hardware_limit
                        // for the concrete, hand-checked scenario; here we only pin that the clamp
                        // landed EXACTLY at the rail it claims.
                        assert!(
                            new_delay == 3 || new_delay == 2000,
                            "hit_rail flag disagrees with new_delay={new_delay}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn corrector_at_floor_rail_can_leave_audio_early_by_hardware_limit() {
        // #926 fix-up finding 9: at the DistroAV floor (3ms) with nowhere further to reduce, a
        // residual that needs MORE reduction than the floor allows stays audio-early -- a real
        // hardware limit, not a corrector bug. Explicitly asserted (never silently excluded).
        let mut c = DockLockCorrector::new(5, 30.0);
        let action = c.decide(true, -20.0, 5.0, 3, 1_000_000_000);
        assert_eq!(
            action,
            DockLockAction::Hold,
            "pinned at the floor -- no room to correct further"
        );
        // No correction applied -- the ORIGINAL offset (-20ms, audio-early) persists.
        let ts_new = -20.0_f64;
        assert!(
            ts_new < 0.0,
            "at the floor rail, audio-early can persist by hardware necessity"
        );
    }

    #[test]
    fn corrector_margin_clamps_to_the_min_when_mad_is_tiny_or_zero() {
        // A suspiciously tiny/zero MAD (very few samples, or a lucky run) must NOT be trusted down
        // to sub-1ms precision -- the margin floors at DOCK_LOCK_MIN_MARGIN_MS regardless.
        let mut c = DockLockCorrector::new(100_000, 30.0); // effectively unclamped step
        match c.decide(true, 10.0, 0.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                let ts_new = 10.0 - (v - 950) as f64;
                assert!(
                    (DOCK_LOCK_MIN_MARGIN_MS..DOCK_LOCK_MIN_MARGIN_MS + 1.0).contains(&ts_new),
                    "must land at the MINIMUM 1ms margin, not a bare 0: ts_new={ts_new}"
                );
            }
            DockLockAction::Hold => panic!("10ms of excess lateness must be corrected even with mad=0"),
        }
    }

    #[test]
    fn corrector_margin_clamps_to_the_max_mad_when_mad_is_huge() {
        let mut c = DockLockCorrector::new(100_000, 30.0); // effectively unclamped step
        match c.decide(true, 100.0, 500.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                let ts_new = 100.0 - (v - 950) as f64;
                assert!(
                    (DOCK_CLUSTER_MAX_MAD_MS..DOCK_CLUSTER_MAX_MAD_MS + 1.0).contains(&ts_new),
                    "margin must clamp at DOCK_CLUSTER_MAX_MAD_MS: ts_new={ts_new}"
                );
            }
            DockLockAction::Hold => panic!("100ms of excess lateness must be corrected even with an absurd mad"),
        }
    }

    #[test]
    fn corrector_rejects_nonfinite_offset_and_never_touches_actuator() {
        // #926 fix-up finding 5: NaN/±inf must never reach the later float->int conversions.
        for &bad in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut c = DockLockCorrector::new(5, 30.0);
            assert_eq!(
                c.decide(true, bad, 5.0, 950, 1_000_000_000),
                DockLockAction::Hold,
                "non-finite offset_ms={bad} must always Hold"
            );
        }
    }

    #[test]
    fn corrector_clamps_an_astronomically_large_finite_offset_without_panicking() {
        // Still FINITE (per is_finite()) but far beyond any real measurement -- must not panic
        // (overflow) and must still move in the CORRECT direction, capped by the step budget.
        let mut c = DockLockCorrector::new(5, 30.0);
        match c.decide(true, 1e18, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(v, 955, "huge positive offset -- step-capped increase")
            }
            DockLockAction::Hold => panic!("a huge positive offset must still trigger a correction"),
        }
        let mut c2 = DockLockCorrector::new(5, 30.0);
        match c2.decide(true, -1e18, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(v, 945, "huge negative offset -- step-capped decrease")
            }
            DockLockAction::Hold => panic!("a huge negative offset must still trigger a correction"),
        }
    }

    #[test]
    fn corrector_step_clamps_a_large_correction_but_never_moves_wrong_direction() {
        // -52.2ms (audio early) with a tight 5ms step budget must not jump straight to the fully
        // converged value -- it moves at most 5ms, in the CORRECT direction (reduce the delay).
        let mut c = DockLockCorrector::new(5, 30.0);
        match c.decide(true, -52.2, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(v, 945, "must reduce by exactly the step budget")
            }
            DockLockAction::Hold => panic!("a -52.2ms error must trigger a correction"),
        }
    }

    #[test]
    fn corrector_respects_the_hardware_clamp_at_the_floor() {
        // Wants to go below the DistroAV floor (3ms) -- the raw target clamps to exactly 3.
        let mut c = DockLockCorrector::new(50, 30.0);
        match c.decide(true, -10.0, 5.0, 5, 1_000_000_000) {
            DockLockAction::Apply(v) => assert_eq!(v, 3, "must clamp at the hardware floor"),
            DockLockAction::Hold => panic!("should have attempted a correction (clamped to floor)"),
        }
        // Already PINNED at the floor with nowhere left to go -- correctly a no-op (Hold), not a
        // pointless Apply(current) actuator write.
        let mut c2 = DockLockCorrector::new(50, 30.0);
        assert_eq!(
            c2.decide(true, -10.0, 5.0, 3, 1_000_000_000),
            DockLockAction::Hold,
            "already at the floor with no room to correct further must Hold, not re-write the same value"
        );
    }

    #[test]
    fn corrector_respects_the_hardware_clamp_at_the_ceiling() {
        // Wants to go above the DistroAV ceiling (2000ms) -- the raw target clamps to exactly 2000.
        let mut c = DockLockCorrector::new(50, 30.0);
        match c.decide(true, 10.0, 5.0, 1998, 1_000_000_000) {
            DockLockAction::Apply(v) => assert_eq!(v, 2000, "must clamp at the hardware ceiling"),
            DockLockAction::Hold => {
                panic!("should have attempted a correction (clamped to ceiling)")
            }
        }
        // Already PINNED at the ceiling -- Hold, not a pointless re-write.
        let mut c2 = DockLockCorrector::new(50, 30.0);
        assert_eq!(
            c2.decide(true, 10.0, 5.0, 2000, 1_000_000_000),
            DockLockAction::Hold,
            "already at the ceiling with no room to correct further must Hold, not re-write the same value"
        );
    }

    #[test]
    fn corrector_enforces_a_cooldown_between_applications() {
        let mut c = DockLockCorrector::new(5, 30.0);
        // First correction at t=1s applies.
        assert!(matches!(
            c.decide(true, -52.2, 5.0, 950, 1_000_000_000),
            DockLockAction::Apply(_)
        ));
        // 10s later (< 30s cooldown) -- must Hold even though a further correction is still due.
        assert_eq!(
            c.decide(true, -47.2, 5.0, 945, 11_000_000_000),
            DockLockAction::Hold,
            "cooldown must suppress a second write within min_reapply_s"
        );
        // 31s after the FIRST application -- cooldown elapsed, must apply again.
        assert!(matches!(
            c.decide(true, -47.2, 5.0, 945, 32_000_000_000),
            DockLockAction::Apply(_)
        ));
    }

    #[test]
    fn corrector_converges_excess_audio_lateness_toward_the_safety_margin() {
        // offset_ms positive (audio ALREADY late by more than the target margin) is not
        // forbidden, but the ticket wants MINIMAL latency -- the corrector must still nudge it
        // down toward the margin (5ms here, not a bare 0), not just leave it (only the
        // negative/audio-early direction is a hard violation; drifting arbitrarily positive would
        // violate "hold FIXED MINIMAL latency").
        let mut c = DockLockCorrector::new(50, 30.0);
        match c.decide(true, 42.0, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(v, 987, "must increase delay to close the gap down to the 5ms margin")
            }
            DockLockAction::Hold => {
                panic!("42ms of excess audio-lateness must trigger a correction")
            }
        }
    }

    #[test]
    fn dock_default_constructor_matches_named_constants() {
        // #926 fix-up finding 10: DockLockCorrector::dock() must behave IDENTICALLY to
        // new(DOCK_LOCK_MAX_STEP_MS, DOCK_LOCK_MIN_REAPPLY_S) -- exercised through a scenario that
        // pins down both the step budget and the cooldown.
        let mut c = DockLockCorrector::dock();
        assert_eq!(
            c.decide(true, -52.2, 5.0, 950, 1_000_000_000),
            DockLockAction::Apply(950 - DOCK_LOCK_MAX_STEP_MS)
        );
        assert_eq!(
            c.decide(true, -47.2, 5.0, 950 - DOCK_LOCK_MAX_STEP_MS, 1_000_000_001),
            DockLockAction::Hold,
            "within DOCK_LOCK_MIN_REAPPLY_S of the first application must Hold"
        );
    }

    #[test]
    fn rolling_cluster_rebase_shifts_every_retained_sample_immediately() {
        // #926 fix-up finding 1/7: rebase() must shift EVERY retained sample by -delta_ms so the
        // window reflects the post-correction state right away -- not 180s later once fresh
        // markers happen to dilute the stale ones.
        let mut c = RollingOffsetCluster::dock();
        let mut t_ns = 0u64;
        let mut last = None;
        for _ in 0..(DOCK_CLUSTER_MIN_MATCHED + 4) {
            t_ns += 100_000_000;
            last = c.push(t_ns, -52.2);
        }
        let before = last.expect("locked at -52.2ms before rebase");
        assert!((before.offset_ms - (-52.2)).abs() < 1e-9, "{before:?}");
        assert!(before.mad_ms < 1.0, "tight cluster before rebase: {before:?}");

        // -52.2ms is audio-early, so the closed-form correction REDUCES the delay by 53ms
        // (delta_applied = floor(-52.2) = -53) -- every retained sample must shift UP by 53ms
        // (subtracting a NEGATIVE delta), landing at 0.8ms (the single-shot converged value).
        c.rebase(-53.0);

        // No NEW sample added -- pushing the SAME already-shifted value (0.8ms, what a fresh
        // marker would now genuinely read) must keep the cluster tight and read ~0.8ms, proving
        // the retained history moved, not just the newest point.
        t_ns += 100_000_000;
        let after = c.push(t_ns, 0.8).expect("still locked after rebase");
        assert!(
            (after.offset_ms - 0.8).abs() < 1e-6,
            "rebase must shift retained samples, got {after:?}"
        );
        assert!(
            after.mad_ms < 1.0,
            "rebasing must NOT inflate dispersion (finding 7): {after:?}"
        );
    }

    #[test]
    fn rebase_prevents_windup_overshoot_into_audio_early_vs_without_rebase() {
        // #926 fix-up finding 1/7 (the review's own windup narrative): the cooldown (30s) is far
        // shorter than the cluster window (180s), so -- WITHOUT re-basing retained samples on
        // Apply -- the window keeps reporting close to a STALE excess-lateness reading for many
        // cooldown ticks after the actuator has already moved, driving the corrector to keep
        // "correcting" an error that (from the actuator's own point of view) no longer exists and
        // overshooting PAST the target straight into the forbidden audio-early (negative) zone.
        // WITH re-basing, every retained sample is shifted the instant a correction lands, so the
        // window sees the TRUE post-correction state immediately and stops once converged.
        //
        // An effectively-infinite window isolates the rebase MECHANISM itself from the window's
        // own (separate, pre-existing) aging-out behavior -- both are fed the exact same raw
        // stream, so any difference in outcome is attributable to rebase() alone.
        fn run(rebase_on_apply: bool) -> Vec<f64> {
            let mut cluster = RollingOffsetCluster::new(
                u64::MAX,
                DOCK_CLUSTER_TOL_MS,
                DOCK_CLUSTER_MIN_MATCHED,
                DOCK_CLUSTER_MAX_MAD_MS,
            );
            let mut corrector = DockLockCorrector::dock();
            let mut t_ns: u64 = 0;
            // Preload a locked cluster at +42ms (excess audio-lateness -- allowed, but the
            // corrector actively pulls it toward the margin).
            for _ in 0..(DOCK_CLUSTER_MIN_MATCHED + 4) {
                t_ns += 3_000_000_000;
                cluster.push(t_ns, 42.0);
            }
            let mut current_delay: i32 = 950;
            let mut true_ts = 42.0_f64; // the REAL physical offset, tracked for the assertion only
            let mut trace = Vec::new();
            for _ in 0..14 {
                t_ns += (DOCK_LOCK_MIN_REAPPLY_S as u64 + 1) * 1_000_000_000; // past cooldown
                // Every tick a fresh raw +42ms sample arrives -- deliberately UNCHANGED regardless
                // of branch, isolating rebase()'s own contribution (see doc comment above).
                let est = cluster.push(t_ns, 42.0).expect("must stay locked");
                let action = corrector.decide(true, est.offset_ms, est.mad_ms, current_delay, t_ns);
                if let DockLockAction::Apply(new_delay) = action {
                    let delta = (new_delay - current_delay) as f64;
                    true_ts -= delta; // the SAME invariant relation used throughout this module
                    current_delay = new_delay;
                    if rebase_on_apply {
                        cluster.rebase(delta);
                    }
                }
                trace.push(true_ts);
            }
            trace
        }

        let without = run(false);
        let with = run(true);

        assert!(
            without.iter().any(|&v| v < 0.0),
            "sanity: reproduces the windup bug -- without rebase the real offset must overshoot \
             negative at some point: {without:?}"
        );
        assert!(
            with.iter().all(|&v| v >= -1e-9),
            "with rebase the real offset must never go audio-early: {with:?}"
        );
    }
}
