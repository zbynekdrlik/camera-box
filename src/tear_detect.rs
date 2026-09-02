//! Projection-tap scanout-TEAR detector (issue 781/1196) — PURE, LIVE gate (`gates_overall_pass()`
//! returns `true` since issue 1196; one-line disarmable).
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
//! ## Why the PRIMARY band alone is blind (the v1 history — cured by the v2 aux pair below)
//!
//! Measured across 5 real `stream-partial-*.json` (~48 000 frames) AND re-confirmed on the
//! known-torn run 1700989544 (`max primary span = 1` on every one of the 9,883 stream-partial frames), the per-frame PRIMARY
//! optical span is exclusively {0,1} and the primary-QR count per frame never exceeds 2 — the "two
//! generations in one frame" signal NEVER fires in the primary band, even under a REAL induced tear.
//! The reason is structural (confirmed by reading real captured frames): both dual-QR halves sit in
//! ONE vertical band (top ~60%), so a horizontal scanout tear crossing that band corrupts BOTH QRs
//! at the same height → the frame goes `undecodable` (tick=None) rather than yielding two clean
//! generations. A tear cannot manufacture a second, older/newer generation of a QR that exists at
//! only one vertical position. So a PRIMARY-ONLY detector's all-zero `tear_fraction` was blind, not
//! green — the exact "a gate that can never fire is worse than no gate" trap (issue 1101/1088). The
//! v2 aux tick pair below CURES it (a second QR band lower on the screen catches the seam), so the
//! gate is now LIVE ([`gates_overall_pass`] returns `true`) with a computed [`TearSignalViability`]
//! so an all-zero reading can never be mistaken for a promotable green.
//!
//! ## v2 (issue 1196) — the aux Vernier tick pair makes the signal VIABLE
//!
//! The vertical tick redundancy the paragraph above calls for now exists: the painter additionally
//! blits a small aux QR pair into the bottom burn-free gaps (`crate::aux_tick` geometry; left =
//! latest EVEN tick, right = latest ODD tick, reserved `AUX_TICK_RUN_ID`, `gen_ts_ns = 0`). A
//! horizontal seam between the primary band and the aux band now yields a clean generation in EACH
//! band, so the v2 detector computes the tear span over the UNION of `(primary_ids, aux_ids)`
//! ([`frame_union_spread`]). On the CAM2 projection leg the operative form is a SINGLE aux mark
//! from a later generation (see the LIVE-gate section below). Report-only companion diagnostics:
//! [`TearStats::aux_any_decode_fraction`] (≥ 1 aux mark — the honest operability signal, ~0.97+ on
//! CAM2), [`TearStats::aux_decode_fraction`] (BOTH aux marks — ~0 on CAM2, high on splitter legs;
//! NOT an operability test), and [`TearStats::primary_dark_aux_alive_fraction`] (a seam INSIDE the
//! primary band corrupts both primary halves while both aux marks decode — band-localized corruption
//! vs whole-frame blur).
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
//! ## LIVE gate (issue 1196, 2026-09-01) — and the CORRECTED per-leg signal attribution
//!
//! [`gates_overall_pass`] is now `true`: the known-torn calibration run 1700989544 (imag projector
//! vsync disabled off-air) made the signal [`TearSignalViability::Observed`] on the CAM2 projection
//! leg and the two-term [`tear_gate_pass`] separates the induced tear from the green background. A
//! window FAILS only if it is `Observed` AND single-tile (`multi_path_suspect_fraction <=
//! MULTI_PATH_SUSPECT_CEILING` — a MULTI-TILE window is UNSCOREABLE and must never false-fail) AND
//! over BOTH the rate [`TEAR_FRACTION_CEILING`] (0.005) and the count [`TEAR_FRAME_COUNT_FLOOR`] (6,
//! since the green background is a 1–3-frame COUNT independent of window length). The machine-checked
//! flip-readiness [`window_promotable`] / [`signal_promotable`] (viability `Observed` + single-tile,
//! mirroring `dup_cadence::signal_promotable`, `verdict-gate-seam-calibration.md` §12) and the
//! report-only blind-spot signal [`signal_operable`] ([`AUX_ANY_OPERABLE_FLOOR`]) are observability.
//!
//! **The operative signal is the AUX SINGLE-MARK CROSS-BAND, not the primary band (per-frame data
//! correction, 2026-09-01, mining the real rqrr decode of run 1700989544 — this SUPERSEDES an earlier
//! grading that read it as the primary band).** The PRIMARY dual-QR span is ALWAYS ≤ 1 (`max primary
//! span = 1` on every one of the 9,883 stream-partial frames) — the primary band is structurally blind to a tear, exactly as
//! the blindness paragraph above says. EVERY one of the 241 torn frames is `primary[X, X+1]` (span 1)
//! plus exactly ONE aux mark `[Y > X+1]` from a LATER generation: the bottom aux band, scanned out later,
//! captures the newer generation during the un-vsynced tear (union span 2–7). That is the v2 aux
//! cross-band cure firing EXACTLY as issue 1196 designed. So the aux is the OPERATIVE signal on the
//! projection leg, and dropping it from the union would make the gate blind (0 torn on CAM2).
//!
//! **`aux_decode_fraction` (BOTH marks) = 0.0 on the CAM2 projection leg is a MISLEADING metric, NOT
//! a dead aux.** It counts frames with `aux.len() >= 2`; on the CAM2 projection window imag's OWN
//! burn (911003, rendered by imag's OBS projector, which cam2 films) OCCLUDES the LEFT (even) aux —
//! present on ~99% of those frames, so only the RIGHT (odd) aux decodes and the both-mark fraction
//! reads 0.0 there (it is ~0.97–0.99 on the SPLITTER legs, which do not carry imag's burn). This is
//! a fixable GEOMETRY defect (the LEFT aux sits in imag's burn zone — issue 1266 relocates it), not
//! a lossy-chain limitation. But the operative cross-band tear needs only ONE aux mark, and
//! [`TearStats::aux_any_decode_fraction`] (≥ 1 mark) reads ~0.97–0.999 on the CAM2 windows — the aux
//! is fully operative there. So an aux-coverage FLOOR on `aux_decode_fraction` was NEVER the right
//! promotion gate (it would have permanently blocked the projection leg); `aux_decode_fraction` stays
//! a report-only DIAGNOSTIC and `aux_any_decode_fraction` is the honest operability signal.
//!
//! **A LOW background of `Observed` tears exists on GREEN runs — the ceiling is 0.005, never 0.0.**
//! Mined across 37 v2.1 verdicts: the aux single-mark cross-band occasionally reads one generation
//! off on a healthy run, so `Observed` windows occur on green runs at `tear_fraction` up to 0.003546
//! (3 frames/846, mostly 1 frame). The known-torn run's CAM2 windows read 0.018846 (16 frames) and
//! 0.237308 (201 frames). `TEAR_FRACTION_CEILING = 0.005` sits ABOVE the green MAX (0 false positives
//! on the 37-run history) and 3.77x BELOW the induced-tear floor — a per-window RATE (the induced tear
//! produces 16–201 frames, far above the ≤3-frame background, so no run-wide COUNT term is needed).
//! The current green content is SINGLE-TILE (`multi_path_suspect_fraction` 0.0 across the green
//! windows), so a multi-tile window (unscoreable) is kept out of promotion by the suspect ceiling.
//!
//! Mirrors the crate-root `gates_overall_pass()` seam pattern shared by `presentation_cadence` /
//! `optical_floor` / `e2e_latency_gate` / `imag_leg_gate`: PURE (default features, Tier-0
//! unit-testable); the probe-gated `recording-verdict.rs` consumer only feeds it the per-frame
//! optical ids and folds the LIVE verdict into `overall_pass`.

use serde::Serialize;

/// The by-design optical span of ONE healthy captured frame: the dual-QR Vernier's LEFT (latest
/// even) and RIGHT (latest odd) halves differ by exactly one tick, so `max(frame_id) - min(frame_id)
/// == 1`. A wider span means the frame captured >= 2 distinct paint generations — a scanout tear.
pub const VERNIER_MAX_SPREAD: u32 = 1;

/// LIVE per-window tear-rate ceiling for [`tear_gate_pass`] (issue 1196 — the seam is now BLOCKING,
/// [`gates_overall_pass`] returns `true`). An `Observed` window whose `tear_fraction` exceeds this
/// FAILS the fused verdict; an `Unproven` window (`tear_fraction` 0.0) always passes.
///
/// Calibrated from the real distributions mined across 37 v2.1-scored local verdicts (per
/// `verdict-gate-seam-calibration.md`): the GREEN Observed background — the aux single-mark
/// cross-band occasionally reading one generation off on a healthy run — tops out at **0.003546**
/// (3 torn frames / 846, run 1801923068 CAM2), while the known-torn calibration run 1700989544's
/// CAM2 projection windows read **0.018846** (16/849) and **0.237308** (201/847). `0.005` sits
/// ABOVE the green background (ZERO false positives on the 37-run history) and 3.77x BELOW the
/// smallest induced-tear window. This is the RATE term of a TWO-TERM gate — the green background is
/// a COUNT phenomenon (1–3 aux single-mark cross-band frames regardless of window length), so the
/// rate alone is thin (a 4–5 frame green window would read ~0.005–0.006 on a short window); the
/// companion [`TEAR_FRAME_COUNT_FLOOR`] requires the window ALSO carry enough torn frames before it
/// can fail (`verdict-gate-seam-calibration.md` §4, count-phenomenon → two terms). One-line
/// re-tighten/relax: change this value (the gate mechanism stays; `CAMERA_BOX_*` is not wired).
pub const TEAR_FRACTION_CEILING: f64 = 0.005;

/// LIVE per-window tear-COUNT floor — the second term of the two-term [`tear_gate_pass`] gate
/// (issue 1196, review-hardening). An `Observed` single-tile window fails ONLY if it exceeds BOTH
/// [`TEAR_FRACTION_CEILING`] (rate) AND this many torn frames (count). The green Observed background
/// is a COUNT of 1–3 aux single-mark cross-band frames (occasional one-gen-off reads on a healthy
/// run) INDEPENDENT of window length, so a rate-only gate is thin (a 4–5 frame spike on a short
/// window false-fails); `6` sits 2x above the green max (3) and 2.7x below the smallest induced-tear
/// window (16), and also stops a tiny-denominator window (a few torn frames out of few decodable)
/// from false-failing on a high rate. Change this value to re-tighten/relax the count term.
pub const TEAR_FRAME_COUNT_FLOOR: u32 = 6;

/// issue 1196 review-hardening — the minimum frame-weighted [`TearStats::aux_any_decode_fraction`]
/// (≥ 1 aux mark) below which the tear signal is considered NON-OPERABLE. The LIVE gate's operative
/// signal on the CAM2 projection leg is the aux SINGLE-MARK cross-band; since imag's burn already
/// occludes the LEFT aux (issue 1266), the gate rides on the RIGHT aux alone — if THAT is ever also
/// occluded (a new overlay, a projector geometry change), no tear can be Observed and the gate would
/// silently pass forever (the issue-1101 blind-signal trap). [`signal_operable`] surfaces that as a
/// report-only observability signal so the blind spot is visible, not silent. `0.5` is well below the
/// live ~0.97–0.999 any-mark coverage and above zero. Report-only today; does NOT gate.
pub const AUX_ANY_OPERABLE_FLOOR: f64 = 0.5;

/// issue 1196 — the highest [`TearStats::multi_path_suspect_fraction`] a window may carry and still
/// be trusted for tear scoring (a promotion guard, per `verdict-gate-seam-calibration.md` §12 and
/// the projection-tap rule's precondition 3). Above this the recorded scene is MULTI-TILE (a frame
/// carries the SAME painted monitor from >= 2 grabber paths), so the union span measures inter-path
/// skew, not a scanout tear, and the window is UNSCOREABLE without pixel positions — it must never
/// be promoted. Calibrated from the real distribution: across 90 green windows the suspect fraction
/// is EXACTLY 0.0 (single-tile content), while a multi-tile window reads ~0.998 — a ~10x margin at
/// this ceiling passes every green run and blocks any genuinely multi-tile window. Used by BOTH the
/// promotion property ([`window_promotable`] / [`signal_promotable`]) AND the LIVE [`tear_gate_pass`]
/// (issue 1196 review-hardening): an above-ceiling (multi-tile, UNSCOREABLE) window can NEVER FAIL
/// the gate — its few single-source residual frames carry inter-path skew, not a real tear, and
/// their tiny-denominator rate must not false-fail (the real multi-tile run 1859005342 would
/// otherwise fail 4/10 windows).
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
/// arm keeps the guard symmetric across bands — rarely the deciding factor on the projection leg
/// (only one aux mark decodes there), but aux marks DO decode (~1 mark/frame on CAM2, both on the
/// splitter legs), and the physical "one band, at most 2 QRs" premise applies to aux too.
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

/// Per-window tear report (a DATA struct — the LIVE gate decision is [`tear_gate_pass`]). Derives
/// only `PartialEq` (not `Eq`) — the fractions are
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
    /// aux tick marks (>= 2 aux payloads; the bottom burn-gap pair). Report-only DIAGNOSTIC of the
    /// aux BOTH-mark coverage — high on the splitter legs (~0.97–0.99), but **0.0 on the CAM2
    /// projection leg** (imag's OWN burn 911003 occludes the LEFT aux there — a geometry defect,
    /// issue 1266 — so only the RIGHT aux decodes). This is NOT a "dead aux" signal and is NOT the
    /// operative-signal test — see
    /// [`Self::aux_any_decode_fraction`]: a SINGLE aux mark decodes ~0.97–0.999 of CAM2-window
    /// frames, and the operative cross-band tear signal needs only ONE. Do NOT gate promotion on
    /// this both-mark fraction (it would permanently block the projection leg). 0.0 on pre-aux
    /// content. Known bootstrap nuance: on the painter's very first tick BOTH aux marks carry
    /// frame_id 0, so decode dedup collapses them to ONE payload and that single frame reads as
    /// not-fully-covered — one frame per painter start, irrelevant at window scale.
    pub aux_decode_fraction: f64,
    /// issue 1196 — fraction of ALL in-window frames that decoded AT LEAST ONE aux tick mark
    /// (>= 1 aux payload). This is the HONEST aux-operability diagnostic: on the CAM2 projection
    /// leg it reads ~0.97–0.999 (a single aux mark decodes nearly every frame) even while
    /// [`Self::aux_decode_fraction`] (BOTH marks) reads 0.0 — proving the aux is the OPERATIVE
    /// cross-band cure, not a dead signal. Every torn frame on the known-torn run 1700989544 is a
    /// `primary[X,X+1]` pair + exactly ONE aux mark from a later generation, so the tear span comes
    /// from this single-mark cross-band. Report-only observability; does not gate.
    pub aux_any_decode_fraction: f64,
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
    let mut aux_any_frames = 0u32;
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
        if !aux.is_empty() {
            aux_any_frames += 1;
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
    let (
        aux_decode_fraction,
        aux_any_decode_fraction,
        primary_dark_aux_alive_fraction,
        multi_path_suspect_fraction,
    ) = if total_frames > 0 {
        (
            aux_full_frames as f64 / total_frames as f64,
            aux_any_frames as f64 / total_frames as f64,
            primary_dark_aux_alive as f64 / total_frames as f64,
            multi_path_suspect_frames as f64 / total_frames as f64,
        )
    } else {
        (0.0, 0.0, 0.0, 0.0)
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
        aux_any_decode_fraction,
        primary_dark_aux_alive_fraction,
        multi_path_suspect_frames,
        multi_path_suspect_fraction,
        max_cluster_count,
        max_multi_path_spread,
        viability,
    }
}

/// Per-window LIVE pass. A window FAILS only when it is a TRUSTWORTHY, single-tile, DEMONSTRABLY-torn
/// window: `Observed` (the signal fired) AND single-tile (`multi_path_suspect_fraction <=
/// MULTI_PATH_SUSPECT_CEILING`, so its span is a real tear not inter-path skew) AND over BOTH the
/// rate ([`TEAR_FRACTION_CEILING`]) and the count ([`TEAR_FRAME_COUNT_FLOOR`]). Everything else passes:
/// - an `Unproven` window (`tear_fraction` 0.0 — no torn frame);
/// - a MULTI-TILE window (issue 1196 review-hardening — its few single-source residual frames carry
///   inter-path skew, and their tiny-denominator rate must not false-fail; without this guard the
///   real multi-tile run 1859005342 fails 4/10 windows, the #1127 "❌ on a passing run" trap);
/// - a LOW-COUNT window (`tear_frames < TEAR_FRAME_COUNT_FLOOR` — the green background is 1–3 aux
///   single-mark cross-band frames regardless of window length; the count term stops a 4–5 frame
///   green spike, or a tiny-denominator window, from false-failing on rate alone).
pub fn tear_gate_pass(stats: &TearStats) -> bool {
    stats.viability != TearSignalViability::Observed
        || stats.multi_path_suspect_fraction > MULTI_PATH_SUSPECT_CEILING
        || stats.tear_frames < TEAR_FRAME_COUNT_FLOOR
        || stats.tear_fraction <= TEAR_FRACTION_CEILING
}

/// Whether the tear gate folds into the fused `overall_pass`. LIVE (`true`) since issue 1196: the
/// known-torn calibration run 1700989544 proved the signal fires on the CAM2 projection leg (via
/// the aux single-mark cross-band — the primary band is structurally blind, max primary span 1) and
/// [`TEAR_FRACTION_CEILING`] separates the induced tear from the green background. One-line disarm:
/// return `false` (the whole mechanism — the const, the per-window/run-level fns — stays dormant and
/// re-armable). See the module-level "Precondition" note + `verdict-gate-seam-calibration.md`.
pub fn gates_overall_pass() -> bool {
    true
}

/// All windows pass — the run-level LIVE fold helper for the probe consumer (folds into
/// `overall_pass` since issue 1196; each window judged by the two-term [`tear_gate_pass`]).
pub fn run_tear_gate_pass(stats: &[TearStats]) -> bool {
    stats.iter().all(tear_gate_pass)
}

/// issue 1196 — the machine-checked PER-WINDOW flip-readiness property (mirrors
/// `dup_cadence::signal_promotable`, the `verdict-gate-seam-calibration.md` §12 doctrine: "Make
/// promotion-readiness a COMPUTED, machine-checked property, not a guess"). A window is promotable
/// when the tear signal has DEMONSTRABLY fired on it ([`TearSignalViability::Observed`]) AND the
/// window is trustworthy single-tile content (`multi_path_suspect_fraction <=
/// MULTI_PATH_SUSPECT_CEILING`). This is deliberately SIGNAL-AGNOSTIC: the known-torn run resolved
/// that on the CAM2 projection leg the operative signal is the AUX SINGLE-MARK CROSS-BAND (the
/// primary band's own span is always <= 1 = blind; the both-mark `aux_decode_fraction` reads 0.0
/// only because imag's burn occludes the LEFT aux — issue 1266 — while the RIGHT aux carries every
/// tear). `aux_decode_fraction` is thus a report-only DIAGNOSTIC, never a promotion floor (a floor on
/// it would permanently block the projection leg). Because promotability REQUIRES `Observed`, if the
/// signal cannot see an induced tear the viability stays `Unproven` and the flip stays blocked — the
/// honest fail-closed behaviour. NOT SUFFICIENT for the flip on its own: a LOW background of `Observed`
/// single-tile tears (~0.001–0.004 tear_fraction) exists on green runs on both CAM2 and CAM3, so
/// `window_promotable` is `true` on ~16 routine windows already; the LIVE gate additionally requires a
/// calibrated [`TEAR_FRACTION_CEILING`] above that background PLUS the [`TEAR_FRAME_COUNT_FLOOR`].
/// REPORT-ONLY: promotability does not
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

/// issue 1196 review-hardening — REPORT-ONLY observability that the aux tear signal is OPERABLE on
/// this run: the frame-weighted [`TearStats::aux_any_decode_fraction`] (≥ 1 aux mark per frame) is
/// at or above [`AUX_ANY_OPERABLE_FLOOR`]. The LIVE gate rides on the aux single-mark cross-band, so
/// if aux decoding ever collapses (both marks occluded — the RIGHT one already carries the whole
/// signal since imag's burn covers the LEFT, issue 1266) the gate would go silently blind (no tear
/// Observed → passes forever, the issue-1101 trap). This surfaces that blind spot; it does NOT gate
/// (a genuinely aux-free run is not a failure — the signal is simply unavailable). Empty run, or all
/// windows carrying no frames → `false` (not operable / nothing to judge).
pub fn signal_operable(stats: &[TearStats]) -> bool {
    let total: u64 = stats.iter().map(|s| s.total_frames as u64).sum();
    if total == 0 {
        return false;
    }
    let covered: f64 = stats
        .iter()
        .map(|s| s.aux_any_decode_fraction * s.total_frames as f64)
        .sum();
    covered / total as f64 >= AUX_ANY_OPERABLE_FLOOR
}

/// issue 1144 (#887 fold) -- a REPORT-ONLY summary of the PROJECTION (CAM2) leg's tear verdict, so
/// the imag content facet can carry the answer to issue 887 ("imag zero-loss proof stops at OBS's
/// compositor -- nothing verifies what leaves HDMI-1 to the projector"). cam2's grabber is fed by
/// imag-nb's HDMI output, so the CAM2 sweep leg IS the physical projection path (imag render -> DRM
/// scanout -> HDMI -> grabber), and the tear detector is already LIVE + BLOCKING on it via
/// [`gates_overall_pass`] / [`run_tear_gate_pass`]. So issue 887's DETECTION gap is ALREADY closed
/// by that gate; this cross-reference only makes the HDMI-1 proof answerable FROM the imag facet and
/// the flip-time precondition ([`ProjectionProof::hdmi1_proof_backed`]) machine-checkable. It gates
/// nothing.
///
/// SCOPE: `hdmi1_proof_backed` proves the SCANOUT-coherence (tear) signal was LIVE and CLEAN on the
/// projection leg. The CAM2 leg's own frame CONTINUITY (copies/gaps) is a SEPARATE signal, already
/// BLOCKING via the stream per-cambox sweep (`all_cambox_continuity.segments[cam2]`); the later flip
/// should additionally require that. The cam2 <- imag-nb HDMI cabling is a runtime-unverifiable
/// premise (a re-cable would silently make the flag a lie), surfaced as an explicit note by the
/// caller.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionProof {
    /// CAM2 (projection-leg) tear windows analyzed this run.
    pub windows: usize,
    /// Fraction of CAM2 windows on which the tear signal was DEMONSTRABLY live
    /// ([`TearSignalViability::Observed`]) -- carried so the flip can require a higher-than-"any" bar.
    pub observed_fraction: f64,
    /// True iff EVERY CAM2 window passes the tear gate ([`tear_gate_pass`]) -- no Observed window
    /// over the ceiling. Vacuously true for zero windows (guarded by `hdmi1_proof_backed`).
    pub tear_gate_clean: bool,
    /// The worst per-window `tear_fraction` across the CAM2 windows (0.0 for zero windows).
    pub worst_tear_fraction: f64,
    /// Frame-weighted >= 1-aux-mark coverage across the CAM2 windows -- the operability signal on the
    /// projection leg (the aux single-mark cross-band is what actually catches a projection tear).
    pub aux_any_coverage: f64,
    /// The issue-887 answer: is the imag HDMI-1 projection SCANOUT proof BACKED this run? True iff at
    /// least one CAM2 window proved the tear signal live (`observed_fraction > 0`) AND every CAM2
    /// window is tear-clean. Report-only today; the flip should require this AND the separately-
    /// blocking CAM2 frame-continuity.
    pub hdmi1_proof_backed: bool,
}

/// issue 1144 (#887 fold) -- summarize the projection (CAM2) leg's tear windows into a
/// [`ProjectionProof`]. Pure / Tier-0. See [`ProjectionProof`] for the scope + the report-only note.
pub fn summarize_projection_leg(cam2_windows: &[&TearStats]) -> ProjectionProof {
    let windows = cam2_windows.len();
    if windows == 0 {
        return ProjectionProof {
            windows: 0,
            observed_fraction: 0.0,
            tear_gate_clean: true,
            worst_tear_fraction: 0.0,
            aux_any_coverage: 0.0,
            hdmi1_proof_backed: false,
        };
    }
    let observed = cam2_windows
        .iter()
        .filter(|s| s.viability == TearSignalViability::Observed)
        .count();
    let observed_fraction = observed as f64 / windows as f64;
    let tear_gate_clean = cam2_windows.iter().all(|s| tear_gate_pass(s));
    let worst_tear_fraction = cam2_windows
        .iter()
        .map(|s| s.tear_fraction)
        .fold(0.0_f64, f64::max);
    let total_frames: u64 = cam2_windows.iter().map(|s| s.total_frames as u64).sum();
    let aux_any_coverage = if total_frames > 0 {
        cam2_windows
            .iter()
            .map(|s| s.aux_any_decode_fraction * s.total_frames as f64)
            .sum::<f64>()
            / total_frames as f64
    } else {
        0.0
    };
    // Report-only: at least one live (Observed) + clean projection window backs the HDMI-1 SCANOUT
    // proof. observed_fraction is carried so a stricter flip can demand more than "any".
    let hdmi1_proof_backed = observed_fraction > 0.0 && tear_gate_clean;
    ProjectionProof {
        windows,
        observed_fraction,
        tear_gate_clean,
        worst_tear_fraction,
        aux_any_coverage,
        hdmi1_proof_backed,
    }
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
        // 2 of 4 frames decoded BOTH aux marks; the same 2 decoded AT LEAST ONE.
        assert!((s.aux_decode_fraction - 0.5).abs() < 1e-9);
        assert!((s.aux_any_decode_fraction - 0.5).abs() < 1e-9);
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
        // A SINGLE torn frame is Observed but BELOW TEAR_FRAME_COUNT_FLOOR (6), so the two-term LIVE
        // gate PASSES it (a lone torn frame is within the green background; a sustained tear is
        // 16-201 frames). The rate alone (0.333 here) would fail, but the count term saves it.
        assert!(s.tear_frames < TEAR_FRAME_COUNT_FLOOR);
        assert!(
            tear_gate_pass(&s),
            "one torn frame is below the count floor -> the two-term gate passes it"
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
        assert_eq!(s.aux_any_decode_fraction, 0.0);
        assert_eq!(s.primary_dark_aux_alive_fraction, 0.0);
        assert_eq!(s.multi_path_suspect_frames, 0);
        assert_eq!(s.multi_path_suspect_fraction, 0.0);
        assert_eq!(s.max_cluster_count, 0);
        assert_eq!(s.max_multi_path_spread, 0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn aux_single_mark_is_operative_while_both_marks_read_zero_1196() {
        // The known-torn / projection-leg reality: a SINGLE aux mark decodes on nearly every frame
        // (the operative cross-band signal), while BOTH marks rarely survive — so aux_decode_fraction
        // (both marks) reads ~0.0 yet aux_any_decode_fraction is high. This is the honest refutation
        // of the "aux_decode_fraction 0.0 = dead aux" misread: every torn frame here is a
        // primary[X,X+1] pair + ONE aux mark from a later generation (union span > 1 = the tear).
        let mut frames = Vec::new();
        // 2 cross-band torn frames (one aux mark, one gen ahead) ...
        frames.push(f(&[100, 101], &[103]));
        frames.push(f(&[200, 201], &[203]));
        // ... + 8 healthy frames each carrying only ONE in-sync aux mark (the odd half).
        for i in 0..8u32 {
            let b = 1000 + i * 2;
            frames.push(f(&[b, b + 1], &[b + 1]));
        }
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 10);
        assert_eq!(
            s.aux_decode_fraction, 0.0,
            "no frame decoded BOTH aux marks — the misleading both-mark metric"
        );
        assert_eq!(
            s.aux_any_decode_fraction, 1.0,
            "every frame decoded at least ONE aux mark — the aux is fully operative"
        );
        assert_eq!(
            s.tear_frames, 2,
            "the two cross-band single-mark tears fire"
        );
        assert_eq!(s.viability, TearSignalViability::Observed);
    }

    // ---- issue 1196 LIVE-gate promotion ----

    #[test]
    fn the_tear_seam_is_live_1196() {
        // issue 1196: the projection-tap tear gate is PROMOTED to a LIVE blocking seam once the
        // known-torn run (1700989544) proved the signal fires and TEAR_FRACTION_CEILING is
        // calibrated. RED against the report-only placeholder (`gates_overall_pass()` == false).
        assert!(
            gates_overall_pass(),
            "issue 1196: the tear gate is promoted to a LIVE blocking gate"
        );
    }

    #[test]
    fn green_background_window_passes_the_calibrated_live_gate_1196() {
        // The REAL green Observed background mined across 37 v2.1 verdicts: the aux single-mark
        // cross-band occasionally reads one generation off on a healthy run — 3 such torn frames
        // out of 846 decodable = tear_fraction 0.003546 (the mined green MAX). The calibrated
        // ceiling (0.005) must PASS this (ZERO false positives on history). RED against the 0.0
        // placeholder ceiling, which would fail every Observed green window.
        let mut frames = vec![
            f(&[100, 101], &[103]), // primary[X,X+1] + one aux mark one gen ahead -> union span 3
            f(&[200, 201], &[203]),
            f(&[300, 301], &[303]),
        ];
        for i in 0..843u32 {
            let b = 1000 + i * 2;
            frames.push(f(&[b, b + 1], &[b, b + 1])); // healthy, aux in sync with primary
        }
        let s = window_tear_stats(&frames);
        assert_eq!(s.tear_frames, 3);
        assert_eq!(s.decodable_frames, 846);
        assert!(
            (s.tear_fraction - 3.0 / 846.0).abs() < 1e-9,
            "the mined green Observed MAX = 0.003546"
        );
        assert_eq!(s.viability, TearSignalViability::Observed);
        assert!(
            tear_gate_pass(&s),
            "the real green Observed background (0.00355) must PASS the calibrated 0.005 ceiling"
        );
        assert!(
            window_promotable(&s),
            "an Observed single-tile window is promotable (necessary, not sufficient)"
        );
    }

    #[test]
    fn known_torn_window_fails_the_calibrated_live_gate_1196() {
        // The known-torn run's smaller CAM2 projection window: 16 aux single-mark cross-band tears
        // out of 849 decodable = tear_fraction 0.018846, well above the 0.005 ceiling. Must FAIL
        // the live gate (a real induced scanout tear). Also a regression pin on the calibration
        // separation (green MAX 0.00355 << this 0.0188).
        let mut frames = Vec::new();
        for i in 0..16u32 {
            let b = 100 + i * 8;
            frames.push(f(&[b, b + 1], &[b + 3])); // cross-band tear, union span 3
        }
        for i in 0..833u32 {
            let b = 5000 + i * 2;
            frames.push(f(&[b, b + 1], &[b, b + 1]));
        }
        let s = window_tear_stats(&frames);
        assert_eq!(s.tear_frames, 16);
        assert_eq!(s.decodable_frames, 849);
        assert!(
            (s.tear_fraction - 16.0 / 849.0).abs() < 1e-9,
            "matches the known-torn run's 0.018846 window"
        );
        assert!(
            s.tear_fraction > TEAR_FRACTION_CEILING,
            "the induced tear sits above the calibrated ceiling"
        );
        assert!(
            !tear_gate_pass(&s),
            "a 0.0188 induced-tear window must FAIL the calibrated live gate"
        );
    }

    #[test]
    fn unproven_window_always_passes_the_scoped_gate_1196() {
        // The gate failure is SCOPED to Observed windows: an Unproven window carries tear_fraction
        // 0.0 (no torn frame) and always passes, regardless of the ceiling. A multi-tile suspect
        // window (Unproven, unscoreable) likewise passes — its inter-path skew is never a tear.
        let green = window_tear_stats(&[f(&[100, 101], &[100, 101]), f(&[102, 103], &[102, 103])]);
        assert_eq!(green.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&green));
        let multi_frames: Vec<(Vec<u32>, Vec<u32>)> =
            (0..5).map(|_| f(&[200, 201, 202, 203], &[])).collect();
        let multi = window_tear_stats(&multi_frames);
        assert_eq!(multi.viability, TearSignalViability::Unproven);
        assert!(
            tear_gate_pass(&multi),
            "a fully-suspect window has no scoreable tear"
        );
    }

    #[test]
    fn observed_multi_tile_window_never_fails_the_live_gate_1196() {
        // THE review-hardening (🔴): a MULTI-TILE window that is ALSO Observed (its few single-source
        // count-2 residual frames span > 1) must NEVER fail the LIVE gate — those spans are inter-path
        // skew, not a tear, and their tiny denominator gives a misleadingly high rate. Real multi-tile
        // run 1859005342 fails 4/10 windows WITHOUT this guard. Build a window that trips the rate AND
        // the count floor yet is dominated by multi-tile suspects: 8 single-source count-2 "tears" +
        // 20 multi-tile suspects.
        let mut frames: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
        for i in 0..8u32 {
            let b = 100 + i * 4;
            frames.push(f(&[b, b + 2], &[])); // single-source, span 2 -> counts as a "tear"
        }
        for _ in 0..20 {
            frames.push(f(&[200, 201, 202, 203], &[])); // multi-tile suspect
        }
        let s = window_tear_stats(&frames);
        assert_eq!(s.viability, TearSignalViability::Observed);
        assert_eq!(s.tear_frames, 8, "8 single-source count-2 frames score");
        assert!(
            s.tear_frames >= TEAR_FRAME_COUNT_FLOOR && s.tear_fraction > TEAR_FRACTION_CEILING,
            "both the count and rate terms are tripped — only the suspect guard saves it"
        );
        assert!(
            s.multi_path_suspect_fraction > MULTI_PATH_SUSPECT_CEILING,
            "20/28 suspect = 0.714 > the 0.10 ceiling"
        );
        assert!(
            tear_gate_pass(&s),
            "a multi-tile (unscoreable) window must NEVER fail the live gate (#1127 false-FAIL trap)"
        );
    }

    #[test]
    fn low_count_tear_passes_but_sustained_single_tile_tear_fails_1196() {
        // The count term (TEAR_FRAME_COUNT_FLOOR = 6): a 5-frame single-tile "tear" on a short window
        // trips the rate (5/50 = 0.10 >> 0.005) but NOT the count (5 < 6) -> PASSES (the green
        // background is a count of 1-3 frames; a 4-5 frame spike must not false-fail).
        let mut five = vec![
            f(&[100, 102], &[]),
            f(&[104, 106], &[]),
            f(&[108, 110], &[]),
            f(&[112, 114], &[]),
            f(&[116, 118], &[]),
        ];
        for i in 0..45u32 {
            let b = 1000 + i * 2;
            five.push(f(&[b, b + 1], &[]));
        }
        let s5 = window_tear_stats(&five);
        assert_eq!(s5.tear_frames, 5);
        assert!(
            s5.tear_fraction > TEAR_FRACTION_CEILING,
            "rate tripped (0.10)"
        );
        assert!(s5.tear_frames < TEAR_FRAME_COUNT_FLOOR);
        assert!(
            tear_gate_pass(&s5),
            "5 torn frames is below the count floor -> passes"
        );

        // A 6th torn frame crosses the count floor -> a sustained single-tile tear FAILS both terms.
        let mut six = five.clone();
        six.push(f(&[120, 122], &[]));
        let s6 = window_tear_stats(&six);
        assert_eq!(s6.tear_frames, 6);
        assert_eq!(s6.multi_path_suspect_frames, 0, "all single-tile");
        assert!(
            !tear_gate_pass(&s6),
            "6 torn single-tile frames trip BOTH the count floor and the rate ceiling -> FAIL"
        );
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

    #[test]
    fn signal_operable_surfaces_aux_blind_spot_1196() {
        // Empty run: not operable (nothing to judge).
        assert!(!signal_operable(&[]));
        // A run whose windows decode a single aux mark per frame (the live CAM2 shape) is OPERABLE.
        let live = window_tear_stats(&[f(&[100, 101], &[101]), f(&[102, 103], &[103])]);
        assert!((live.aux_any_decode_fraction - 1.0).abs() < 1e-9);
        assert!(
            signal_operable(std::slice::from_ref(&live)),
            "single-mark aux coverage ~1.0 is well above the 0.5 operability floor"
        );
        // A run where the aux collapsed (no aux mark decodes at all — both occluded) is NON-operable,
        // surfacing the blind spot even though the window is Unproven (which alone would look green).
        let blind = window_tear_stats(&[f(&[100, 101], &[]), f(&[102, 103], &[])]);
        assert_eq!(blind.aux_any_decode_fraction, 0.0);
        assert_eq!(blind.viability, TearSignalViability::Unproven);
        assert!(
            !signal_operable(std::slice::from_ref(&blind)),
            "zero aux coverage must read NON-operable (the issue-1101 blind-signal trap surfaced)"
        );
    }

    // issue 1144 (#887 fold) -- summarize_projection_leg tests.

    fn proj_win(
        viability: TearSignalViability,
        tear_fraction: f64,
        tear_frames: u32,
        aux_any: f64,
        total_frames: u32,
    ) -> TearStats {
        TearStats {
            total_frames,
            decodable_frames: total_frames,
            tear_frames,
            tear_fraction,
            max_spread: if tear_frames > 0 { 3 } else { 1 },
            aux_decode_fraction: 0.0,
            aux_any_decode_fraction: aux_any,
            primary_dark_aux_alive_fraction: 0.0,
            multi_path_suspect_frames: 0,
            multi_path_suspect_fraction: 0.0,
            max_cluster_count: 1,
            max_multi_path_spread: 0,
            viability,
        }
    }

    #[test]
    fn projection_leg_backed_when_observed_and_clean_1144() {
        // A green CAM2 projection run: one window Observed + clean (tear_fraction below the ceiling),
        // one Unproven (signal blind on that window). The HDMI-1 scanout proof IS backed.
        let obs = proj_win(TearSignalViability::Observed, 0.003, 3, 0.98, 848);
        let unp = proj_win(TearSignalViability::Unproven, 0.0, 0, 0.99, 847);
        let p = summarize_projection_leg(&[&obs, &unp]);
        assert_eq!(p.windows, 2);
        assert_eq!(p.observed_fraction, 0.5);
        assert!(p.tear_gate_clean);
        assert!(
            p.hdmi1_proof_backed,
            "an Observed + clean projection window backs HDMI-1: {p:?}"
        );
        let expect_aux = (0.98 * 848.0 + 0.99 * 847.0) / (848.0 + 847.0);
        assert!((p.aux_any_coverage - expect_aux).abs() < 1e-9);
    }

    #[test]
    fn projection_leg_not_backed_when_all_unproven_1144() {
        // No CAM2 window ever proved the tear signal live -> the HDMI-1 proof is NOT backed (a
        // signal-blind run cannot claim the projection was verified) even though the tear gate is
        // vacuously clean.
        let a = proj_win(TearSignalViability::Unproven, 0.0, 0, 0.99, 847);
        let b = proj_win(TearSignalViability::Unproven, 0.0, 0, 0.98, 848);
        let p = summarize_projection_leg(&[&a, &b]);
        assert_eq!(p.observed_fraction, 0.0);
        assert!(p.tear_gate_clean);
        assert!(
            !p.hdmi1_proof_backed,
            "an all-Unproven (blind) projection run is not backed: {p:?}"
        );
    }

    #[test]
    fn projection_leg_not_backed_when_torn_1144() {
        // A real projection tear: Observed, tear_fraction over the ceiling, count over the floor,
        // single-tile -> the tear gate fails and the HDMI-1 proof is NOT backed.
        let torn = proj_win(TearSignalViability::Observed, 0.02, 20, 0.97, 849);
        let p = summarize_projection_leg(&[&torn]);
        assert!(
            !p.tear_gate_clean,
            "a torn CAM2 window fails the tear gate: {p:?}"
        );
        assert!(!p.hdmi1_proof_backed);
        assert!((p.worst_tear_fraction - 0.02).abs() < 1e-9);
    }

    #[test]
    fn projection_leg_empty_not_backed_1144() {
        let p = summarize_projection_leg(&[]);
        assert_eq!(p.windows, 0);
        assert!(p.tear_gate_clean, "vacuously clean for zero windows");
        assert!(
            !p.hdmi1_proof_backed,
            "no projection window analyzed -> not backed"
        );
    }
}
