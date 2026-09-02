//! #758 item 4 — the frozen-leg classifier: distinguishes a SUSTAINED camera freeze (a hard-fail
//! the fused verdict must gate on) from isolated stale-replay frames (informational-only, never
//! gates). Pure Tier-0 module (no probe deps, no `image`/`rqrr`) — unit-tests via
//! `cargo test --lib frozen_leg # airuleset:build-ok`, mirroring `src/frozen_camera.rs`'s own
//! crate-root pattern.
//!
//! ## The data this classifies
//!
//! `probe::recording_segments::CamboxSegment` (probe-gated, #726) already computes, per camera
//! per program-switch window: `copies` (a per-window cumulative count of RECORDED frames whose
//! content repeats the immediately-preceding tick — see that module's own doc), `frames` (total
//! frames placed in the window), and `[start_ns, end_ns)` (the window's own span). This module
//! takes those THREE primitives (never the probe-gated struct itself — crate-root pure code never
//! depends on probe types, only the reverse) and decides which of two tiers a window falls into.
//!
//! ## Why "sustained" needs an approximate DURATION, not just a copy COUNT
//!
//! A raw `copies` count alone can't distinguish "5 isolated duplicate frames scattered across a
//! 40-minute healthy window" (harmless — e.g. the vendored DistroAV warm-up-latch replay
//! documented in `scripts/recording-e2e.sh`'s own CLAUDE.md GOTCHA: 1-3 replayed frames on a
//! source's re-activation) from "5 duplicate frames because the camera genuinely froze for a few
//! seconds and only just unstuck". `CamboxSegment` doesn't track the run LENGTH directly, so this
//! module approximates "how long the camera was plausibly stuck" from the copy DENSITY over the
//! window's own duration (`copies / frames * window_secs`) — a reasonable, conservative proxy: a
//! genuinely frozen camera contributes a copy for its ENTIRE stuck span, so the density-derived
//! duration tracks the true stuck duration closely whenever frames arrive at a roughly steady
//! cadence (the genlocked #312/#624 delivery this whole rig is built on). A window is HARD-FROZEN
//! when either that approximate duration exceeds [`FROZEN_SUSTAINED_SECS`], OR the raw density
//! exceeds [`FROZEN_DENSITY_THRESHOLD`] (catches a short window whose approximate duration is
//! small in absolute seconds but whose camera was stuck for almost its ENTIRE span). Anything with
//! copies but NEITHER threshold tripped is `StaleReplay` — reported, never gated — REGARDLESS of
//! the raw copy count.
//!
//! ## issue 905 item 2 follow-up (2026-09-03) — Frozen is EXCLUSIVELY the two `#758` thresholds now
//!
//! `classify_leg` originally carried a THIRD, conservative branch: a copy count above
//! [`STALE_REPLAY_MAX_ISOLATED`] classified `Frozen` even when NEITHER real threshold tripped
//! ("err on failing loud"). That branch was harmless while issue 914 made `frozen_leg`
//! report-only; once item 2 restored it to BLOCKING, the branch started failing genuinely healthy
//! runs. Live evidence (verdict-611325119.json, the ".615" tag, CAM2 window since
//! `1788388542251771245`): `copies=18`, `frames=847`, window ≈30.2s — `density≈0.0213` and
//! `approx_stale_secs≈0.64` sit FAR under both `#758` thresholds, yet the old branch forced
//! `Frozen` purely because 18 > 5. The window's own `residual_events` show all 18 copies as
//! ISOLATED single-frame duplicates (`kind:"copy"`, `tick_before==tick_after`) spread across
//! offsets 1.2s/6.07s/6.5s/7.57s/8.34s/9.47s/10.9s… — a diffuse FIFO-jitter signature (see
//! `genlock-fifo-limit-cycle-diagnosis.md`), not a contiguous stuck run — and the window sits
//! comfortably inside CAM2's own per-cambox copies/gaps tolerance (25, issue 1249). That diffuse
//! class is already owned by the per-cambox `copies_gaps_tolerance` gate (issue 1220/1243); two
//! gates disagreeing over one signal is the bug, not a missing safety margin. `classify_leg` is
//! now strictly two-tier: `Frozen` ⇔ one of the two `#758` thresholds trips; everything else with
//! `copies > 0` is `StaleReplay { copies }`, count still visible, never gating. See
//! `.claude/rules/self-heal-frozen-leg-attribution.md` for the restore-precondition + this
//! follow-up's own section.

/// #758 acceptance: an approximate sustained staleness of MORE than 5 real seconds in a camera's
/// active window is a HARD frozen classification — never informational-only.
pub const FROZEN_SUSTAINED_SECS: f64 = 5.0;

/// #758 acceptance: copy DENSITY of MORE than 10% of a camera's active window is ALSO a HARD
/// frozen classification, independent of the absolute duration (catches a short window whose
/// camera was stuck for nearly all of it, where the absolute-seconds threshold alone would not
/// yet have tripped).
pub const FROZEN_DENSITY_THRESHOLD: f64 = 0.10;

/// #758's ORIGINAL acceptance wording named "AT MOST 5 isolated stale/duplicate frames" as the
/// informational (`StaleReplay`) boundary — the vendored DistroAV warm-up-latch single-frame
/// replay on source re-activation (documented in `scripts/recording-e2e.sh`'s CLAUDE.md) is
/// exactly this class of artifact, not a freeze. **issue 905 item 2 follow-up (2026-09-03):**
/// `classify_leg` no longer branches on this constant at all — a copy count no longer
/// participates in the Frozen/StaleReplay decision by itself (see the module-doc section above).
/// Kept as a DOCUMENTED, unused informational constant recording the original `#758` wording; the
/// per-cambox `copies_gaps_tolerance` gate (issue 1220/1243) is now the single owner of the
/// diffuse-copies class this constant used to (mis)gate.
pub const STALE_REPLAY_MAX_ISOLATED: u32 = 5;

/// The health verdict for ONE camera's ONE program-switch window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LegHealth {
    /// No duplicate/stale frames at all in this window.
    Healthy,
    /// `copies > 0` and NEITHER `#758` threshold (sustained duration or density) tripped —
    /// informational only, never gates the fused verdict, REGARDLESS of the copy count (issue 905
    /// item 2 follow-up).
    StaleReplay { copies: u32 },
    /// Sustained staleness — a HARD fail. `approx_stale_secs` and `density` are BOTH reported
    /// (whichever tripped the threshold, or both) so the operator message can name the actual
    /// mechanism, not just "frozen".
    Frozen {
        copies: u32,
        approx_stale_secs: f64,
        density: f64,
    },
}

/// Classify ONE camera's ONE window from its raw `copies`/`frames`/`window_secs` — see the
/// module doc for the mechanism. `window_secs` is the window's own span; pass `0.0` for a
/// degenerate/`frames == 0` window (mirrors the #333 "frames=0 is empty, not loss" convention
/// this crate already follows elsewhere — such a window necessarily has `copies == 0` too, so it
/// always classifies [`LegHealth::Healthy`] regardless).
pub fn classify_leg(copies: u32, frames: u32, window_secs: f64) -> LegHealth {
    if copies == 0 {
        return LegHealth::Healthy;
    }
    let density = if frames > 0 {
        copies as f64 / frames as f64
    } else {
        0.0
    };
    let approx_stale_secs = if frames > 0 && window_secs > 0.0 {
        density * window_secs
    } else {
        0.0
    };
    if approx_stale_secs > FROZEN_SUSTAINED_SECS || density > FROZEN_DENSITY_THRESHOLD {
        return LegHealth::Frozen {
            copies,
            approx_stale_secs,
            density,
        };
    }
    // issue 905 item 2 follow-up: NEITHER `#758` threshold tripped -- StaleReplay REGARDLESS of
    // the copy count. The conservative "count over the isolated allowance -> Frozen anyway"
    // branch is REMOVED (see the module-doc section above); the diffuse-copies class it used to
    // (mis)gate is already owned by the per-cambox copies_gaps_tolerance gate.
    LegHealth::StaleReplay { copies }
}

/// One camera's one window, as the caller (the probe-gated fused verdict, which owns the real
/// `CamboxSegment` data) hands it to [`frozen_leg_report`] — three primitives, never the
/// probe-gated struct itself.
#[derive(Debug, Clone)]
pub struct SegmentLeg<'a> {
    pub cambox: &'a str,
    pub copies: u32,
    pub frames: u32,
    pub start_ns: i64,
    pub end_ns: i64,
}

/// A HARD-FROZEN window, named for the operator message (`frozen_leg: camN since <ts>`).
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenLeg {
    pub cambox: String,
    pub since_ns: i64,
    pub copies: u32,
    pub approx_stale_secs: f64,
    pub density: f64,
}

impl FrozenLeg {
    /// The #758 acceptance-criteria operator message shape: `frozen_leg: camN since <ts>`.
    pub fn message(&self) -> String {
        format!(
            "frozen_leg: {} since {} (copies={}, approx_stale_secs={:.1}, density={:.2})",
            self.cambox, self.since_ns, self.copies, self.approx_stale_secs, self.density
        )
    }
}

/// An isolated stale-replay window — reported, never gated (`stale_replay: camN xN`).
#[derive(Debug, Clone, PartialEq)]
pub struct StaleReplayLeg {
    pub cambox: String,
    pub copies: u32,
}

impl StaleReplayLeg {
    /// The #758 acceptance-criteria informational message shape: `stale_replay: camN xN`.
    pub fn message(&self) -> String {
        format!("stale_replay: {} x{}", self.cambox, self.copies)
    }
}

/// The full frozen-leg report over every window of a fused verdict run: which cameras HARD-FAIL
/// ([`FrozenLeg`]) and which merely carry isolated stale-replay windows ([`StaleReplayLeg`],
/// informational — reported but never gates). A camera can appear in EITHER list per window it
/// triggers; sorted (cambox, then window start) for deterministic output.
///
/// This standalone aggregator is a legacy convenience, exercised only by this module's own
/// tests — `recording-verdict.rs`'s actual attribution path goes through
/// `crate::self_heal_attribution::attribute_self_heal` (which calls [`classify_leg`] directly, not
/// this function), whose own report additionally re-attributes a hard-frozen window to
/// `self_heal_reset` when a correlating USB-reset event fired (issue 895). See
/// `self_heal_attribution::SelfHealAttributionReport::overall_pass_contribution`: issue 914
/// (2026-08-01) temporarily decoupled `any_frozen`/`any_self_heal()` from `overall_pass` while
/// cam1's grabber defect (issue 909) was unresolved; issue 905 item 2 (2026-09-02) RESTORED the
/// fold once a green E2E series proved both dimensions clean, so the caller gates on them again.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrozenLegReport {
    pub frozen: Vec<FrozenLeg>,
    pub stale_replay: Vec<StaleReplayLeg>,
}

impl FrozenLegReport {
    /// True the moment ANY window HARD-FROZE.
    pub fn any_frozen(&self) -> bool {
        !self.frozen.is_empty()
    }
}

/// Classify every window in `segments` and aggregate into one [`FrozenLegReport`]. Pure — no I/O,
/// no probe deps; the caller supplies the segment data (from `CamboxSegment` fields) and prints
/// [`FrozenLeg::message`] / [`StaleReplayLeg::message`] for each entry. See this module's own
/// struct-level doc above for why the real `recording-verdict.rs` attribution path calls
/// `self_heal_attribution::attribute_self_heal` (which uses [`classify_leg`] directly) instead of
/// this aggregator; that path DOES fold into `overall_pass` again since issue 905 item 2.
pub fn frozen_leg_report(segments: &[SegmentLeg<'_>]) -> FrozenLegReport {
    let mut frozen = Vec::new();
    let mut stale_replay = Vec::new();
    for s in segments {
        let window_secs = ((s.end_ns - s.start_ns).max(0) as f64) / 1_000_000_000.0;
        match classify_leg(s.copies, s.frames, window_secs) {
            LegHealth::Healthy => {}
            LegHealth::StaleReplay { copies } => stale_replay.push(StaleReplayLeg {
                cambox: s.cambox.to_string(),
                copies,
            }),
            LegHealth::Frozen {
                copies,
                approx_stale_secs,
                density,
            } => frozen.push(FrozenLeg {
                cambox: s.cambox.to_string(),
                since_ns: s.start_ns,
                copies,
                approx_stale_secs,
                density,
            }),
        }
    }
    frozen.sort_by(|a, b| a.cambox.cmp(&b.cambox).then(a.since_ns.cmp(&b.since_ns)));
    stale_replay.sort_by(|a, b| a.cambox.cmp(&b.cambox));
    FrozenLegReport {
        frozen,
        stale_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_copies_is_always_healthy() {
        assert_eq!(classify_leg(0, 1000, 30.0), LegHealth::Healthy);
        // Even a degenerate frames=0/window=0 window with (impossibly) copies=0 stays Healthy.
        assert_eq!(classify_leg(0, 0, 0.0), LegHealth::Healthy);
    }

    #[test]
    fn a_handful_of_isolated_copies_is_stale_replay_not_frozen() {
        // 3 copies out of 9000 frames over a 30s window: density ~0.03%, approx_stale ~0.01s --
        // nowhere near either sustained threshold, and copies <= STALE_REPLAY_MAX_ISOLATED (5).
        // Mirrors the #674 vendor warm-up-latch single-frame replay this tier exists for.
        match classify_leg(3, 9000, 30.0) {
            LegHealth::StaleReplay { copies } => assert_eq!(copies, 3),
            other => panic!("expected StaleReplay, got {other:?}"),
        }
    }

    #[test]
    fn sustained_duration_over_5s_is_frozen_even_at_low_density() {
        // 300 copies out of 9000 frames over a 300s (5 min) window: density ~3.3% (well under the
        // 10% density threshold) but approx_stale = 0.0333 * 300 = 10s > FROZEN_SUSTAINED_SECS.
        match classify_leg(300, 9000, 300.0) {
            LegHealth::Frozen {
                copies,
                approx_stale_secs,
                ..
            } => {
                assert_eq!(copies, 300);
                assert!(
                    approx_stale_secs > FROZEN_SUSTAINED_SECS,
                    "approx_stale_secs={approx_stale_secs}"
                );
            }
            other => panic!("expected Frozen, got {other:?}"),
        }
    }

    #[test]
    fn high_density_over_10_pct_is_frozen_even_in_a_short_window() {
        // 20 copies out of 100 frames over a SHORT 2s window: density = 20% (over the 10%
        // threshold) even though approx_stale = 0.2 * 2 = 0.4s (well under the 5s duration
        // threshold) -- the density path catches this on its own.
        match classify_leg(20, 100, 2.0) {
            LegHealth::Frozen { density, .. } => {
                assert!(density > FROZEN_DENSITY_THRESHOLD, "density={density}");
            }
            other => panic!("expected Frozen, got {other:?}"),
        }
    }

    #[test]
    fn exactly_at_both_thresholds_is_not_yet_frozen() {
        // copies=6 (one over the OLD, now-removed isolated allowance of 5) with frames/window
        // sized so BOTH the duration and density land EXACTLY at (not over) their thresholds:
        // density = 6/60 = 10% exactly (not > 10%), approx_stale = 0.10 * 50 = 5.0s exactly (not
        // > 5.0s). Neither "sustained" branch fires (both use strict >).
        //
        // issue 905 item 2 follow-up (REWRITTEN, this test's own name was always the intended
        // behaviour): before the fix this fixture wrongly classified Frozen via the removed
        // conservative "copies (6) > the old isolated allowance (5) -> Frozen anyway" branch,
        // directly contradicting the test's own name ("...is_not_yet_frozen") -- exactly the
        // "genuinely wrong test now" case the item-2 follow-up design flagged. With that branch
        // gone, a boundary that sits AT (not over) both thresholds now correctly stays
        // StaleReplay, finally matching what the name always said.
        match classify_leg(6, 60, 50.0) {
            LegHealth::StaleReplay { copies } => assert_eq!(copies, 6),
            other => panic!(
                "905: a boundary exactly AT (not over) both thresholds must stay StaleReplay, \
                 not {other:?}"
            ),
        }
    }

    #[test]
    fn diffuse_copies_below_both_thresholds_are_stale_replay_not_frozen_905() {
        // issue 905 item 2 follow-up: the .615 live evidence shape (verdict-611325119.json,
        // CAM2 window since 1788388542251771245) -- 18 copies out of 847 frames over a ~30.2s
        // window: density=18/847≈0.0213 (well under FROZEN_DENSITY_THRESHOLD 0.10),
        // approx_stale=0.0213*30.2≈0.64s (well under FROZEN_SUSTAINED_SECS 5.0). Neither #758
        // threshold trips. The `residual_events` for this exact window show all 18 copies as
        // ISOLATED single-frame duplicates (kind=copy, tick_before==tick_after) spread across
        // offsets 1.2s/6.07s/6.5s/7.57s/8.34s/9.47s/10.9s... -- a diffuse FIFO-jitter signature,
        // not a contiguous stuck run -- and the window sits comfortably inside CAM2's own
        // per-cambox copies/gaps tolerance (25, issue 1249). This must classify StaleReplay, not
        // Frozen: the removed conservative branch (copies=18 > the old
        // STALE_REPLAY_MAX_ISOLATED=5 allowance) used to force Frozen here even though neither
        // real threshold ever tripped.
        match classify_leg(18, 847, 30.2) {
            LegHealth::StaleReplay { copies } => assert_eq!(copies, 18),
            other => {
                panic!("905: diffuse below-threshold copies must be StaleReplay, not {other:?}")
            }
        }
    }

    #[test]
    fn copies_just_over_the_old_isolated_allowance_at_low_density_is_stale_replay_not_frozen_905() {
        // issue 905 item 2 follow-up boundary case: copies=6 is one MORE than the removed
        // STALE_REPLAY_MAX_ISOLATED allowance (5), but at a LOW density/duration (6 copies out of
        // 9000 frames over a 30s window: density=6/9000≈0.00067, approx_stale≈0.02s) -- nowhere
        // near either #758 threshold. Under the old conservative branch this classified Frozen
        // purely on the count (6 > 5); it must now classify StaleReplay, since the count no
        // longer participates in the decision at all once the two real thresholds don't trip.
        match classify_leg(6, 9000, 30.0) {
            LegHealth::StaleReplay { copies } => assert_eq!(copies, 6),
            other => panic!(
                "905: a count one over the old isolated allowance, at low density, must be \
                 StaleReplay, not {other:?}"
            ),
        }
    }

    #[test]
    fn isolated_allowance_boundary_is_stale_replay_not_frozen() {
        // copies=5 (exactly at STALE_REPLAY_MAX_ISOLATED) with negligible density/duration.
        match classify_leg(5, 9000, 30.0) {
            LegHealth::StaleReplay { copies } => assert_eq!(copies, 5),
            other => panic!(
                "expected StaleReplay at the exact isolated-allowance boundary, got {other:?}"
            ),
        }
    }

    #[test]
    fn frozen_leg_report_separates_frozen_from_stale_replay_and_sorts() {
        let segments = vec![
            SegmentLeg {
                cambox: "cam5",
                copies: 300,
                frames: 9000,
                start_ns: 5_000_000_000,
                end_ns: 305_000_000_000, // 300s window -> approx_stale ~10s -> Frozen
            },
            SegmentLeg {
                cambox: "cam2",
                copies: 2,
                frames: 9000,
                start_ns: 0,
                end_ns: 30_000_000_000, // 30s window, 2 copies -> StaleReplay
            },
            SegmentLeg {
                cambox: "cam4",
                copies: 0,
                frames: 9000,
                start_ns: 30_000_000_000,
                end_ns: 60_000_000_000, // Healthy -> absent from both lists
            },
        ];
        let report = frozen_leg_report(&segments);
        assert!(report.any_frozen());
        assert_eq!(report.frozen.len(), 1);
        assert_eq!(report.frozen[0].cambox, "cam5");
        assert_eq!(report.frozen[0].since_ns, 5_000_000_000);
        assert_eq!(report.stale_replay.len(), 1);
        assert_eq!(report.stale_replay[0].cambox, "cam2");
    }

    #[test]
    fn empty_segments_is_a_clean_empty_report() {
        let report = frozen_leg_report(&[]);
        assert!(!report.any_frozen());
        assert!(report.frozen.is_empty());
        assert!(report.stale_replay.is_empty());
    }

    #[test]
    fn message_shapes_match_the_758_acceptance_criteria() {
        let frozen = FrozenLeg {
            cambox: "cam7".to_string(),
            since_ns: 123,
            copies: 40,
            approx_stale_secs: 6.2,
            density: 0.15,
        };
        assert!(frozen.message().starts_with("frozen_leg: cam7 since 123"));

        let stale = StaleReplayLeg {
            cambox: "cam3".to_string(),
            copies: 2,
        };
        assert_eq!(stale.message(), "stale_replay: cam3 x2");
    }
}
