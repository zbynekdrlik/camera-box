//! (#889) The mechanism-visibility log of the dupe-preferring decimation gate: the per-window
//! shed / copy / retire / drain / starvation-repeat counters and their 5 s summary line.
//!
//! Issue 1367 slice D2 moved this block out of `gate.rs` unchanged (the #414 ~1000-line budget,
//! the #1165 split precedent): every item keeps its public path
//! `camera_box::dupe_decimation::X` through the glob re-export in `mod.rs`, and the summary
//! line's text is byte-identical.

// ── (#889) mechanism-visibility log (comprehensive-logging) ──────────────────

/// Per-run accumulator proving the mechanism is live on a real box: counts how many captured
/// frames were shed because they were the preferred-dupe victim vs the pre-fix blind pacing
/// drop, PLUS (#1111) how many content-dupes were EMITTED as the late-dupe release valve (a copy
/// passed downstream), drained on the SAME 5s Streaming-report cadence as
/// [`crate::emit_skip_log::EmitGateSkipLog`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DupeShedLog {
    dupe_shed: u64,
    blind_shed: u64,
    dupe_emitted: u64,
    retired: u64,
    drained: u64,
    fast_drained: u64,
    /// (#1167 v4) How many STARVATION last-frame repeats were emitted (empty-queue 60fps slots
    /// filled by re-emitting the current good frame). Drained SEPARATELY via
    /// [`take_starvation_repeats`](Self::take_starvation_repeats) so the byte-frozen 6-tuple
    /// [`take`](Self::take) / `take_shed_counts` signature (main.rs destructure + external greps)
    /// is unchanged; surfaced as an APPENDED segment on the (#889) summary line.
    starvation_repeats: u64,
}

impl DupeShedLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record ONE captured frame that was shed (never emitted) this poll: `dupe` when it was
    /// preferred as a content-duplicate victim (the #889 on-time deferral), otherwise the ORIGINAL
    /// blind pacing drop (between boundaries).
    pub fn record_shed(&mut self, dupe: bool) {
        if dupe {
            self.dupe_shed = self.dupe_shed.saturating_add(1);
        } else {
            self.blind_shed = self.blind_shed.saturating_add(1);
        }
    }

    /// (#1111) Record ONE frame EMITTED as a copy rather than shed. TWO contributors land here:
    /// the #1111 late-dupe valve (a genuine sub-60 starvation deficit — the historical meaning) AND
    /// (#1167) a corrupted-slot MAKE-UP (a would-be-skipped over-rate Retire/Drain converted to a
    /// copy of the nearest good frame to reclaim a slot a corrupted-buffer drop vacated). Attribute
    /// the two via the `corrupted` count on the same 5s Streaming line (make-ups ≈ the corrupted
    /// rate; a healthy over-rate box with no corruption shows ~0 here). See [`DecimationGate::poll`].
    ///
    /// [`DecimationGate::poll`]: crate::dupe_decimation::DecimationGate::poll
    pub fn record_dupe_emitted(&mut self) {
        self.dupe_emitted = self.dupe_emitted.saturating_add(1);
    }

    /// (#1145) Record ONE over-rate content-dupe RETIRED (shed while advancing the already-stale
    /// boundary, emitting nothing). (#1167 v3) On the over-rate box this is now a LEGITIMATE small
    /// nonzero in STEADY state — `poll`'s steady shallow-lag TRICKLE (once lag ≥
    /// [`SHALLOW_DRAIN_LAG_MIN`]) takes a PACED retire skip (≤1 per [`CONVERGE_SKIP_MIN_GAP_INTERVALS`])
    /// to bleed the grid-lag creep off before it bursts, so expect `retired ≈ 1 per gap`, NOT ~0 (do
    /// not misread that as a regression). Below the trickle threshold, or when paced-out, a steady
    /// shallow-lag dupe still FILLS the slot (counted as a copy in
    /// [`record_dupe_emitted`](Self::record_dupe_emitted)); the CONVERGING tail also records here (its
    /// paced retire). See [`DecimationGate::poll`].
    ///
    /// [`DecimationGate::poll`]: crate::dupe_decimation::DecimationGate::poll
    /// [`SHALLOW_DRAIN_LAG_MIN`]: crate::dupe_decimation::SHALLOW_DRAIN_LAG_MIN
    /// [`CONVERGE_SKIP_MIN_GAP_INTERVALS`]: crate::dupe_decimation::CONVERGE_SKIP_MIN_GAP_INTERVALS
    pub fn record_retired(&mut self) {
        self.retired = self.retired.saturating_add(1);
    }

    /// (#1145 v2 + #1167) Record ONE frame SHED by the queue-depth drain (dropped the oldest to bound
    /// residence). #1145 v2 advanced the boundary on the shed; (#1167) in STEADY over-rate it now
    /// HOLDS the boundary (the next fresher frame fills the slot) — still a shed, still counted here —
    /// and advances only while CONVERGING a deep backlog. NOT counted on the panic-floor copy-fill
    /// (that EMITS, so it lands in [`record_dupe_emitted`](Self::record_dupe_emitted), never here).
    /// See [`DecimationGate::poll`].
    ///
    /// [`DecimationGate::poll`]: crate::dupe_decimation::DecimationGate::poll
    pub fn record_drained(&mut self) {
        self.drained = self.drained.saturating_add(1);
    }

    /// (#1145 v2.1) Record ONE deep-backlog FAST-drain (a content-dupe shed while advancing TWO
    /// stale boundaries, under sustained over-rate at lag > `RETIRE_MAX_LAG_INTERVALS`) — the
    /// mechanism that converges a deep delivery-latency backlog in single-digit seconds instead of
    /// the send-slack-limited ~35 s. See [`DecimationGate::poll`] / [`ShedAction::FastDrain`].
    ///
    /// [`DecimationGate::poll`]: crate::dupe_decimation::DecimationGate::poll
    /// [`ShedAction::FastDrain`]: crate::dupe_decimation::ShedAction::FastDrain
    pub fn record_fast_drained(&mut self) {
        self.fast_drained = self.fast_drained.saturating_add(1);
    }

    /// (#1167 v4) Record `n` STARVATION last-frame repeats emitted this poll — empty-queue 60fps
    /// slots filled by re-emitting the current good frame (see [`DecimationGate::poll`]). Accumulated
    /// separately from the byte-frozen 6-tuple and drained by
    /// [`take_starvation_repeats`](Self::take_starvation_repeats).
    ///
    /// [`DecimationGate::poll`]: crate::dupe_decimation::DecimationGate::poll
    pub fn record_starvation_repeats(&mut self, n: u64) {
        self.starvation_repeats = self.starvation_repeats.saturating_add(n);
    }

    /// (#1167 v4) Drain + reset the accumulated starvation-repeat count. Separate from
    /// [`take`](Self::take) so the byte-frozen 6-tuple signature (main.rs + external greps) is
    /// unchanged; main.rs calls both each 5s window.
    pub fn take_starvation_repeats(&mut self) -> u64 {
        let out = self.starvation_repeats;
        self.starvation_repeats = 0;
        out
    }

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired, drained, fast_drained)`
    /// counts and RESET.
    pub fn take(&mut self) -> (u64, u64, u64, u64, u64, u64) {
        let out = (
            self.dupe_shed,
            self.blind_shed,
            self.dupe_emitted,
            self.retired,
            self.drained,
            self.fast_drained,
        );
        self.dupe_shed = 0;
        self.blind_shed = 0;
        self.dupe_emitted = 0;
        self.retired = 0;
        self.drained = 0;
        self.fast_drained = 0;
        out
    }
}

/// The periodic INFO line proving the mechanism is live: printed on every 5s Streaming-report
/// window (while genlock decimation is active) so a live box shows the mechanism working —
/// never suppressed on an all-zero window (a healthy card legitimately shows 0/0, which is the
/// self-neutralizing behavior by design, not the mechanism being off).
// A flat per-window counter-summary formatter — a params struct would add churn at every
// call site (incl. the rustc scratch replicas) for zero clarity gain on plain u64 counters.
#[allow(clippy::too_many_arguments)]
pub fn dupe_shed_summary(
    dupe_shed: u64,
    blind_shed: u64,
    dupe_emitted: u64,
    retired: u64,
    drained: u64,
    fast_drained: u64,
    starvation_repeats: u64,
    window_secs: u64,
) -> String {
    // (#1167 v4) The `starvation_repeats` segment is APPENDED — every substring before it is
    // byte-frozen (main.rs + external journal greps read them), so a new counter is additive only.
    format!(
        "(#889) dupe-preferring decimation: {dupe_shed} dupe-victim shed / {blind_shed} \
         blind-pacing shed / {dupe_emitted} late-dupe copies emitted (#1111 grid-lock valve) / \
         {retired} boundaries retired (#1145 over-rate absorption) / {drained} depth-drained \
         (#1145 v2 over-rate absorption) / {fast_drained} fast-drained (#1145 v2.1 deep-backlog \
         convergence) / {starvation_repeats} starvation last-frame repeats (#1167 v4 empty-queue \
         slot-fill) over the last ~{window_secs}s"
    )
}
