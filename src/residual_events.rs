//! #707 EVENT-FORENSICS — per-event residual copy/gap detection for the all-cambox painted-tick
//! window continuity check (`probe::recording_segments::window_segment`).
//!
//! Mirrors the `painted_tick_gaps.rs` / `presentation_cadence.rs` / `imag_tick_gate.rs` Tier-0 seam
//! pattern: the WHOLE `probe` module is `#[cfg(feature = "probe")]` (CI-only, never compiled/tested
//! locally per this repo's Local Build Policy), so the PURE decision logic lives here at the crate
//! root where it unit-tests on DEFAULT features.
//!
//! ## Why this exists (the user's binding #707 decision, 2026-07-13)
//!
//! "od zaciatku to riesime ze kazda odchylka musi mat svoj dovod" — every residual copy/gap event
//! that `window_segment`'s aggregate `copies`/`gaps` counts must get its OWN documented reason; a
//! calibrated tolerance for an *unexplained* residual is explicitly rejected. That requires turning
//! each window's aggregate count into a list of LOCATABLE events — a specific recorded frame, tick
//! value, and wall-clock instant a human/tool can then pull pixel proof + strih FIFO-audit / camera
//! journal lines for (see the collector script `scripts/event-forensics-dossier.py`).
//!
//! ## What counts as an event, and why NOT every oversized delta
//!
//! The `#707` event-anatomy investigation (issue comment, 2026-07-13; also documented in
//! `.claude/skills/e2e`) found that the painted tick's RECORDED-order delta histogram is IDENTICAL
//! in shape across every window checked — clean or residual-heavy alike — dominated by small deltas
//! (the dual-QR Vernier's normal asynchronous even/odd sampling beat, already accepted as benign by
//! `painted_tick_gaps`'s #681 fix) plus a periodic ~130-140-per-window catch-up cluster (deltas of
//! roughly `2*expected_step` to `4*expected_step`) that is CONSTANT regardless of the window's
//! duplicate count and NOT itself a residual defect. An exhaustive sweep for outliers beyond
//! `|delta| > 10` across 5 real recordings (2 large residual spikes + 3 small-residual windows)
//! found ZERO — the routine beat never produces a delta that large. So:
//!
//! - A **Copy** event is a genuine, unambiguous residual signal: the SAME painted tick presented on
//!   two adjacent RECORDED frames (delta 0) — this is exactly `CamboxSegment.copies`'s own
//!   recorded-order adjacent-repeat definition, reused here frame-for-frame so every copy counted
//!   in the aggregate has a matching locatable event.
//! - A **Gap** event is a delta that is NOT part of the routine beat: a backward jump (never
//!   observed live, but not silently ignored), or a forward jump whose magnitude exceeds
//!   [`GAP_OUTLIER_ABS_DELTA`] — the exact `|Δ|>10` threshold the #707 anatomy table's own
//!   "outliers" column already used to conclude "no outliers anywhere". A routine catch-up delta
//!   (6/7/8 in the real data at `expected_step=2`) is NOT flagged as a Gap event — flagging it would
//!   flood the dossier with hundreds of "events" per window that are not residual defects at all.
//!
//! This is a DIFFERENT (recorded-order, per-transition, exact) view than `CamboxSegment.gaps` /
//! `painted_tick_gaps` (an order-independent WHOLE-WINDOW net-span calculation, #625/#681) — the
//! two are not expected to sum to the same total. The aggregate `gaps` figure remains the
//! authoritative PASS/FAIL signal; `residual_events` exists purely to LOCATE candidate frames for
//! forensics, exactly mirroring how the ad-hoc `event_anatomy.py` script the #707 investigation
//! already used computed its own delta histograms in recorded order.

use serde::Serialize;

/// One decoded sample of the shared painted (cam2 optical Vernier) tick on a recorded frame, in
/// RECORDED order — the crate-root Tier-0 mirror of `probe::recording_segments::SegmentFrame`
/// (that type lives inside the `#[cfg(feature = "probe")]`-gated `probe` module, so a Tier-0 caller
/// can't reference it directly). Carries exactly the same three fields; the probe-gated caller
/// converts one to the other with a trivial field copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickSample {
    pub frame_index: u64,
    pub gen_ts_ns: i64,
    pub tick: Option<u32>,
}

/// The absolute forward-delta beyond which a RECORDED-order tick jump is treated as a genuinely
/// anomalous, locatable "gap" candidate rather than the routine dual-QR Vernier sampling beat's
/// periodic catch-up jump. NOT a tuned/invented constant — it is the exact `|Δ|>10` threshold the
/// #707 event-anatomy investigation already used (issue comment, 2026-07-13) to conclude "no
/// outliers anywhere" across every window it checked (2 large residual spikes + 3 small-residual
/// windows, real rig data): the routine catch-up cluster never exceeds a delta of 8 at
/// `expected_step=2`, so 10 cleanly separates "routine beat" from "genuinely anomalous".
pub const GAP_OUTLIER_ABS_DELTA: i64 = 10;

/// A residual copy/gap event's kind — see the module doc for the discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidualEventKind {
    /// The painted tick repeated the immediately-preceding RECORDED present tick (delta 0) — a
    /// stale/frozen frame. Mirrors `CamboxSegment.copies`'s own definition exactly.
    Copy,
    /// A RECORDED-order tick jump that is NOT part of the routine sampling-beat catch-up cluster —
    /// a backward jump, or a forward jump beyond [`GAP_OUTLIER_ABS_DELTA`].
    Gap,
}

/// One locatable residual event, carrying everything a forensics dossier needs: the recorded frame
/// to pixel-proof (± a small neighbourhood), its position on the switch-schedule timeline, and a
/// cross-reference key for pulling the strih genlock-FIFO-audit line + the per-camera sender
/// journal line for the SAME wall-clock second.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResidualEvent {
    pub kind: ResidualEventKind,
    /// Which cambox's schedule window this event was found in. Empty when computed by
    /// [`residual_events`] directly (the pure kernel does not know its own caller's cambox label) —
    /// the probe-gated caller (`window_segment`) fills this in immediately after calling it.
    pub cambox: String,
    /// The recorded frame this event is anchored to — the pixel-proof target. For a Copy, the
    /// duplicate frame itself; for a Gap, the frame immediately AFTER the anomalous jump (the
    /// first frame to resume after it).
    pub frame_index: u64,
    /// That frame's `gen_ts_ns` — the burn/optical timeline the switch schedule and the strih
    /// FIFO-audit / camera journal logs are all keyed on.
    pub gen_ts_ns: i64,
    /// Offset since the window's own switch-in (`gen_ts_ns - window_start_ns`) — the
    /// switch-schedule position requested by the #707 decision.
    pub window_offset_ns: i64,
    /// The whole second of `gen_ts_ns` (`gen_ts_ns.div_euclid(1_000_000_000)`) — the
    /// cross-reference key `scripts/event-forensics-dossier.py` greps the strih FIFO-audit lines
    /// and per-camera sender journal lines against.
    pub wall_clock_epoch_s: i64,
    /// The tick value immediately BEFORE this event (a Copy's held value; a Gap's last-seen value
    /// before the jump).
    pub tick_before: Option<u32>,
    /// The tick value immediately AFTER this event (a Copy's repeat; a Gap's resuming value).
    pub tick_after: Option<u32>,
    /// For a Gap on a forward jump only: how many painted-tick slots are unaccounted for in this
    /// span (`delta / expected_step - 1`). `None` for a Copy, and for a backward-jump Gap (a
    /// negative delta is a distinct anomaly kind, not a missing-slot count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_slots: Option<u32>,
    /// The #726 presentation-cadence CONTEXT for this specific event: whether the adjacent delta
    /// on the OTHER side of this event matches the paired duplicate+catchup ("15fps-like") shape —
    /// for a Copy, whether the delta right AFTER it is `2*expected_step`; for a Gap, whether the
    /// delta right BEFORE it was `0` (a held frame immediately followed by this jump).
    pub paired_with_catchup: bool,
    /// The documented REASON for this specific residual event — the #707 decision's core
    /// requirement ("every residual deviation must get its own documented reason"). `None` until a
    /// human/tool investigates this event and annotates it (e.g. via the collector script's
    /// dossier, or a manual JSON patch) — an unannotated event is honestly reported as still OPEN,
    /// never silently assumed benign.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The RECORDED-order delta immediately BEFORE `present[i-1] -> present[i]`'s own transition
/// (i.e. `present[i-2] -> present[i-1]`), or `None` when there is no such prior pair
/// (`i < 2`). Used to detect the #726 "paired duplicate+catchup" context on the OTHER side of a
/// Gap event: a Gap immediately preceded by a held (delta-0) frame.
fn recorded_delta_before(present: &[&TickSample], i: usize, prev_tick: u32) -> Option<i64> {
    if i < 2 {
        return None;
    }
    let before_prev = present[i - 2].tick.expect("filtered to Some above");
    Some(i64::from(prev_tick) - i64::from(before_prev))
}

/// #883 — when the whole-window net-span `gaps` (computed separately by
/// [`crate::painted_tick_gaps::painted_tick_gaps`], order-independent) is non-zero but the
/// per-transition walk in [`residual_events`] found NOTHING to blame it on — every individual
/// recorded-order delta sat at or under [`GAP_OUTLIER_ABS_DELTA`] and no backward jump occurred —
/// the counted gap is otherwise completely UNLOCATABLE. Live evidence: CAM1 window 9, run
/// 1412981627 (issue 883, 2026-07-30): `gaps=2, residual_events=0`, delta histogram `{1:11,
/// 2:840, 3:9, 8:1}` — the largest individual delta (8) sits comfortably under the outlier
/// ceiling (10), so nothing in [`residual_events`] flags it, yet the whole-window net-span
/// accounting is honestly short by 2 slots (a diffuse residual from many near-nominal
/// transitions, not one big anomaly).
///
/// This re-walks `frames` (RECORDED order, the same input [`residual_events`] already consumed)
/// and appends ONE Gap event anchored on the SINGLE LARGEST forward delta in the window — the
/// best available candidate location to start pixel-proofing from. `missing_slots` is set to the
/// full CREDITED `gaps` value (the authoritative figure), not the local delta's own
/// `delta/expected_step - 1` estimate, precisely so a reader never sees two conflicting numbers
/// for the same window: this event's whole point is "here is where to start looking for the
/// counted deficit", not an independent re-measurement that could disagree with it.
///
/// Never touches a window that already has a located Gap event (a genuine outlier or backward
/// jump stays the primary, more concrete lead — this fallback never doubles up on it) and never
/// fires when `gaps == 0` — so it can NEVER touch the #852 scenario (`gaps == 0`, events
/// non-empty), which stays intentionally locked as a separate, valid divergence (see
/// `residual_gap_events_can_be_nonzero_while_authoritative_gaps_stays_zero_852` in
/// `probe::recording_segments`).
pub fn locate_best_candidate_for_unattributed_gap(
    frames: &[TickSample],
    window_start_ns: i64,
    gaps: u32,
    mut events: Vec<ResidualEvent>,
) -> Vec<ResidualEvent> {
    if gaps == 0 || events.iter().any(|e| e.kind == ResidualEventKind::Gap) {
        return events;
    }
    let present: Vec<&TickSample> = frames.iter().filter(|f| f.tick.is_some()).collect();
    let mut best_i: Option<usize> = None;
    let mut best_delta: i64 = 0;
    for i in 1..present.len() {
        let prev = present[i - 1].tick.expect("filtered to Some above");
        let cur = present[i].tick.expect("filtered to Some above");
        let delta = i64::from(cur) - i64::from(prev);
        if delta > best_delta {
            best_delta = delta;
            best_i = Some(i);
        }
    }
    // Defensive only: `gaps > 0` mathematically requires >= 2 present ticks spanning a positive
    // range (see `crate::painted_tick_gaps`), so a forward-progressing pair always exists here —
    // and any backward jump would already have been caught above (unconditional, no threshold),
    // which would have made the `events.iter().any(...)` guard above return early.
    let Some(i) = best_i else {
        return events;
    };
    let prev_tick = present[i - 1].tick.expect("filtered to Some above");
    let cur_tick = present[i].tick.expect("filtered to Some above");
    let f = present[i];
    events.push(ResidualEvent {
        kind: ResidualEventKind::Gap,
        cambox: String::new(), // caller fills in, same convention as residual_events()
        frame_index: f.frame_index,
        gen_ts_ns: f.gen_ts_ns,
        window_offset_ns: f.gen_ts_ns - window_start_ns,
        wall_clock_epoch_s: f.gen_ts_ns.div_euclid(1_000_000_000),
        tick_before: Some(prev_tick),
        tick_after: Some(cur_tick),
        missing_slots: Some(gaps),
        paired_with_catchup: false,
        reason: Some(format!(
            "issue 883 fallback: whole-window net-span accounting is short {gaps} slot(s) with \
             no delta above the outlier ceiling ({GAP_OUTLIER_ABS_DELTA}) -- this is the single \
             largest recorded-order delta ({best_delta}) in the window, the best available \
             candidate; the residual may be diffuse across smaller deviations rather than fully \
             attributable to this one transition"
        )),
    });
    events
}

/// Detect residual copy/gap events in `frames` (RECORDED order — do NOT sort first, unlike
/// `painted_tick_gaps`'s net-loss accounting; event LOCATION is inherently order-dependent).
/// `window_start_ns` is the enclosing schedule window's own start (for `window_offset_ns`); pass
/// `0` when there is no enclosing window. `expected_step` is the by-design painted-tick decimation
/// (e.g. 2 for a 60fps painter into a 30fps canvas) — floored to 1.
///
/// Returns events in RECORDED order (interleaved Copy/Gap, exactly as they occur), with `cambox`
/// left empty (the caller fills it in — see [`ResidualEvent::cambox`]'s doc).
pub fn residual_events(
    frames: &[TickSample],
    window_start_ns: i64,
    expected_step: i64,
) -> Vec<ResidualEvent> {
    let expected_step = expected_step.max(1);
    let present: Vec<&TickSample> = frames.iter().filter(|f| f.tick.is_some()).collect();
    let mut events = Vec::new();

    for i in 1..present.len() {
        let prev_tick = present[i - 1].tick.expect("filtered to Some above");
        let cur_tick = present[i].tick.expect("filtered to Some above");
        let delta = i64::from(cur_tick) - i64::from(prev_tick);
        let f = present[i];

        if delta == 0 {
            // COPY — mirrors `window_segment`'s exact recorded-order adjacent-repeat definition.
            let delta_next = present
                .get(i + 1)
                .map(|n| i64::from(n.tick.expect("filtered to Some above")) - i64::from(cur_tick));
            events.push(ResidualEvent {
                kind: ResidualEventKind::Copy,
                cambox: String::new(),
                frame_index: f.frame_index,
                gen_ts_ns: f.gen_ts_ns,
                window_offset_ns: f.gen_ts_ns - window_start_ns,
                wall_clock_epoch_s: f.gen_ts_ns.div_euclid(1_000_000_000),
                tick_before: Some(prev_tick),
                tick_after: Some(cur_tick),
                missing_slots: None,
                paired_with_catchup: delta_next == Some(2 * expected_step),
                reason: None,
            });
        } else if delta < 0 {
            // A backward jump — never observed live (the #707 anatomy sweep found zero across 5
            // real recordings), but a genuine anomaly if it ever occurs; report it, never silently
            // drop it.
            let delta_prev = recorded_delta_before(&present, i, prev_tick);
            events.push(ResidualEvent {
                kind: ResidualEventKind::Gap,
                cambox: String::new(),
                frame_index: f.frame_index,
                gen_ts_ns: f.gen_ts_ns,
                window_offset_ns: f.gen_ts_ns - window_start_ns,
                wall_clock_epoch_s: f.gen_ts_ns.div_euclid(1_000_000_000),
                tick_before: Some(prev_tick),
                tick_after: Some(cur_tick),
                missing_slots: None,
                paired_with_catchup: delta_prev == Some(0),
                reason: None,
            });
        } else if delta > GAP_OUTLIER_ABS_DELTA {
            // A forward jump well beyond the routine catch-up cluster — a genuinely anomalous,
            // locatable gap candidate.
            let missing = ((delta / expected_step) - 1).max(0) as u32;
            let delta_prev = recorded_delta_before(&present, i, prev_tick);
            events.push(ResidualEvent {
                kind: ResidualEventKind::Gap,
                cambox: String::new(),
                frame_index: f.frame_index,
                gen_ts_ns: f.gen_ts_ns,
                window_offset_ns: f.gen_ts_ns - window_start_ns,
                wall_clock_epoch_s: f.gen_ts_ns.div_euclid(1_000_000_000),
                tick_before: Some(prev_tick),
                tick_after: Some(cur_tick),
                missing_slots: Some(missing),
                paired_with_catchup: delta_prev == Some(0),
                reason: None,
            });
        }
        // else: a routine beat delta (small forward step, or a catch-up jump within the outlier
        // ceiling) — not a residual event, never flagged.
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(frame_index: u64, gen_ts_ns: i64, tick: Option<u32>) -> TickSample {
        TickSample {
            frame_index,
            gen_ts_ns,
            tick,
        }
    }

    /// Build a RECORDED-order tick sequence from a flat delta list (starting at `start_tick`),
    /// synthesizing sequential frame_index/gen_ts_ns (100ns apart — arbitrary, only relative
    /// ordering matters for these tests).
    fn ticks_from_deltas(start_tick: u32, deltas: &[i64]) -> Vec<TickSample> {
        let mut ticks = vec![start_tick as i64];
        for &d in deltas {
            ticks.push(ticks.last().unwrap() + d);
        }
        ticks
            .iter()
            .enumerate()
            .map(|(i, &t)| s(i as u64, 100 + (i as i64) * 100, Some(t as u32)))
            .collect()
    }

    /// Flatten a (delta, count) histogram into a delta list, in the given order (order does not
    /// matter for the plain copy/gap COUNT assertions below — only for the specific paired-context
    /// tests, which build their own explicit sequences instead).
    fn histogram_deltas(hist: &[(i64, usize)]) -> Vec<i64> {
        let mut out = Vec::new();
        for &(d, n) in hist {
            out.extend(std::iter::repeat_n(d, n));
        }
        out
    }

    // ---- real #707 event-anatomy fixtures (issue comment, 2026-07-13) ---------------------

    #[test]
    fn cam1_802117826_spike_window_delta_histogram_707() {
        // Real #707 event-anatomy data: this window's delta histogram is
        // {0:33, 1:626, 2:47, 6:14, 7:120, 8:6} — 33 duplicate events, zero backward jumps, zero
        // outliers (|delta|>10) anywhere. residual_events() must report exactly 33 Copy events and
        // ZERO Gap events — the routine catch-up cluster (deltas 6/7/8) is NOT a residual defect.
        let deltas = histogram_deltas(&[(0, 33), (1, 626), (2, 47), (6, 14), (7, 120), (8, 6)]);
        let frames = ticks_from_deltas(10_000, &deltas);
        let events = residual_events(&frames, 0, 2);
        let copies = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Copy)
            .count();
        let gaps = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Gap)
            .count();
        assert_eq!(
            copies, 33,
            "matches the real #707 anatomy: 33 duplicate events"
        );
        assert_eq!(
            gaps, 0,
            "the routine catch-up cluster (delta 6/7/8) must never be flagged as a gap anomaly"
        );
    }

    #[test]
    fn cam2_670137317_small_residual_window_delta_histogram_707() {
        // Real #707 small-residual data: delta histogram {0:2, 1:699, 2:3, 3:1, 6:2, 7:137, 8:1} —
        // 2 duplicate events, zero outliers.
        let deltas =
            histogram_deltas(&[(0, 2), (1, 699), (2, 3), (3, 1), (6, 2), (7, 137), (8, 1)]);
        let frames = ticks_from_deltas(50_000, &deltas);
        let events = residual_events(&frames, 0, 2);
        let copies = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Copy)
            .count();
        let gaps = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Gap)
            .count();
        assert_eq!(copies, 2);
        assert_eq!(gaps, 0);
    }

    #[test]
    fn real_duplicate_context_from_the_cam1_spike_first_event_707() {
        // The #707 anatomy comment's own posted context for the FIRST duplicate of the CAM1
        // spike: "frame_idx=5926 tick=14626 ... ctx=[...14616,14622,14624,14625,14626,14626,
        // 14628,14634,14636,14636,14638]". frame_idx=5926 is the SECOND 14626 in that context —
        // build it verbatim and confirm residual_events locates exactly that duplicate.
        let ctx: [u32; 11] = [
            14616, 14622, 14624, 14625, 14626, 14626, 14628, 14634, 14636, 14636, 14638,
        ];
        let frames: Vec<TickSample> = ctx
            .iter()
            .enumerate()
            .map(|(i, &t)| s(i as u64, 1000 + (i as i64) * 133, Some(t)))
            .collect();
        let events = residual_events(&frames, 0, 2);
        let copies: Vec<&ResidualEvent> = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Copy)
            .collect();
        // Two duplicates in this real context: index 5 (14626,14626) and index 8 (14636,14636).
        assert_eq!(copies.len(), 2, "events: {events:?}");
        assert_eq!(
            copies[0].frame_index, 5,
            "the second 14626 is at ctx index 5"
        );
        assert_eq!(copies[0].tick_before, Some(14626));
        assert_eq!(copies[0].tick_after, Some(14626));
        assert!(
            !copies[0].paired_with_catchup,
            "the delta right after (14628-14626=2=expected_step) is NOT a catchup jump"
        );
        assert_eq!(
            copies[1].frame_index, 9,
            "the second 14636 is at ctx index 9"
        );
    }

    // ---- window_offset_ns / wall_clock_epoch_s -------------------------------------------

    #[test]
    fn window_offset_and_epoch_second_are_computed_from_gen_ts_ns() {
        let frames = vec![
            s(0, 30_500_000_000_000, Some(1000)),
            s(1, 30_500_000_000_000, Some(1000)), // duplicate at the SAME gen_ts (degenerate but valid)
        ];
        let events = residual_events(&frames, 30_000_000_000_000, 2);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].window_offset_ns, 500_000_000_000);
        assert_eq!(events[0].wall_clock_epoch_s, 30_500);
    }

    // ---- genuine anomalies (synthetic — proves the Gap branch is not dead code) ------------

    #[test]
    fn extreme_forward_outlier_beyond_ten_is_a_gap_event() {
        // A jump of 50 at expected_step=2 (25x the step, 5x the outlier ceiling) — far beyond any
        // routine catch-up cluster (the real data never exceeds 8).
        let frames = vec![
            s(0, 100, Some(1000)),
            s(1, 200, Some(1001)),
            s(2, 300, Some(1051)), // delta = 50
            s(3, 400, Some(1052)),
        ];
        let events = residual_events(&frames, 0, 2);
        let gaps: Vec<&ResidualEvent> = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Gap)
            .collect();
        assert_eq!(gaps.len(), 1, "events: {events:?}");
        assert_eq!(gaps[0].frame_index, 2);
        assert_eq!(gaps[0].tick_before, Some(1001));
        assert_eq!(gaps[0].tick_after, Some(1051));
        assert_eq!(gaps[0].missing_slots, Some(24)); // 50/2 - 1 = 24
    }

    #[test]
    fn backward_jump_is_a_gap_event_with_no_missing_slots() {
        let frames = vec![
            s(0, 100, Some(2000)),
            s(1, 200, Some(2002)),
            s(2, 300, Some(1998)), // backward jump
        ];
        let events = residual_events(&frames, 0, 2);
        let gaps: Vec<&ResidualEvent> = events
            .iter()
            .filter(|e| e.kind == ResidualEventKind::Gap)
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].tick_before, Some(2002));
        assert_eq!(gaps[0].tick_after, Some(1998));
        assert_eq!(gaps[0].missing_slots, None);
    }

    #[test]
    fn a_delta_within_the_outlier_ceiling_is_never_flagged_a_gap() {
        // delta=10 sits AT the ceiling (not beyond it) — must not be flagged, matching the strict
        // `> GAP_OUTLIER_ABS_DELTA` (not `>=`) comparison.
        let frames = vec![s(0, 100, Some(1000)), s(1, 200, Some(1010))];
        let events = residual_events(&frames, 0, 2);
        assert!(events.is_empty(), "events: {events:?}");
    }

    #[test]
    fn paired_duplicate_then_catchup_is_flagged_707_726() {
        // The #726 "15fps-like" paired signature: a duplicate immediately followed by a
        // 2*expected_step catchup jump.
        let frames = vec![
            s(0, 100, Some(1000)),
            s(1, 200, Some(1000)), // duplicate
            s(2, 300, Some(1004)), // catchup: delta = 4 = 2*2
        ];
        let events = residual_events(&frames, 0, 2);
        let copy = events
            .iter()
            .find(|e| e.kind == ResidualEventKind::Copy)
            .expect("a copy event must be reported");
        assert!(copy.paired_with_catchup, "events: {events:?}");
    }

    // ---- degenerate inputs ----------------------------------------------------------------

    #[test]
    fn too_few_or_all_undecodable_frames_report_no_events() {
        assert!(residual_events(&[], 0, 2).is_empty());
        assert!(residual_events(&[s(0, 100, Some(1000))], 0, 2).is_empty());
        assert!(residual_events(&[s(0, 100, None), s(1, 200, None)], 0, 2).is_empty());
    }

    #[test]
    fn undecodable_frames_are_skipped_not_treated_as_a_jump() {
        // An undecodable (None) frame between two present ticks must not itself manufacture a
        // spurious delta — the walk only ever compares PRESENT ticks against each other.
        let frames = vec![
            s(0, 100, Some(1000)),
            s(1, 200, None),
            s(2, 300, Some(1002)),
        ];
        let events = residual_events(&frames, 0, 2);
        assert!(
            events.is_empty(),
            "a clean step-2 advance around an undecodable frame: {events:?}"
        );
    }

    #[test]
    fn expected_step_floors_to_one() {
        let frames = vec![s(0, 100, Some(10)), s(1, 200, Some(10))];
        let a = residual_events(&frames, 0, 0);
        let b = residual_events(&frames, 0, 1);
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].paired_with_catchup, b[0].paired_with_catchup);
    }

    #[test]
    fn reason_defaults_to_none_until_annotated() {
        let frames = vec![s(0, 100, Some(10)), s(1, 200, Some(10))];
        let events = residual_events(&frames, 0, 2);
        assert_eq!(
            events[0].reason, None,
            "every event starts OPEN, undocumented"
        );
    }

    // ---- #883 -- locate a best candidate when the aggregate says something's missing but the
    // per-transition walk found nothing (a diffuse residual, not one big outlier) --------------

    #[test]
    fn zero_gaps_never_adds_a_fallback_event() {
        let frames = vec![s(0, 100, Some(1000)), s(1, 200, Some(1002))];
        let events = residual_events(&frames, 0, 2);
        assert!(events.is_empty());
        let augmented = locate_best_candidate_for_unattributed_gap(&frames, 0, 0, events);
        assert!(
            augmented.is_empty(),
            "gaps==0 must never add a fallback event"
        );
    }

    #[test]
    fn an_existing_gap_event_is_never_duplicated_by_the_fallback() {
        // A genuine large outlier is already located by the walk above; the fallback must defer
        // to it, never adding a second, vaguer event on top.
        let frames = vec![
            s(0, 100, Some(1000)),
            s(1, 200, Some(1001)),
            s(2, 300, Some(1051)), // delta = 50, already flagged by residual_events()
            s(3, 400, Some(1052)),
        ];
        let events = residual_events(&frames, 0, 2);
        assert_eq!(events.len(), 1, "events: {events:?}");
        let augmented = locate_best_candidate_for_unattributed_gap(&frames, 0, 2, events.clone());
        assert_eq!(
            augmented, events,
            "an already-located gap must not be duplicated by the fallback"
        );
    }

    #[test]
    fn diffuse_small_gap_with_no_outlier_delta_is_still_located_883() {
        // Mirrors CAM1 window 9 of run 1412981627 (issue 883): a lone delta=6 catch-up (well
        // under GAP_OUTLIER_ABS_DELTA=10) amid otherwise-clean step=2 sampling. painted_tick_gaps
        // nets this to exactly 2 -- the SAME shape (a moderate, sub-threshold jump with no
        // compensating fine sampling) as the real window's histogram {1:11, 2:840, 3:9, 8:1}
        // (max delta 8, also under the ceiling), which likewise nets to gaps=2.
        let present = [1000, 1002, 1004, 1010, 1012, 1014];
        let gaps = crate::painted_tick_gaps::painted_tick_gaps(&present, 0, 2);
        assert_eq!(
            gaps, 2,
            "sanity: this shape nets the same 2-slot deficit issue 883 reported"
        );

        let frames: Vec<TickSample> = present
            .iter()
            .enumerate()
            .map(|(i, &t)| s(i as u64, 1000 + (i as i64) * 1000, Some(t)))
            .collect();
        let base_events = residual_events(&frames, 0, 2);
        assert!(
            base_events.is_empty(),
            "issue 883's exact defect: gaps={gaps} > 0 but zero located events from the base \
             walk: {base_events:?}"
        );

        let events = locate_best_candidate_for_unattributed_gap(&frames, 0, gaps, base_events);
        assert_eq!(events.len(), 1, "events: {events:?}");
        assert_eq!(events[0].kind, ResidualEventKind::Gap);
        assert_eq!(
            events[0].missing_slots,
            Some(2),
            "the located event carries the FULL credited gaps value, not a re-derived local \
             estimate: {:?}",
            events[0]
        );
        // The delta=6 jump is 1004 -> 1010, at present[3] (frame_index 3).
        assert_eq!(events[0].frame_index, 3);
        assert_eq!(events[0].tick_before, Some(1004));
        assert_eq!(events[0].tick_after, Some(1010));
    }

    #[test]
    fn fallback_picks_the_single_largest_forward_delta() {
        // Two candidate deltas below the outlier ceiling (4 and 6); the fallback must anchor on
        // the LARGER one (6), not the first one encountered.
        let present = [1000, 1004, 1006, 1012, 1014];
        // deltas: 4, 2, 6, 2 (monotonic, already sorted) --
        // expected_count=(1014-1000)/2+1=8, present_count=5, gaps=3.
        let gaps = crate::painted_tick_gaps::painted_tick_gaps(&present, 0, 2);
        let frames: Vec<TickSample> = present
            .iter()
            .enumerate()
            .map(|(i, &t)| s(i as u64, 100 + (i as i64) * 100, Some(t)))
            .collect();
        let base = residual_events(&frames, 0, 2);
        assert!(base.is_empty(), "{base:?}");
        let events = locate_best_candidate_for_unattributed_gap(&frames, 0, gaps, base);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(
            events[0].tick_before,
            Some(1006),
            "the delta=6 transition, not delta=4: {:?}",
            events[0]
        );
        assert_eq!(events[0].tick_after, Some(1012));
    }
}
