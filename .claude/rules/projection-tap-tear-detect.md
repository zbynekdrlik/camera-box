---
paths:
  - "src/tear_detect.rs"
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

## Consequences for anyone touching this

- **It is REPORT-ONLY and stays report-only until proven.** `gates_overall_pass()` returns `false`
  (mirrors `optical_floor`/`e2e_latency_gate`/`imag_leg_gate`). The emitted `TearSignalViability`
  (`observed`/`unproven`) is the machine-checked promotion gate — a LIVE flip (`gates_overall_pass →
  true`, one line) is valid ONLY once the signal is `Observed` on a known-torn run AND a bound is
  calibrated from real data (per `verdict-gate-seam-calibration.md`). Do NOT flip it blind: an
  all-zero green distribution here is the issue-1101 "blind signal" trap, not a tight ceiling.
- **The real fix for a VIABLE payload-level LIVE gate is a PAINTER change** (rig-side, out of a
  software-only lane): the painted pattern needs VERTICAL tick redundancy — a tick indicator in BOTH
  the top and bottom halves (a second dual-QR row lower down, or a full-height tick strip) — so a
  horizontal tear yields two clean generations instead of an undecodable. The alternative is a
  pixel-seam detector on the on-box extract (heavy; a `src/probe/` decode change + schema carry, and
  per #1166 the lossy `.mp4` may need a codec-tolerant measure to not be blind too).
- **Window attribution reuses the sweep primitives** (`frame_gen_ts_anchor` + `place_frame_in_window`)
  and the `NODE_BURN_RUN_IDS` optical filter — the SAME definition `RecordingFrame::tick` uses — so
  the tear windows align 1:1 with the strict `all_cambox_continuity.segments`. Do not re-derive a
  different window/optical definition.
- **Tier-0:** the pure module RED→GREENs via `rustc --edition 2021 --test` with the `serde::Serialize`
  derive stripped (the imag-leg-report-only rule's recipe); the real-frame fixture
  (`tests/fixtures/tear-781/cam2_window_optical_ids.txt`, a real 847-frame CAM2 window) proves the
  detector against real decode output (`pattern-change-needs-decode-fixture`). `recording-verdict.rs`
  is probe-gated (CI-first) — verify the wiring with `cargo fmt --all --check` + a hand type-audit.
