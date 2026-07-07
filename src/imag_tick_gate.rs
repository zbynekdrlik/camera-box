//! #461 — burn-less optical zero-loss gate for imag-nb (the 60fps low-latency IMAG box, EPIC
//! #466 Topology v2), extended by #463 to AND-in imag's OWN digital corner burn now that it has
//! one (run_id [`crate::probe::recording_latency::BURN_RUN_ID_IMAG`] = 911003, burned by the OBS
//! filter's new `Corner::BottomCenterLeft` — `vendor/distroav/src/burn-geom.hpp`).
//!
//! Every OTHER node's zero-loss proof (`probe::burn_contiguity::burn_contiguity`) is a
//! first..=last CONTIGUITY check over a DIGITAL burn id injected at the OBS render tick. Before
//! #463, imag had no burn — but it did not strictly need one: imag records the cam2 painter's
//! 60Hz dual-QR at 60fps, 1:1, with NO 60->30 beat (the beat that forces strih/stream to treat
//! the optical read as diagnostic-only lives at the cam->strih / strih->stream hops, not here).
//! So the cam2 OPTICAL tick's own first..=last contiguity (step=1) IS a genuine zero-loss proof
//! for imag on its own: every painted tick that imag's camera captured maps 1:1 onto a recorded
//! frame, so a missing tick integer in the analyzed span means imag's camera FAILED to capture
//! that instant — the same "any gap in the span is a candidate drop" logic `burn_contiguity`
//! applies to a digital id, applied here to the optical tick instead.
//!
//! **#463 — now imag ALSO carries a digital burn, so BOTH signals gate it (stricter, per the
//! strict-test mandate — never accept a weaker proof once a stronger one is available):**
//! [`ImagVerdict`] ANDs the optical tick contiguity with the burn's OWN first..=last contiguity
//! WHEN the burn is present in the recording. A recording with NO burn decoded at all (an older
//! recording, or a build not yet carrying the corner burn) falls back to the ORIGINAL
//! optical-only proof — see [`ImagVerdict::is_zero_loss`].
//!
//! This is a SIBLING of `probe::burn_contiguity::burn_contiguity`, deliberately duplicated as a
//! crate-root, non-probe module rather than reused from `probe::` — the WHOLE `probe` module is
//! `#[cfg(feature = "probe")]` (CI-only), so a pure decision that lives there can never be
//! RED->GREEN-verified locally. Mirrors the `reannounce.rs` / `colour_scale.rs` /
//! `recording_span_gate.rs` Tier-0 seam pattern: this module holds ONLY primitive-typed pure
//! logic (`&[u32]` in, a plain result struct out) so it compiles + tests on DEFAULT features; the
//! probe-gated `bin/recording-verdict` extracts `RecordingFrame::tick` (+ the burn ids via
//! `probe::recording_latency::burn_ids_in` / `probe::burn_contiguity::burn_contiguity`) and
//! calls in.

/// The tick-contiguity verdict for imag's optical proof: first..=last integer contiguity over
/// every distinct cam2 painted tick imag's recording decoded. Mirrors
/// `probe::burn_contiguity::NodeContiguity`'s shape (so the probe-gated caller can convert this
/// straight into the existing `NodeContiguity` / `NodeVerdict` reporting machinery) without
/// depending on that probe-gated type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickContiguity {
    /// First tick value seen (the start of the analyzed span). `None` ⇒ no tick decoded at all.
    pub first_tick: Option<u32>,
    /// Last tick value seen (the end of the analyzed span). `None` ⇒ no tick decoded at all.
    pub last_tick: Option<u32>,
    /// How many DISTINCT tick values were decoded.
    pub present_count: u32,
    /// How many tick values the contiguous span `first..=last` should contain.
    pub expected_count: u32,
    /// The exact tick values missing from `first..=last`, sorted ascending. Empty ⇒ contiguous.
    pub missing_ticks: Vec<u32>,
}

impl TickContiguity {
    /// ZERO loss ⇔ the optical tick sequence is contiguous (no missing tick value). A recording
    /// with NO decoded tick at all (`first_tick == None`) is NOT a pass — there is nothing proven
    /// zero on, mirroring `NodeContiguity::is_contiguous`'s same "empty is not a pass" rule.
    pub fn is_contiguous(&self) -> bool {
        self.first_tick.is_some() && self.missing_ticks.is_empty()
    }
}

/// #463 — imag's FULL zero-loss verdict: the optical tick contiguity ANDed with the digital
/// corner-burn contiguity, WHEN the burn is present in the recording. Kept as a separate pure
/// combinator (rather than folding the AND into the caller) so the "burn present but broken" vs
/// "burn absent, fall back to optical-only" decision is unit-tested here, Tier-0, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagVerdict {
    /// Whether the cam2 optical tick sequence was contiguous ([`TickContiguity::is_contiguous`]).
    pub optical_contiguous: bool,
    /// `None` ⇒ NO digital burn id was decoded anywhere in the recording at all — the pre-#463
    /// state (an older recording, or a build not yet carrying imag's corner burn). The verdict
    /// falls back to the optical proof alone, unchanged from pre-#463 behaviour.
    /// `Some(contiguous)` ⇒ the burn WAS decoded; `contiguous` is whether ITS OWN first..=last
    /// span was gap-free. #463's whole point: once a stronger (digital) proof exists, it must
    /// ALSO hold — a present-but-gappy burn now fails the node even if the optical read is clean.
    pub burn_contiguous: Option<bool>,
}

impl ImagVerdict {
    /// ZERO loss ⇔ the optical tick is contiguous AND (no burn was present in this recording OR
    /// the burn is ALSO contiguous). See the struct doc for the fallback rationale — this is
    /// STRICTER than pre-#463 (never looser): a node with a decoded-but-gappy burn now fails even
    /// though its optical read alone was clean, per the strict-test mandate.
    pub fn is_zero_loss(&self) -> bool {
        self.optical_contiguous && optional_signal_ok(self.burn_contiguous)
    }
}

/// #463 — is an OPTIONAL second proof signal "not a problem"? `None` (the signal does not
/// apply at all — e.g. no digital burn was decoded anywhere in the recording) is ALWAYS fine,
/// nothing to fail on; `Some(false)` (the signal WAS present but broken) is the only failing
/// case; `Some(true)` (present and clean) is fine. Shared by [`ImagVerdict::is_zero_loss`] and
/// `NodeVerdict::imag_burn_ok` (`src/bin/recording-verdict.rs`) so the "absent is fine,
/// present-but-broken fails" rule lives in exactly ONE place instead of being independently
/// reimplemented at each call site (the #463 review caught the duplication).
pub fn optional_signal_ok(present_and_contiguous: Option<bool>) -> bool {
    present_and_contiguous.unwrap_or(true)
}

/// Build imag's [`ImagVerdict`] from its optical [`TickContiguity`] and, separately, whether ANY
/// digital corner-burn id was decoded at all in the recording (`burn_present`) and — only when it
/// was — whether that burn's OWN span was contiguous (`burn_contiguous_if_present`). Splitting the
/// two burn booleans (rather than a single `Option<bool>` at the call site) keeps the caller
/// honest: it cannot accidentally collapse "burn absent" and "burn present and contiguous" into
/// the same `true`, which would silently hide a real burn from ever being checked.
pub fn imag_verdict(
    optical: &TickContiguity,
    burn_present: bool,
    burn_contiguous_if_present: bool,
) -> ImagVerdict {
    ImagVerdict {
        optical_contiguous: optical.is_contiguous(),
        burn_contiguous: burn_present.then_some(burn_contiguous_if_present),
    }
}

/// THE pure check: given every cam2 optical tick value decoded from imag's recording (duplicates
/// and any order allowed — a tick sampled twice, or frames in file order, are never loss), report
/// whether the first..=last integer span is contiguous and, if not, every missing tick value.
///
/// Identical algorithm to `probe::burn_contiguity::burn_contiguity`, intentionally duplicated (see
/// the module doc) so it is unit-testable without the `probe` feature.
///
/// An empty input ⇒ `first_tick == None` ⇒ NOT contiguous (nothing proven). A single-tick input is
/// trivially contiguous (a span of one; nothing can be missing inside it).
pub fn tick_contiguity(ticks: &[u32]) -> TickContiguity {
    use std::collections::BTreeSet;
    let present: BTreeSet<u32> = ticks.iter().copied().collect();
    let first_tick = present.iter().next().copied();
    let last_tick = present.iter().next_back().copied();
    let (first, last) = match (first_tick, last_tick) {
        (Some(f), Some(l)) => (f, l),
        _ => {
            return TickContiguity {
                first_tick: None,
                last_tick: None,
                present_count: 0,
                expected_count: 0,
                missing_ticks: Vec::new(),
            };
        }
    };
    // expected = last - first + 1 (size of the contiguous integer span). Saturating math so a
    // degenerate full-u32-range span (unreachable for a run-bounded tick counter, but defensive)
    // can never panic on a debug-build overflow — mirrors `burn_contiguity`'s own guard.
    let expected_count = last.saturating_sub(first).saturating_add(1);
    let missing_ticks: Vec<u32> = (first..=last).filter(|t| !present.contains(t)).collect();
    TickContiguity {
        first_tick: Some(first),
        last_tick: Some(last),
        present_count: present.len() as u32,
        expected_count,
        missing_ticks,
    }
}

/// #480 — confirmed live root cause of imag's digital corner burn (run_id 911003) reading
/// as ~50% "missing" under the OLD strict-1:1 [`tick_contiguity`]-style check: imag's OBS runs
/// Studio Mode ON (`studio-mode-always-on.md`), and the Studio-Mode "Program" monitor widget
/// re-renders the ACTIVE (on-air) scene as a SEPARATE `obs_display_t` draw pass, independent of
/// the main output render that actually reaches the encoder/recording (a well-known OBS
/// filter-authoring caveat: `obs_source_video_render` on a filter fires once per VIEW that
/// composites the source, not once per emitted output frame). The DistroAV burn filter's
/// `frame_id` counter (`vendor/distroav/src/ndi-burn-filter.cpp:370`,
/// `burn_filter_videorender`) increments on EVERY `video_render` call, so it advances EXACTLY
/// TWICE per recorded output frame — once for the Program monitor's own draw, once for the
/// actual output render that lands in the recording. Only ONE of the two lands on disk, so the
/// recorded burn-id sequence is a clean, DETERMINISTIC every-other-integer alternation (evens
/// present / odds absent, confirmed on a live 300s rig recording, run_id 911003: 18596 of 37191
/// ids present, ALL missing ids odd) — a much CLEANER signal than strih's own free-running burn
/// (#360), which measured an IRREGULAR per-frame step (mean ~4, range 0-10) and was therefore
/// modeled with unconditional gap-ignore. imag's clean, reproducible step=2 is a strictly
/// STRONGER case: it supports the full decimation-aware EXCESS-GAP charging model
/// (`probe::burn_contiguity::burn_contiguity_in_window_with_step`'s doc, mirrored here) instead
/// of blanket gap-ignore, so a genuine dropped output frame (a forward gap LARGER than the
/// double-render step) is STILL caught as a real loss — never silently waved through.
///
/// This is a Rust-only fix: it does NOT touch the vendored `vendor/distroav` C++ filter (that
/// would rebuild `distroav.so` and require a fresh live pin of `genlock_build_sha_imag` +
/// on-rig re-verification, out of scope for this bug-fix PR — see `vendor/README.md`). The
/// alternative "gate the counter to the program render pass only" fix from the C++ side may be
/// revisited separately if the vendored filter can cheaply distinguish the two render passes;
/// this Rust-side model already restores a correctly-modeled, still-strict gate today.
pub const IMAG_BURN_RENDER_STEP: u32 = 2;

/// The step-aware contiguity verdict for imag's OWN digital corner burn (run_id 911003, #463) —
/// the second, independently-proven signal ANDed with the optical [`TickContiguity`] in
/// [`ImagVerdict`]. Same field shape as [`TickContiguity`] (and the probe-gated
/// `NodeContiguity` the caller converts this into) so the existing reporting/JSON machinery is
/// unchanged; only HOW `missing_ids` is computed differs (step-aware, not strict 1:1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnStepContiguity {
    /// First burn id seen (the start of the analyzed span). `None` ⇒ no burn at all.
    pub first_id: Option<u32>,
    /// Last burn id seen (the end of the analyzed span). `None` ⇒ no burn at all.
    pub last_id: Option<u32>,
    /// How many DISTINCT burn ids were decoded (diagnostic only — NOT part of the pass/fail
    /// decision, which is `first_id.is_some() && missing_ids.is_empty()`).
    pub present_count: u32,
    /// How many step-grid points (`first, first+step, .., last`) the contiguous span should
    /// contain — the step-aware analogue of [`TickContiguity::expected_count`].
    pub expected_count: u32,
    /// Every step-grid id genuinely missing (a forward gap LARGER than `step` between two
    /// present ids charges the excess — `gap / step - 1`, integer division so beat jitter of
    /// `step ± 1` charges zero, mirroring `burn_contiguity_in_window_with_step`'s excess-gap
    /// math). A clean `step`-spaced run has this empty. Sorted ascending.
    pub missing_ids: Vec<u32>,
}

impl BurnStepContiguity {
    /// ZERO loss for this signal ⇔ the step-grid sequence is contiguous (no missing id). A burn
    /// with NO id decoded at all (`first_id == None`) is NOT a pass — mirrors
    /// [`TickContiguity::is_contiguous`] / `NodeContiguity::is_contiguous`.
    pub fn is_contiguous(&self) -> bool {
        self.first_id.is_some() && self.missing_ids.is_empty()
    }
}

/// THE pure step-aware check (#480): given every burn id decoded for imag's digital corner burn
/// (duplicates and any order allowed — a set-membership check over the sorted present ids, same
/// as [`tick_contiguity`]/`burn_contiguity`), report whether the sequence is contiguous ON THE
/// `step`-SPACED GRID and, if not, every step-grid id genuinely missing.
///
/// `step == 1` degenerates to the exact same strict first..=last contiguity as
/// [`tick_contiguity`] (every present id is its own grid point, so ANY gap is charged in full) —
/// this function is a strict superset, not a separate weaker path.
///
/// A forward gap of EXACTLY `step` between two consecutive present ids is the expected
/// free-running decimation (imag's Studio-Mode double-render, [`IMAG_BURN_RENDER_STEP`]) and is
/// NOT loss. A gap LARGER than `step` means one or more real output frames never reached the
/// recording between them — the excess (`gap / step - 1`, integer division so a beat-jitter gap
/// of `step ± 1` charges zero — the SAME tolerance `burn_contiguity_in_window_with_step` uses)
/// is reported as genuinely missing step-grid ids.
///
/// An empty input ⇒ `first_id == None` ⇒ NOT contiguous (nothing proven). A single-id input is
/// trivially contiguous (a span of one; nothing can be missing inside it).
pub fn burn_step_contiguity(ids: &[u32], step: u32) -> BurnStepContiguity {
    use std::collections::BTreeSet;
    let step = step.max(1);
    let present: BTreeSet<u32> = ids.iter().copied().collect();
    let first_id = present.iter().next().copied();
    let last_id = present.iter().next_back().copied();
    let (first, last) = match (first_id, last_id) {
        (Some(f), Some(l)) => (f, l),
        _ => {
            return BurnStepContiguity {
                first_id: None,
                last_id: None,
                present_count: 0,
                expected_count: 0,
                missing_ids: Vec::new(),
            };
        }
    };
    // expected = the number of step-grid points from first to last inclusive. Diagnostic only
    // (mirrors `TickContiguity::expected_count`'s role); saturating math so a degenerate
    // full-u32-range span can never wrap/panic — mirrors `tick_contiguity`'s own guard.
    let expected_count = (last.saturating_sub(first) / step).saturating_add(1);
    let mut missing_ids: Vec<u32> = Vec::new();
    let mut prev = first;
    for &id in present.iter().skip(1) {
        // `present` (a BTreeSet) iterates in ascending order, so `id > prev` always — plain u32
        // subtraction can never underflow here.
        let gap = id - prev;
        if gap > step {
            // `gap > step` ⇒ `gap / step >= 1` ⇒ `excess` can never underflow. Every
            // `prev + k*step` for `k <= excess` stays `< id` (by construction, see the doc
            // comment above) — never overflows past the next present id.
            let excess = gap / step - 1;
            for k in 1..=excess {
                missing_ids.push(prev + k * step);
            }
        }
        prev = id;
    }
    BurnStepContiguity {
        first_id: Some(first),
        last_id: Some(last),
        present_count: present.len() as u32,
        expected_count,
        missing_ids,
    }
}

/// #580 — imag's PRIMARY (cam2 optical) zero-loss expected per-recorded-frame step: cam2's 60Hz
/// painter and imag's 60fps capture are the SAME rate (1:1, no beat — see the module doc), so the
/// optical tick sequence should advance by exactly this much per captured frame. Named for
/// symmetry with [`IMAG_BURN_RENDER_STEP`] so the value's origin is explicit rather than an
/// inline magic `1` at every call site.
pub const IMAG_OPTICAL_EXPECTED_STEP: u32 = 1;

/// #580 — the optical-BEAT verdict for imag's PRIMARY (cam2 dual-QR) zero-loss signal, replacing
/// strict step-1 [`tick_contiguity`] as the hard optical gate. cam2's 60Hz monitor and the
/// broadcast camera's free-running 60fps are two UNSYNCHRONIZED same-rate clocks — they BEAT: the
/// camera captures some painter ticks twice (a duplicate) and misses others (a skip), and when
/// dups and skips are BALANCED (frame-count conserved) that is ZERO NET loss, not a fault. Strict
/// step-1 contiguity false-fails a truly-zero-loss run whenever ANY skip occurs, even when fully
/// compensated. Confirmed live (run 572001, post-#575 trim + #576 calibration): expected=21870,
/// frames=21873, present=21851, missing=19, dups=22, surplus=-3 — a genuinely zero-net-loss run
/// that strict step-1 (missing=19) false-fails.
///
/// Two independent checks, both required (see [`Self::is_net_zero`]):
/// - **advance-guard** ([`Self::is_advancing`]): the tick sequence must genuinely ADVANCE at
///   ~[`Self::expected_step`] — a frozen/stuck optical read (the camera stuck on one QR: the tick
///   range collapses, `avg_step` ≈ 0) FAILS. This is STRICTER than the OLD strict-step-1 gate,
///   which ALSO false-passed a frozen read (`first_tick == last_tick` is trivially "contiguous",
///   `missing_ticks` empty) — #580 closes that pre-existing hole, it does not open a new one.
/// - **net-zero** ([`Self::is_net_zero`]'s `surplus <= 0` term): `surplus = expected_count -
///   frames_count` — an AGGREGATE window count of painter ticks NET missing after every
///   duplicate-oversampled frame is credited against a skip. `surplus <= 0` ⇒ dups net-cover skips
///   across the window ⇒ OPTICAL tracking PASS. `surplus > 0` ⇒ more skips than dups ⇒ a net
///   optical loss ⇒ FAIL. This is aggregate, NOT per-value (see [`Self::is_net_zero`]'s doc): a real
///   one-off drop offset by an unrelated beat dup can still net to `<= 0` — deliberate (a beat skip
///   and a real drop are optically indistinguishable at 60/60), with per-frame DELIVERY proven
///   independently by the STRICT digital burn ANDed in `NodeVerdict::is_zero` (#463).
///
/// This is the SAME `avg_step` / `surplus` beat math already computed (diagnostic-only) in
/// `probe::recording_verdict::verdict` (`expected_step`, `surplus`, `beat_balanced` — see that
/// function's doc), promoted here into imag's HARD optical gate. For `expected_step == 1` (imag's
/// only real configuration, [`IMAG_OPTICAL_EXPECTED_STEP`]) `verdict()`'s `surplus = sum_steps -
/// num_pairs` telescopes to EXACTLY `expected_count - frames_count` (`sum_steps` telescopes to
/// `last - first` for ANY chronological walk regardless of individual step values, `num_pairs =
/// frames_count - 1`, so `surplus = (last-first) - (frames_count-1) = expected_count -
/// frames_count`) — a faithful port of the already-proven formula, not a new one. The final step
/// `(last - first) + 1 == expected_count` assumes the CHRONOLOGICAL endpoints equal the NUMERIC
/// min/max — i.e. a monotonically non-decreasing capture, which a genuinely advancing tick always
/// is. The CODE does not rely on that equality (it takes `expected_count`/`present_count` straight
/// from [`tick_contiguity`]'s BTreeSet and `avg_step` from the positional endpoints, independently);
/// a misdecoded OUT-OF-ORDER tick would merely push `avg_step` off `expected_step` and FAIL
/// [`Self::is_advancing`] — a SAFE false-FAIL direction, never a false-PASS.
///
/// A minor, deliberately-accepted simplification vs `verdict()`: `verdict()` walks the RAW frame
/// stream and breaks the pairing chain across an undecodable (`None`) frame, so a step is never
/// computed ACROSS such a hole; here `ticks_in_order` has ALREADY had undecodable frames excluded
/// (see [`optical_beat_net_zero`]), so an adjacent pair MAY span a former undecodable hole. This is
/// negligible for imag: the #376 optical-undecodable floor caps that hole rate near zero
/// (`OPTICAL_UNDECODABLE_RATE_MAX` in `bin/recording-verdict.rs`), and it is a SEPARATE, unchanged
/// hard gate (`NodeVerdict::optical_undecodable_ok`) ANDed alongside this one regardless.
///
/// `Serialize` is derived so `NodeVerdict` (which stores the full verdict, not just its
/// `is_net_zero()` bool, #580 review finding C) can keep its own blanket `#[derive(Serialize)]` —
/// `bin/recording-verdict.rs`'s `node_verdict_json` still hand-picks individual fields for the
/// actual JSON output, so this derive exists for compile-time compatibility, not because the
/// whole struct is serialized wholesale anywhere today.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct OpticalBeatVerdict {
    /// The average per-sample tick step over the analyzed (already boundary-trimmed) window, in
    /// the CHRONOLOGICAL order the samples were captured. `0.0` when there are fewer than 2
    /// samples (nothing to average over — never proven advancing, see [`Self::is_advancing`]).
    pub avg_step: f64,
    /// The step the optical tick SHOULD advance by per captured frame ([`IMAG_OPTICAL_EXPECTED_STEP`]
    /// for imag's real 60Hz/60fps configuration). Clamped to `>= 1` (a `0` would make every
    /// forward motion read as infinite deviation).
    pub expected_step: u32,
    /// The size of the contiguous VALUE span `first_tick..=last_tick` (mirrors
    /// [`TickContiguity::expected_count`]) — how many distinct painter ticks the analyzed window
    /// SHOULD have captured if nothing were lost.
    pub expected_count: u32,
    /// How many DISTINCT tick values were actually decoded (mirrors
    /// [`TickContiguity::present_count`]).
    pub present_count: u32,
    /// How many raw (possibly duplicate) decodable samples were analyzed — the count BEFORE
    /// deduplication. `frames_count - present_count` is the number of duplicate-oversampled
    /// frames; `expected_count - present_count` is the number of genuinely skipped ticks.
    pub frames_count: u32,
    /// `expected_count as i64 - frames_count as i64`. `<= 0` ⇒ net-zero (every distinct tick is
    /// covered by the raw frame count); `> 0` ⇒ a genuine net loss of that many painter ticks.
    pub surplus: i64,
}

impl OpticalBeatVerdict {
    /// Does the optical tick sequence genuinely ADVANCE at (very close to) [`Self::expected_step`]
    /// per sample? `false` for: no samples at all (`expected_count == 0`); exactly ONE decodable
    /// sample (no adjacent pair exists to prove any advance at all — deliberately NOT a trivial
    /// pass, unlike [`TickContiguity::is_contiguous`]'s single-value case, because a lone sample
    /// proves nothing about the camera tracking a moving painter tick); or a FROZEN/stuck read
    /// (`avg_step` rounds to something other than `expected_step` — most starkly `0` for a fully
    /// stuck camera).
    pub fn is_advancing(&self) -> bool {
        self.expected_count > 0
            && self.frames_count > 1
            && self.avg_step.round() as i64 == self.expected_step as i64
    }

    /// THE #580 pass/fail decision: genuinely advancing AND the analyzed span is net-zero loss
    /// (`surplus <= 0`); STRICTER than the old strict-step-1 check for a frozen/stuck read (see
    /// [`Self::is_advancing`]'s doc).
    ///
    /// `surplus` is a WHOLE-WINDOW AGGREGATE (`expected_count - frames_count`), NOT a per-value
    /// pairing — so this is an OPTICAL-TRACKING proof, not a per-frame delivery proof. A genuine
    /// one-off optical drop CAN be numerically offset to `surplus <= 0` by an unrelated,
    /// naturally-occurring beat duplicate elsewhere in the same window (e.g. a real skip of 110
    /// offset by a beat dup at 121), so this term ALONE is NOT sufficient to prove every frame was
    /// delivered — and the aggregate is deliberate: at matched 60Hz/60fps a beat skip and a real
    /// drop are OPTICALLY indistinguishable, so per-value pairing would re-introduce the exact
    /// 572001 false-FAIL #580 exists to close. Per-frame DELIVERY is proven independently by the
    /// STRICT digital corner burn (`NodeVerdict::imag_burn_ok`, #463 — a clean free-running render
    /// tick where a genuinely dropped frame DOES show a gap); `NodeVerdict::is_zero` ANDs both, so a
    /// real drop coincidentally optical-offset here is still caught by the burn. On the pre-#463
    /// optical-only fallback (no burn) that aggregate-offset edge is unguarded — tracked as a
    /// follow-up; the live rig always paints the burn.
    pub fn is_net_zero(&self) -> bool {
        self.is_advancing() && self.surplus <= 0
    }
}

/// Build an [`OpticalBeatVerdict`] from PRECOMPUTED aggregate counts — the seam that lets a
/// REAL-DATA-derived fixture (e.g. the exact confirmed 572001 numbers) be asserted directly
/// without reconstructing a synthetic ~21873-sample chronological tick sequence just to recompute
/// the same aggregates [`optical_beat_net_zero`] would derive from it.
pub fn optical_beat_verdict_from_counts(
    expected_count: u32,
    present_count: u32,
    frames_count: u32,
    avg_step: f64,
    expected_step: u32,
) -> OpticalBeatVerdict {
    OpticalBeatVerdict {
        avg_step,
        expected_step: expected_step.max(1),
        expected_count,
        present_count,
        frames_count,
        surplus: expected_count as i64 - frames_count as i64,
    }
}

/// Build an [`OpticalBeatVerdict`] straight from the CHRONOLOGICAL (capture order preserved,
/// duplicates allowed, undecodable frames already excluded) trimmed optical tick sequence — the
/// entry point [`crate`]'s probe-gated `node_verdict_for_imag` glue calls with imag's own
/// boundary-trimmed tick samples.
///
/// `avg_step` is derived from the walk of consecutive per-sample steps in the GIVEN order: the sum
/// of consecutive differences ALWAYS telescopes to `ticks_in_order.last() - ticks_in_order.first()`
/// regardless of the individual dup/skip values in between (a pure algebraic identity), so this is
/// exactly the same `avg_step` `probe::recording_verdict::verdict`'s diagnostic computes.
pub fn optical_beat_net_zero(ticks_in_order: &[u32], expected_step: u32) -> OpticalBeatVerdict {
    optical_beat_from_contiguity(
        ticks_in_order,
        &tick_contiguity(ticks_in_order),
        expected_step,
    )
}

/// #580 review finding-2 — the SAME verdict as [`optical_beat_net_zero`] but reusing a
/// [`tick_contiguity`] result the caller ALREADY computed for the raw strict `contiguity` field it
/// still prints (`bin/recording-verdict.rs`'s `node_verdict_for_imag`), so the identical slice is
/// not walked into a second `BTreeSet`. `tc` MUST be `tick_contiguity(ticks_in_order)` for the SAME
/// slice — the aggregate counts (`expected_count`/`present_count`) come from it.
///
/// `avg_step` derives from the CHRONOLOGICAL endpoints, `(last - first) / (frames - 1)`: the sum of
/// consecutive per-sample steps ALWAYS telescopes to `ticks_in_order.last() - ticks_in_order.first()`
/// regardless of the dup/skip values between (a pure algebraic identity), so this is byte-identical
/// to walking `.windows(2)`. Note it is NOT `tc.last_tick - tc.first_tick`: those are the SORTED
/// min/max, not the positional endpoints a non-monotonic glitch could distinguish.
pub fn optical_beat_from_contiguity(
    ticks_in_order: &[u32],
    tc: &TickContiguity,
    expected_step: u32,
) -> OpticalBeatVerdict {
    let frames_count = ticks_in_order.len() as u32;
    let avg_step = match (ticks_in_order.first(), ticks_in_order.last()) {
        (Some(&first), Some(&last)) if frames_count > 1 => {
            (last as f64 - first as f64) / (frames_count as f64 - 1.0)
        }
        _ => 0.0,
    };
    optical_beat_verdict_from_counts(
        tc.expected_count,
        tc.present_count,
        frames_count,
        avg_step,
        expected_step,
    )
}

/// #576 — minimum distinct burn ids [`calibrate_burn_step`] requires before it trusts a
/// calibrated mode over the [`IMAG_BURN_RENDER_STEP`] fallback (fewer distinct ids than this
/// yields too few consecutive deltas to call any one value "dominant" rather than noise).
const MIN_IDS_FOR_STEP_CALIBRATION: usize = 4;

/// #576 — self-calibrate the free-running burn step from the OBSERVED cadence instead of
/// trusting a hardcoded constant to still match the live render pipeline. #480 confirmed step 2
/// at the time (OBS Studio-Mode double-render); the #572 live-rig investigation (run 554307)
/// found the REAL rig now free-running at step 3 (e.g. consecutive ids 22197/22200/22203) —
/// Studio-Mode render timing is not a stable API contract, so a hardcoded constant is latent
/// brittleness: it can attribute the WRONG grid ids (and possibly the wrong COUNT) to a future
/// genuine drop. imag's corner burn is a CLEAN free-running render tick (one dominant,
/// near-uniform step) — UNLIKE cam1's own capture burn, which rides a genuine 60→30 beat and
/// where a naive modal/median step is WRONG (#571) — so the MODE of consecutive present-id
/// deltas is the correct estimator HERE.
///
/// Ties (two deltas equally frequent) resolve to the SMALLER delta — the stricter, safer choice
/// whenever ambiguous, never the looser one. Falls back to [`IMAG_BURN_RENDER_STEP`] when there
/// are fewer than [`MIN_IDS_FOR_STEP_CALIBRATION`] distinct ids to trust a calibrated estimate
/// (never invent a number from noise — mirrors every other pure gate's "not enough signal ⇒ fall
/// back to the safe/known default" rule).
pub fn calibrate_burn_step(ids: &[u32]) -> u32 {
    use std::collections::{BTreeSet, HashMap};
    // Dedup + sort first (mirrors every other function in this module) — a duplicate id must
    // never manufacture a spurious zero-delta.
    let present: BTreeSet<u32> = ids.iter().copied().collect();
    if present.len() < MIN_IDS_FOR_STEP_CALIBRATION {
        return IMAG_BURN_RENDER_STEP;
    }
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut prev: Option<u32> = None;
    for id in present {
        if let Some(p) = prev {
            // `present` iterates ascending (BTreeSet) and is deduped, so `id > p` always —
            // plain subtraction can never underflow, and the delta can never be 0.
            *counts.entry(id - p).or_insert(0) += 1;
        }
        prev = Some(id);
    }
    // The MODE (most frequent delta) is the dominant free-running step. `Reverse(delta)` in the
    // tie-break key makes a SMALLER delta compare as "more", so ties resolve to the stricter
    // (smaller) step, never the looser one. `counts` is never empty here (>=3 deltas guaranteed
    // by the length check above), so `unwrap_or` is defensive only.
    counts
        .into_iter()
        .max_by_key(|&(delta, count)| (count, std::cmp::Reverse(delta)))
        .map(|(delta, _)| delta.max(1))
        .unwrap_or(IMAG_BURN_RENDER_STEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_not_contiguous_461() {
        let tc = tick_contiguity(&[]);
        assert_eq!(tc.first_tick, None);
        assert_eq!(tc.last_tick, None);
        assert_eq!(tc.present_count, 0);
        assert_eq!(tc.expected_count, 0);
        assert!(tc.missing_ticks.is_empty());
        assert!(
            !tc.is_contiguous(),
            "no tick decoded at all must never read as a zero-loss pass"
        );
    }

    #[test]
    fn single_tick_is_trivially_contiguous_461() {
        let tc = tick_contiguity(&[42]);
        assert_eq!(tc.first_tick, Some(42));
        assert_eq!(tc.last_tick, Some(42));
        assert_eq!(tc.present_count, 1);
        assert_eq!(tc.expected_count, 1);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn a_clean_run_of_consecutive_ticks_is_contiguous_461() {
        let ticks: Vec<u32> = (100..=200).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(100));
        assert_eq!(tc.last_tick, Some(200));
        assert_eq!(tc.present_count, 101);
        assert_eq!(tc.expected_count, 101);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn a_single_missing_tick_in_the_middle_fails_461() {
        // 100..=200 minus 150 -> imag's camera failed to capture that one painted instant.
        let ticks: Vec<u32> = (100..=200).filter(|&t| t != 150).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(100));
        assert_eq!(tc.last_tick, Some(200));
        assert_eq!(tc.present_count, 100);
        assert_eq!(tc.expected_count, 101);
        assert_eq!(tc.missing_ticks, vec![150]);
        assert!(
            !tc.is_contiguous(),
            "a single missing tick value inside the span is a candidate dropped frame"
        );
    }

    #[test]
    fn multiple_missing_ticks_are_all_reported_sorted_461() {
        let ticks: Vec<u32> = (0..=10).filter(|t| ![3, 7, 8].contains(t)).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.missing_ticks, vec![3, 7, 8]);
        assert!(!tc.is_contiguous());
    }

    #[test]
    fn duplicates_do_not_break_contiguity_461() {
        // A tick sampled on more than one recorded frame (e.g. a slow-shutter straddle) is never
        // loss -- the SAME painted instant, not a second one.
        let mut ticks: Vec<u32> = (0..=20).collect();
        ticks.extend([5, 5, 5, 12, 12]);
        let tc = tick_contiguity(&ticks);
        assert_eq!(
            tc.present_count, 21,
            "duplicates collapse to distinct ticks"
        );
        assert_eq!(tc.expected_count, 21);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn out_of_order_input_is_handled_identically_461() {
        // Recorded-frame order is irrelevant -- this is a SET membership test over first..=last.
        let mut ticks: Vec<u32> = (0..=50).collect();
        ticks.reverse();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(0));
        assert_eq!(tc.last_tick, Some(50));
        assert!(tc.is_contiguous());
    }

    #[test]
    fn expected_count_saturates_instead_of_panicking_on_overflow() {
        // Defensive: `last - first + 1` must never PANIC on a debug-build overflow, mirroring
        // `burn_contiguity`'s own guard. A run-bounded tick counter never realistically reaches
        // u32::MAX, so this checks only the saturating ARITHMETIC in isolation — NOT the full
        // `tick_contiguity` (which additionally walks every integer in the span to find gaps;
        // walking a genuine 4-billion-wide span is intentionally never exercised, same as
        // `burn_contiguity` never tests it end-to-end either).
        let expected_count = u32::MAX.saturating_sub(0).saturating_add(1);
        assert_eq!(
            expected_count,
            u32::MAX,
            "saturates instead of overflowing to 0"
        );
    }

    // ---- #463 — imag's optical+burn AND gate (ImagVerdict) ----

    #[test]
    fn no_burn_present_falls_back_to_optical_only_463() {
        // Pre-#463 behaviour preserved: an older recording (or a build not yet carrying imag's
        // corner burn) has NO burn id decoded at all — the verdict must fall back to the optical
        // proof alone, in BOTH directions (optical clean passes; optical broken still fails).
        let clean = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&clean, false, false);
        assert_eq!(
            v.burn_contiguous, None,
            "no burn decoded ⇒ None, not Some(false)"
        );
        assert!(
            v.is_zero_loss(),
            "optical contiguous + no burn present ⇒ zero loss (unchanged pre-#463 behaviour)"
        );

        let broken = tick_contiguity(&(100..=159).filter(|&t| t != 130).collect::<Vec<_>>());
        let v2 = imag_verdict(&broken, false, false);
        assert!(
            !v2.is_zero_loss(),
            "a missing optical tick still fails even with no burn present"
        );
    }

    #[test]
    fn burn_present_and_contiguous_plus_optical_contiguous_is_zero_loss_463() {
        // #463: BOTH signals present and clean ⇒ zero loss — the stricter, doubly-proven pass.
        let optical = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, true);
        assert_eq!(v.burn_contiguous, Some(true));
        assert!(
            v.is_zero_loss(),
            "optical + burn both contiguous ⇒ zero loss"
        );
    }

    #[test]
    fn burn_present_but_not_contiguous_fails_even_though_optical_is_clean_463() {
        // #463's whole point: the optical tick alone is NOT enough once imag has a digital burn —
        // a present-but-gappy burn must FAIL the node even though the optical read is perfect
        // (never let a weaker proof override a stronger one that disagrees).
        let optical = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, false);
        assert!(optical.is_contiguous(), "sanity: optical alone is clean");
        assert_eq!(v.burn_contiguous, Some(false));
        assert!(
            !v.is_zero_loss(),
            "a present-but-non-contiguous burn FAILS the node even with a clean optical read (#463)"
        );
    }

    #[test]
    fn optical_broken_fails_even_when_burn_is_contiguous_463() {
        // The optical read stays a HARD requirement (mirrors #363's stance for strih/stream): a
        // contiguous digital burn can never paper over a broken optical proof.
        let optical = tick_contiguity(&(100..=159).filter(|&t| t != 130).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, true);
        assert!(
            !v.is_zero_loss(),
            "a missing optical tick FAILS even when the digital burn is perfectly contiguous"
        );
    }

    #[test]
    fn optional_signal_ok_absent_is_fine_present_broken_fails_463() {
        // The shared "second signal" rule NodeVerdict::imag_burn_ok (recording-verdict.rs)
        // also uses: None (not applicable) and Some(true) (present, clean) are both fine;
        // Some(false) (present but broken) is the ONLY failing case.
        assert!(optional_signal_ok(None), "signal not applicable ⇒ fine");
        assert!(
            optional_signal_ok(Some(true)),
            "signal present and clean ⇒ fine"
        );
        assert!(
            !optional_signal_ok(Some(false)),
            "signal present but broken ⇒ the only failing case"
        );
    }

    // ============================================================================
    // #480 — the live rig bug: imag's digital corner burn free-runs at 2x the recorded
    // capture rate (Studio-Mode double-render, see `IMAG_BURN_RENDER_STEP`'s doc), so the OLD
    // strict-1:1 check (`tick_contiguity` — the SAME algorithm `probe::burn_contiguity::
    // burn_contiguity` uses in production) reads every odd-parity gap as "missing" and
    // FALSE-FAILS a recording that dropped zero real frames. `burn_step_contiguity` fixes this
    // by modeling the free-running step, while still catching a GENUINE dropped frame.
    // ============================================================================

    /// The exact live-rig failure signature (2026-07-04, run_id 911003, scaled down): burn ids
    /// present are ALL EVEN (0, 2, 4, .., 190), ALL ODD ids absent — the deterministic
    /// double-render alternation, not scattered/random loss.
    fn free_running_step2_burn_ids() -> Vec<u32> {
        (0..=190).step_by(2).collect()
    }

    #[test]
    fn reproduces_the_480_bug_the_old_strict_check_false_fails_a_free_running_step2_burn() {
        let ids = free_running_step2_burn_ids();
        // The OLD model (imag's burn was gated with the SAME strict first..=last algorithm as
        // `tick_contiguity`/`probe::burn_contiguity::burn_contiguity` before this fix) declares
        // every odd id "missing" — 95 phantom drops on a recording that lost ZERO real frames.
        let old_strict = tick_contiguity(&ids);
        assert!(
            !old_strict.is_contiguous(),
            "the reproduced #480 bug: the OLD strict-1:1 check FALSE-FAILS the free-running \
             step-2 burn: {old_strict:?}"
        );
        assert_eq!(
            old_strict.missing_ticks.len(),
            95,
            "every one of the 95 odd ids in 0..=190 reads as a phantom drop under strict 1:1"
        );
    }

    #[test]
    fn burn_step_contiguity_fixes_480_a_clean_free_running_step2_burn_is_zero_loss() {
        let ids = free_running_step2_burn_ids();
        let sc = burn_step_contiguity(&ids, IMAG_BURN_RENDER_STEP);
        assert!(
            sc.is_contiguous(),
            "the SAME ids the strict check false-fails must be ZERO loss once modeled at the \
             correct step: {sc:?}"
        );
        assert!(sc.missing_ids.is_empty());
        assert_eq!(sc.first_id, Some(0));
        assert_eq!(sc.last_id, Some(190));
        assert_eq!(sc.present_count, 96);
    }

    #[test]
    fn burn_step_contiguity_step_of_1_is_identical_to_strict_contiguity() {
        // step==1 must degenerate to the exact same strict behaviour as `tick_contiguity` — this
        // is a strict SUPERSET, not a separate weaker path.
        let ids: Vec<u32> = (100..=160).filter(|&t| t != 130).collect();
        let sc = burn_step_contiguity(&ids, 1);
        let strict = tick_contiguity(&ids);
        assert_eq!(sc.first_id, strict.first_tick);
        assert_eq!(sc.last_id, strict.last_tick);
        assert_eq!(sc.missing_ids, strict.missing_ticks);
        assert!(!sc.is_contiguous(), "sanity: still catches the real gap");
    }

    #[test]
    fn burn_step_contiguity_still_catches_a_genuine_dropped_frame_inside_the_step2_grid() {
        // A REAL dropped output frame removes an ENTIRE step-2 slot (both its would-be renders),
        // opening a gap of 2*step between the surviving present ids — this must still FAIL.
        let mut ids = free_running_step2_burn_ids();
        ids.retain(|&id| id != 100); // the step-grid id 100 never reached the recording at all
        let sc = burn_step_contiguity(&ids, IMAG_BURN_RENDER_STEP);
        assert!(
            !sc.is_contiguous(),
            "a genuinely missing step-grid slot must still fail: {sc:?}"
        );
        assert_eq!(
            sc.missing_ids,
            vec![100],
            "the exact missing step-grid id must be reported, not the whole odd/even span"
        );
    }

    #[test]
    fn burn_step_contiguity_tolerates_one_step_of_beat_jitter() {
        // A gap of step+1 (a genlock beat wobble, same tolerance
        // `burn_contiguity_in_window_with_step` grants strih/stream) must NOT be charged —
        // integer division absorbs it exactly like the production in-window model.
        let ids = [0u32, 2, 4, 7, 9, 11]; // 4->7 is a gap of 3 (step+1), not a real drop
        let sc = burn_step_contiguity(&ids, 2);
        assert!(
            sc.is_contiguous(),
            "a single step+1 jitter gap must be tolerated, not charged: {sc:?}"
        );
    }

    #[test]
    fn burn_step_contiguity_empty_input_is_not_a_pass() {
        let sc = burn_step_contiguity(&[], IMAG_BURN_RENDER_STEP);
        assert_eq!(sc.first_id, None);
        assert_eq!(sc.last_id, None);
        assert_eq!(sc.present_count, 0);
        assert_eq!(sc.expected_count, 0);
        assert!(sc.missing_ids.is_empty());
        assert!(
            !sc.is_contiguous(),
            "no burn decoded at all must never pass"
        );
    }

    #[test]
    fn burn_step_contiguity_single_id_is_trivially_contiguous() {
        let sc = burn_step_contiguity(&[42], IMAG_BURN_RENDER_STEP);
        assert!(sc.is_contiguous());
        assert_eq!(sc.first_id, Some(42));
        assert_eq!(sc.last_id, Some(42));
        assert_eq!(sc.expected_count, 1);
    }

    #[test]
    fn burn_step_contiguity_expected_count_saturates_instead_of_panicking_on_overflow() {
        // Mirrors `expected_count_saturates_instead_of_panicking_on_overflow` above: checks ONLY
        // the saturating ARITHMETIC in isolation — NOT the full `burn_step_contiguity` (which
        // additionally walks the gap to enumerate missing step-grid ids; walking a genuine
        // 4-billion-wide span is intentionally never exercised, same as `tick_contiguity`/
        // `burn_contiguity` never test it end-to-end either — it would iterate ~4 billion times).
        // A run-bounded burn counter never realistically reaches u32::MAX, so this is defensive.
        let step: u32 = 1; // already `.max(1)`-guarded in `burn_step_contiguity` itself
        let expected_count = (u32::MAX.saturating_sub(0) / step).saturating_add(1);
        assert_eq!(
            expected_count,
            u32::MAX,
            "saturates instead of wrapping to 0"
        );
    }

    // ============================================================================
    // #576 — self-calibrate IMAG_BURN_RENDER_STEP from the OBSERVED burn cadence. The #572
    // live-rig investigation (run 554307) found imag's REAL corner burn free-running at step 3
    // (e.g. 22197/22200/22203), not the hardcoded step-2 constant confirmed by #480. Harmless
    // TODAY (the floor-division tolerance absorbs a step-2-or-3 gap as zero either way — see
    // `burn_step_contiguity`'s doc), but a hardcoded step that disagrees with the real cadence
    // WILL attribute the wrong grid ids (and possibly the wrong count) to a future genuine drop.
    // ============================================================================

    /// A clean free-running burn at step 3 (matches the confirmed live rig cadence, run 554307).
    fn free_running_step3_burn_ids() -> Vec<u32> {
        (22197..22197 + 3 * 40).step_by(3).collect()
    }

    #[test]
    fn calibrate_burn_step_finds_the_dominant_step_for_a_clean_step3_burn_576() {
        let ids = free_running_step3_burn_ids();
        assert_eq!(
            calibrate_burn_step(&ids),
            3,
            "a clean free-running step-3 cadence must calibrate to 3, not the hardcoded \
             IMAG_BURN_RENDER_STEP (2)"
        );
    }

    #[test]
    fn calibrate_burn_step_tolerates_one_genuine_extra_gap_in_an_otherwise_clean_step3_burn_576() {
        // One step-grid slot is genuinely missing (a real dropped output frame) — the dominant
        // delta must still calibrate to 3 (the vast majority of deltas), not be thrown off by
        // the single outlier gap of 6.
        let mut ids = free_running_step3_burn_ids();
        let dropped = ids[10]; // remove one interior id -> a gap of 6 at that one spot
        ids.retain(|&id| id != dropped);
        assert_eq!(
            calibrate_burn_step(&ids),
            3,
            "one outlier gap must not throw off the dominant (modal) step"
        );
    }

    #[test]
    fn calibrate_burn_step_falls_back_to_the_constant_when_too_few_ids_576() {
        assert_eq!(
            calibrate_burn_step(&[]),
            IMAG_BURN_RENDER_STEP,
            "no ids at all -> fall back, nothing to calibrate from"
        );
        assert_eq!(
            calibrate_burn_step(&[42]),
            IMAG_BURN_RENDER_STEP,
            "a single id has no delta at all -> fall back"
        );
        assert_eq!(
            calibrate_burn_step(&[10, 13, 16]),
            IMAG_BURN_RENDER_STEP,
            "only 3 distinct ids (2 deltas) is below MIN_IDS_FOR_STEP_CALIBRATION -> fall back, \
             never trust a 2-sample mode"
        );
    }

    #[test]
    fn calibrate_burn_step_prefers_the_smaller_delta_on_a_tie_576() {
        // deltas: 2,2,3,3 — both step 2 and step 3 occur twice. The smaller (stricter) delta
        // must win the tie, never the looser one.
        let ids = [0u32, 2, 4, 7, 10];
        assert_eq!(
            calibrate_burn_step(&ids),
            2,
            "a tied mode must resolve to the SMALLER (stricter) delta"
        );
    }

    #[test]
    fn calibrate_burn_step_feeds_correct_missing_grid_id_into_burn_step_contiguity_576() {
        // #576's real payoff: calibrating to the TRUE step attributes the CORRECT missing grid
        // id for a genuine drop, where the OLD hardcoded step-2 constant would attribute the
        // WRONG ids entirely (a real drop at true step 3 reads as a gap of 6 under step 2 ->
        // excess = 6/2-1 = 2 phantom ids, neither of which is the real missing grid point).
        let mut ids = free_running_step3_burn_ids();
        let dropped = ids[10];
        ids.retain(|&id| id != dropped);

        let calibrated = calibrate_burn_step(&ids);
        let sc_calibrated = burn_step_contiguity(&ids, calibrated);
        assert_eq!(
            sc_calibrated.missing_ids,
            vec![dropped],
            "calibrated to the true step, the EXACT missing grid id must be reported: {:?}",
            sc_calibrated
        );

        // Sanity: the OLD hardcoded step-2 constant on the SAME ids attributes the WRONG grid
        // ids (never the true missing one) — this is the latent brittleness #576 fixes.
        let sc_hardcoded = burn_step_contiguity(&ids, IMAG_BURN_RENDER_STEP);
        assert_ne!(
            sc_hardcoded.missing_ids,
            vec![dropped],
            "sanity: the hardcoded step-2 model does NOT recover the true missing grid id \
             (demonstrates the #576 brittleness): {:?}",
            sc_hardcoded
        );
    }

    // ============================================================================
    // #580 — the imag optical BEAT net-zero gate: cam2's 60Hz monitor and the free-running 60fps
    // camera are two unsynced same-rate clocks that BEAT (balanced dup+skip = zero NET loss).
    // Replaces strict step-1 `tick_contiguity` as imag's PRIMARY optical zero-loss decision.
    // ============================================================================

    #[test]
    fn optical_beat_net_zero_passes_the_real_572001_pattern_580() {
        // Confirmed live (run 572001, post-#575 trim + #576 calibration): expected=21870,
        // frames=21873, present=21851, missing=19, dups=22, surplus=-3, digital burn 0-missing.
        // avg_step derived the SAME way `probe::recording_verdict::verdict` would (telescoping:
        // (expected_count-1)/(frames_count-1), true whenever there is no undecodable-frame chain
        // break — confirmed here, optical_undecodable=0 on this run).
        let avg_step = (21870.0 - 1.0) / (21873.0 - 1.0);
        let v = optical_beat_verdict_from_counts(
            21870,
            21851,
            21873,
            avg_step,
            IMAG_OPTICAL_EXPECTED_STEP,
        );
        assert_eq!(v.surplus, -3, "expected(21870) - frames(21873) = -3: {v:?}");
        assert!(
            v.is_advancing(),
            "avg_step {avg_step} must round to the expected step 1: {v:?}"
        );
        assert!(
            v.is_net_zero(),
            "the real 572001 pattern is genuinely zero NET loss (19 skips fully compensated by \
             22 dups) — strict step-1 false-fails it, #580 must not: {v:?}"
        );
    }

    #[test]
    fn optical_beat_net_zero_exact_balance_passes_580() {
        // No skip, no dup — a perfectly clean 1:1 run. surplus == 0 (the boundary case) must PASS.
        let ticks: Vec<u32> = (100..=149).collect();
        let v = optical_beat_net_zero(&ticks, IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(v.surplus, 0, "{v:?}");
        assert!(
            v.is_net_zero(),
            "exact balance (surplus == 0) must pass: {v:?}"
        );
    }

    #[test]
    fn optical_beat_net_zero_pure_oversample_passes_580() {
        // One duplicate sample, no skip at all — pure oversampling (surplus < 0, no missing tick
        // whatsoever). Must pass: excess frames are never a defect.
        let mut ticks: Vec<u32> = vec![100];
        ticks.extend(100..=149);
        let v = optical_beat_net_zero(&ticks, IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(v.expected_count, 50);
        assert_eq!(v.frames_count, 51);
        assert_eq!(v.surplus, -1, "{v:?}");
        assert!(
            v.is_net_zero(),
            "pure oversampling (surplus < 0) must pass: {v:?}"
        );
    }

    #[test]
    fn optical_beat_net_zero_genuine_net_loss_fails_580() {
        // 8 unpaired skips, ZERO compensating duplicates — a real net loss, not a beat. Must FAIL
        // even though the sequence is genuinely advancing (this is the net-zero term catching it,
        // not the advance-guard — checked explicitly below).
        let removed = [10u32, 20, 30, 40, 50, 60, 70, 80];
        let ticks: Vec<u32> = (0..=99).filter(|t| !removed.contains(t)).collect();
        let v = optical_beat_net_zero(&ticks, IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(v.expected_count, 100);
        assert_eq!(v.frames_count, 92);
        assert_eq!(v.surplus, 8, "8 net-uncompensated skips: {v:?}");
        assert!(
            v.is_advancing(),
            "sanity: the sequence itself is genuinely advancing — the FAIL below is the \
             net-zero term, not the advance-guard: {v:?}"
        );
        assert!(
            !v.is_net_zero(),
            "8 unpaired skips with zero compensating dups is a genuine net loss: {v:?}"
        );
    }

    #[test]
    fn optical_beat_net_zero_frozen_read_fails_580() {
        // The camera stuck on ONE painted QR value for the whole window — tick range collapses
        // (first == last), avg_step == 0. The OLD strict-step-1 check ALSO false-passed this
        // (trivially "contiguous", nothing missing in a span of one) — #580's advance-guard closes
        // that pre-existing hole; this must now FAIL despite a hugely negative (naively
        // "oversampled") surplus.
        let ticks = vec![500u32; 50];
        let v = optical_beat_net_zero(&ticks, IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(
            v.expected_count, 1,
            "frozen: span collapses to a single value: {v:?}"
        );
        assert_eq!(v.avg_step, 0.0);
        assert!(
            !v.is_advancing(),
            "a frozen/stuck read must never read as genuinely advancing: {v:?}"
        );
        assert!(
            !v.is_net_zero(),
            "a frozen read must FAIL even though surplus is hugely negative (naive oversample \
             reading): {v:?}"
        );
    }

    #[test]
    fn optical_beat_net_zero_single_sample_does_not_prove_advancing_580() {
        // A single decoded sample has no adjacent pair to prove ANY advance — deliberately not a
        // trivial pass (unlike `TickContiguity::is_contiguous`'s single-value case): a lone sample
        // proves nothing about the camera tracking a moving painter tick.
        let v = optical_beat_net_zero(&[42], IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(v.expected_count, 1);
        assert_eq!(v.frames_count, 1);
        assert!(
            !v.is_advancing(),
            "one sample proves no advance at all: {v:?}"
        );
        assert!(!v.is_net_zero(), "{v:?}");
    }

    #[test]
    fn optical_beat_net_zero_empty_input_fails_580() {
        let v = optical_beat_net_zero(&[], IMAG_OPTICAL_EXPECTED_STEP);
        assert_eq!(v.expected_count, 0);
        assert!(!v.is_advancing());
        assert!(
            !v.is_net_zero(),
            "no tick decoded at all must never read as a pass: {v:?}"
        );
    }

    #[test]
    fn optical_beat_from_contiguity_uses_positional_endpoints_for_avg_step_580() {
        // #580 review: `avg_step` MUST derive from the CHRONOLOGICAL (positional) endpoints, NOT
        // `tick_contiguity`'s SORTED min/max. (An earlier "parity lock" comparing the tc-reusing
        // entry to the slice-only one was a TAUTOLOGY — `optical_beat_net_zero` now DELEGATES to
        // `optical_beat_from_contiguity`, so it compared a wrapper to its own wrappee and could
        // never fail. Replaced with concrete hand-computed values on a non-monotonic input that
        // actually distinguishes positional from sorted.)
        //
        // A misdecoded 105 appears at position 1 (out of numeric order): positional first/last are
        // 100/103, while the sorted min/max are 100/105. Hand-computed:
        //   avg_step (positional)  = (103 - 100) / (5 - 1) = 0.75   (sorted would give 1.25)
        //   tick_contiguity spans 100..=105 -> expected_count 6, present 5 (104 missing), frames 5
        //   surplus = 6 - 5 = 1 (> 0) -> a genuine net loss, NOT net-zero.
        let ticks = [100u32, 105, 101, 102, 103];
        let v = optical_beat_from_contiguity(
            &ticks,
            &tick_contiguity(&ticks),
            IMAG_OPTICAL_EXPECTED_STEP,
        );
        assert!(
            (v.avg_step - 0.75).abs() < 1e-9,
            "avg_step must use POSITIONAL endpoints (0.75), not sorted min/max (1.25): {v:?}"
        );
        assert_eq!(v.expected_count, 6, "sorted span 100..=105: {v:?}");
        assert_eq!(v.present_count, 5, "{v:?}");
        assert_eq!(v.frames_count, 5, "{v:?}");
        assert_eq!(v.surplus, 1, "expected_count 6 - frames_count 5: {v:?}");
        assert!(
            !v.is_net_zero(),
            "surplus 1 > 0 is a genuine net loss, never net-zero: {v:?}"
        );
    }
}
