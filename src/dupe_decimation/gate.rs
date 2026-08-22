use super::*;
use std::collections::VecDeque;

// ── (#889) per-stream gate (boundary + dupe-preference state) ────────────────

/// (#1145 v3 review 🔵 F4) Drop every front entry `<= cutoff` from a trailing-timestamp window.
/// Shared by the unique-rate AND all-captures windows so they prune in lock-step by construction.
fn prune_before(times: &mut VecDeque<u64>, cutoff: u64) {
    while let Some(&front) = times.front() {
        if front <= cutoff {
            times.pop_front();
        } else {
            break;
        }
    }
}

/// Owns the per-box decimation bookkeeping: the pacing boundary state (mirrors what
/// `src/main.rs` tracked as a bare `next_boundary_ns` local before this ticket) PLUS the
/// dupe-preference state (previous captured frame's content hash + whether a dupe was already
/// deferred for the currently-pending boundary). [`poll`](Self::poll) is the single
/// per-captured-frame call: pure math driven entirely by its inputs, so behavior is fully
/// testable off real hardware — feed synthetic capture timestamps + content hashes, collect
/// which get emitted.
#[derive(Debug, Default, Clone)]
pub struct DecimationGate {
    next_boundary_ns: u64,
    prev_hash: Option<u64>,
    deferred_this_boundary: bool,
    shed_log: DupeShedLog,
    /// (#1145) Trailing wall-clock timestamps of the recent UNIQUE (non-dupe) captures within
    /// [`UNIQUE_RATE_WINDOW_NS`]. Its length is the measured unique RATE — the robust "enough
    /// distinct content to hold 60 fps" signal that gates retirement vs the starvation copy valve.
    unique_capture_times: VecDeque<u64>,
    /// (#1145 v3) Trailing wall-clock timestamps of ALL captures (dupe or unique) within
    /// [`UNIQUE_RATE_WINDOW_NS`], pruned in lock-step with [`unique_capture_times`](Self::unique_capture_times).
    /// The `unique / all` RATIO is the GAP-IMMUNE occupancy floor (see [`RETIRE_OCCUPANCY_MIN_PERCENT`]):
    /// a capture hiccup admits NO captures, so it depresses BOTH counts equally and the ratio holds,
    /// whereas the absolute unique COUNT drops below [`retire_min_uniques`] for ~the gap duration.
    all_capture_times: VecDeque<u64>,
    /// (#1145 v2) The MONOTONIC capture instant of the previous frame, to derive the capture
    /// takt (inter-capture interval) that feeds [`takt_ema_interval_ns`]. `0` = uninitialized.
    prev_capture_mono_ns: u64,
    /// (#1145 v2) Integer EMA of the capture takt (inter-capture interval, ns), smoothed with
    /// [`TAKT_EMA_SHIFT`]. Init-seeded to the first observed interval (no long cold-start). Below
    /// [`RETIRE_MIN_TAKT_INTERVAL_NS`] means sustained over-rate — the gate that enables the
    /// queue-depth drain (a healthy 60.00 card reads above it and never drains). `0` = not yet seen.
    takt_ema_interval_ns: u64,
    /// (#1145 v3 review 🟡 F1) Run length of CONSECUTIVE over-[`TAKT_GAP_EXCLUDE_NS`] inter-capture
    /// samples. A one-off hiccup is exactly ONE; at [`TAKT_GAP_SUSTAINED_COUNT`] the takt EMA is reset
    /// so a genuine sub-20 fps COLLAPSE disarms `sustained_over_rate` instead of latching it forever.
    /// Reset to 0 by any in-bound sample.
    consecutive_takt_gaps: u32,
    /// (#1145 v2.1) How many EXTRA boundary intervals the MOST RECENT [`poll`](Self::poll) advanced
    /// beyond the normal single-interval step — i.e. the INTENTIONAL retirement of already-stale
    /// boundaries by [`ShedAction::FastDrain`] (`1` when the +2 fast-drain fired, `0` otherwise).
    /// `main.rs` reads it via [`last_poll_intentional_extra_advance`](Self::last_poll_intentional_extra_advance)
    /// and DEDUCTS it from the `#707` [`crate::genlock_pacing::boundary_skip_count`] diagnostic, so an
    /// intentional fast-drain is NOT miscounted as an un-emitted-content boundary SKIP (the sick-leg
    /// / clock-step signature `leg-health-guard.sh` hard-fails on). Reset to 0 at the start of every
    /// poll.
    last_poll_fast_drain_extra: u64,
    /// (#1145 round 3) The CURRENT frame's luma signature lattice ([`dupe_content_sig`]), staged by
    /// [`note_frame_luma`](Self::note_frame_luma) immediately BEFORE [`poll`](Self::poll) and consumed
    /// (`take()`) inside it. `None` = no lattice noted for this poll → the noise-tolerant compare is
    /// SKIPPED and `prev_luma` is cleared (the fail-safe path: no data → not-dupe → today's exact-hash
    /// behavior, so every one of the pre-round-3 tests that never call `note_frame_luma` is unchanged).
    pending_luma: Option<Vec<u8>>,
    /// (#1145 round 3) The luma lattice noted at the PREVIOUS poll, for the noise-tolerant compare
    /// against the current one. Rotated from `pending_luma` each poll; cleared on a poll with no
    /// pending lattice.
    prev_luma: Option<Vec<u8>>,
    /// (#1145 round 3) Did the PREVIOUS poll classify a NOISY (not byte-identical) content-dupe? The
    /// comparator NEVER classifies two CONSECUTIVE frames as noisy-dupes (an exact byte-identical dupe
    /// is exempt — it is proven): a ~61 fps grabber's surplus is ~1 isolated dupe/s, never a run, so a
    /// run of "dupes" would be a slow content FADE the sparse-diff cannot tell from a still — this cap
    /// hard-bounds even a mis-tuned comparator to shedding at most every other frame and kills the
    /// fade-chaining false-positive class.
    prev_was_noisy_dupe: bool,
    /// (#1167) Pending corrupted-slot make-up deficit: how many GOOD frames a corrupted-buffer drop
    /// (in `src/capture.rs`, before the emit gate) has removed from the stream that the gate still
    /// owes a slot-fill for. Accrued by [`note_corrupted_frame`](Self::note_corrupted_frame),
    /// reclaimed 1:1 in [`poll`](Self::poll) by converting the next slot-skipping
    /// [`ShedAction::Retire`] / [`ShedAction::Drain`] into a copy emit (the nearest good frame), so
    /// an over-rate box with corruption holds the target emit instead of under-running by the
    /// corrupted rate. Bounded by [`CORRUPTED_MAKEUP_MAX_DEFICIT`]. `0` = nothing owed.
    corrupted_makeup_deficit: u64,
    /// (#1167) Set by [`note_corrupted_frame`](Self::note_corrupted_frame): the NEXT inter-capture
    /// takt sample spans the dropped (corrupted) frame, so it is a GAP, not a takt change — exclude
    /// it from the over-rate arming EMA ([`note_capture_takt`](Self::note_capture_takt)), mirroring
    /// the #1145 v3 delivery-hiccup handling so a corrupted drop cannot flip `sustained_over_rate`
    /// off. Consumed (cleared) on the next takt fold.
    pending_takt_gap: bool,
    /// (#1167) How many CONSECUTIVE [`ShedAction::Drain`] polls have HELD the current boundary
    /// (dropped the oldest to bound residence, emitted nothing) without it advancing. The #1167 Drain
    /// no longer skips its slot — it holds the boundary so the next fresher frame fills it — but that
    /// introduces one theoretical loop: a bogus stuck-high residence signal (a garbage `capture_mono`
    /// clamped at [`QUEUE_DEPTH_SANE_MAX_INTERVALS`]) could hold the SAME boundary forever, collapsing
    /// the emit to ~0 (fail-black). The PANIC FLOOR: after [`QUEUE_DEPTH_SANE_MAX_INTERVALS`]
    /// consecutive holds (deeper than any real 4-buffer V4L2 queue), fill the boundary with a copy
    /// instead — the hold-side analogue of the #1111 double-dupe guard, fail-SAFE not fail-black.
    /// Reset to 0 whenever the boundary ADVANCES (any emit / fast-drain), so a genuine over-rate that
    /// alternates drop-and-fill never approaches the floor.
    consecutive_drain_holds: u64,
    /// (#1167) Latch: is the gate CONVERGING a deep emit-grid backlog (a reconnect / restart /
    /// burn-toggle left the grid many intervals behind)? Set true when [`ShedAction::FastDrain`] fires
    /// (`lag > RETIRE_MAX_LAG_INTERVALS`); cleared once the grid is caught up (`lag == 0`) or the card
    /// is no longer over-rate. It selects how [`poll`](Self::poll) applies a shallow-lag
    /// [`ShedAction::Retire`]: while CONVERGING → a PACED retire (advance the stale boundary, emit
    /// nothing, but SMEARED to <=1 skip per [`CONVERGE_SKIP_MIN_GAP_INTERVALS`] so the tail never
    /// bursts; paced-out → FILL). (#1167 v3) When NOT converging (steady over-rate), the shallow Retire
    /// is no longer an unconditional fill: once lag has crept to [`SHALLOW_DRAIN_LAG_MIN`] it takes a
    /// PACED single-slot TRICKLE skip to bleed the creep off before it can reach this band and trip a
    /// FastDrain BURST, and FILLS only below that threshold or when paced-out. So a steady over-rate box
    /// now shows `retired ≈ 1 per gap` (the trickle), not ~0 — this is the v3 root-cause fix for the
    /// 300/293 window oscillation, not a regression. FastDrain (deep band) is left UNPACED at +2 (a
    /// genuine reconnect must converge <=12s); it only fires when a big jitter spike outruns the trickle.
    converging_deep_backlog: bool,
    /// (#1167 v3) The MONOTONIC instant of the most recent convergence slot-SKIP (any boundary-
    /// advance-emit-nothing shed: FastDrain, a latched-Retire, a latched-Drain, or the steady
    /// shallow trickle-drain). The single, shared depth-1 pace budget: a convergence skip is only
    /// PERFORMED when at least [`CONVERGE_SKIP_MIN_GAP_INTERVALS`] intervals of monotonic time have
    /// elapsed since this stamp; otherwise the poll FILLS the slot instead (or, for a Drain, falls
    /// back to the v2 HOLD), so the skips are SMEARED to <=1 per gap and never burst. `0` = never
    /// skipped (the first skip is allowed immediately). A min-gap predicate on elapsed time is
    /// inherently depth-1 (no saved-up-burst path), and ALL skip sites share this one field so they
    /// cannot re-compose the burst between them.
    last_converge_skip_mono_ns: u64,
    /// (#1167 v4) How many STARVATION last-frame repeats the MOST RECENT [`poll`](Self::poll) asks
    /// `main.rs` to emit — how many empty-queue 60fps boundaries passed with NO capture to fill them
    /// (an UNDER-rate dip, `queue_had_frame == false`), each to be filled by re-emitting the current
    /// GOOD frame so emit holds 60. `main.rs` reads it via
    /// [`last_poll_starvation_repeats`](Self::last_poll_starvation_repeats) and, because a repeat is a
    /// FILLED boundary (not a sick-leg SKIP), it is ALSO folded into
    /// [`last_poll_intentional_extra_advance`](Self::last_poll_intentional_extra_advance) so a filled
    /// slot is deducted from the #707 boundary-skip diagnostic. Reset to 0 at the start of every poll.
    last_poll_starvation_repeats: u64,
    /// (#1167 v4) Run length of CONSECUTIVE starvation last-frame repeats emitted WITHOUT an
    /// intervening on-time capture. The empty-queue fill is bounded by [`STARVATION_REPEAT_MAX`]:
    /// each poll may emit at most `STARVATION_REPEAT_MAX - consecutive_starvation_repeats` repeats,
    /// and this is RESET to 0 by any on-time crossing (`lag_intervals == 0` — the source proved it
    /// is keeping pace). A live-but-slightly-slow grabber (57.9 fps) crosses a boundary only ~2×/s
    /// with ~27 on-time frames between (each resetting this), so it never approaches the cap and is
    /// fully filled; a source that NEVER delivers an on-time frame (≤~30 fps — every poll ≥1 interval
    /// late) accumulates to the cap, stops being filled, and under-runs. A moderate sustained
    /// under-rate (~31–56 fps) still has occasional on-time resets so it is filled to 60 here — its
    /// exposure is the capture-rate health guards on the same takt EMA, not this cap (see
    /// [`STARVATION_REPEAT_MAX`]); a FROZEN source (dupes) is excluded by the `!copy` gate. So the cap
    /// is the fail-SAFE that bounds a burst + keeps a genuinely dead/frozen camera looking down.
    consecutive_starvation_repeats: u64,
}

impl DecimationGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pacing boundary state AFTER the most recent [`poll`](Self::poll) call — main.rs reads
    /// this before/after each poll to feed the pre-existing `#707`
    /// [`crate::genlock_pacing::boundary_skip_count`] diagnostic, unchanged by this ticket.
    pub fn next_boundary_ns(&self) -> u64 {
        self.next_boundary_ns
    }

    /// (#1145 v2.1) How many EXTRA boundary intervals the MOST RECENT [`poll`](Self::poll) advanced
    /// beyond a normal single step — the INTENTIONAL stale-boundary retirement of a fast-drain (`1`
    /// when the +2 fired, else `0`). main.rs DEDUCTS this from the `#707`
    /// [`crate::genlock_pacing::boundary_skip_count`] before recording, so an intentional fast-drain
    /// is never miscounted as an un-emitted-content boundary SKIP (the sick-leg / clock-step signal
    /// `leg-health-guard.sh` hard-fails on). Read it right after `poll`, alongside `next_boundary_ns`.
    pub fn last_poll_intentional_extra_advance(&self) -> u64 {
        self.last_poll_fast_drain_extra + self.last_poll_starvation_repeats
    }

    /// (#1167 v4) How many STARVATION last-frame repeats the MOST RECENT [`poll`](Self::poll) asks
    /// `main.rs` to emit for this frame — empty-queue 60fps boundaries that an UNDER-rate capture dip
    /// left unfilled. `main.rs` re-emits the current GOOD frame this many times (with distinct per-slot
    /// timecodes) BEFORE emitting the current frame, so emit holds 60 across the grabber's sub-60
    /// wander. `0` when the source is keeping pace (or over-rate — that regime is v2/v3's). Read it
    /// right after `poll`, alongside `next_boundary_ns` / `last_poll_intentional_extra_advance`.
    pub fn last_poll_starvation_repeats(&self) -> u64 {
        self.last_poll_starvation_repeats
    }

    /// (#1145 round 3) Stage this frame's luma signature lattice ([`dupe_content_sig`]'s second
    /// element) for the NEXT [`poll`](Self::poll) to consume — call it immediately BEFORE `poll`,
    /// mirroring how `content_hash` is computed for the same frame. `poll` compares it to the
    /// previous frame's lattice with the noise-tolerant [`frames_are_content_dupes`] test to catch a
    /// marginal over-rate card's noisy re-sample dupes (which the exact `content_hash` misses). NOT
    /// calling it (any pre-round-3 caller) leaves the pending lattice `None`, so the noisy path is
    /// skipped and behavior is byte-identical to the exact-hash-only gate — the self-neutralizing
    /// fail-safe. Takes the lattice BY VALUE ([`dupe_content_sig`] returns a fresh `Vec`), so the
    /// per-frame capture loop moves it in with no extra copy.
    pub fn note_frame_luma(&mut self, luma: Vec<u8>) {
        self.pending_luma = Some(luma);
    }

    /// (#1167) Register that ONE captured buffer was dropped for content corruption
    /// (`V4L2_BUF_FLAG_ERROR` / a short buffer) in `src/capture.rs::process_frame` BEFORE it could
    /// reach [`poll`](Self::poll). At an over-rate that removes a would-be-emitted GOOD frame from
    /// the stream, so its 60 fps slot would otherwise be absorbed by the over-rate shed machinery
    /// (a [`ShedAction::Retire`] / [`ShedAction::Drain`] that advances the boundary but emits
    /// nothing) → the emit rate falls below target by exactly the corrupted rate (a strih genlock
    /// FIFO hold → the cam1 align sawtooth). This accrues a bounded make-up DEFICIT
    /// ([`CORRUPTED_MAKEUP_MAX_DEFICIT`]) that [`poll`](Self::poll) reclaims 1:1 on the next
    /// slot-skipping shed by emitting the nearest good frame as a copy instead. Corrupted CONTENT
    /// is never forwarded — the make-up emits a subsequent GOOD frame, never the corrupted one.
    /// Call it (from `main.rs`) exactly once per corrupted-buffer drop, only while genlock
    /// decimation is active. The corrupted-spanning inter-capture takt sample is also marked as a
    /// GAP so it is excluded from the over-rate arming EMA (mirrors the #1145 v3 hiccup handling —
    /// keeps `sustained_over_rate` armed through the drop).
    pub fn note_corrupted_frame(&mut self) {
        self.corrupted_makeup_deficit =
            (self.corrupted_makeup_deficit + 1).min(CORRUPTED_MAKEUP_MAX_DEFICIT);
        // (#1167) the next inter-capture takt sample spans this dropped frame — mark it a GAP so it
        // is excluded from the over-rate arming EMA (mirrors the #1145 v3 hiccup handling).
        self.pending_takt_gap = true;
    }

    /// (#1167) The pending corrupted-slot make-up deficit — how many corrupted-induced slots the
    /// gate still owes a fill for (see [`note_corrupted_frame`](Self::note_corrupted_frame)). `0`
    /// when nothing is owed. Diagnostic/read-only, exercised by the #1167 tests.
    pub fn corrupted_makeup_deficit(&self) -> u64 {
        self.corrupted_makeup_deficit
    }

    /// (#1145) Prune the trailing unique-capture window to entries within [`UNIQUE_RATE_WINDOW_NS`]
    /// of `now_ns`. Called on EVERY poll (dupe or unique) so the COUNT is honest at every instant —
    /// a dupe read must NOT see a stale-high count (that is the #1145-review 🔴 frozen-source
    /// blackout), and the honest count is what makes the tight over-rate-vs-starved separation
    /// predictable. A COUNT over the window (not an interval EMA) reads the true unique RATE
    /// regardless of per-frame jitter or dupe clustering.
    fn prune_unique_window(&mut self, now_ns: u64) {
        let cutoff = now_ns.saturating_sub(UNIQUE_RATE_WINDOW_NS);
        // (#1145 v3) prune BOTH the unique-rate AND the all-captures windows with the SAME cutoff on
        // every poll, so the occupancy ratio is honest at every instant.
        prune_before(&mut self.unique_capture_times, cutoff);
        prune_before(&mut self.all_capture_times, cutoff);
    }

    /// (#1145) Does the trailing window prove the source carries enough DISTINCT content to hold a
    /// steady emit at `interval_ns` after shedding its surplus dupes — the signal that gates
    /// retirement vs the late-dupe copy valve? TWO conditions, both required:
    /// - COUNT: at least [`retire_min_uniques`] uniques in the trailing [`UNIQUE_RATE_WINDOW_NS`]
    ///   (a near-full-target unique rate) — separates a genuine over-rate from a starved sub-target
    ///   source (a 50->60 pulldown), which stays on the copy valve.
    /// - FRESHNESS (#1145 review 🔴): the most recent unique arrived within
    ///   [`RETIRE_UNIQUE_FRESH_BOUND_INTERVALS`] emit intervals of `now`. A genuinely FROZEN source
    ///   (100% content-dupes — a dead painter / wedged upstream) never refreshes the window, so its
    ///   stale COUNT stays high; without the freshness gate retirement would fire forever and
    ///   collapse the emit to ~0 fps (a total BLACKOUT). Freshness makes a freeze fall back to the
    ///   copy valve (a frozen picture on a LIVE stream — the pre-#1145 behavior).
    fn enough_unique_to_hold_target(
        &self,
        now_ns: u64,
        interval_ns: u64,
        sustained_over_rate: bool,
    ) -> bool {
        // FRESHNESS gate (unchanged, #1145 review 🔴): a genuinely FROZEN source (no recent unique —
        // a dead painter / wedged upstream) must fall to the #1111 copy valve, never retire into a
        // ~0 fps BLACKOUT. Checked FIRST so neither floor below can override a stale window.
        let fresh = match self.unique_capture_times.back() {
            Some(&last_unique_ns) => {
                now_ns.saturating_sub(last_unique_ns)
                    <= RETIRE_UNIQUE_FRESH_BOUND_INTERVALS.saturating_mul(interval_ns)
            }
            None => return false,
        };
        if !fresh {
            return false;
        }
        // ABSOLUTE COUNT floor (unchanged, #666-aligned): a near-full-target unique COUNT in the
        // trailing window == an absolute `>= 57`-unique/s guarantee.
        if self.unique_capture_times.len() >= retire_min_uniques(interval_ns) {
            return true;
        }
        // (#1145 v3) OCCUPANCY floor — the GAP-IMMUNE supplement. A capture hiccup transiently
        // depresses the absolute count above (the window spans dead time), forcing a genuine
        // over-rate card onto the copy valve for ~the gap duration (the surplus then exports into
        // the strih FIFO). The `unique / all` ratio is gap-immune. Gated on `sustained_over_rate`
        // (capture rate `> 60.3`) so `unique >= 95% × total` keeps the retired emit `>= 57` (the
        // #666 floor) — an under-rate / starved source never reaches this arm. See
        // [`RETIRE_OCCUPANCY_MIN_PERCENT`].
        if sustained_over_rate {
            let total = self.all_capture_times.len();
            if total >= RETIRE_OCCUPANCY_MIN_SAMPLES
                && (self.unique_capture_times.len() as u64) * 100
                    >= (total as u64) * RETIRE_OCCUPANCY_MIN_PERCENT
            {
                return true;
            }
        }
        false
    }

    /// (#1145 v2) Fold this frame's MONOTONIC capture instant into the capture-takt EMA (the
    /// inter-capture interval smoothed with [`TAKT_EMA_SHIFT`]). Init-seeded to the first observed
    /// interval so there is no long cold-start. `capture_mono_ns == 0` (the FrameInfo "no real
    /// measurement" sentinel) is skipped — it carries no interval; a non-advancing / backward
    /// monotonic stamp is likewise ignored (a genuine capture clock only moves forward).
    // (#1165 split) `pub(crate)` (was private) so the test module — now a sibling submodule after
    // the file split, no longer a child of this `impl`'s module — can still exercise it. No
    // behaviour change; still crate-internal.
    pub(crate) fn note_capture_takt(&mut self, capture_mono_ns: u64) {
        if capture_mono_ns == 0 {
            return;
        }
        // (#1167) a corrupted frame was dropped since the previous good capture (it never reached
        // poll), so THIS inter-capture interval spans the missing sample — a known benign GAP,
        // exactly like a #1145 v3 delivery hiccup. Do NOT fold it (folding it would pull the takt
        // EMA up and risk disarming `sustained_over_rate`); just advance the baseline so the NEXT
        // interval is measured cleanly. LEAVE `consecutive_takt_gaps` UNTOUCHED — a corrupted-
        // spanning interval carries NO evidence about the takt (it was never measured), so it must
        // neither RESET the #1145 v3 F1 collapse counter (which would erase genuine-collapse
        // evidence — a dying card producing corrupted storms could then never reach
        // `TAKT_GAP_SUSTAINED_COUNT` and would latch `sustained_over_rate` on a collapsed source)
        // nor INCREMENT it (which would falsely count toward collapse). Evidence-neutral.
        let pending_takt_gap = core::mem::take(&mut self.pending_takt_gap);
        if self.prev_capture_mono_ns != 0 && capture_mono_ns > self.prev_capture_mono_ns {
            if pending_takt_gap {
                self.prev_capture_mono_ns = capture_mono_ns;
                return;
            }
            let interval = capture_mono_ns - self.prev_capture_mono_ns;
            // (#1145 v3) gap-excluded fold: a delivery HICCUP (a blocked V4L2 dequeue) produces ONE
            // huge inter-capture interval that is NOT a takt change — fold it and it poisons the
            // ~256-frame EMA, flipping `sustained_over_rate` off for ~7-12 s and disarming every
            // over-rate drain (the #1145 v3 residual). A genuine rate change shows in EVERY sample;
            // a one-off gap in ONE, so SKIP a lone outlier (never folded), but still advance
            // `prev_capture_mono_ns` below so the NEXT interval is measured cleanly. See
            // [`TAKT_GAP_EXCLUDE_NS`].
            if interval <= TAKT_GAP_EXCLUDE_NS {
                self.consecutive_takt_gaps = 0;
                self.takt_ema_interval_ns = if self.takt_ema_interval_ns == 0 {
                    interval // seed
                } else {
                    let e = self.takt_ema_interval_ns as i128;
                    (e + ((interval as i128 - e) >> TAKT_EMA_SHIFT)) as u64
                };
            } else {
                // (#1145 v3 review 🟡 F1) over-bound sample. A HICCUP is exactly ONE such sample; a
                // GENUINE rate COLLAPSE (a card dropping to < 20 fps — every interval over-bound) must
                // NOT latch `sustained_over_rate` on forever (that would keep the unique-blind
                // depth-Drain arm + the occupancy floor armed for a non-over-rate source). After
                // [`TAKT_GAP_SUSTAINED_COUNT`] CONSECUTIVE over-bound samples it is no longer a lone
                // hiccup — RESET the EMA so `sustained_over_rate` disarms and re-seeds cleanly when the
                // card recovers. K >= 2 fully preserves the one-off-hiccup fix (a hiccup never reaches K).
                self.consecutive_takt_gaps = self.consecutive_takt_gaps.saturating_add(1);
                if self.consecutive_takt_gaps >= TAKT_GAP_SUSTAINED_COUNT {
                    self.takt_ema_interval_ns = 0;
                }
            }
        }
        self.prev_capture_mono_ns = capture_mono_ns;
    }

    /// (#1145 v2) Is the capture takt genuine SUSTAINED over-rate — the gate that enables the
    /// queue-depth drain? True iff the EMA capture interval is below [`RETIRE_MIN_TAKT_INTERVAL_NS`]
    /// (`1e9 / 60.3`). A healthy 60.00 card reads ~16.667 ms (ABOVE → false → the depth-drain is off
    /// and the card is byte-identical to v1, including a #1131 buffered-drain stall-recovery); a
    /// 61.x card reads ~16.3 ms (below → true → drain engages). `0` (not yet seen) → false.
    // (#1165 split) `pub(crate)` (was private) so the sibling test submodule can still exercise it
    // after the file split. No behaviour change; still crate-internal.
    pub(crate) fn sustained_over_rate(&self) -> bool {
        self.takt_ema_interval_ns != 0 && self.takt_ema_interval_ns < RETIRE_MIN_TAKT_INTERVAL_NS
    }

    /// (#1167 v5) Is there a LIVE capture takt — a real, non-collapsed EMA measurement
    /// (`takt_ema_interval_ns != 0`)? The REGIME-INDEPENDENT arming signal for the empty-queue
    /// last-frame repeat: v5 replaced v4's positive-`sustained_under_rate` rate gate with this so the
    /// fill fires in BOTH wander directions. The sick ShadowCast grabber averages OVER-rate
    /// (61–63 fps) yet a 25–40 ms VIDIOC_DQBUF stall drains the V4L2 queue empty at a 60 fps boundary
    /// — v4's under-rate gate read that as over-rate and left the slot UNFILLED, so the grid crept
    /// behind, the #131 resync skipped the accumulated boundaries, emit under-ran, and the strih
    /// receiver saw the gap+burst → the cam1 [4i/8align] sawtooth. Keyed on the takt EMA (not a rate
    /// threshold) for the two reasons the rate gate also relied on: (1) a caller that disables the EMA
    /// (`capture_mono_ns == 0`, the legacy over-rate/starved unit tests) reads `takt_ema == 0` →
    /// never arms the fill, so it stays byte-inert in exactly those tests (the mono=0 collision that
    /// broke v4's first `!sustained_over_rate` attempt — solved here identically, since this ALSO
    /// requires `takt_ema != 0`); (2) a GENUINELY collapsing leg (≥ [`TAKT_GAP_SUSTAINED_COUNT`]
    /// consecutive > [`TAKT_GAP_EXCLUDE_NS`] gaps — a card dropping below ~20 fps) RESETS the EMA to
    /// 0, so the fill disarms and the dead leg under-runs, staying visible to #666 / the frozen-leg
    /// attribution (the ticket's hard constraint — a dead camera must still look down). A live grabber
    /// near 60 (over/at/under) keeps a non-zero EMA, so the fill is regime-independent. The remaining
    /// gates (`!copy`, `!queue_had_frame`, `!converging`, the consecutive cap, `1 <= lag <= 8`) do
    /// the rest — see [`apply_starvation_fill`](Self::apply_starvation_fill).
    fn has_live_capture_takt(&self) -> bool {
        self.takt_ema_interval_ns != 0
    }

    /// (#1167) FILL the current 60fps slot: advance the boundary one interval AND emit the current
    /// GOOD frame as a #1111 copy (the nearest good frame). The shared tail of the three slot-fill
    /// sites — the corrupted make-up reclaim, the steady shallow-lag Retire-fill, and the Drain-hold
    /// panic floor — so they stay lock-step. Resets the deferral + the Drain-hold streak (the boundary
    /// advanced). Returns `true` (this poll emits). The [`ShedAction::Emit`] arm is deliberately NOT a
    /// caller: it advances unconditionally but records a copy only for `copy: true`, so it keeps its
    /// own inline body.
    fn fill_slot(&mut self, candidate_next: u64) -> bool {
        self.next_boundary_ns = candidate_next;
        self.deferred_this_boundary = false;
        self.consecutive_drain_holds = 0;
        self.shed_log.record_dupe_emitted();
        true
    }

    /// (#1167 v4/v5) The empty-queue STARVATION fill, applied by the `Emit` arm of [`poll`](Self::poll)
    /// (extracted for the #414 poll-length budget). At an empty-queue dip — an UNDER-rate window
    /// (v4) OR a capture STALL on an AVERAGE-over-rate box (v5: a 25–40 ms VIDIOC_DQBUF stall drains
    /// the queue) — the V4L2 queue empties and 60fps boundaries pass with NO capture to fill them
    /// (the loop genuinely waited → `queue_had_frame == false`); the v2/v3 fills can't help — they
    /// only convert a captured frame's shed into a copy, none fabricates an emit when no frame
    /// arrived. This reports up to [`STARVATION_REPEAT_MAX`] last-frame repeats (`main.rs` re-emits
    /// the CURRENT GOOD frame — it passed the corruption check, so never corrupted content) for the
    /// boundaries this dip left unfilled, and advances the grid to catch up. Gated: a FRESH unique
    /// (`!copy` — a dupe/frozen source takes the copy valve and gets NO repeats, so a dead/frozen leg
    /// still under-runs), an EMPTY queue (`!queue_had_frame` — the ONE gate that keeps the over-rate
    /// v2/v3 machinery byte-unchanged for a NON-empty queue), a LIVE capture takt
    /// ([`has_live_capture_takt`](Self::has_live_capture_takt) — v5 made this REGIME-INDEPENDENT,
    /// replacing v4's positive-under-rate gate so a stall on an over-rate box also fills; a collapsed
    /// leg resets the EMA → not filled → dead-leg semantics) and NOT converging (decoupled from the
    /// over-rate v2/v3 CONVERGENCE tail — a SEPARATE counter, no shared pace budget), a stale boundary
    /// (`lag >= 1`) within the non-resync catch-up band (`lag <= GENLOCK_MAX_CATCHUP_INTERVALS`) so a
    /// deeper empty-queue GAP still resyncs honestly (dead-leg semantics). Bounded to STARVATION_REPEAT_MAX
    /// CONSECUTIVE repeats, RESET by any on-time capture (`lag_intervals == 0`) — so a source that
    /// keeps pace (57.9 fps, crossing a boundary ~2×/s with on-time frames between) is fully filled,
    /// while a source that NEVER catches up (≤~30 fps, every poll late) accumulates to the cap and
    /// stays exposed. Advancing `candidate_next + repeats*interval` is `<=` the first boundary after
    /// `now` (the `lag <= 8` gate guarantees no resync fired, so `candidate_next == boundary +
    /// interval`, and `repeats <= lag`), so it drains the accumulating lag without overshooting.
    fn apply_starvation_fill(
        &mut self,
        copy: bool,
        queue_had_frame: bool,
        lag_intervals: u64,
        candidate_next: u64,
        interval_ns: u64,
    ) {
        let starvation_repeats = if !copy
            && !queue_had_frame
            && self.has_live_capture_takt()
            && !self.converging_deep_backlog
            && (1..=crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS).contains(&lag_intervals)
        {
            let budget = STARVATION_REPEAT_MAX.saturating_sub(self.consecutive_starvation_repeats);
            lag_intervals.min(budget)
        } else {
            0
        };
        if starvation_repeats > 0 {
            self.consecutive_starvation_repeats = self
                .consecutive_starvation_repeats
                .saturating_add(starvation_repeats);
            self.last_poll_starvation_repeats = starvation_repeats;
            self.shed_log.record_starvation_repeats(starvation_repeats);
            self.next_boundary_ns = candidate_next + starvation_repeats * interval_ns;
        } else {
            self.next_boundary_ns = candidate_next;
        }
        if lag_intervals == 0 {
            self.consecutive_starvation_repeats = 0;
        }
    }

    /// Feed ONE captured frame (`now_ns` wall-clock capture instant, `content_hash` from
    /// [`dupe_content_hash`], `queue_had_frame` from
    /// [`crate::capture_stall::frame_from_nonempty_queue`] — was this frame already buffered in the
    /// V4L2 queue?) through the pacing + dupe-preference gate. `interval_ns == 0` disables
    /// decimation entirely (mirrors [`crate::genlock_pacing::genlock_emit_gate`]'s own guard) —
    /// always emits, no hashing/state kept. Returns whether THIS captured frame should be
    /// emitted.
    ///
    /// (#1145 v2) `now_mono_ns` (`monotonic_clock_ns()`) and `capture_mono_ns`
    /// (`FrameInfo::capture_monotonic_100ns * 100`, `0` = no measurement) are the MONOTONIC clocks
    /// the queue-depth drain needs: their difference is this frame's queue residence
    /// ([`queue_depth_intervals`]), and consecutive `capture_mono_ns` values feed the capture-takt
    /// EMA ([`note_capture_takt`](Self::note_capture_takt)). Both are monotonic (a duration + an
    /// interval), so they are immune to the DanteSync realtime steps `now_ns` is gridded to. Pass `0`
    /// for both to disable the v2 depth-drain (e.g. a test exercising only the pre-v2 arms).
    pub fn poll(
        &mut self,
        now_ns: u64,
        interval_ns: u64,
        content_hash: u64,
        queue_had_frame: bool,
        now_mono_ns: u64,
        capture_mono_ns: u64,
    ) -> bool {
        if interval_ns == 0 {
            return true;
        }
        // (#1145 v2.1) reset the per-poll intentional-extra-advance accounting; only the FastDrain
        // arm sets it (see the field doc — it keeps a fast-drain out of the #707 skip diagnostic).
        self.last_poll_fast_drain_extra = 0;
        // (#1167 v4) reset the per-poll starvation-repeat count; only the Emit arm sets it (an
        // under-rate empty-queue fill — see the field doc).
        self.last_poll_starvation_repeats = 0;
        // (#1145 v2) fold the capture takt + measure this frame's monotonic queue residence.
        self.note_capture_takt(capture_mono_ns);
        let sustained_over_rate = self.sustained_over_rate();
        let queue_depth = queue_depth_intervals(now_mono_ns, capture_mono_ns, interval_ns);
        // (#1131) `queue_had_frame` — did THIS captured frame come from a NON-EMPTY V4L2 queue (the
        // driver already had it buffered, per `capture_stall::frame_from_nonempty_queue`)? A buffered
        // frame PROVES a real captured frame exists to fill the next un-emitted boundary, so the gate
        // must catch up one interval and never grid-resync past it (the #1131 multi-slot-skip judder,
        // whose 0-capture-dropped signature confirms the frames exist). A frame from an empty queue
        // (the loop genuinely waited — a device/clock gap) keeps the pre-existing #131 resync.
        let (would_emit, candidate_next) = crate::genlock_pacing::genlock_emit_gate(
            now_ns,
            self.next_boundary_ns,
            interval_ns,
            queue_had_frame,
        );
        // (#1145) numeric boundary staleness: 0 when on-time/surplus (deferring a dupe is
        // lag-neutral) or between boundaries; >= 1 once the crossed boundary is already stale (its
        // downstream hold already happened) so a dupe crossing it can RETIRE it. Shares the boundary
        // math with `genlock_emit_gate` above.
        let lag_intervals = crate::genlock_pacing::genlock_lag_intervals(
            now_ns,
            self.next_boundary_ns,
            interval_ns,
        );
        // (#1167) The deep-backlog convergence latch clears once the grid is caught up (`lag == 0`) OR
        // the source is no longer over-rate. Set by the FastDrain arm below (which itself requires
        // `sustained_over_rate`), it must not persist if over-rate DISARMS (a takt-hiccup EMA reset, or
        // the card settling at ~60.0) while lag plateaus at 1..4 — otherwise lag never reaches 0
        // (boundary-advance rate == wall rate) and the latch would strand every shallow-lag dupe on the
        // RETIRE (no-emit) path forever, re-arming the very deficit this fix removes. (review 🟡)
        if lag_intervals == 0 || !sustained_over_rate {
            self.converging_deep_backlog = false;
        }

        // (#1145 review 🔵) A BACKWARD DanteSync clock step (#131) leaves the window's pre-step
        // timestamps "in the future" (`> now_ns`), which would block pruning for the step's duration
        // and inflate the count in the aggressive (retire-forcing) direction. Clear the window so a
        // capture after a backward step re-latches from scratch — mirrors `genlock_emit_gate`'s own
        // backward re-latch.
        // (#1145 v3 review 🔵 F3) clear BOTH windows when EITHER holds a future-timestamped entry, so
        // a backward step can never leave `unique_capture_times` populated while `all_capture_times`
        // was cleared (which would read a >100% occupancy ratio from mixed clock epochs). Symmetric
        // by construction.
        let backward_step = self
            .unique_capture_times
            .back()
            .is_some_and(|&back| back > now_ns)
            || self
                .all_capture_times
                .back()
                .is_some_and(|&back| back > now_ns);
        if backward_step {
            self.unique_capture_times.clear();
            self.all_capture_times.clear();
        }

        let exact_dupe = self.prev_hash == Some(content_hash);
        self.prev_hash = Some(content_hash);
        // (#1145 round 3) noise-tolerant content-dupe detection. A marginal jittery over-rate card
        // (CAM2, the painter box) delivers surplus dupes that are noisy optical RE-SAMPLES of the
        // same painted frame — NOT byte-identical repeats like CAM1's steady buffer-repeat — so the
        // exact hash above reads `is_dupe=false` and the dupe is EMITTED as a "unique" (a held
        // painted-id downstream = Δ1), leaving the over-rate unabsorbed so a later frame is
        // force-shed (a skipped painted-id = Δ3): the balanced Δ1/Δ3 aliasing churn the #1142
        // uniformity gate REDs. Compare this frame's luma lattice to the previous one with the
        // noise-tolerant test. ARMED only under `sustained_over_rate` (a healthy 60.00 card never
        // consults it → byte-identical to the exact-hash-only gate) and NEVER on two consecutive
        // frames (a run would be a content fade, not an isolated grabber dupe). No staged lattice
        // (any pre-round-3 caller) → clear prev + not-dupe (fail-safe = today's exact behavior).
        let this_luma = self.pending_luma.take();
        let noisy_dupe = !exact_dupe
            && sustained_over_rate
            && !self.prev_was_noisy_dupe
            && match (self.prev_luma.as_deref(), this_luma.as_deref()) {
                (Some(prev), Some(now)) => frames_are_content_dupes(prev, now),
                _ => false,
            };
        self.prev_luma = this_luma;
        self.prev_was_noisy_dupe = noisy_dupe;
        // (#1145 round 3 [green]) fold the noise-tolerant result into `is_dupe`. Exact FIRST (a
        // byte-identical dupe is proven regardless of the lattice — CAM1 unchanged); the noisy path
        // only ADDS detections under sustained over-rate, so a marginal card's re-sample dupes are
        // now shed as PROVEN dupes (retire / dupe-drain) instead of emitted-as-unique + a
        // compensating shed — killing the Δ1/Δ3 churn. A noisy dupe is also NOT counted below in the
        // unique-rate window (it carries no distinct content), correcting `enough_unique`.
        let is_dupe = exact_dupe || noisy_dupe;

        // (#1145) A unique capture updates the trailing unique-rate window; a dupe carries no new
        // distinct content so it only READS it. `enough_unique` is the robust "source can hold the
        // target without copies" signal (a near-full unique rate AND a recent unique) that separates
        // over-rate retirement from BOTH a genuine starved-source (pulldown — the copy valve holds
        // the grid) and a frozen source (no recent unique — the copy valve holds a frozen picture on
        // a live stream, never a blackout).
        if !is_dupe {
            self.unique_capture_times.push_back(now_ns);
        }
        // (#1145 v3) EVERY capture (dupe or unique) feeds the ALL-captures window — the denominator
        // of the gap-immune occupancy floor in `enough_unique_to_hold_target`.
        self.all_capture_times.push_back(now_ns);
        self.prune_unique_window(now_ns);
        let enough_unique =
            self.enough_unique_to_hold_target(now_ns, interval_ns, sustained_over_rate);

        let action = dupe_shed_action(
            would_emit,
            is_dupe,
            self.deferred_this_boundary,
            lag_intervals,
            enough_unique,
            queue_depth,
            sustained_over_rate,
        );
        // (#1167) corrupted-slot make-up: a corrupted buffer dropped in `src/capture.rs` before the
        // gate removed a would-be-emitted GOOD frame from an over-rate stream, so this otherwise
        // slot-skipping over-rate shed (Retire / Drain — advance the boundary, emit nothing) would
        // leave a hole the strih genlock FIFO holds through (the cam1 align sawtooth). While a
        // make-up is owed, RECLAIM the slot: emit the current GOOD frame (a byte/optical dupe in the
        // Retire case = a repeat of the nearest good frame; a fresh good frame in the Drain case) as
        // a copy and consume one deficit unit — fills the slot with the nearest good frame (issue's
        // invariant: a single-slot dupe is acceptable, a skipped slot never is). Corrupted CONTENT is
        // never forwarded — only a subsequent GOOD frame is emitted. Bounded 1:1 to the corrupted
        // count, so the genuine over-rate latency drain beyond the deficit is untouched. Counted as a
        // #1111 copy (`record_dupe_emitted`) — cross-reference the `corrupted` count on the Streaming
        // line to attribute it. Advances the boundary one interval, exactly as Retire/Drain/Emit do.
        if corrupted_makeup_reclaims(action, self.corrupted_makeup_deficit) {
            self.corrupted_makeup_deficit -= 1;
            return self.fill_slot(candidate_next); // advance + emit the nearest good frame as a copy
        }
        // (#1167 v3) the SHARED depth-1 pace budget: is a convergence slot-SKIP allowed this poll?
        // At least CONVERGE_SKIP_MIN_GAP_INTERVALS of MONOTONIC time must have elapsed since the last
        // performed skip. A paced-out convergence shed FILLS the slot (Retire/FastDrain) or HOLDS
        // (Drain) instead — smearing the skips to <=1 per gap so a burst can never bunch into one 5s
        // window (the cam1 [4i/8align] sawtooth). Monotonic, so DanteSync realtime steps can neither
        // freeze convergence (backward step) nor grant a free skip (forward step).
        let paced_skip_ok = now_mono_ns.saturating_sub(self.last_converge_skip_mono_ns)
            >= CONVERGE_SKIP_MIN_GAP_INTERVALS.saturating_mul(interval_ns);
        match action {
            ShedAction::BlindShed => {
                // The ORIGINAL blind pacing drop (between boundaries) -- boundary unchanged
                // (candidate_next == the pending boundary here), deferral state untouched.
                self.next_boundary_ns = candidate_next;
                self.shed_log.record_shed(false);
                false
            }
            ShedAction::Defer => {
                // #889 on-time deferral: shed the dupe, keep the SAME boundary pending -- the next
                // captured frame is re-evaluated against it (bounded to one deferral).
                self.deferred_this_boundary = true;
                self.shed_log.record_shed(true);
                false
            }
            ShedAction::Retire => {
                // (#1145 v1 decision) shallow-stale over-rate dupe. (#1167 v3) its APPLICATION now has
                // THREE cases — deep-convergence retire (paced), steady trickle-drain (paced), or fill:
                if self.converging_deep_backlog {
                    // Deep-backlog convergence tail (a FastDrain fired, grid still behind): the shallow
                    // tail must SKIP (advance the already-stale boundary, emit nothing) to finish the
                    // convergence — but (#1167 v3) PACED, so the tail is smeared to <=1 skip per gap
                    // instead of a burst. Boundary lag still drains at the over-rate surplus even while
                    // paced-out (a fill advances +1 too), so the reconnect still converges fast.
                    if paced_skip_ok {
                        self.deferred_this_boundary = false;
                        self.consecutive_drain_holds = 0; // the boundary advances -> hold streak broken
                        self.next_boundary_ns = candidate_next;
                        self.last_converge_skip_mono_ns = now_mono_ns; // stamp the shared pace budget
                        self.shed_log.record_retired();
                        false
                    } else {
                        // paced-out: FILL (the v2 steady behavior) — a dupe in hand is byte/optical
                        // identical to the current content, so the copy loses nothing; the boundary
                        // still advances +1 so lag keeps draining.
                        self.fill_slot(candidate_next)
                    }
                } else if sustained_over_rate
                    && lag_intervals >= SHALLOW_DRAIN_LAG_MIN
                    && paced_skip_ok
                {
                    // (#1167 v3) STEADY shallow trickle-drain: a real interval of grid lag has crept
                    // in (jitter re-injected it) but we never went deep. Take ONE paced skip to drain
                    // it before it can accumulate past RETIRE_MAX_LAG_INTERVALS and trip a FastDrain
                    // BURST — the root-cause fix for the v2 300/293 oscillation. Paced (<=1 per gap),
                    // so at the slow steady accumulation rate it fires ~1 skip per 5s window (299-300,
                    // never a burst). Only a dupe reaches here (path 3 routes every unique to Emit
                    // first), so no unique is dropped; #666 stays safe (>=58 fps at this pace cap).
                    self.deferred_this_boundary = false;
                    self.consecutive_drain_holds = 0; // the boundary advances -> hold streak broken
                    self.next_boundary_ns = candidate_next;
                    self.last_converge_skip_mono_ns = now_mono_ns; // stamp the shared pace budget
                    self.shed_log.record_retired();
                    false
                } else {
                    // Steady over-rate, lag below the trickle threshold OR paced-out: FILL the slot
                    // with a copy of this good frame and advance — the #1167 invariant (every buffered
                    // 60fps slot filled, never skipped). #1142-safe: fills a slot a missed boundary left
                    // owed (no unique displaced). Counted as a #1111 copy (`record_dupe_emitted`).
                    self.fill_slot(candidate_next)
                }
            }
            ShedAction::Drain => {
                // (#1145 v2 + #1167) queue-depth drain: shed the OLDEST (this) frame to bound the
                // residence — the sustained-over-rate absorption that keeps the delivery-latency
                // sawtooth flat and pre-empts the V4L2-overflow burst. Application splits on whether a
                // deep backlog is CONVERGING (symmetric with the shallow-lag Retire arm):
                self.deferred_this_boundary = false;
                if self.converging_deep_backlog && paced_skip_ok {
                    // Converging a deep backlog, PACED skip available: ADVANCE the boundary (as #1145
                    // v2 did) so this drop CONTRIBUTES to the convergence, and stamp the shared budget.
                    // (#1167 v3) A paced-out converging Drain falls through to the v2 STEADY-HOLD below
                    // (NEVER a FILL — Drain's frame is >=2 intervals STALE, so filling it would emit
                    // stale content; the HOLD drops it and the next fresher frame fills the slot).
                    self.consecutive_drain_holds = 0;
                    self.next_boundary_ns = candidate_next;
                    self.last_converge_skip_mono_ns = now_mono_ns; // stamp the shared pace budget
                    self.shed_log.record_drained();
                    return false;
                }
                // Steady over-rate (or a paced-out converging Drain): #1167 HOLDS the boundary (do NOT advance) so the NEXT fresher frame
                // fills the same 60fps slot — dropping the oldest still bounds residence, but no slot is
                // ever skipped while a captured frame is buffered (the ticket invariant; the emitted
                // filler has residence below the threshold, so latency stays bounded).
                self.consecutive_drain_holds = self.consecutive_drain_holds.saturating_add(1);
                if self.consecutive_drain_holds >= DRAIN_HOLD_PANIC_FLOOR {
                    // PANIC FLOOR (fail-SAFE, not fail-black): a bogus stuck-high residence (garbage
                    // `capture_mono`, clamped at QUEUE_DEPTH_SANE_MAX_INTERVALS) would hold the SAME
                    // boundary forever -> fill the slot with a copy instead. `fill_slot` EMITS (not a
                    // shed), so it does NOT `record_drained` — that would over-count `drained` AND
                    // mislabel the emitted fresh frame, muddying the very diagnostic the floor exists for.
                    return self.fill_slot(candidate_next);
                }
                // HOLD the boundary (do NOT advance) so the next fresher frame fills this slot; the
                // hold IS a shed (dropped the oldest, emitted nothing).
                self.shed_log.record_drained();
                false
            }
            ShedAction::FastDrain => {
                // (#1145 v2.1) deep-backlog accelerated drain: shed the dupe AND advance TWO intervals
                // (retire an extra already-stale boundary), emitting nothing. (#1167 v3) FastDrain is
                // DELIBERATELY LEFT UNPACED and at +2: a genuine DEEP backlog (reconnect / restart /
                // burn-toggle, lag > RETIRE_MAX_LAG_INTERVALS) must converge FAST (<=12s), and boundary
                // lag drains only when a shed actually SKIPS (a paced-out FILL advances +1 but consumes
                // ~an interval of `now`, so it does NOT drain lag). Pacing FastDrain OR halving it to +1
                // throttles the deep drain to the (low) dupe rate and blows the 12s bound — measured
                // 15.3s at the 61.5 fps rig takt (dupe rate only ~1.5/s), vs 9.3s with the +2 (this is
                // the empirical correction to the design consult, which assumed lag convergence was
                // pace-independent). So the deep reconnect keeps the v2 +2 burst (an accepted rare
                // window dip). The STEADY 300/293 oscillation is NOT fixed here — it is prevented
                // UPSTREAM by the paced steady trickle-drain (the Retire arm) keeping lag below this
                // band so FastDrain essentially never fires in steady state; when a big jitter spike
                // does reach it, the paced converging TAIL below smears the recovery. FastDrain stamps
                // the shared pace budget so the paced tail/trickle around it stay suppressed (one
                // coherent skip stream, never re-composed into a double burst).
                let fast_next = if candidate_next.saturating_add(interval_ns) <= now_ns {
                    candidate_next + interval_ns
                } else {
                    candidate_next
                };
                // (#1145 v2.1 review 🟡) record the intentional extra boundary advance so main.rs
                // deducts it from the #707 boundary-skip diagnostic (a fast-drain is not a sick-leg SKIP).
                self.last_poll_fast_drain_extra =
                    (fast_next.saturating_sub(candidate_next)) / interval_ns;
                self.next_boundary_ns = fast_next;
                self.deferred_this_boundary = false;
                self.consecutive_drain_holds = 0; // the grid advanced -> hold streak broken
                self.converging_deep_backlog = true; // latch: the shallow tail keeps converging (paced)
                self.last_converge_skip_mono_ns = now_mono_ns; // stamp the shared pace budget
                self.shed_log.record_fast_drained();
                false
            }
            ShedAction::Emit { copy } => {
                // (#1167 v4) empty-queue STARVATION fill: report + record the bounded last-frame
                // repeats for this poll and set next_boundary. Extracted to `apply_starvation_fill`
                // (the #414 poll-length budget); it also resets the consecutive-repeat cap on an
                // on-time capture. 0 repeats in every healthy / over-rate window (byte-inert there).
                self.apply_starvation_fill(
                    copy,
                    queue_had_frame,
                    lag_intervals,
                    candidate_next,
                    interval_ns,
                );
                self.deferred_this_boundary = false;
                self.consecutive_drain_holds = 0; // (#1167) an emit fills + advances -> hold streak broken
                if copy {
                    // (#1111) The late-dupe valve fired: a content-dupe was EMITTED (a copy). Under
                    // v2 this now means genuine starvation (the trailing unique rate cannot hold 60
                    // -- a sub-60 source padded by duplication) or the bounded double-dupe guard, NOT
                    // the routine over-rate case (those dupes retire). Count it so a live box shows
                    // the valve engaging only for a genuine deficit.
                    self.shed_log.record_dupe_emitted();
                }
                true
            }
        }
    }

    /// Drain the accumulated `(dupe_shed, blind_shed, dupe_emitted, retired, drained, fast_drained)`
    /// counters for the periodic INFO log — see [`DupeShedLog::take`].
    pub fn take_shed_counts(&mut self) -> (u64, u64, u64, u64, u64, u64) {
        self.shed_log.take()
    }

    /// (#1167 v4) Drain + reset the accumulated STARVATION last-frame-repeat count for the periodic
    /// INFO log — see [`DupeShedLog::take_starvation_repeats`]. Kept SEPARATE from
    /// [`take_shed_counts`](Self::take_shed_counts) so its byte-frozen 6-tuple is unchanged.
    pub fn take_starvation_repeats(&mut self) -> u64 {
        self.shed_log.take_starvation_repeats()
    }
}

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
    pub fn record_retired(&mut self) {
        self.retired = self.retired.saturating_add(1);
    }

    /// (#1145 v2 + #1167) Record ONE frame SHED by the queue-depth drain (dropped the oldest to bound
    /// residence). #1145 v2 advanced the boundary on the shed; (#1167) in STEADY over-rate it now
    /// HOLDS the boundary (the next fresher frame fills the slot) — still a shed, still counted here —
    /// and advances only while CONVERGING a deep backlog. NOT counted on the panic-floor copy-fill
    /// (that EMITS, so it lands in [`record_dupe_emitted`](Self::record_dupe_emitted), never here).
    /// See [`DecimationGate::poll`].
    pub fn record_drained(&mut self) {
        self.drained = self.drained.saturating_add(1);
    }

    /// (#1145 v2.1) Record ONE deep-backlog FAST-drain (a content-dupe shed while advancing TWO
    /// stale boundaries, under sustained over-rate at lag > `RETIRE_MAX_LAG_INTERVALS`) — the
    /// mechanism that converges a deep delivery-latency backlog in single-digit seconds instead of
    /// the send-slack-limited ~35 s. See [`DecimationGate::poll`] / [`ShedAction::FastDrain`].
    pub fn record_fast_drained(&mut self) {
        self.fast_drained = self.fast_drained.saturating_add(1);
    }

    /// (#1167 v4) Record `n` STARVATION last-frame repeats emitted this poll — empty-queue 60fps
    /// slots filled by re-emitting the current good frame (see [`DecimationGate::poll`]). Accumulated
    /// separately from the byte-frozen 6-tuple and drained by
    /// [`take_starvation_repeats`](Self::take_starvation_repeats).
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
