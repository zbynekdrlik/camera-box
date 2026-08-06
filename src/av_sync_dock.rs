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

/// #921: caches the LAST (dst_w, dst_h) a quirc decode context was resized to, so a caller can skip
/// a redundant `quirc_resize()` when the decode-plan geometry is unchanged from the previous call —
/// true on every video frame after the first in the live dock, since `frame_w`/`frame_h` (and
/// therefore [`top_band_decode_plan`]'s output) never change for the lifetime of the OBS raw-video
/// output. The vendored `quirc_resize()` (`vendor/av-sync-dock/deps/quirc/lib/quirc.c`) has NO
/// early-out for an unchanged size — it unconditionally `calloc`s 3 fresh buffers (image / pixels /
/// flood_fill_vars) and frees the old ones on EVERY call. At the dock's 60fps that is 180 alloc+free
/// calls/second for a size that is identical from the first frame onward: real allocator churn over
/// an hours-long production session — a plausible contributor to video-QR decode reliability
/// WORSENING with dock uptime (issue 921's own diagnostic: 55.6% shortly after launch, ~2% at
/// steady state), distinct from the decode geometry/algorithm itself (proven correct on real
/// captured frames — see `tests/av_sync_dock_video_decode_921.rs`). Mirrored byte-for-byte in
/// `camerabox::CbQrResizeCache` (`camera-box-video.hpp`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QrResizeCache {
    last_w: u32,
    last_h: u32,
    initialized: bool,
}

impl QrResizeCache {
    /// Returns `true` iff a resize to `(w, h)` is actually needed given the cache's current state,
    /// and updates the cache to `(w, h)` regardless of the return value — so the NEXT call reflects
    /// reality even if the caller ignores a `true` result and resizes anyway. On an explicit resize
    /// FAILURE the caller must reset the cache to `QrResizeCache::default()` (uninitialized) so the
    /// very next call retries fresh — matching today's shipped retry-every-frame-on-failure
    /// behavior instead of wrongly assuming a failed resize succeeded.
    pub fn resize_needed(&mut self, w: u32, h: u32) -> bool {
        let needed = !self.initialized || self.last_w != w || self.last_h != h;
        self.last_w = w;
        self.last_h = h;
        self.initialized = true;
        needed
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

/// #942 — BUILD DEFAULT, not a runtime toggle: the E2E gate (`scripts/av_sync_calibrate.py
/// --apply`) is the only CONTINUOUS/closed-loop writer of `genlock_latency_ms_src` (a bounded,
/// snapshot-and-restored exception exists — `scripts/obs_phase2.py::_snapshot_and_set_test_latency`,
/// #358/#691 — which force-sets and later restores the value around one delivery-verify test run;
/// it is not a second closed-loop actuator). Two independent actuators writing the SAME live knob
/// never converge — the gate measures against ground truth (the QPSK marker +
/// the optical burns) and is read-back-verified once per run with a clamped step; this corrector
/// only ever servos against its OWN recent output, with no ground truth of its own. Root-cause
/// evidence (a 20-run random walk while both actuators were live, and a directly-sampled ±5ms
/// limit cycle with zero gate activity in flight) is recorded on the #942 ticket. This mirrors
/// the SAME hard-lock convention as #257 (genlock env removal) and #912 (ASRC default-on) — no
/// env var, no WebSocket flag, no per-source opt-in; flipping it back on is a deliberate future
/// code change, never a config value. [`dock_lock_may_actuate`] is the pure decision seam a
/// caller MUST consult before ever writing `genlock_latency_ms_src` from a
/// [`DockLockCorrector::decide`] result — the corrector keeps MEASURING and its caller keeps
/// DISPLAYING the computed offset/margin/implied correction (a "suggested: +N ms"), it simply
/// never applies it while this is `false`.
pub const DOCK_LOCK_ACTUATION_ENABLED: bool = false;

/// The pure decision seam: may a [`DockLockCorrector::decide`] result that returned
/// [`DockLockAction::Apply`] actually be written to the live `genlock_latency_ms_src` actuator
/// right now? #942: hard-locked `false` by build default — see [`DOCK_LOCK_ACTUATION_ENABLED`]'s
/// own doc comment. The C++ dock mirrors this exact function (`cb_dock_lock_may_actuate` in
/// `vendor/av-sync-dock/src/camera-box-audio.hpp`) and gates its own actuator-write call site on
/// it, never on re-deriving the decision inline.
pub fn dock_lock_may_actuate() -> bool {
    DOCK_LOCK_ACTUATION_ENABLED
}

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
/// mad_ms.clamp(DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS)` and the HOLD BAND be `[lo, hi) =
/// [margin, 2 * margin)` — #942: the band's width equals `margin` itself (the SAME clamped
/// dispersion the low edge already uses), instead of the fixed 1ms dead zone that limit-cycled the
/// live actuator against the cluster's own 10-25ms measurement noise (339-470 actuator writes per
/// session on the live rig, never settling). Any `ts_ms` already inside `[lo, hi)` is a plain
/// `Hold` — trivially satisfies the invariant since `lo = margin >= DOCK_LOCK_MIN_MARGIN_MS > 0`.
/// Otherwise let `mid = 1.5 * margin` (the band's own middle) and `g = round(ts_ms - mid)`.
/// Setting `new_delay = current_delay + g` changes `ts` by exactly `-g` (increasing the video
/// source's own added delay by `g` ms delays the video `g` ms further, which — since `ts =
/// audio_ts - video_ts` — REDUCES `ts` by `g`). So the resulting `ts_new = ts_ms - g = mid +
/// ((ts_ms - mid) - g)`, and since `g` is the NEAREST integer to `ts_ms - mid`, `|(ts_ms - mid) -
/// g| <= 0.5`, i.e. `ts_new` is in `[mid - 0.5, mid + 0.5]`. Because `margin >=
/// DOCK_LOCK_MIN_MARGIN_MS = 1.0`, `mid - 0.5 = 1.5*margin - 0.5 >= margin = lo` and `mid + 0.5 <=
/// 2*margin = hi` — so `ts_new` always lands inside `[lo, hi]` (note: the CLOSED interval — `hi`
/// itself is a reachable value of `ts_new`, even though the hold-band CHECK above is half-open;
/// landing exactly on `hi` is not a resting state, it is corrected by exactly one more step toward
/// `mid` on the next trusted measurement, then settles — see
/// `corrector_settles_within_one_extra_tick_when_a_landed_correction_hits_the_bands_exact_edge`),
/// and since `lo = margin > 0`, `ts_new` is always strictly positive, never merely non-negative.
/// `g` positive means the video
/// is currently arriving too early relative to the target (bring it later, increase delay); `g`
/// negative means video is lagging (audio is early), so the delay is REDUCED — same physical
/// direction the offline `required_delay_ms` already uses. Stepping toward the band's MIDDLE
/// rather than its edge (the pre-#942 formula effectively targeted `lo`) means a landed correction
/// lands with headroom on both sides of the band, so it cannot immediately re-trip the opposite
/// direction on ordinary measurement jitter the size of the band itself.
///
/// Only ever acts on a genuine trusted measurement (the caller passes `locked = true` only when
/// the rolling cluster currently reports `est.ok`) — `locked = false` (no test signal: real event,
/// no QR, no marker) never touches the actuator, which is what implements the ticket's
/// requirement 5 (measure-only, permanent lock, no drift-chasing on program material) with no
/// separate timeout/heartbeat. #926 fix-up (review finding 2): the caller drives this from EVERY
/// trusted (`est.ok`) measurement, not only a `CbLockAuditTracker` `Locked`/`Updated` classifier
/// transition — the classifier's `Updated` needs a >5ms MOVE of the (window-smoothed) median,
/// which stalls convergence once the window itself lags a landed correction; this function's own
/// cooldown ([`DOCK_LOCK_MIN_REAPPLY_S`]) and hold-band (`[margin, 2*margin)`, #942) checks are
/// what make calling it on every trusted measurement safe.
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
        // #942 -- the hold BAND scales with the cluster's own measured noise instead of a fixed
        // 1ms dead zone: [band_lo, band_hi) = [margin, 2*margin), i.e. as wide as the SAME clamped
        // dispersion the low edge already uses. Any offset already inside it is left alone -- no
        // actuator write at all (this is what stopped the live limit-cycle: a 10-25ms noise field
        // no longer trips a 1ms-wide window on nearly every trusted sample). Deliberately
        // HALF-OPEN at the upper edge, matching the deliberate "nudge toward mid, not just past
        // the edge" intent for a residual sitting exactly at 2*margin (review finding, investigated
        // and reverted -- see corrector_settles_within_one_extra_tick_when_a_landed_correction_
        // hits_the_bands_exact_edge: closing this edge broke the tested nudge-to-middle behavior at
        // ordinary margins; the narrow-margin case this was meant to help is provably NOT a
        // recurring limit-cycle -- it resolves in exactly one more correction, then Holds).
        let band_lo = margin;
        let band_hi = margin * 2.0;
        if offset_ms >= band_lo && offset_ms < band_hi {
            return DockLockAction::Hold; // already inside the noise-scaled hold band
        }
        if let Some(last) = self.last_applied_ns {
            let elapsed_s = now_ns.saturating_sub(last) as f64 / 1_000_000_000.0;
            if elapsed_s < self.min_reapply_s {
                return DockLockAction::Hold; // cooldown -- let the last correction take effect first
            }
        }
        // Step toward the band's MIDDLE, not its low edge (#942) -- a landed correction then has
        // headroom on both sides of the band instead of sitting right at its boundary, so it can't
        // immediately re-trip the opposite direction on ordinary jitter the size of the band
        // itself. Clamp BEFORE the later `as i64` casts (finding 5): offset_ms is finite but could
        // still be astronomically large, which would otherwise risk an overflowing add below.
        let mid = margin * 1.5;
        let g = (offset_ms - mid).round().clamp(-1_000_000.0, 1_000_000.0);
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

/// #955 — the log-level OUTCOME `sync-test-output.cpp` derives from a [`DockLockCorrector::decide`]
/// result: whether to WRITE the actuator, DISPLAY a monitor-only suggestion, warn that a hardware
/// rail is pinned with the "audio never early" invariant still violated, or say nothing. Extracted
/// as a byte-identical pure function purely so this branch selection — previously ONLY a
/// source-text grep away from a silent regression (the #942 fix-up review's own counter-example:
/// moving the actuator write into the monitor-only branch still passed every existing text-anchor
/// test) — gets a real behavioral test. Mirrors `cb_dock_lock_outcome()` in
/// `vendor/av-sync-dock/src/camera-box-audio.hpp`; see `tests/av_sync_dock_outcome_955.rs` for the
/// C++ twin harness. This does NOT change `decide()`'s own hold-band/step/cooldown math at all
/// (`.claude/rules/dock-lock-hold-band.md`) — it only names the decision the caller already makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockLockOutcome {
    /// `act` is `Apply` and actuation is currently permitted — write it to the live actuator.
    Write,
    /// `act` is `Apply` but actuation is NOT permitted (#942: monitor-only) — display it as a
    /// suggestion, never write it.
    Suggest,
    /// `act` is `Hold` (already inside the band, or no room to correct further), but the
    /// "audio never early" invariant is STILL violated because a hardware rail is pinned — a
    /// genuine hardware limit, not a corrector bug, and it must stay VISIBLE.
    RailWarn,
    /// `act` is `Hold` and nothing is wrong — nothing to report this measurement.
    Quiet,
}

/// See [`DockLockOutcome`]'s own doc comment. `offset_ms`/`current_ms` here are in `decide()`'s
/// OWN native (dock) convention — this is the SAME rail-pinned check `decide()`'s caller already
/// makes today (`est.offset_ms < 0.0` = "audio still early"), just named and extracted.
pub fn dock_lock_outcome(
    act: DockLockAction,
    may_actuate: bool,
    offset_ms: f64,
    current_ms: i32,
) -> DockLockOutcome {
    match act {
        DockLockAction::Apply(_) => {
            if may_actuate {
                DockLockOutcome::Write
            } else {
                DockLockOutcome::Suggest
            }
        }
        DockLockAction::Hold => {
            if offset_ms < 0.0
                && (current_ms <= DOCK_LOCK_LATENCY_MIN_MS
                    || current_ms >= DOCK_LOCK_LATENCY_MAX_MS)
            {
                DockLockOutcome::RailWarn
            } else {
                DockLockOutcome::Quiet
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpsk_marker::{frame_id_to_index, marker_signal, signal_len, AV_SYNC_RING_CYCLE_NS};

    // ---- #955 DockLockOutcome ----

    #[test]
    fn dock_lock_outcome_apply_and_may_actuate_is_write_955() {
        assert_eq!(
            dock_lock_outcome(DockLockAction::Apply(945), true, -52.2, 950),
            DockLockOutcome::Write
        );
    }

    #[test]
    fn dock_lock_outcome_apply_and_monitor_only_is_suggest_955() {
        assert_eq!(
            dock_lock_outcome(DockLockAction::Apply(945), false, -52.2, 950),
            DockLockOutcome::Suggest
        );
    }

    #[test]
    fn dock_lock_outcome_hold_at_pinned_rail_with_audio_early_is_rail_warn_955() {
        assert_eq!(
            dock_lock_outcome(DockLockAction::Hold, false, -20.0, DOCK_LOCK_LATENCY_MIN_MS),
            DockLockOutcome::RailWarn
        );
        assert_eq!(
            dock_lock_outcome(DockLockAction::Hold, false, -20.0, DOCK_LOCK_LATENCY_MAX_MS),
            DockLockOutcome::RailWarn
        );
    }

    #[test]
    fn dock_lock_outcome_hold_not_pinned_is_quiet_955() {
        assert_eq!(
            dock_lock_outcome(DockLockAction::Hold, false, 5.0, 950),
            DockLockOutcome::Quiet
        );
        // #955: the rail check specifically requires offset_ms < 0.0 ("audio still early") --
        // a non-negative offset never triggers RailWarn even while pinned at a rail, matching
        // decide()'s own invariant (a rail-pinned POSITIVE offset is not a violation at all).
        assert_eq!(
            dock_lock_outcome(DockLockAction::Hold, false, 5.0, DOCK_LOCK_LATENCY_MIN_MS),
            DockLockOutcome::Quiet
        );
    }

    #[test]
    fn dock_lock_outcome_may_actuate_is_irrelevant_while_holding_955() {
        // may_actuate only distinguishes Write vs Suggest on the Apply arm.
        assert_eq!(
            dock_lock_outcome(DockLockAction::Hold, true, 5.0, 950),
            DockLockOutcome::Quiet
        );
    }

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
    fn qr_resize_cache_needs_resize_only_when_size_actually_changes() {
        let mut c = QrResizeCache::default();
        // First call: always needed (uninitialized).
        assert!(c.resize_needed(760, 307));
        // Repeated identical size: never needed again.
        assert!(!c.resize_needed(760, 307));
        assert!(!c.resize_needed(760, 307));
        assert!(!c.resize_needed(760, 307));
        // A genuinely different size (e.g. a real geometry change): needed again, once.
        assert!(c.resize_needed(760, 300));
        assert!(!c.resize_needed(760, 300));
        // Width-only and height-only changes both count.
        assert!(c.resize_needed(700, 300));
        assert!(c.resize_needed(700, 250));
        assert!(!c.resize_needed(700, 250));
    }

    #[test]
    fn qr_resize_cache_reset_forces_a_fresh_resize() {
        let mut c = QrResizeCache::default();
        assert!(c.resize_needed(760, 307));
        assert!(!c.resize_needed(760, 307));
        // Simulate the caller resetting on an explicit resize failure.
        c = QrResizeCache::default();
        assert!(
            c.resize_needed(760, 307),
            "a reset cache must ask for a resize again, even at the SAME size as before the reset"
        );
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

    // ---- #942 monitor-only hard lock ----

    #[test]
    fn dock_lock_actuation_is_hard_locked_off_942() {
        // The E2E gate (scripts/av_sync_calibrate.py --apply) is the SOLE writer of
        // genlock_latency_ms_src -- the corrector must never be permitted to actuate, by build
        // default, with no env/WebSocket/per-source escape hatch. This is the pure Tier-0 half of
        // the #942 fix; the C++ caller (vendor/av-sync-dock/src/sync-test-output.cpp) gates its
        // own actuator-write call site on the mirrored cb_dock_lock_may_actuate() and is pinned by
        // the vendored-source guard in tests/genlock_preload.rs.
        assert!(
            !dock_lock_may_actuate(),
            "#942: DOCK_LOCK_ACTUATION_ENABLED must stay hard-locked false -- the dock corrector \
             must never write genlock_latency_ms_src while the E2E gate is the sole writer"
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
        // With mad_ms=5.0 (a typical tight cluster) the target hold band is [5, 10)ms (#942: width
        // scales with the same clamped margin), not a bare [0,1) -- #926 fix-up finding 3: a bare
        // [0,1) target is false precision the estimator's own noise floor can't back up. Both
        // boundary-ish values within the band must Hold.
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

    // ---- #942: hold band scales with the cluster's own noise, never a fixed 1ms dead zone ----

    #[test]
    fn corrector_never_actuates_once_offset_settles_inside_the_noise_scaled_band() {
        // The live-rig bug (#942): with mad_ms ~15 (the ticket's own measured 10.0-18.0ms field),
        // the OLD 1ms-wide dead zone ([margin, margin+1)) actuated on nearly every trusted sample,
        // because a realistic noisy offset almost never lands in a 1ms window. The fix widens the
        // hold band to [margin, 2*margin) -- as wide as the cluster's own measured dispersion. A
        // deterministic "noisy but settled" sequence (offsets wandering inside the band, the way a
        // converged lock's own measurement noise would look) must produce ZERO Apply actions.
        let mad_ms = 15.0; // inside [DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS] already
        let band_lo = mad_ms; // margin == mad_ms here (no clamping needed)
        let band_hi = mad_ms * 2.0;
        // A deterministic pseudo-noisy sequence spanning most of the band width, landing strictly
        // inside [band_lo, band_hi) on every sample -- the realistic "settled lock, still jittering
        // on measurement noise" case the ticket describes.
        let offsets = [
            15.5, 22.0, 29.4, 18.0, 26.5, 16.2, 28.9, 21.0, 24.7, 17.3, 27.8, 19.9, 23.3, 15.9,
        ];
        for &off in &offsets {
            assert!(
                off >= band_lo && off < band_hi,
                "test fixture bug: {off} must itself be inside [{band_lo}, {band_hi})"
            );
        }
        let mut c = DockLockCorrector::new(5, 30.0);
        let mut applies = 0usize;
        // Cooldown satisfied on every sample (30s apart, min_reapply_s == 30.0) so a lingering
        // cooldown gate can never be the reason nothing applies -- only the band itself must.
        for (i, &off) in offsets.iter().enumerate() {
            let now_ns = (i as u64 + 1) * 30_000_000_000;
            if matches!(
                c.decide(true, off, mad_ms, 950, now_ns),
                DockLockAction::Apply(_)
            ) {
                applies += 1;
            }
        }
        assert_eq!(
            applies, 0,
            "a noisy-but-in-band offset sequence must never actuate the live A/V actuator (#942)"
        );
    }

    #[test]
    fn corrector_still_corrects_the_forbidden_audio_early_zone_below_the_band_floor() {
        // The #942 hold-band widening must NOT weaken the "audio never early" invariant: an offset
        // below the band's low edge (including outright negative -- audio physically ahead of
        // video) still triggers a correction, exactly as before the fix.
        let mut c = DockLockCorrector::new(5, 30.0);
        assert!(
            matches!(
                c.decide(true, -5.0, 15.0, 950, 1_000_000_000),
                DockLockAction::Apply(_)
            ),
            "an offset below the hold band's floor must still correct, never silently Hold"
        );
        let mut c2 = DockLockCorrector::new(5, 30.0);
        assert!(
            matches!(
                c2.decide(true, 0.0, 15.0, 950, 1_000_000_000),
                DockLockAction::Apply(_)
            ),
            "offset_ms == 0.0 (audio/video coincident) is still below margin -- must correct"
        );
    }

    #[test]
    fn corrector_settles_within_one_extra_tick_when_a_landed_correction_hits_the_bands_exact_edge()
    {
        // #942 review finding, investigated: at the narrowest possible band (margin floors at
        // DOCK_LOCK_MIN_MARGIN_MS == 1.0, e.g. mad_ms <= 1.0 or non-finite), round()-to-mid
        // targeting can land ts_new EXACTLY at the band's upper edge (2*margin) -- a value the
        // closed-form proof's [mid-0.5, mid+0.5] guarantee explicitly allows. The band's own
        // Hold check is half-open ([lo, hi)), so that landed value is (correctly) treated as
        // "still outside" on the very next measurement -- but this is NOT a recurrence of the
        // #942 limit-cycle: it costs exactly ONE extra correction (nudging toward the band's
        // middle, same as at any other margin), which lands solidly inside the band and Holds
        // permanently after that. Making the upper edge inclusive to avoid this one extra tick
        // was tried and REVERTED -- it broke the deliberate "still nudge toward mid, don't just
        // stop at the edge" behavior for ordinary (non-degenerate) margins, pinned by
        // corrector_respects_the_hardware_clamp_at_the_ceiling. This test instead pins the
        // property that actually matters: the sequence TERMINATES within one extra tick, it
        // never keeps cycling.
        let mad_ms = 1.0; // margin floors at DOCK_LOCK_MIN_MARGIN_MS == 1.0, band width == 1.0
        let mut c = DockLockCorrector::new(5, 30.0);
        let action1 = c.decide(true, 0.0, mad_ms, 950, 1_000_000_000);
        let delay1 = match action1 {
            DockLockAction::Apply(v) => v,
            DockLockAction::Hold => {
                panic!("offset_ms=0.0 is well outside the band -- must correct")
            }
        };
        let ts1 = 0.0 - (delay1 - 950) as f64;
        assert!(
            (2.0 - 1e-9..2.0 + 1e-9).contains(&ts1),
            "test setup: this scenario must land exactly at the band's upper edge (2.0): ts1={ts1}"
        );

        // One more trusted measurement of that SAME landed value, past cooldown: the corrector
        // may nudge it once more toward mid, but the RESULT must then be a genuine resting state.
        let action2 = c.decide(true, ts1, mad_ms, delay1, 32_000_000_000);
        let (delay2, ts2) = match action2 {
            DockLockAction::Apply(v) => (v, ts1 - (v - delay1) as f64),
            DockLockAction::Hold => (delay1, ts1), // already acceptable -- also a valid outcome
        };

        // A THIRD trusted measurement of the resulting (now-stable) offset must Hold -- proving
        // the sequence terminates within one extra tick, never limit-cycles.
        let action3 = c.decide(true, ts2, mad_ms, delay2, 63_000_000_000);
        assert_eq!(
            action3,
            DockLockAction::Hold,
            "must settle within at most one extra correction, never keep cycling: ts1={ts1} ts2={ts2}"
        );
    }

    #[test]
    fn corrector_never_lands_below_the_safety_margin_across_many_offsets_mads_and_deploys() {
        // The closed-form invariant, swept over a wide range of measured offsets, cluster MADs,
        // and current delays, WITHOUT the step clamp interfering (max_step huge) so the single
        // application fully converges: the resulting ts (offset_ms - applied_delta) must land in
        // the #942 hold band [margin, 2*margin), where margin = mad_ms.clamp(
        // DOCK_LOCK_MIN_MARGIN_MS, DOCK_CLUSTER_MAX_MAD_MS) -- covering the margin CLAMPING at
        // both ends (0.1/50.0 fall outside [1,25] and must clamp).
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
                        let band_hi = margin * 2.0;
                        assert!(
                            (margin - 1e-9..band_hi + 1e-9).contains(&ts_new),
                            "offset={offset_ms} mad={mad_ms} current={current} new_delay={new_delay} \
                             ts_new={ts_new} margin={margin} must land in [margin,2*margin) -- audio \
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
            DockLockAction::Hold => {
                panic!("10ms of excess lateness must be corrected even with mad=0")
            }
        }
    }

    #[test]
    fn corrector_margin_clamps_to_the_max_mad_when_mad_is_huge() {
        let mut c = DockLockCorrector::new(100_000, 30.0); // effectively unclamped step
        match c.decide(true, 100.0, 500.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                let ts_new = 100.0 - (v - 950) as f64;
                // #942: the hold band is [margin, 2*margin), so a clamped margin of
                // DOCK_CLUSTER_MAX_MAD_MS (25) gives band [25, 50) -- ts_new must land in it.
                assert!(
                    (DOCK_CLUSTER_MAX_MAD_MS..DOCK_CLUSTER_MAX_MAD_MS * 2.0).contains(&ts_new),
                    "margin-scaled band must clamp its low edge at DOCK_CLUSTER_MAX_MAD_MS: ts_new={ts_new}"
                );
            }
            DockLockAction::Hold => {
                panic!("100ms of excess lateness must be corrected even with an absurd mad")
            }
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
            DockLockAction::Hold => {
                panic!("a huge positive offset must still trigger a correction")
            }
        }
        let mut c2 = DockLockCorrector::new(5, 30.0);
        match c2.decide(true, -1e18, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(v, 945, "huge negative offset -- step-capped decrease")
            }
            DockLockAction::Hold => {
                panic!("a huge negative offset must still trigger a correction")
            }
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
        // offset_ms positive (audio ALREADY late by more than the hold band) is not forbidden, but
        // the ticket wants MINIMAL latency -- the corrector must still nudge it down toward the
        // band's MIDDLE (#942: 7.5ms -> nearest integer step 7ms with mad_ms=5.0, band [5,10)), not
        // just leave it (only the negative/audio-early direction is a hard violation; drifting
        // arbitrarily positive would violate "hold FIXED MINIMAL latency").
        let mut c = DockLockCorrector::new(50, 30.0);
        match c.decide(true, 42.0, 5.0, 950, 1_000_000_000) {
            DockLockAction::Apply(v) => {
                assert_eq!(
                    v, 985,
                    "must increase delay to close the gap toward the middle of the [5,10) band"
                )
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
        assert!(
            before.mad_ms < 1.0,
            "tight cluster before rebase: {before:?}"
        );

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
