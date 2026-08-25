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

**Promotion preconditions (ALL of them, in order — the LIVE flip stays out of scope until then):**

1. **Real-captured-frame fixture WITH aux marks** — mined from the first rig run after the cam2
   painter redeploy (supervisor step) and committed under `tests/fixtures/tear-781/`; the
   painter-level synthetic render→decode round-trip in `src/probe/painter.rs`
   (`aux_tick_pair_round_trips_alongside_the_dual_qr_1196`) is deliberately NOT sufficient —
   small-optical-QR decodability through the real chain (projection → grabber → 2×NDI → 4K
   upscale → mp4) is THE open risk (`pattern-change-needs-decode-fixture`). A LOW real
   `aux_decode_fraction` here escalates to Approach 2 (the full-height tick ladder — see the
   issue-1196 design synthesis) instead of promoting.
2. **A known-torn calibration run** (imag pre-1107 build or the projector-vsync env escape —
   needs owner agreement, asked at that step) making the signal `Observed`.
3. **Calibrate `TEAR_FRACTION_CEILING` + an aux-coverage floor** from real green vs torn
   distributions (`verdict-gate-seam-calibration.md`) — the coverage floor is what makes a silent
   aux loss demote honestly instead of false-greening.
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
  derive stripped (the imag-leg-report-only rule's recipe); the real-frame fixture
  (`tests/fixtures/tear-781/cam2_window_optical_ids.txt`, a real 847-frame CAM2 window) proves the
  detector against real decode output (`pattern-change-needs-decode-fixture`). `recording-verdict.rs`
  is probe-gated (CI-first) — verify the wiring with `cargo fmt --all --check` + a hand type-audit.
