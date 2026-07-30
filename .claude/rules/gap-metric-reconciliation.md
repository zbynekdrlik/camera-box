---
paths:
  - "src/residual_events.rs"
  - "src/painted_tick_gaps.rs"
  - "src/probe/recording_segments.rs"
---

# Two independently-correct gap metrics WILL disagree in both directions — reconcile with a fallback, never by changing either metric's own math

`CamboxSegment.gaps` (`crate::painted_tick_gaps::painted_tick_gaps`) and
`CamboxSegment.residual_events` (`crate::residual_events::residual_events`) measure the SAME
painted-tick continuity from the SAME frames, but at genuinely different granularities:

- `gaps` is a WHOLE-WINDOW, order-independent net-span calculation (`expected_count =
  (last-first)/step+1` minus the distinct present count, credited against `undecodable`) — no
  per-transition breakdown at all.
- `residual_events` is a RECORDED-order, per-transition walk that only flags a forward delta as
  anomalous when it exceeds `GAP_OUTLIER_ABS_DELTA` (10) — a constant calibrated against OTHER
  real windows where the routine dual-QR catch-up cluster tops out at 6-8.

**Both directions of disagreement are real and both are already locked by their own tests —
never "fix" one metric to match the other:**

1. **`gaps==0` while `residual_events` is non-empty** (issue 852,
   `residual_gap_events_can_be_nonzero_while_authoritative_gaps_stays_zero_852`): a moderate
   per-transition delta exceeds the outlier ceiling, but ample `undecodable` credit nets the
   whole-window deficit back to zero. `residual_events` is deliberately UNCREDITED (it doesn't
   know about `undecodable` at all), so it still flags the moderate jump even though the
   authoritative `gaps` figure is clean. **Do not "correct" `gaps` to match — that would silently
   break the already-proven credit logic (issue 625 / issue 681).**
2. **`gaps>0` while `residual_events` is empty** (issue 883,
   `diffuse_small_gap_with_no_outlier_delta_is_still_located_883`): every individual delta sits
   at/under the outlier ceiling and there's no backward jump, but the whole-window net-span
   arithmetic is still honestly short a slot or two — a diffuse residual, not one big anomaly.
   Fixed via `residual_events::locate_best_candidate_for_unattributed_gap`: a FALLBACK that fires
   ONLY when `gaps>0` AND the base walk found zero Gap-kind events, anchoring on the single
   LARGEST recorded-order forward delta as the best available candidate (never re-deriving its own
   `missing_slots` — it reuses the authoritative `gaps` value so a reader never sees two
   conflicting numbers for the same window).

**Before touching either metric's own arithmetic, or the outlier threshold, run BOTH real-anatomy
fixture tests first** (`cam1_802117826_spike_window_delta_histogram_707` /
`cam2_670137317_small_residual_window_delta_histogram_707` in `residual_events.rs`, and
`residual_gap_events_can_be_nonzero_while_authoritative_gaps_stays_zero_852` in
`recording_segments.rs`) — a naive "flag every locally-positive-contribution transition" approach
looks reasonable in isolation but adds ~140 phantom Gap events to the 802117826 fixture, which its
own test locks at zero. A reconciliation fix belongs in a NEW, narrowly-gated fallback function,
never in an edit to the two existing pure functions' core logic.

## Pulling real per-segment data out of a HISTORICAL CI run without re-triggering the gate

When investigating a specific past `all_cambox_continuity` verdict (e.g. explaining an anomalous
per-window frame count / duration), the retained verdict JSON artifact
(`gh run download <run-id>`) only has the AGGREGATE per-window numbers — no raw per-frame ticks
and no per-segment switch timestamps. Those DO exist in the CI run's own stdout log, printed by
`recording-e2e.sh`'s `[6/8]` sweep loop (`[seg N/M] <label> via '<scene>' switched at <ns> ns`):

```bash
gh run view <run-id> -R zbynekdrlik/camera-box --log 2>/dev/null | grep -E "\[seg [0-9]+/[0-9]+\]|switched at"
```

This is read-only (no new gate run) and gave the exact real switch-to-switch durations that
explained issue 883's window-length anomaly (one specific switch call ran ~0.5s longer than
every other one in the run — a one-off OBS-WS/ssh latency blip, not a systemic pattern) directly
from measured data instead of speculation.
