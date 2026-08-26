---
paths:
  - "src/tear_detect.rs"
  - "src/aux_tick.rs"
  - "tests/tear_detect_781.rs"
  - "tests/fixtures/tear-781/**"
---

# Projection-tap scanout-TEAR detector (issue 781) — report-only, and PROVEN-BLIND on current content

## The tap already exists — cam2's leg IS the projection path

Owner confirmed 2026-08-24 (issue 781 comment 5396407545): cam2's USB grabber card is fed by
**imag-nb's HDMI output**. So cam2's window in the all-cambox E2E sweep already captures the physical
projection path (imag render → DRM scanout → HDMI → grabber) — "what the audience sees". No separate
cam2 `.mkv` is pulled to dev1; the content flows through the STREAM recording, in the `window_cam2`
segment, and reaches the dev1 merge as the per-frame `payloads` already carried in
`stream-partial-*.json` (partial schema v6). The tear metric is therefore computed MERGE-side with
NO partial schema bump and NO on-box work (contrast the #1088/#1166 content-hash saga, which needed
an on-box extract + a schema carry).

## The signal, and why it is currently BLIND

The painted content is cam2's optical **dual-QR Vernier**: LEFT QR = latest EVEN tick, RIGHT = latest
ODD tick, so a HEALTHY captured frame carries exactly two cam2-optical payloads whose `frame_id`s are
adjacent (`max-min == 1` = `VERNIER_MAX_SPREAD`). The ticket's tear ("halves of two consecutive ticks
in one frame") generalizes to: a captured frame whose optical span is `> 1` carried ≥2 paint
GENERATIONS = a scanout tear. `src/tear_detect.rs` is the pure Tier-0 classifier (span → torn),
consumed by the `all_cambox_continuity.tear` block in `recording-verdict.rs`.

**Measured across 5 real `stream-partial-*.json` (~48k frames): the per-frame optical span is
EXCLUSIVELY {0,1} and the optical-QR count per frame NEVER exceeds 2 — the payload-level signal never
fires on the current content.** The reason is STRUCTURAL (confirmed by reading the retained
`*-pixels/frame-*.png`): both dual-QR halves sit in ONE vertical band (top ~60%), so a horizontal
scanout tear crossing that band corrupts BOTH halves at the same height → the frame goes `undecodable`
(tick=None), it does NOT yield two clean generations. A tear cannot manufacture a second generation of
a QR that exists at only one vertical position. So an all-zero `tear_fraction` means EITHER "no tears"
(e.g. post the issue-1107 render-side fix) OR "signal blind" — indistinguishable without a known-torn
run.

## v2 (issue 1196) — the aux Vernier tick pair, the viability cure

The vertical tick redundancy the section above calls for LANDED as PR-1 of issue 1196 (report-only,
Approach 1 of the design synthesis on that ticket):

- **Painter:** `paint_one_frame`'s dual-QR branch additionally blits two SMALL (~210px,
  payload-minimal) QRs into the burn-free bottom gaps — geometry in the pure crate-root
  `src/aux_tick.rs` (left x[466,676), right x[1224,1434), y[745,955) at the rig 1920×1080/700/24
  layout; machine-proven disjoint from the primary halves, colour column, motion sweep, and the
  strih-BL/cambox-center/stream-BR burn overlays; returns `None` → no aux where the layout can't
  fit, e.g. the 2560-wide override canvas). LEFT = latest EVEN tick, RIGHT = latest ODD tick
  (`vernier_ids`), reserved run_id `recording_latency::AUX_TICK_RUN_ID` (911013), `gen_ts_ns = 0`
  (constant → the settled aux mark is byte-identical across ticks by construction, the #854
  property with zero state). **Documented exception:** the LEFT aux sits inside imag's
  BottomCenterLeft burn zone [382,684) — accepted because the real-partial run_id census proves
  imag's burn is NOT in the projected-scene path; only imag's OWN leg recording may cover it
  (lower report-only aux coverage there). Pinned by `left_aux_overlaps_the_imag_burn_zone_by_
  documented_exception` in `src/aux_tick.rs`.
- **Decoder: NO changes** — the existing passes find the aux QRs; they flow into the partial's
  per-frame `payloads` with zero schema change. `AUX_TICK_RUN_ID` joined `NODE_BURN_RUN_IDS`
  (`[u32; 11]`) so tick/split/optical/cadence/copies/latency all ignore the aux marks
  automatically; ONLY the tear consumer reads them, by run_id. The python mirrors
  (`qr_align_pins.py`, `mv_skew_snapshot.py` RESERVED_RUN_IDS) must stay in sync — the aux is
  UNIVERSAL painted content with a small id and a constant gen_ts=0, so an unmirrored copy would
  win the smallest-id painter auto-detect tie-break and poison align/skew math (the #1159 class).
- **Detector v2:** `window_tear_stats` takes per-frame `(primary_ids, aux_ids)`; torn ⇔ span of
  primary ∪ aux > `VERNIER_MAX_SPREAD` — a seam BETWEEN the bands now fires. Two report-only
  promotion fields: `aux_decode_fraction` (frames with BOTH aux decoded / ALL attributed frames —
  did the small marks survive the lossy chain?) and `primary_dark_aux_alive_fraction` (primary
  empty ∧ both aux decoded — a seam INSIDE the 700px primary band vs a whole-frame blur).

## v2.1 (issue 1196) — MULTI-TILE SAFE: only SINGLE-SOURCE frames are scored for tear

The first real rig run after the aux-painter redeploy (E2E 1859005342, ticket comment 5415952812;
durable evidence `~/.claude/work-products/1196-fixture/` — verdict JSON + real frames) exposed a
false positive the plain-union v2 could not see, and killed aux as the cure ON CURRENT CONTENT:

- **`aux_decode_fraction = 0.0` in ALL 10 windows** — the ~210px aux QRs are half-size inside a
  composited tile and did NOT survive the camera+encode chain. **SUPERSEDED by the next two runs:**
  the 0.0 was the MULTI-TILE composition, not chain death — on single-tile content the aux marks
  decode fine through the full real chain (run 1547854091: 0.679; run 2099068429: 0.60–0.82 across
  all 10 windows, suspects 0.0). The Approach-2 (full-height tick ladder) escalation is NOT needed;
  precondition (1) is now LANDED (see below).
- **`tear_fraction ~0.99`, `max_spread 4` (one window 14) = MULTI-TILE SKEW, not a tear.** The
  recorded program is MULTI-TILE — an ALL_CAMBOX composition carries TWO grabber-path tiles of the
  SAME painted cam2 monitor (plus production scenes), so one recorded frame decodes the primary
  dual-QR from BOTH paths offset ~2-4 ticks. `recording-verdict.rs`'s `tear_by_window` collects ALL
  cam2 run_id payloads of a frame into `primary`, so v2's plain union span measured that inter-path
  temporal skew.

**The v2.1 fix (this sub-step), position-free by necessity:** partial schema v6 `Payload` carries
only `run_id`/`frame_id`/`gen_ts_ns` — NO pixel positions — so the ids of a multi-tile frame cannot
be attributed back to their tile MERGE-side. The physical fact used instead: **one tile's dual-QR
band produces AT MOST 2 optical QRs** (left even + right odd; a tear through one band corrupts, it
does not multiply — the same single-vertical-band structure that makes v1 blind). So a frame with
**≥ 3** primary optical ids is composited from **≥ 2** tiles: `tear_detect::frame_cluster_count =
ceil(count/2)`, `is_multi_path_suspect` = `≥ 2` clusters. `window_tear_stats` scores tear ONLY on
SINGLE-SOURCE frames (≤ 2 primary ids) and EXCLUDES suspects, surfacing them via new report-only
fields `multi_path_suspect_frames`/`_fraction`, `max_cluster_count`, `max_multi_path_spread`
(`max_spread` now stays the clean single-tile tear magnitude). `is_torn_frame` gained the
single-source guard, so a genuine single-cluster 2-generation frame (`{100,102}`, or a cross-band
primary∪aux split) still fires. On the real 1859005342 window: `multi_path_suspect_fraction ~0.998`,
`tear_frames 0`, `viability Unproven` — the honest "multi-tile, tear unscoreable here" verdict
replacing v2's false 0.99. Real-data regression fixture:
`tests/fixtures/tear-781/cam2_window_multitile_ids_1196.txt` (first 846 real frames of
stream-partial-1859005342).

**Honest limitation + the named follow-up:** without positions a genuine tear INSIDE a multi-source
frame is not separable from inter-tile skew (`{100,101,102,103}` from a single-tile 2-gen tear is
byte-identical to two tiles offset by 2), so such frames are conservatively suspect-not-torn; and a
count-2 frame whose two ids come from two tiles' single surviving halves (8 of 9690 real frames)
still reads single-source. The COMPLETE fix is **geometric per-cluster scoping** — carry the QR
centre/bbox on each payload (partial schema bump + `src/probe/qr.rs` position capture + fleet
redeploy), group decoded QRs by pixel position, compute the span WITHIN each tile. That is the
named follow-up design on the ticket (Approach: "positions available end-to-end → per-cluster
union"), deliberately OUT of this sub-step's scope. A `multi_path_suspect_fraction` ceiling is added
to the promotion preconditions so a multi-tile window can never be promoted.

**Promotion preconditions (ALL of them, in order — the LIVE flip stays out of scope until then):**

1. **Real-captured-frame fixture WITH aux marks — LANDED** (`tests/fixtures/tear-781/
   stream-2099068429-frame-{1399,4792}.png` + probe-gated `tests/aux_tick_fixture_decode_1196.rs`):
   real frames from run 2099068429's box-side pixel-proof retention, pinning that the PRODUCTION
   stream-extraction decode AND the plain robust decode read both aux marks (ids == the primary
   pair) and that the ids flow through v2.1 as clean single-source content. The painter-level
   synthetic round-trip (`aux_tick_pair_round_trips_alongside_the_dual_qr_1196`,
   `src/probe/painter.rs`) was deliberately NOT sufficient — small-optical-QR decodability through
   the real chain (optical → grabber → 2×NDI → 4K upscale → mp4) was THE open risk
   (`pattern-change-needs-decode-fixture`), now proven. HONEST residual: the fixture frames are
   CAM3-window (the splitter grabber leg) — the run had no cam2/projection window — so the
   projection-leg confirmation rides the later preconditions. **Mining recipe (reusable):** the
   `<partial>-pixels/frame-N.png` retention + the partial's own `frames[]` entry for index N (the
   REAL rqrr output for those exact pixels) is the ground truth; `zbarimg` full-frame + the two
   aux design-rect crops is the independent second decoder — zero rig access, zero mkv pulls.
2. **A known-torn calibration run** (imag pre-1107 build or the projector-vsync env escape —
   needs owner agreement, asked at that step) making the signal `Observed`.
3. **Calibrate `TEAR_FRACTION_CEILING` + an aux-coverage floor + a `multi_path_suspect_fraction`
   ceiling** from real green vs torn distributions (`verdict-gate-seam-calibration.md`) — the
   coverage floor makes a silent aux loss demote honestly instead of false-greening, and the
   suspect ceiling (v2.1) is what keeps a MULTI-TILE window (nearly all frames suspect, tear
   unscoreable) from ever being promoted. On the current multi-tile rig `multi_path_suspect_fraction
   ~0.998`, so promotion is impossible until the recorded scene is single-tile OR the geometric
   per-cluster follow-up (schema-carried positions) lands.
4. Then the one-line `gates_overall_pass() → true` flip + the repo-wide re-arm grep
   (`ci-testing-gotchas.md`'s re-arm section).

## Consequences for anyone touching this

- **It is REPORT-ONLY and stays report-only until proven.** `gates_overall_pass()` returns `false`
  (mirrors `optical_floor`/`e2e_latency_gate`/`imag_leg_gate`). The emitted `TearSignalViability`
  (`observed`/`unproven`) is the machine-checked promotion gate — a LIVE flip (`gates_overall_pass →
  true`, one line) is valid ONLY once every precondition above holds. Do NOT flip it blind: an
  all-zero green distribution here is the issue-1101 "blind signal" trap, not a tight ceiling.
- **The pixel-seam detector alternative stays rejected** (heavy; a `src/probe/` decode change +
  schema carry, and per #1166 the lossy `.mp4` may need a codec-tolerant measure to not be blind
  too) — Approach 2 (full-height tick ladder, schema v7 + fleet redeploy) is the named escalation
  if the aux marks prove undecodable through the real chain.
- **Window attribution reuses the sweep primitives** (`frame_gen_ts_anchor` + `place_frame_in_window`)
  and the `NODE_BURN_RUN_IDS` optical filter — the SAME definition `RecordingFrame::tick` uses — so
  the tear windows align 1:1 with the strict `all_cambox_continuity.segments`. Do not re-derive a
  different window/optical definition.
- **Tier-0:** the pure module RED→GREENs via `rustc --edition 2021 --test` with the `serde::Serialize`
  derive stripped (the imag-leg-report-only rule's recipe); TWO real-frame fixtures prove the
  detector against real decode output (`pattern-change-needs-decode-fixture`) —
  `tests/fixtures/tear-781/cam2_window_optical_ids.txt` (a real 847-frame single-band CAM2 window,
  healthy → tear-free/Unproven) and `tests/fixtures/tear-781/cam2_window_multitile_ids_1196.txt`
  (a real 846-frame MULTI-TILE window from stream-partial-1859005342 → 844 multi_path_suspect,
  0 torn under v2.1, ~0.99 torn under v2 = the observed RED→GREEN). `recording-verdict.rs`
  is probe-gated (CI-first) — verify the wiring with `cargo fmt --all --check` + a hand type-audit;
  the new `TearStats` fields flow into `all_cambox_continuity.tear.windows[]` via `serde_to_value`
  with no consumer change.
  **Multi-module replica assembly (issue 1196):** when the pure module under test depends on OTHER
  crate-root modules (`aux_tick` needs `colour_scale` + `motion_sweep` + `painter_mode`), assemble
  ONE standalone file that wraps each dependency's test-stripped source as `pub mod <name> { … }`
  and KEEP the `crate::` paths verbatim — a standalone rustc file IS its own crate root, so
  `crate::colour_scale` resolves; a naive `crate::` → `super::` rewrite breaks inside the nested
  `mod tests` (its `super` is the module, not the root — first attempt failed exactly there).
