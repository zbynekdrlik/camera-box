---
paths:
  - "src/probe/recording_segments.rs"
  - "src/cold_cut.rs"
  - "src/bin/recording-verdict.rs"
---

# The cam2 Vernier tick decodes on EVERY cambox window on the splitter rig (the `recording_segments.rs` "non-cam2 → None" doc is stale)

`SegmentFrame::tick` is the cam2 optical dual-QR Vernier tick (node digital burns EXCLUDED). Two
doc lines in `recording_segments.rs` state that a **non-cam2** window in a `CAMBOX_SWEEP` carries
no tick:

- the `presentation_cadence` field doc: *"any non-cam2 window in a CAMBOX_SWEEP: `tick` is `None`
  on every frame"*
- the `measure_cadence_evenness` call site: *"`None` on a window with fewer than 2 decoded ticks
  (incl. every non-cam2 window, whose `present_ticks` is always empty)"*

**Those lines are STALE for the current rig and reasoning from them is a trap** (issue #768: a
competent code reviewer concluded a new tick-based metric was "cam2-only / non-actionable for
CAM1/CAM3" purely from these doc lines — it isn't). The current rig is **ONE physical camera
through an HDMI splitter into every cambox** (memory: rig-one-camera-splitter; `.claude/skills/e2e`),
and cam2 paints the Vernier monitor that ALL boxes film — so **every** box's recorded program
window decodes the SAME cam2 tick, not just cam2's. The doc almost certainly predates the
splitter topology (a per-box-own-camera era, where only cam2's box saw the Vernier).

## Verify from verdict data, not the doc

Mine `/tmp/recording-e2e-*/verdict-*.json` → `all_cambox_continuity.segments[]`. On real runs
(measured across 76 local verdicts, #768) the non-cam2 windows decode fine:

- `CAM1` / `CAM3` windows: `undecodable` = **0–1** of ~847 frames (NOT ~847),
- non-null `first_tick` / `last_tick`,
- a **populated** `presentation_cadence` block — which itself requires ≥ 2 decoded ticks.

So `tick.is_some()` is TRUE for the vast majority of frames on **every** active cambox window; a
tick-based per-window metric (continuity, cadence, or #768's cold-cut onset decodability) IS
cross-cambox meaningful here.

## Caveat before gating on it LIVE

The empirical cross-cambox decodability is a property of the intact splitter path, not a guarantee.
Before flipping any tick-based per-cambox gate LIVE (e.g. `cold_cut::gates_overall_pass()`),
re-confirm per-cambox onset/window tick-decodability on the target rig from the verdict data — a
future rig where the splitter path to one box is broken (or the stale doc's per-box-camera scenario
returns) would make that box's tick genuinely `None`, and a health check keyed on `tick.is_some()`
would read a healthy window as black and false-red. If that ever holds, scope the check to
tick-bearing windows or add a non-tick signal (brightness/black detection) for the affected boxes.
