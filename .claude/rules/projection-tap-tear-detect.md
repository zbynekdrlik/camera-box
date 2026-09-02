---
paths:
  - "src/tear_detect.rs"
  - "src/aux_tick.rs"
  - "tests/tear_detect_781.rs"
  - "tests/tear_detect_torn_fixture_1196.rs"
  - "tests/aux_tick_fixture_decode_1196.rs"
  - "tests/fixtures/tear-781/**"
---

# Projection-tap scanout-TEAR detector (issue 781/1196) — LIVE gate; the AUX SINGLE-MARK cross-band is the operative signal (the primary band is blind)

## The tap already exists — cam2's leg IS the projection path

Owner confirmed 2026-08-24 (issue 781 comment 5396407545): cam2's USB grabber card is fed by
**imag-nb's HDMI output**. So cam2's window in the all-cambox E2E sweep already captures the physical
projection path (imag render → DRM scanout → HDMI → grabber) — "what the audience sees". No separate
cam2 `.mkv` is pulled to dev1; the content flows through the STREAM recording, in the `window_cam2`
segment, and reaches the dev1 merge as the per-frame `payloads` already carried in
`stream-partial-*.json` (partial schema v6). The tear metric is therefore computed MERGE-side with
NO partial schema bump and NO on-box work (contrast the #1088/#1166 content-hash saga, which needed
an on-box extract + a schema carry).

## The signal, and why the PRIMARY band alone is BLIND (cured by the v2 aux pair — see the LIVE section)

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
  `src/aux_tick.rs`. **Positions since issue 1270 (de-confliction): BOTH marks are CO-LOCATED in
  the RIGHT gap `[1120,1578)` — even x[1137,1347), odd x[1351,1561), y[745,955) at the rig
  1920×1080/700/24 layout** (was one-per-gap, even x[466,676) / odd x[1224,1434), before 1270; see
  the SATURATED section at the bottom for why co-location is forced). Machine-proven disjoint from
  the primary halves, colour column, motion sweep, and ALL four burn overlays (strih-BL / imag-BCL
  / cambox-center / stream-BR); returns `None` → no aux where the layout can't fit, e.g. the
  2560-wide override canvas. LEFT slot = latest EVEN tick, RIGHT slot = latest ODD tick
  (`vernier_ids`), reserved run_id `recording_latency::AUX_TICK_RUN_ID` (911013), `gen_ts_ns = 0`
  (constant → the settled aux mark is byte-identical across ticks by construction, the #854
  property with zero state). Pinned by `both_aux_marks_clear_the_imag_burn_zone_1270` +
  `canonical_rects_are_the_design_values` in `src/aux_tick.rs` (the former replaced the pre-1270
  `left_aux_overlaps_the_imag_burn_zone_by_documented_exception` occlusion pin).
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

## PROMOTED TO LIVE — 2026-09-01 (issue 1196), and the OPERATIVE-SIGNAL correction

**`gates_overall_pass()` is now `true` and `TEAR_FRACTION_CEILING` = 0.005.** All preconditions
below are MET. The known-torn calibration run 1700989544 (imag projector vsync disabled off-air)
graded CAM2 (projection leg) `observed`, `tear_fraction` 0.018846 (16/849) and 0.237308 (201/847);
every splitter leg unproven / 0.0. `multi_path_suspect_fraction` 0.0 everywhere (single-tile).

**CORRECTION — the operative signal is the AUX SINGLE-MARK CROSS-BAND, not the primary band.**
Per-frame mining of `stream-partial-1700989544.json` (the real rqrr decode) proves the PRIMARY
dual-QR span is ALWAYS ≤ 1 (`max primary span = 1` on every one of the 9,883 stream-partial frames — the primary band is
structurally blind, as the blindness section says). EVERY one of the 241 torn frames is
`primary[X, X+1]` (span 1) + exactly ONE aux mark `[Y > X+1]` from a later generation (union span
2–7): the bottom aux band, scanned out later, catches the newer generation during the un-vsynced
tear — the v2 aux cross-band cure firing EXACTLY as designed. `zbarimg` independently reads the aux
mark 37781 on the retained torn `frame-8090` (one gen ahead of the primary 37779/37780). This
REVERSES the 2026-09-01 grading that read `max_spread` (the primary∪aux UNION span) as "primary
span" and called the aux dead: dropping the aux from the union would make the gate BLIND (0 torn on
CAM2). **`aux_decode_fraction` = 0.0 on CAM2 is a MISLEADING BOTH-marks metric** — scoped to the
CAM2 windows, `≥ 2 aux marks` = 0.0 but `≥ 1 aux mark` (`aux_any_decode_fraction`) = 0.967–0.999.
The aux is fully operative on the projection leg via single marks; an aux-coverage FLOOR was NEVER
the right gate. Ceiling 0.005: GREEN Observed max 0.003546 (37 v2.1 verdicts) < 0.005 < the 0.018846
induced floor — 0 false positives on history, 3.77x margin.

**LIVE lock-step consumers (all landed):** `src/tear_detect.rs` (const + scoped `tear_gate_pass` +
`gates_overall_pass()` true + `aux_any_decode_fraction` field), `src/bin/recording-verdict.rs`
(`tear_gate` string + comment + any/both aux coverage line), `scripts/e2e_discord_report.py`
`_blocking_failures` item 13 (guarded `gates_overall_pass is True`), and the real-frame TORN fixture
`tests/tear_detect_torn_fixture_1196.rs` + `tests/fixtures/tear-781/stream-1700989544-frame-*.png`.
Re-disarm = one line (`gates_overall_pass()` → `false`); the mechanism stays dormant.

**Historical promotion preconditions (ALL MET — kept for the audit trail):**

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
2. **A known-torn calibration run — DONE (run 1700989544, 2026-09-01, projector-vsync env escape,
   off-air).** Made the signal `Observed` on CAM2 with `tear_fraction` 0.018846 / 0.237308, cleanly
   separable from the green background. **RESOLVED which band fires: the AUX SINGLE-MARK CROSS-BAND**
   — every torn frame is `primary[X,X+1]` + one aux mark from a later generation; the primary band's
   own span is always ≤ 1 (blind). So the aux mechanism IS the operative cure on the projection leg
   (the v2 design is vindicated), and `aux_any_decode_fraction` (≥ 1 mark, ~0.97+ on CAM2) — not the
   both-mark `aux_decode_fraction` (0.0) — is the honest operability signal.
3. **Calibrate `TEAR_FRACTION_CEILING` + the `multi_path_suspect_fraction` ceiling (`MULTI_PATH_
   SUSPECT_CEILING`, LANDED at 0.10, issue 1196) from real green vs torn distributions**
   (`verdict-gate-seam-calibration.md`). The suspect ceiling keeps a MULTI-TILE window (tear
   unscoreable) from ever being promoted — calibratable from the current data alone (green
   `multi_path_suspect_fraction` is 0.0 across 90 windows, a multi-tile window ~0.998, so 0.10 has a
   ~10x margin). The machine-checked flip-readiness is `tear_detect::signal_promotable` /
   `window_promotable` (viability `Observed` + suspect ≤ ceiling), emitted per-window
   (`window_promotable`) and run-level (`tear.signal_promotable`) in the verdict block.
   **Two real-data corrections (2026-09-01, mined across 44 verdicts) — the aux floor is dropped and
   the ceiling can never be 0.0:**
   - **The aux-coverage FLOOR is REMOVED as a promotion gate — `aux_decode_fraction` (BOTH marks) is
     a report-only DIAGNOSTIC; `aux_any_decode_fraction` (≥ 1 mark) is the operability signal.** The
     CAM2 PROJECTION leg reads `aux_decode_fraction` = 0.0 because imag's OWN burn (911003, rendered
     by imag's OBS projector, which cam2 films) OCCLUDES the LEFT (even) aux — present on ~99% of the
     CAM2-window frames (240/241 torn frames carry it; all 241 torn aux marks are ODD/RIGHT). A
     GEOMETRY defect (the LEFT aux sits in imag's burn zone [382,684); issue 1266 relocates it), NOT a
     lossy-chain limitation and NOT "aux dead": scoped to the CAM2 windows a SINGLE (right) aux mark
     decodes 0.967–0.999 of frames, and the operative cross-band tear needs only ONE.
     The known-torn run PROVED the CAM2 tears come from the aux single-mark cross-band (the primary
     band is blind, span always ≤ 1) — CORRECTING the earlier "tears surface via the PRIMARY band"
     reading. A hard both-mark aux floor would have permanently blocked the projection leg;
     `signal_promotable` (which requires `Observed`) is the fail-closed gate. Aux decodes BOTH marks
     on the splitter legs (CAM1/CAM3/CAM6/CAM7), which are not the projector-scanout path.
   - **`TEAR_FRACTION_CEILING` cannot be 0.0.** A LOW background of `Observed` single-tile tears
     (~0.00118–0.00355 `tear_fraction`, 1–3 frames/window) exists on GREEN runs on both CAM2 (14
     windows) and CAM3 (2 windows), so `signal_promotable` already reads `true` on ~12 of 32
     v2.1-scored routine runs — it is NECESSARY but NOT SUFFICIENT, and NOT by itself evidence of a
     known-torn run. The ceiling must be calibrated ABOVE this ~0.004 green background and BELOW the
     known-torn run's induced-tear distribution (a per-window RATE; a run-wide COUNT term may also be
     warranted, §4). SUPERSEDES the earlier "current multi-tile rig `multi_path_suspect_fraction
     ~0.998`, promotion impossible" note: the current green content is SINGLE-TILE, so promotion IS
     possible on it once the torn ceiling separates the induced tear from the green background.
4. **DONE — the one-line `gates_overall_pass() → true` flip landed** (2026-09-01), with the
   `_blocking_failures` item 13 + verdict-string re-arm. `ci-testing-gotchas.md`'s re-arm section
   still applies to any FUTURE re-tighten/disarm of the ceiling.

## Consequences for anyone touching this

- **It is now LIVE (issue 1196, 2026-09-01).** `gates_overall_pass()` returns `true` — an `Observed`
  window over `TEAR_FRACTION_CEILING` (0.005) FAILS the fused verdict; an `Unproven` window always
  passes. Re-disarm is one line (`gates_overall_pass()` → `false`), the mechanism stays dormant.
  Do NOT re-tighten the ceiling below the ~0.004 green Observed background (the aux single-mark
  cross-band's noise floor) — that would false-fail green runs; and do NOT drop the aux from the
  union (it is the OPERATIVE signal — the primary band is blind, so dropping it makes the gate blind,
  the issue-1101 trap). Calibrate any ceiling change from `/tmp/recording-e2e-*/verdict-*.json` per
  `verdict-gate-seam-calibration.md`.
- **The pixel-seam detector alternative stays rejected** (heavy; a `src/probe/` decode change +
  schema carry, and per #1166 the lossy `.mp4` may need a codec-tolerant measure to not be blind
  too) — Approach 2 (full-height tick ladder, schema v7 + fleet redeploy) is the named escalation
  if the aux marks prove undecodable through the real chain.
- **Window attribution reuses the sweep primitives** (`frame_gen_ts_anchor` + `place_frame_in_window`)
  and the `NODE_BURN_RUN_IDS` optical filter — the SAME definition `RecordingFrame::tick` uses — so
  the tear windows align 1:1 with the strict `all_cambox_continuity.segments`. Do not re-derive a
  different window/optical definition.
- **Tier-0:** the pure module RED→GREENs via `rustc --edition 2021 --test` with the `serde::Serialize`
  derive stripped (the imag-leg-report-only rule's recipe — the LIVE-flip RED→GREEN used exactly
  this: `green_background_window_passes_the_calibrated_live_gate_1196` fails at ceiling 0.0, passes
  at 0.005). Real-frame fixtures prove the detector against real decode output
  (`pattern-change-needs-decode-fixture`): the id-list txts `cam2_window_optical_ids.txt` (a real
  847-frame single-band CAM2 window, healthy → tear-free/Unproven) and
  `cam2_window_multitile_ids_1196.txt` (a real 846-frame MULTI-TILE window → 844 multi_path_suspect,
  0 torn); the healthy-run aux-decode PNGs `stream-2099068429-frame-{1399,4792}.png`
  (`aux_tick_fixture_decode_1196.rs`); and the known-torn PNGs `stream-1700989544-frame-8090-torn.png`
  (+ `-849{7,8}-healthy.png`) proving the aux single-mark cross-band tear on real projection-leg
  pixels (`tests/tear_detect_torn_fixture_1196.rs`). `recording-verdict.rs` is probe-gated (CI-first)
  — verify the wiring with `cargo fmt --all --check` + a hand type-audit; the new `TearStats` fields
  flow into `all_cambox_continuity.tear.windows[]` via `serde_to_value` with no consumer change.
  **Multi-module replica assembly (issue 1196):** when the pure module under test depends on OTHER
  crate-root modules (`aux_tick` needs `colour_scale` + `motion_sweep` + `painter_mode`), assemble
  ONE standalone file that wraps each dependency's test-stripped source as `pub mod <name> { … }`
  and KEEP the `crate::` paths verbatim — a standalone rustc file IS its own crate root, so
  `crate::colour_scale` resolves; a naive `crate::` → `super::` rewrite breaks inside the nested
  `mod tests` (its `super` is the module, not the root — first attempt failed exactly there).

## The aux tick pair's bottom row is GEOMETRICALLY SATURATED — a 210px aux mark cannot be relocated clear of every burn (issue 1266) → RESOLVED by co-location (issue 1270)

**RESOLVED (issue 1270, painter-only): both aux marks are now CO-LOCATED in the RIGHT gap
`[1120,1578)` (even x[1137,1347), odd x[1351,1561)), clear of imag's burn, with imag untouched.**
The packing is forced (this section's own numbers): imag's own 302px burn + one 210px aux = 512 >
458, so no gap holds imag alongside an aux — the ONLY arrangement is BOTH aux in ONE gap (420 ≤
458) and imag ALONE in the other. That is why 1266's "relocate ONLY the LEFT mark at 210px to a
clear band" was infeasible (a single mark still had no ≥210px slot while the RIGHT mark held its
own), but co-locating BOTH is not. Each 210px box carries its own 4-module quiet zone
(`render_payload_qr`'s `quiet_zone(true)`; version-3 EC-H = 37 modules, 5.68 px/module), so the
pair 4px apart has ≥8 modules of white between data regions and decodes as two independent rqrr
grids. **HONEST residuals (design comment on 1270):** both marks share one y-band, so a horizontal
scanout tear still cuts both → near-zero EXTRA tear redundancy over the single mark; the value is a
truthful `aux_decode_fraction` on the projection leg (was a misleading 0.0) + redundancy vs a
localized one-sided loss. Framing tolerance drops from ~104px to ~17px. So the co-located pair's
real-chain decodability is a **post-deploy CAM2-fixture precondition** — mine a CAM2-window real
frame from the first rig E2E after the painter redeploy and commit the frame + a decode assertion
(`pattern-change-needs-decode-fixture`). **Acceptance / rollback:** CAM2 `aux_any_decode_fraction ≥
0.995` (the LIVE gate must not lose sensitivity), CAM2 `aux_decode_fraction > 0`, CAM1/CAM3
`aux_decode_fraction` non-regressing (the sharper canary — both marks already decode there),
`tear_fraction ≤ 0.005` on the healthy run; roll back if the odd mark drops below its current
0.995. Safe by construction against FALSE reds meanwhile (garbled mark → CRC/EC reject; a healthy
frame's even+odd aux ids are Vernier-adjacent, union span ≤ 1). The rest of this section is kept as
the geometric proof that co-location is the ONLY fit.

Issue 1266 asked to relocate the occluded LEFT aux (`[466,676)`, under imag's burn `[382,684)` on
the CAM2 leg) to a clear band at the SAME 210px. **A SINGLE-mark relocation is geometrically
infeasible — do not try it, and do not shrink the mark as a stopgap** (co-location of BOTH per 1270
above is the fit). The evidence:

- The aux y-band is FORCED to `[724,960)` (below the primary band bottom 724, above the sweep band
  top 960) — it holds exactly ONE 210px mark vertically, no stacking.
- The bottom row carries FOUR burns on the CAM2 leg (empirically confirmed, run 1700989544:
  911002/911003/cam-capture/911004 each in 1770/1770 frames): strih `[40,342)`, imag `[382,684)`,
  cam-capture `[800,1120)`, stream `[1578,1880)`. Plus the fixed RIGHT aux `[1224,1434)`. Free
  x-gaps: `40 / 116([684,800)) / 104([1120,1224)) / 144([1434,1578)) / 40`. **Widest clear gap =
  144px; NO ≥210px slot**, and imag's own 302px burn has no alternative slot either — the row is
  saturated (4 burns 302+302+320+302 = 1226px + 2 aux 420px + quiet zones > the clear budget). The
  "documented exception" (LEFT overlaps imag) is a SYMPTOM of that saturation, not a free choice.
- Decodability physics: the aux payload `P{run}.{tick}.0.{crc}` is 26 alphanumeric chars → EC-H
  version-3 = 37 modules incl. the quiet zone. The RIGHT at 210px = **5.68 px/module** (decodes
  0.967–0.999 on CAM2, already near the edge through imag-HDMI → grabber → 2×NDI → 4K upscale → mp4,
  macroblocks ~16px). Any sub-210px mark (≤144px = ≤3.89 px/module) is UNVERIFIED and likely
  marginal/near-zero — never report it as "expected to decode".

**Two further reasons relocation is the WRONG call (they bit issue 1266):**

1. The painted pattern is ONE pattern every leg films. The LEFT at 210px decodes 0.99 on the
   SPLITTER legs (cam1/cam3…, which carry no imag burn). Shrinking it everywhere REGRESSES that
   working control for a maybe-mark on one leg.
2. Both aux marks share the SAME y-band `[745,955)`, so a horizontal scanout tear cuts every mark at
   the same rows — correlated, **near-zero redundancy against the dominant failure (a tear)**. A
   second same-row mark helps only a LOCALIZED non-tear loss of the RIGHT, which is rare and the
   count-floor gate absorbs: the 8/1770 zero-aux CAM2 frames are 7 healthy span-1 primary (NOT
   tears) + 1 whole-frame blur.

**Consequence:** the single-mark gate is sufficient (RIGHT 0.995, `TEAR_FRAME_COUNT_FLOOR` 6). The
real fix is DE-CONFLICTING the painted aux band and the OBS burn layout — cross-cutting
(`vendor/distroav/src/burn-geom.hpp` `Corner` assignment + `src/aux_tick.rs` + decoder ROIs +
`tests/fixtures/tear-781/` + the `scripts/qr_align_pins.py`/`mv_skew_snapshot.py` mirrors), tracked
as issue 1270. If a painted best-effort is ever demanded, it must be ADDITIVE (a THIRD mark, LEFT
and RIGHT byte-identical/untouched so the splitter control + committed fixtures are safe) with a
written post-deploy kill criterion — NEVER a relocation.
