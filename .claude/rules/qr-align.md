---
paths:
  - "scripts/qr_align_pins.py"
  - "scripts/lib/qr-align.sh"
  - "tests/python/test_qr_align_pins_1003.py"
  - "tests/harness_qr_align_step_1003.rs"
---

# Floor-3 per-run camera auto-align (`qr_align_pins.py`, the [4i/8align] E2E step) — #1003

Production camera alignment is a per-run AUTOMATIC process (owner rework, 2026-08-20): measure the
simultaneous painter-QR spread across the on-air strih inputs → floor-3 pins → apply → RE-MEASURE →
FAIL if it cannot align. It is `recording-e2e.sh`'s BLOCKING `[4i/8align]` step. The prior deep-pin
model (90/160/184) was owner-REJECTED and REVERTED — never re-derive absolute depths.

## The signal is `gen_ts_ns` + `t_send`, NOT `frame_id × fps`

- The painter QR is `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (`src/probe/payload.rs`). ONE camera
  is optically split to every box, so at a SIMULTANEOUS barrier `GetSourceScreenshot` each strih
  input decodes a DIFFERENT painter frame; **`gen_ts_ns` (the painter's own per-frame timestamp,
  identical across boxes for a given frame) is the EXACT, frame-rate-independent latency signal** —
  a box showing an older `gen_ts_ns` is more delayed by that ns difference. Do NOT convert
  `frame_id` to ms via an assumed fps (the dual-QR is ~2 ids/frame @60fps ≈ 8.33 ms/id — a wrong
  33.3 ms/id assumption inflates deltas ~4×; use `gen_ts_ns` directly, keep `frame_id` only for the
  spread-≤1 parity gate).
- **`t_send` compensation (borrowed from `mv-skew-measurement.md`)**: the barrier equalizes the
  request-SEND instant, but the graphics thread serializes the renders, so a later-served camera
  latches a NEWER frame. `round_deltas` computes `latency_i = t_send_i − gen_ts_i` and is
  **cross-clock-safe**: `gen_ts` is the painter's clock, `t_send` is dev1's, so it only ever takes
  SAME-clock DIFFERENCES (`gen_ts_i − g0`, `t_send_i − t0`) — the cross-clock offset cancels, exactly
  as `mv_skew_snapshot.skew_sample_ms` does. Source order is also rotated per round.

## The floor-3 model + the sanity bound

- `m_i = current_pin_i − latency_i`; the MAX-transport (slowest) camera has the MIN `m_i`;
  `new_pin_i = 3 + (m_i − min_k m_k)` — the slowest floors to 3, others get 3 + their RELATIVE delta
  (relative-only, never absolute depth). Medianed over full rounds, undecodable/underrun excluded.
- **The sanity bound (`--max-delta-ms`) MUST stay BELOW the owner's "94 ms between identical cards is
  nonsense"** (default 66 ms ≈ 2 frames). A 100 ms default silently re-enabled the rejected deep-pin
  behavior (a degraded card would pass sanity, get a ~97 ms pin, and the re-measure would certify
  it). A delta above the bound = a degraded/underrun card → FAIL; the abort names the SLOWEST
  (likely-degraded) camera, not a healthy fast one.

## Gate interactions + set membership (both cost a review round)

- **The `[4h/8]` #893 active-floor gate is MUTUALLY EXCLUSIVE with `[4i/8align]`** (`QR_ALIGN != 1`
  in the #893 condition). Both enforce "slowest camera at 3 ms"; if floor-3 floors cam4, no
  ACTIVE-set camera is at 3 and the NEXT run's #893 would abort. `[4i/8align]` owns the floor when on.
- **`CAMERA_ALIGN_SET` is a deliberate SUPERSET of `CAMERA_ACTIVE_SET`** (on-air alignment incl.
  cam4, which the measurable E2E sweep excludes) — but the aligner drops acked-OFFLINE boxes via
  `camera_align_ndi_sources_excluding_csv "$PREFLIGHT_EXCLUDED_CAMS"`, so a wedged/acked cam4 cannot
  abort the whole run. Never a literal cam range (`camera-active-set.md`).
- **DOMAINS the aligner never crosses**: strih per-source pins ONLY. The stream `NDI 2ME PGM` hold
  (operator A/V-align domain) and imag's 3 ms floor are never in the align set; `--pins` refuses an
  underscore/imag-floor-sentinel key.

## Reuse, and Tier-0

- Reuse `mv_skew_snapshot.parse_payload`/`tick_map`/`dominant_run_id` (CRC-validated decode) and
  `apply_latency_pins.apply_pins` (read-back-verified, fail-loud writer) — the aligner imports
  `apply_pins` DIRECTLY (it does NOT shell out to `--pins`; that is the manual runbook path).
- Pure functions are Tier-0 (pytest + fake WS, no rig); the live barrier/decode/apply needs a rig,
  validated by the supervisor. The revert left DANGLING tests asserting the deep pins
  (`test_apply_latency_pins TestPromotedBaseline`, the `test_latency_pins_verify` drift fixture) — a
  baseline-VALUES change (either direction) MUST update those fixtures in the same change.
