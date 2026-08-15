---
paths:
  - "src/burn_hold.rs"
  - "src/probe/burn_contiguity.rs"
---

# Burn-id contiguity is PRESENCE-ONLY — a repeating/freezing hop needs the separate max-hold term (#870)

`probe::burn_contiguity::burn_contiguity()` collapses the decoded burn ids into a `BTreeSet` before
checking the `first..=last` span for missing integers. It is therefore **presence-only and
order-independent**: `present_count` / `expected_count` / `missing_ids` (and the whole
`full_chain.loss.<node>` accounting built on them) prove no id was DROPPED, but are **blind to a hop
that REPEATS frames** — the identical rendered image delivered on many consecutive recorded frames
adds no missing id, so contiguity stays satisfied and the headline reads clean. This hid a real
3-day defect (issue 707: run 396782734 carried the same strih burn id on 61% of consecutive stream
frames while `real_drops==0`).

**Never try to make `burn_contiguity` itself catch repeats** — it is a proven set-based check with
extensive tests (gap-metric-reconciliation.md: add a new narrowly-gated term, never change an
existing metric's math). The repeat/freeze question lives in the SEPARATE pure module
`src/burn_hold.rs` (`burn_hold_distribution` → run-length distribution + `max_hold_frames` +
`duplicate_pair_fraction`, asserted `<= MAX_HOLD_FRAMES=4`). It is **REPORT-ONLY** today via the
one-line `burn_hold::gates_overall_pass() -> false` seam (mirrors
`optical_floor`/`presentation_cadence`); the flip-to-LIVE is tracked on #870 and needs the
`full_chain.loss.<node>.hold.max_hold_frames` field's green-run distribution to accumulate first and
survive the cam1-grabber issue-909 defect (verdict-gate-seam-calibration.md §5).

## GOTCHA — a run-length / repeat metric MUST use `burn_ids_with_frame_index_in`, not `burn_ids_in`, and break on non-adjacent `frame_index`

`recording_latency::burn_ids_in` yields ONLY the ids of frames whose burn DECODED — it silently
drops undecodable-burn frames. Feeding that flat id list into a consecutive-duplicate walk MERGES
two separate deliveries of the same id across a recorded gap (an undecodable frame in between) into
one inflated hold. Use `burn_ids_with_frame_index_in` (the `(frame_index, id)` extractor) and extend
a run only when the frames are recording-ADJACENT (`frame_index` steps by exactly 1); a recorded gap
breaks the run. `adjacent_pairs` (the `duplicate_pair_fraction` denominator) must likewise count
only recording-adjacent pairs, so the fraction is exactly "% of consecutive RECORDED frames
byte-identical", never diluted by decode gaps. See `burn_hold::burn_hold_distribution` +
`recorded_gap_breaks_a_run_never_merges_it`.

## Topology fixes the legit bound (why 4)

Topology v2 (#459/#466): both strih and stream RECORD at 30fps and every node burn is DECIMATED
60→30 into the stream recording, so consecutive KEPT ids STEP (distinct) — legit hold = **1 recorded
frame** per burn id. `MAX_HOLD_FRAMES=4` clears any legit transient / genlock-FIFO convergence hold
with margin, matches the ticket's "may hold for 2, never 5", and mirrors
`imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN=3` (3 Δ0-pairs = 4 frames — the imag OPTICAL-tick
sibling of this NODE-BURN gate; that module has `stuck_run_stats().max_run` but no per-length
histogram, which is why burn_hold has its own single walk).
