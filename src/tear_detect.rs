//! Projection-tap scanout-TEAR detector (issue 781) — PURE, report-only.
//!
//! ## What it measures
//!
//! cam2's USB grabber is fed by imag-nb's HDMI output (owner-confirmed 2026-08-24), so cam2's leg
//! in the all-cambox E2E sweep already captures the physical projection path (imag render → DRM
//! scanout → HDMI → grabber) — "what the audience sees". This module formalizes the tear check the
//! ticket asks for: a captured frame that carries HALVES of two DIFFERENT consecutive painted ticks
//! (top half tick N, bottom half tick N+1) is a scanout TEAR event.
//!
//! ## The signal, derived from the REAL painted content (not geometry-only)
//!
//! The painted source is cam2's optical **dual-QR Vernier**: the LEFT QR carries the latest EVEN
//! tick, the RIGHT the latest ODD tick (`probe::recording_latency::split_payloads`,
//! `RecordingFrame::tick`). A HEALTHY captured frame therefore decodes exactly two cam2-optical
//! payloads whose `frame_id`s are adjacent — `max(frame_id) - min(frame_id) == 1`
//! ([`VERNIER_MAX_SPREAD`]). A frame that captured TWO distinct paint GENERATIONS (a scanout tear
//! straddling a page-flip) carries a WIDER optical span — `max - min > VERNIER_MAX_SPREAD`. That
//! wider span IS the ticket's "two different consecutive painted ticks in one frame", generalized
//! correctly to the Vernier's even/odd pair. Node digital burns (`probe::recording::NODE_BURN_RUN_IDS`)
//! are NOT the optical Vernier and MUST be excluded by the caller before the ids reach this module.
//!
//! ## Report-only, and WHY (a proven-blind signal on the CURRENT content)
//!
//! Measured across 5 real `stream-partial-*.json` (~48 000 frames), the per-frame optical span is
//! exclusively {0,1} and the optical-QR count per frame never exceeds 2 — the "two generations in
//! one frame" signal NEVER fires on the current content. The reason is structural (confirmed by
//! reading real captured frames): both dual-QR halves sit in ONE vertical band (top ~60%), so a
//! horizontal scanout tear crossing that band corrupts BOTH QRs at the same height → the frame goes
//! `undecodable` (tick=None) rather than yielding two clean generations. A tear cannot manufacture a
//! second, older/newer generation of a QR that exists at only one vertical position. So an all-zero
//! `tear_fraction` on this content means EITHER "no tears occurred" (e.g. post the issue-1107
//! render-side fix) OR "the signal is blind here" — the two are indistinguishable without a
//! known-torn run. Per the "a gate that can never fire is worse than no gate" doctrine (issue
//! 1101/1088), this module is REPORT-ONLY ([`gates_overall_pass`] returns `false`) and carries a
//! computed [`TearSignalViability`] so an all-zero reading can never be mistaken for a promotable
//! green.
//!
//! ## v2 (issue 1196) — the aux Vernier tick pair makes the signal VIABLE
//!
//! The vertical tick redundancy the paragraph above calls for now exists: the painter additionally
//! blits a small aux QR pair into the bottom burn-free gaps (`crate::aux_tick` geometry; left =
//! latest EVEN tick, right = latest ODD tick, reserved `AUX_TICK_RUN_ID`, `gen_ts_ns = 0`). A
//! horizontal seam between the primary band and the aux band now yields a clean generation in EACH
//! band, so the v2 detector computes the tear span over the UNION of `(primary_ids, aux_ids)`
//! ([`frame_union_spread`]). Two report-only companion fields gate the future promotion honestly:
//! [`TearStats::aux_decode_fraction`] (did the small aux marks actually survive the lossy chain?)
//! and [`TearStats::primary_dark_aux_alive_fraction`] (a seam INSIDE the primary band corrupts both
//! primary halves while both aux marks decode — band-localized corruption vs whole-frame blur).
//!
//! ## v2.1 (issue 1196) — MULTI-TILE SAFE: only SINGLE-SOURCE frames are scored for tear
//!
//! The first real rig run after the painter redeploy (E2E 1859005342, verdict + real frames in
//! `~/.claude/work-products/1196-fixture/`; ticket comment 5415952812) exposed a false-positive
//! the plain-union v2 could not see: the recorded program is **MULTI-TILE** — an ALL_CAMBOX
//! composition that carries TWO grabber-path tiles of the SAME painted cam2 monitor (plus
//! production scenes). One recorded frame therefore decodes the primary dual-QR from BOTH capture
//! paths, offset by ~2-4 ticks of inter-path latency, so ~99% of frames carry 3-4 primary optical
//! QRs and the plain union span reads 2-4 constantly (`tear_fraction ~0.99`, `max_spread ~4`, one
//! window 14). That span is **inter-path temporal SKEW, not a scanout tear** — v2 mismeasured it.
//!
//! The physical fact v2.1 keys on: **one tile's dual-QR band produces AT MOST 2 optical QRs**
//! (a LEFT even + a RIGHT odd). A single band cannot yield 3+ clean generations — a horizontal
//! tear through it corrupts, it does not multiply (the same "single vertical band" structural fact
//! the blindness paragraph above rests on). So a frame carrying **≥ 3** primary optical ids MUST
//! have been composited from **≥ 2** capture paths/tiles: `frame_cluster_count(primary) =
//! ceil(count/2)` is the inferred number of tiles. Without pixel positions in the partial JSON
//! (schema v6 payloads carry only `run_id`/`frame_id`/`gen_ts_ns`, no QR centre/bbox — see the
//! follow-up below), the individual ids CANNOT be attributed back to their tile, so a multi-source
//! frame is **unscorable for tear**: [`is_multi_path_suspect`] flags it, it is EXCLUDED from
//! `decodable_frames`/`tear_frames`, and it is counted in [`TearStats::multi_path_suspect_frames`]
//! / [`TearStats::multi_path_suspect_fraction`]. Only SINGLE-SOURCE frames (≤ 2 primary ids = one
//! tile) are scored, so a genuine single-cluster tear — 2 ids spanning > 1, e.g. `{100, 102}`, or a
//! cross-band primary∪aux split — still fires ([`is_torn_frame`]). On the real 1859005342 window
//! this reads `multi_path_suspect_fraction ~0.998`, `tear_frames 0`, `viability Unproven` — the
//! honest "this window is multi-tile; tear is unscoreable here" verdict, replacing v2's false 0.99.
//!
//! **Honest limitation of the position-free fallback:** a genuine tear that lands INSIDE a
//! multi-source frame is not separable from inter-path skew from the id multiset alone (`{100,101,
//! 102,103}` from a real single-tile 2-generation tear is byte-identical to two tiles offset by 2),
//! so such frames are conservatively marked suspect rather than torn. And two tiles that each
//! decode only ONE half (a count-2 frame whose ids span > 1 — 8 of 9690 real frames) still read as
//! a single-source "tear"; that residual is the honest cost of no positions, dwarfed by the
//! ~0.998 suspect fraction that flags the window as untrustworthy for tear scoring anyway. The
//! **complete** fix — geometric per-cluster scoping (group decoded QRs by pixel position, compute
//! the span WITHIN each tile) — needs the QR centre/bbox carried on each payload, i.e. a partial
//! schema bump + a decode-side position capture + a fleet redeploy. That is the named follow-up
//! design on issue 1196 (Approach: "positions available end-to-end → v2.1 per-cluster union"),
//! deliberately OUT of this sub-step's scope.
//!
//! ## Precondition for a LIVE gate (still report-only) — and the per-leg aux correction (issue 1196)
//!
//! [`gates_overall_pass`] stays `false` until a [`TEAR_FRACTION_CEILING`] is calibrated from a
//! known-torn run's torn distribution that SEPARATES the induced tear from the green background
//! (below), AND [`signal_promotable`] holds on that run. The machine-checked flip-readiness is
//! [`window_promotable`] / [`signal_promotable`] (viability `Observed` + single-tile
//! `multi_path_suspect_fraction <= MULTI_PATH_SUSPECT_CEILING`), mirroring
//! `dup_cadence::signal_promotable` (`verdict-gate-seam-calibration.md` §12). `signal_promotable` is
//! NECESSARY but NOT SUFFICIENT — see the background-rate caveat below.
//!
//! **The aux-coverage-floor precondition was found MIS-SHAPED for the projection leg (real-data
//! correction, 2026-09-01, mined across 44 verdicts).** The CAM2 PROJECTION leg — the leg whose
//! grabber captures imag's HDMI scanout, the point of this gate — reads `aux_decode_fraction` = 0.0
//! in EVERY window (the ~210px aux QRs are not present/decodable in imag's projected scanout), so a
//! CAM2 tear surfaces via the PRIMARY band, not the aux cross-band; the aux marks decode only on the
//! SPLITTER legs (CAM1/CAM3/CAM6/CAM7, up to ~0.99), which are not the projector-scanout path. So an
//! aux-coverage FLOOR is NOT the projection-leg promotion gate — it would permanently block that
//! leg. `aux_decode_fraction` stays a report-only per-leg DIAGNOSTIC; promotion is gated on
//! `signal_promotable` (which requires `Observed`, so it is fail-closed regardless of which band is
//! operative) PLUS the calibrated tear ceiling.
//!
//! **A LOW background of `Observed` single-tile tears exists on GREEN runs — the ceiling can never
//! be 0.0.** Mined: v2.1 `observed` single-tile windows occur on BOTH CAM2 (14 windows) and CAM3 (2
//! windows) across routine runs, `tear_fraction` ~0.00118–0.00355 (1–3 frames/window), so
//! `signal_promotable` reads `true` on ~12 of 32 v2.1-scored routine runs — it is NOT by itself
//! evidence of a known-torn run. The known-torn run's value is a HIGH `tear_fraction` well above
//! this ~0.004 background; `TEAR_FRACTION_CEILING` must be calibrated ABOVE the background and BELOW
//! the induced distribution (a per-window RATE; a genuine tear can be a single frame, so a run-wide
//! COUNT term may also be warranted — `verdict-gate-seam-calibration.md` §4). Which band actually
//! fires under a REAL induced projection tear on CAM2 is resolved by that run, not assumed here.
//! Also corrected: the current green content is SINGLE-TILE (`multi_path_suspect_fraction` 0.0 across
//! 90 green windows), superseding the earlier "current multi-tile rig, promotion impossible" note —
//! promotion IS possible on the current content once the torn datapoint calibrates the ceiling. The
//! flip itself is one line, out of this change's scope.
//!
//! Mirrors the crate-root `gates_overall_pass()` seam pattern shared by `presentation_cadence` /
//! `optical_floor` / `e2e_latency_gate` / `imag_leg_gate`: PURE (default features, Tier-0
//! unit-testable); the probe-gated `recording-verdict.rs` consumer only feeds it the per-frame
//! optical ids and folds the report-only verdict.

use serde::Serialize;

/// The by-design optical span of ONE healthy captured frame: the dual-QR Vernier's LEFT (latest
/// even) and RIGHT (latest odd) halves differ by exactly one tick, so `max(frame_id) - min(frame_id)
/// == 1`. A wider span means the frame captured >= 2 distinct paint generations — a scanout tear.
pub const VERNIER_MAX_SPREAD: u32 = 1;

/// Provisional report-only ceiling for [`tear_gate_pass`]. This module is report-only
/// ([`gates_overall_pass`] returns `false`), so this value does NOT gate today; it is `0.0` as a
/// placeholder. RECALIBRATE from a real known-torn run's distribution (per
/// `verdict-gate-seam-calibration.md`) before any LIVE flip.
pub const TEAR_FRACTION_CEILING: f64 = 0.0;

/// issue 1196 — the highest [`TearStats::multi_path_suspect_fraction`] a window may carry and still
/// be trusted for tear scoring (a promotion guard, per `verdict-gate-seam-calibration.md` §12 and
/// the projection-tap rule's precondition 3). Above this the recorded scene is MULTI-TILE (a frame
/// carries the SAME painted monitor from >= 2 grabber paths), so the union span measures inter-path
/// skew, not a scanout tear, and the window is UNSCOREABLE without pixel positions — it must never
/// be promoted. Calibrated from the real distribution: across 90 green windows the suspect fraction
/// is EXACTLY 0.0 (single-tile content), while a multi-tile window reads ~0.998 — a ~10x margin at
/// this ceiling passes every green run and blocks any genuinely multi-tile window. Used only by the
/// promotion property ([`window_promotable`] / [`signal_promotable`]); it does NOT gate today
/// ([`gates_overall_pass`] returns `false`).
pub const MULTI_PATH_SUSPECT_CEILING: f64 = 0.10;

/// The optical `frame_id` span within ONE band of a captured frame — `max - min` over the given
/// payload ids (node burns already excluded by the caller). `None` when the band carries no
/// payload at all (an undecodable band — counted elsewhere as `undecodable`, never a tear).
pub fn frame_optical_spread(optical_ids: &[u32]) -> Option<u32> {
    let min = *optical_ids.iter().min()?;
    let max = *optical_ids.iter().max()?;
    Some(max - min)
}

/// issue 1196 (v2) — the `frame_id` span over the UNION of the primary dual-QR band's ids and the
/// bottom aux tick pair's ids. This is what makes a seam BETWEEN the two bands detectable: the
/// primary pair reads gen G+1 while the aux pair still reads gen G — neither band alone spans
/// more than the Vernier adjacency, but the union does. `None` when NEITHER band decoded.
pub fn frame_union_spread(primary_ids: &[u32], aux_ids: &[u32]) -> Option<u32> {
    let min = *primary_ids.iter().chain(aux_ids).min()?;
    let max = *primary_ids.iter().chain(aux_ids).max()?;
    Some(max - min)
}

/// issue 1196 (v2.1) — the number of SPATIAL CLUSTERS (capture paths / composited tiles) inferred
/// from a frame's PRIMARY optical id count. One tile's dual-QR band produces AT MOST two optical
/// QRs (a LEFT even + a RIGHT odd), so `ceil(count / 2)` is the minimum number of independent tiles
/// that could have produced these ids. Positions are not carried in the partial (schema v6 payloads
/// have no QR centre/bbox), so this count is the only cluster signal available; the true geometric
/// per-cluster grouping is the named follow-up (see the module doc). `0` for an undecodable frame.
pub fn frame_cluster_count(primary_ids: &[u32]) -> u32 {
    (primary_ids.len() as u32).div_ceil(2)
}

/// issue 1196 (v2.1) — a frame is MULTI-PATH SUSPECT when EITHER band carries more optical QRs than
/// ONE tile can produce (>= 3 ids => [`frame_cluster_count`] >= 2). One tile's dual-QR yields at
/// most 2 primary QRs (left even + right odd) AND one tile's aux pair yields at most 2 aux QRs, so
/// at least 3 in either band means the frame was composited from >= 2 capture paths of the SAME painted
/// monitor. Its union span then measures inter-path temporal SKEW, not a scanout tear — and without
/// pixel positions the ids cannot be attributed to a tile, so the frame is UNSCORABLE for tear
/// (excluded from the tear count, surfaced via [`TearStats::multi_path_suspect_fraction`]). The aux
/// arm keeps the guard symmetric across bands (a review-hardening — inert while `aux_decode_fraction`
/// is 0 on the real rig, but the physical "one band, at most 2 QRs" premise applies to aux too).
pub fn is_multi_path_suspect(primary_ids: &[u32], aux_ids: &[u32]) -> bool {
    frame_cluster_count(primary_ids) >= 2 || frame_cluster_count(aux_ids) >= 2
}

/// A captured frame is TORN when it is SINGLE-SOURCE (NOT [`is_multi_path_suspect`] — one tile's
/// worth of dual-QR in each band) AND the UNION of its primary dual-QR and aux tick-pair `frame_id`s
/// spans more than the by-design even/odd adjacency ([`VERNIER_MAX_SPREAD`]) — i.e. one tile captured
/// at least 2 distinct paint generations (issue 781 within one band; issue 1196 across the primary/aux
/// bands). A multi-source (multi-tile) frame is NEVER torn: its wide span is inter-path skew, not a
/// tear (issue 1196 v2.1) — scoping it requires per-cluster pixel positions the payloads do not carry.
pub fn is_torn_frame(primary_ids: &[u32], aux_ids: &[u32]) -> bool {
    !is_multi_path_suspect(primary_ids, aux_ids)
        && frame_union_spread(primary_ids, aux_ids).is_some_and(|s| s > VERNIER_MAX_SPREAD)
}

/// Whether the tear signal has DEMONSTRABLY fired on the analyzed data — the machine-checked
/// promotion-readiness property (mirrors `dup_cadence`'s viability classifier, issue 1101).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TearSignalViability {
    /// at least 1 torn frame observed: the signal provably CAN fire on this content/run.
    Observed,
    /// No torn frame observed: cannot distinguish "no tears" from "signal blind on this content"
    /// (the single-vertical-band dual-QR layout, issue 781; or a multi-tile window where every
    /// frame is [`is_multi_path_suspect`] and thus unscoreable, issue 1196 v2.1). A LIVE flip
    /// stays gated on `Observed`.
    Unproven,
}

/// Per-window tear report (report-only). Derives only `PartialEq` (not `Eq`) — the fractions are
/// `f64` (the #726 Eq-on-f64 trap).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TearStats {
    /// EVERY frame attributed to this window, including fully undecodable ones — the denominator
    /// for the aux-coverage and multi-path-suspect fractions (issue 1196: a whole-frame blur kills
    /// the aux marks too and must lower coverage honestly, so coverage is judged against ALL
    /// captured frames).
    pub total_frames: u32,
    /// In-window SINGLE-SOURCE frames whose primary-or-aux UNION carried at least one payload —
    /// the tear denominator. Fully undecodable frames AND multi-source (multi-tile) frames are
    /// excluded: a tear is scored only on a frame that decoded a tick AND presents as one tile
    /// (issue 1196 v2.1 — multi-source frames are unscoreable without pixel positions).
    pub decodable_frames: u32,
    /// SINGLE-SOURCE frames whose UNION span exceeded [`VERNIER_MAX_SPREAD`] (>= 2 paint
    /// generations captured in one tile).
    pub tear_frames: u32,
    /// `tear_frames / decodable_frames` (0.0 when no decodable single-source frame).
    pub tear_fraction: f64,
    /// The largest union span observed among SINGLE-SOURCE frames (0 or 1 = clean; >= 2 = a tear).
    /// Multi-source frames' (skew) spans are reported separately in [`Self::max_multi_path_spread`]
    /// so this stays a clean single-tile tear magnitude, never polluted by inter-path skew.
    pub max_spread: u32,
    /// issue 1196 — fraction of ALL in-window frames ([`Self::total_frames`]) that decoded BOTH
    /// aux tick marks (>= 2 aux payloads; the bottom burn-gap pair). The promotion-gating
    /// coverage signal: a LIVE flip additionally requires this above a calibrated floor on the
    /// same run, so a silent aux loss demotes honestly instead of false-greening. 0.0 on pre-aux
    /// content (and on the first real rig run, where the ~210px aux QRs did not survive the lossy
    /// chain). Known bootstrap nuance: on the painter's very first tick BOTH aux marks carry
    /// frame_id 0, so decode dedup collapses them to ONE payload and that single frame reads as
    /// not-fully-covered — one frame per painter start, irrelevant at window scale.
    pub aux_decode_fraction: f64,
    /// issue 1196 — fraction of ALL in-window frames where the PRIMARY band decoded NOTHING while
    /// BOTH aux marks decoded: band-localized corruption (e.g. a seam inside the 700px primary
    /// band, which corrupts both primary halves at the same height) as opposed to a whole-frame
    /// blur (which kills the aux marks too). Report-only discriminator; 0.0 on pre-aux content.
    pub primary_dark_aux_alive_fraction: f64,
    /// issue 1196 (v2.1) — count of in-window frames flagged [`is_multi_path_suspect`] (>= 3 ids in
    /// EITHER band => composited from >= 2 capture paths/tiles). These are EXCLUDED from
    /// `decodable_frames`/`tear_frames` because their union span is inter-path skew, not a tear,
    /// and cannot be scoped without pixel positions. On a multi-tile recording this is nearly all
    /// frames.
    pub multi_path_suspect_frames: u32,
    /// issue 1196 (v2.1) — `multi_path_suspect_frames / total_frames`. The window's
    /// trustworthiness flag for tear scoring: ~1.0 means the window is multi-tile and the tear
    /// reading is unscoreable (a LIVE flip must gate on this staying LOW). 0.0 on single-tile
    /// content.
    pub multi_path_suspect_fraction: f64,
    /// issue 1196 (v2.1) — the largest inferred cluster (tile) count over the window's frames
    /// (`ceil(count / 2)`, taken over whichever band shows more QRs). 1 on single-tile content;
    /// at least 2 whenever any frame carried a multi-tile composite; 0 on an empty / all-undecodable
    /// window. Report-only observability of how many capture paths the recording mixed.
    pub max_cluster_count: u32,
    /// issue 1196 (v2.1) — the largest primary∪aux union span among the MULTI-PATH-SUSPECT frames:
    /// the peak inter-path skew magnitude (in ticks) between the composited tiles (the union folds
    /// in any aux component, but on a multi-tile frame every band is the same monitor's tiles, so
    /// this is skew, not a cross-band tear). Report-only — this is the number v2 mis-reported as
    /// `max_spread`; separating it keeps the honest tear magnitude ([`Self::max_spread`]) clean
    /// while still surfacing the skew for diagnosis. 0 when no suspect frame.
    pub max_multi_path_spread: u32,
    /// Whether the signal fired on this window (see [`TearSignalViability`]).
    pub viability: TearSignalViability,
}

/// Aggregate per-window tear stats from each in-window frame's `(primary_ids, aux_ids)` —
/// `primary_ids` = the cam2-optical dual-QR Vernier `frame_id`s (node burns already excluded by
/// the caller), `aux_ids` = the bottom aux tick pair's `frame_id`s (`AUX_TICK_RUN_ID` payloads,
/// issue 1196). Undecodable bands are passed as empty slices. issue 1196 v2.1: a frame carrying
/// at least 3 primary optical ids is MULTI-PATH SUSPECT (composited from >= 2 tiles) and is excluded
/// from the tear count — only single-source frames are scored (see the module doc).
pub fn window_tear_stats(per_frame_ids: &[(Vec<u32>, Vec<u32>)]) -> TearStats {
    let total_frames = per_frame_ids.len() as u32;
    let mut decodable_frames = 0u32;
    let mut tear_frames = 0u32;
    let mut max_spread = 0u32;
    let mut aux_full_frames = 0u32;
    let mut primary_dark_aux_alive = 0u32;
    let mut multi_path_suspect_frames = 0u32;
    let mut max_cluster_count = 0u32;
    let mut max_multi_path_spread = 0u32;
    for (primary, aux) in per_frame_ids {
        // Inferred tile count is the max over both bands — one tile yields at most 2 QRs per band.
        let clusters = frame_cluster_count(primary).max(frame_cluster_count(aux));
        if clusters > max_cluster_count {
            max_cluster_count = clusters;
        }
        if aux.len() >= 2 {
            aux_full_frames += 1;
            if primary.is_empty() {
                primary_dark_aux_alive += 1;
            }
        }
        if is_multi_path_suspect(primary, aux) {
            // Multi-source frame: its union span is inter-path skew, not a scanout tear, and the
            // ids cannot be attributed to a tile without pixel positions -> UNSCOREABLE for tear.
            multi_path_suspect_frames += 1;
            if let Some(spread) = frame_union_spread(primary, aux) {
                if spread > max_multi_path_spread {
                    max_multi_path_spread = spread;
                }
            }
            continue;
        }
        // Single-source frame (<= 1 inferred tile): scoreable for tear.
        if let Some(spread) = frame_union_spread(primary, aux) {
            decodable_frames += 1;
            if spread > max_spread {
                max_spread = spread;
            }
            if spread > VERNIER_MAX_SPREAD {
                tear_frames += 1;
            }
        }
    }
    let tear_fraction = if decodable_frames > 0 {
        tear_frames as f64 / decodable_frames as f64
    } else {
        0.0
    };
    let (aux_decode_fraction, primary_dark_aux_alive_fraction, multi_path_suspect_fraction) =
        if total_frames > 0 {
            (
                aux_full_frames as f64 / total_frames as f64,
                primary_dark_aux_alive as f64 / total_frames as f64,
                multi_path_suspect_frames as f64 / total_frames as f64,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
    let viability = if tear_frames > 0 {
        TearSignalViability::Observed
    } else {
        TearSignalViability::Unproven
    };
    TearStats {
        total_frames,
        decodable_frames,
        tear_frames,
        tear_fraction,
        max_spread,
        aux_decode_fraction,
        primary_dark_aux_alive_fraction,
        multi_path_suspect_frames,
        multi_path_suspect_fraction,
        max_cluster_count,
        max_multi_path_spread,
        viability,
    }
}

/// Per-window report-only pass: `tear_fraction <= TEAR_FRACTION_CEILING`. Does NOT gate while
/// [`gates_overall_pass`] is `false`.
pub fn tear_gate_pass(stats: &TearStats) -> bool {
    stats.tear_fraction <= TEAR_FRACTION_CEILING
}

/// Whether the tear gate folds into the fused `overall_pass`. REPORT-ONLY (`false`): flip to `true`
/// (one line) only after the signal is [`TearSignalViability::Observed`] on a known-torn run AND a
/// bound is calibrated (see the module-level "Precondition" note + `verdict-gate-seam-calibration.md`).
pub fn gates_overall_pass() -> bool {
    false
}

/// All windows pass — the run-level report-only fold helper for the probe consumer.
pub fn run_tear_gate_pass(stats: &[TearStats]) -> bool {
    stats.iter().all(tear_gate_pass)
}

/// issue 1196 — the machine-checked PER-WINDOW flip-readiness property (mirrors
/// `dup_cadence::signal_promotable`, the `verdict-gate-seam-calibration.md` §12 doctrine: "Make
/// promotion-readiness a COMPUTED, machine-checked property, not a guess"). A window is promotable
/// when the tear signal has DEMONSTRABLY fired on it ([`TearSignalViability::Observed`]) AND the
/// window is trustworthy single-tile content (`multi_path_suspect_fraction <=
/// MULTI_PATH_SUSPECT_CEILING`). This is deliberately SIGNAL-AGNOSTIC and fail-safe for the
/// aux-vs-primary operative-signal question the known-torn run resolves: real data shows the CAM2
/// projection leg decodes NO aux marks (aux_decode_fraction 0.0 in every recorded window — the
/// small aux QRs are not in imag's projected scanout), so a CAM2 tear surfaces via the PRIMARY band,
/// while the splitter legs decode aux but are not the projector path. So `aux_decode_fraction` is a
/// report-only DIAGNOSTIC, NOT a hard promotion floor — a floor on it would permanently block that
/// leg. Because promotability REQUIRES `Observed`, if neither the primary nor the aux signal can see
/// the induced tear the viability stays `Unproven` and the flip stays blocked — the honest
/// fail-closed behaviour. NOT SUFFICIENT for the flip on its own: a LOW background of `Observed`
/// single-tile tears (~0.001–0.004 tear_fraction) exists on green runs on both CAM2 and CAM3, so
/// `window_promotable` is `true` on ~16 routine windows already; the flip additionally requires a
/// calibrated [`TEAR_FRACTION_CEILING`] above that background. REPORT-ONLY: promotability does not
/// itself flip [`gates_overall_pass`]; it is emitted so a known-torn run is auto-gradable.
pub fn window_promotable(stats: &TearStats) -> bool {
    stats.viability == TearSignalViability::Observed
        && stats.multi_path_suspect_fraction <= MULTI_PATH_SUSPECT_CEILING
}

/// issue 1196 — the RUN-LEVEL machine-checked flip-readiness: the run analyzed at least one window,
/// the tear signal FIRED on at least one of them ([`TearSignalViability::Observed`]) AND EVERY
/// window is trustworthy single-tile content (`multi_path_suspect_fraction <=
/// MULTI_PATH_SUSPECT_CEILING`). It is NECESSARY but NOT SUFFICIENT for the [`gates_overall_pass`]
/// flip: because a LOW background of `Observed` single-tile tears (~0.001–0.004 tear_fraction, 1–3
/// frames/window) occurs on routine green runs on both CAM2 and CAM3, this reads `true` on ~12 of 32
/// v2.1-scored routine runs — so `signal_promotable == true` is NOT by itself evidence of a
/// known-torn run. The flip is additionally gated on a calibrated [`TEAR_FRACTION_CEILING`] that
/// separates the known-torn run's HIGH tear_fraction from this green background (see the module-level
/// caveat). It correctly guards two things regardless: a run with NO observed tear at all cannot
/// promote (the issue-1101 blind-signal trap), and a run with ANY multi-tile window is not promotable
/// (a LIVE flip would gate that unscoreable window). REPORT-ONLY companion to [`window_promotable`].
pub fn signal_promotable(stats: &[TearStats]) -> bool {
    !stats.is_empty()
        && stats
            .iter()
            .any(|s| s.viability == TearSignalViability::Observed)
        && stats
            .iter()
            .all(|s| s.multi_path_suspect_fraction <= MULTI_PATH_SUSPECT_CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand: a per-frame `(primary_ids, aux_ids)` pair for `window_tear_stats`.
    fn f(primary: &[u32], aux: &[u32]) -> (Vec<u32>, Vec<u32>) {
        (primary.to_vec(), aux.to_vec())
    }

    #[test]
    fn healthy_vernier_pair_is_not_torn() {
        // LEFT=even 100, RIGHT=odd 101 -> span 1 -> the by-design adjacency, NOT a tear —
        // with or without the aux pair (issue 1196) echoing the same generation.
        assert!(!is_torn_frame(&[100, 101], &[]));
        assert!(!is_torn_frame(&[101, 100], &[]));
        assert!(!is_torn_frame(&[100, 101], &[100, 101]));
        assert_eq!(frame_optical_spread(&[100, 101]), Some(1));
        assert_eq!(frame_union_spread(&[100, 101], &[100, 101]), Some(1));
    }

    #[test]
    fn single_optical_half_is_not_torn() {
        // Only one half decoded (span 0) — clean, not a tear.
        assert!(!is_torn_frame(&[100], &[]));
        assert_eq!(frame_optical_spread(&[100]), Some(0));
        assert_eq!(frame_union_spread(&[100], &[]), Some(0));
    }

    #[test]
    fn undecodable_frame_has_no_spread_and_is_not_torn() {
        assert_eq!(frame_optical_spread(&[]), None);
        assert_eq!(frame_union_spread(&[], &[]), None);
        assert!(!is_torn_frame(&[], &[]));
    }

    #[test]
    fn single_cluster_two_generations_is_torn() {
        // A genuine SINGLE-TILE tear: one tile's dual-QR captured gen G's even (100) and gen G+1's
        // even (102) — 2 ids, span 2 > VERNIER_MAX_SPREAD -> TORN. This is the "genuine
        // single-cluster 2-generation frame" v2.1 must still catch (issue 1196).
        assert!(is_torn_frame(&[100, 102], &[]));
        assert_eq!(frame_union_spread(&[100, 102], &[]), Some(2));
        assert!(!is_multi_path_suspect(&[100, 102], &[]), "2 ids = one tile");
    }

    #[test]
    fn three_or_more_primary_ids_is_multi_path_suspect_not_torn_1196() {
        // A single tile's dual-QR band produces AT MOST 2 optical QRs (left even + right odd), so
        // a frame with >= 3 primary optical ids was composited from >= 2 capture paths/tiles: it
        // is MULTI-PATH SUSPECT and NEVER torn — its wide span is inter-path skew, not a scanout
        // tear, and cannot be scoped without pixel positions (issue 1196 v2.1). This replaces v2's
        // treatment of {100,101,102,103} as a single-tile tear (a single band cannot yield 4 clean
        // generations; that shape is a multi-tile composite).
        assert!(is_multi_path_suspect(&[100, 101, 102, 103], &[]));
        assert!(
            !is_torn_frame(&[100, 101, 102, 103], &[]),
            "multi-tile skew, not a tear"
        );
        assert!(
            is_multi_path_suspect(&[100, 101, 102], &[]),
            "3 ids = 2 tiles"
        );
        assert!(!is_torn_frame(&[100, 101, 102], &[]));
        assert_eq!(frame_cluster_count(&[]), 0);
        assert_eq!(frame_cluster_count(&[100]), 1);
        assert_eq!(frame_cluster_count(&[100, 101]), 1);
        assert_eq!(frame_cluster_count(&[100, 101, 102]), 2);
        assert_eq!(frame_cluster_count(&[100, 101, 102, 103]), 2);
    }

    #[test]
    fn aux_side_multi_tile_is_suspect_not_torn_1196() {
        // Review hardening: the multi-source guard is symmetric across bands. A frame with a
        // single-source PRIMARY band (<= 2 ids) but >= 3 AUX ids was still composited from >= 2
        // tiles (one tile's aux pair yields at most 2 aux QRs), so it must be suspect, not scored
        // as a spurious aux-side-skew tear. Inert on the real rig (aux_decode_fraction 0.0) but
        // keeps the physical "one band, at most 2 QRs" premise consistent for both bands.
        assert!(is_multi_path_suspect(&[100, 101], &[200, 201, 202]));
        assert!(
            !is_torn_frame(&[100, 101], &[200, 201, 202]),
            "aux-side multi-tile skew is not a tear"
        );
        // Two aux marks (one tile's pair) is still single-source and scoreable.
        assert!(!is_multi_path_suspect(&[100, 101], &[200, 201]));
    }

    #[test]
    fn cross_band_generation_split_is_torn_1196() {
        // THE issue-1196 capability: a horizontal seam BETWEEN the primary band and the aux
        // band — the primary pair decodes gen G+1 (ticks 102/103) while the aux pair still
        // shows gen G (ticks 100/101). Neither band alone spans > 1, but the UNION does. This is
        // a SINGLE-SOURCE frame (2 primary ids = one tile), so v2.1 scores it.
        assert!(!is_torn_frame(&[102, 103], &[]), "primary alone is clean");
        assert!(!is_torn_frame(&[], &[100, 101]), "aux alone is clean");
        assert!(
            is_torn_frame(&[102, 103], &[100, 101]),
            "the primary-vs-aux generation split IS the tear"
        );
        assert_eq!(frame_union_spread(&[102, 103], &[100, 101]), Some(3));
        // Minimal cross-band tear: a SINGLE aux mark one generation behind is enough —
        // union {101, 102, 103} spans 2 > VERNIER_MAX_SPREAD.
        assert!(is_torn_frame(&[102, 103], &[101]));
        assert_eq!(frame_union_spread(&[102, 103], &[101]), Some(2));
        assert!(
            is_torn_frame(&[102, 103], &[100]),
            "span 3 via one aux mark"
        );
    }

    #[test]
    fn window_all_healthy_is_unproven_zero_tears() {
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[102, 103], &[102, 103]),
            f(&[104], &[]),
            f(&[], &[]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 4, "every attributed frame is counted");
        assert_eq!(s.decodable_frames, 3, "undecodable frame excluded");
        assert_eq!(s.tear_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.max_spread, 1);
        // 2 of 4 frames decoded BOTH aux marks.
        assert!((s.aux_decode_fraction - 0.5).abs() < 1e-9);
        assert_eq!(s.primary_dark_aux_alive_fraction, 0.0);
        // All frames are single-source (<= 2 primary ids) -> zero multi-path suspects.
        assert_eq!(s.multi_path_suspect_frames, 0);
        assert_eq!(s.multi_path_suspect_fraction, 0.0);
        assert_eq!(s.max_cluster_count, 1);
        assert_eq!(s.max_multi_path_spread, 0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn window_with_a_single_cluster_tear_is_observed() {
        let frames = vec![
            f(&[100, 101], &[]),
            f(&[102, 104], &[]), // ONE tile, 2 ids spanning 2 -> a genuine tear
            f(&[106, 107], &[]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.decodable_frames, 3);
        assert_eq!(s.tear_frames, 1);
        assert_eq!(s.max_spread, 2);
        assert_eq!(s.multi_path_suspect_frames, 0);
        assert!((s.tear_fraction - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(s.viability, TearSignalViability::Observed);
        assert!(
            !tear_gate_pass(&s),
            "a nonzero tear fraction fails the (report-only) gate"
        );
    }

    #[test]
    fn window_cross_band_tear_is_observed_1196() {
        // A cross-band seam frame inside an otherwise healthy window fires the signal.
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[102, 103], &[100, 101]), // primary advanced, aux one generation behind
            f(&[104, 105], &[104, 105]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.tear_frames, 1);
        assert_eq!(s.max_spread, 3);
        assert_eq!(
            s.multi_path_suspect_frames, 0,
            "all single-source (2 primary ids each)"
        );
        assert_eq!(s.viability, TearSignalViability::Observed);
    }

    #[test]
    fn window_multi_tile_skew_is_suspect_not_torn_1196() {
        // The core issue-1196 defect: a window where every frame carries the primary dual-QR from
        // TWO grabber-path tiles of the SAME monitor, offset ~2-3 ticks. Each frame has 3-4 primary
        // optical ids (a contiguous run) -> MULTI-PATH SUSPECT -> excluded from the tear count. v2
        // read this as ~100% torn; v2.1 reads it as multi-path skew, 0 tears, Unproven.
        let frames = vec![
            f(&[100, 101, 102], &[]),      // 2 tiles offset by 1, one half dropped
            f(&[104, 105, 106, 107], &[]), // 2 tiles offset by 2
            f(&[108, 109, 110, 111], &[]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 3);
        assert_eq!(s.multi_path_suspect_frames, 3, "every frame is multi-tile");
        assert_eq!(s.decodable_frames, 0, "no single-source frame to score");
        assert_eq!(s.tear_frames, 0, "inter-path skew is not a tear");
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(
            s.max_spread, 0,
            "clean single-tile magnitude untouched by skew"
        );
        assert_eq!(s.max_cluster_count, 2);
        assert_eq!(
            s.max_multi_path_spread, 3,
            "peak inter-path skew surfaced separately"
        );
        assert!((s.multi_path_suspect_fraction - 1.0).abs() < 1e-9);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(
            tear_gate_pass(&s),
            "a fully-suspect window has no scoreable tear"
        );
    }

    #[test]
    fn primary_dark_aux_alive_discriminator_1196() {
        // A seam INSIDE the 700px primary band corrupts both primary halves (undecodable
        // primary) while BOTH bottom aux marks still decode — band-localized corruption, the
        // exact shape the primary-only v1 detector counted as a plain undecodable. The frame
        // is decodable via the union (aux) and NOT torn (aux span 1); the discriminator
        // fraction counts it against ALL attributed frames.
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[], &[102, 103]), // primary dark, both aux alive
            f(&[], &[104]),      // primary dark, only ONE aux — NOT the discriminator shape
            f(&[], &[]),         // fully undecodable
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 4);
        assert_eq!(s.decodable_frames, 3, "union-decodable: frames 0, 1, 2");
        assert_eq!(s.tear_frames, 0);
        // Frames 0 and 1 decoded both aux marks: 2/4.
        assert!((s.aux_decode_fraction - 0.5).abs() < 1e-9);
        // Only frame 1 is primary-dark with BOTH aux alive: 1/4.
        assert!((s.primary_dark_aux_alive_fraction - 0.25).abs() < 1e-9);
        // Primary-dark frames carry 0 primary ids -> 0 clusters -> never suspect.
        assert_eq!(s.multi_path_suspect_frames, 0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
    }

    #[test]
    fn empty_window_is_unproven_and_passes() {
        let s = window_tear_stats(&[]);
        assert_eq!(s.total_frames, 0);
        assert_eq!(s.decodable_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.aux_decode_fraction, 0.0);
        assert_eq!(s.primary_dark_aux_alive_fraction, 0.0);
        assert_eq!(s.multi_path_suspect_frames, 0);
        assert_eq!(s.multi_path_suspect_fraction, 0.0);
        assert_eq!(s.max_cluster_count, 0);
        assert_eq!(s.max_multi_path_spread, 0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn report_only_seam_is_disarmed() {
        assert!(!gates_overall_pass(), "issue 781/1196 ships report-only");
    }

    #[test]
    fn window_promotable_requires_observed_and_single_tile_1196() {
        // A GREEN window (no tear observed) is NOT promotable — an all-zero distribution cannot
        // prove the signal works (the issue-1101 blind-signal trap).
        let green = window_tear_stats(&[f(&[100, 101], &[]), f(&[102, 103], &[])]);
        assert_eq!(green.viability, TearSignalViability::Unproven);
        assert!(
            !window_promotable(&green),
            "green/unproven is never promotable"
        );

        // A TORN SINGLE-TILE window (a genuine 2-generation frame, suspect 0) IS promotable.
        let torn = window_tear_stats(&[f(&[100, 101], &[]), f(&[102, 104], &[])]);
        assert_eq!(torn.viability, TearSignalViability::Observed);
        assert_eq!(torn.multi_path_suspect_fraction, 0.0);
        assert!(
            window_promotable(&torn),
            "observed single-tile tear is promotable"
        );

        // An OBSERVED-BUT-MULTI-TILE window is NOT promotable: a multi-tile frame is unscoreable,
        // so even one clean single-source tear in the window cannot lift a suspect-heavy window
        // over the ceiling. Build a window that is BOTH observed (one single-source tear) and
        // dominated by multi-tile suspects.
        let mut frames = vec![f(&[100, 102], &[])]; // 1 single-source tear -> observed
        for _ in 0..20 {
            frames.push(f(&[200, 201, 202, 203], &[])); // multi-tile suspects
        }
        let mixed = window_tear_stats(&frames);
        assert_eq!(mixed.viability, TearSignalViability::Observed);
        assert!(
            mixed.multi_path_suspect_fraction > MULTI_PATH_SUSPECT_CEILING,
            "20/21 suspect frames exceed the ceiling"
        );
        assert!(
            !window_promotable(&mixed),
            "an observed window dominated by multi-tile suspects is not promotable"
        );
    }

    #[test]
    fn signal_promotable_run_level_1196() {
        // Empty run: not promotable.
        assert!(!signal_promotable(&[]), "empty run is not promotable");

        // All-green run (every window unproven): not promotable.
        let g = || window_tear_stats(&[f(&[100, 101], &[]), f(&[102, 103], &[])]);
        assert!(
            !signal_promotable(&[g(), g(), g()]),
            "an all-green run cannot prove the signal fired"
        );

        // A run where ONE window observed a genuine single-tile tear and the rest are clean
        // single-tile: promotable (the known-torn run's shape).
        let torn = window_tear_stats(&[f(&[100, 101], &[]), f(&[102, 104], &[])]);
        assert!(
            signal_promotable(&[g(), torn.clone(), g()]),
            "one observed single-tile window in an otherwise clean run is promotable"
        );

        // A run with a MULTI-TILE window is NOT promotable even alongside an observed one — a LIVE
        // flip would gate the unscoreable multi-tile window.
        let mut multi_frames = vec![f(&[100, 102], &[])];
        for _ in 0..20 {
            multi_frames.push(f(&[200, 201, 202, 203], &[]));
        }
        let multi = window_tear_stats(&multi_frames);
        assert!(
            !signal_promotable(&[torn, multi]),
            "any multi-tile window blocks run-level promotion"
        );
    }
}
