---
paths:
  - "scripts/qr_align_pins.py"
  - "scripts/lib/qr-align.sh"
  - "tests/python/test_qr_align_pins_1003.py"
  - "tests/python/test_qr_align_tail_1160.py"
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

## The floor-3 model + the two bounds

- **FLOOR-AWARE pins (#1161 — supersedes the old `3 + delta` on `--execute`).** `m_i =
  current_pin_i − latency_i`; the MAX-transport (slowest) camera has the MIN `m_i` and anchors to
  pin 3 (inert, stays at its natural floor). Every FASTER camera is pinned to `round(arrival_floor_i
  + pure_delta_i)` — its own ABSOLUTE arrival transport floor plus the RELATIVE hold it must add to
  match the slowest (= the slowest's floor). The old `new_pin_i = 3 + delta` was INERT because the
  genlock FIFO is `latency = max(pin, transport)`, not `pin + transport`: in the transport-dominated
  regime (frames arrive ~59-66 ms old, deltas ~1 canvas frame) `3 + delta` lands BELOW the arrival
  floor and has no leverage (see the #1161 section below). Still RELATIVE-only in effect (the faster
  cameras reach the SLOWEST's NATURAL floor — no net latency beyond the physical transport, never the
  rejected 90/160/184 absolute depth), just computed ABOVE each floor so it actually moves the frame.
- **Two SEPARATE bounds, both hard, neither ever widened:**
  - The SPREAD sanity `--max-delta-ms` (default `DEFAULT_MAX_DELTA_MS = 66` ms ≈ 2 frames) — the
    cross-camera delta (max−min). MUST stay BELOW the owner's "94 ms between identical cards is
    nonsense". A delta above it = a degraded/underrun card → FAIL, naming the SLOWEST (likely-degraded)
    camera, not a healthy fast one. UNCHANGED by #1161; runs FIRST.
  - The ABSOLUTE achievable-latency ceiling `--max-abs-latency-ms` (default `DEFAULT_MAX_ABS_LATENCY_MS
    = 94` ms, the owner's 94 ms line) — a floor-aware target (`arrival_floor_i + delta_i`) above it =
    the transport floor is too high to align within budget → `floor_aware_pins` FAILs LOUD per-camera
    ("cam3 arrival floor 66ms + delta 33ms = 99ms > bound 94ms — investigate the transport floor, do
    NOT raise the bound"). Never deep-pins, never widens the bound.

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

## The burn QR shares the painter's EXACT wire format — exclude burn run_ids (#1159)

Under E2E the `[4i/8align]` step runs AFTER the measurement burns are added (`[4b/8]`
`obs_burn_filter.py add`), so every barrier screenshot carries the painter dual-QR **and** the
per-source burn QR. The burn (`vendor/distroav/src/ndi-burn-filter.cpp`) emits its QR in the
**BYTE-IDENTICAL** painter wire format `P{run_id}.{frame_id}.{gen_ts_ns}.{crc}` — it differs ONLY
in `run_id` (a fixed per-node id derived from the host role: **strih=911002 on EVERY strih input**,
stream=911004, imag=911003, plus per-camera capture burns — the full set is
`NODE_BURN_RUN_IDS` = 911001-911012, mirrored in `qr_align_pins.py` from
`src/probe/recording.rs::NODE_BURN_RUN_IDS`). So `parse_payload` accepts a burn as a valid painter
payload; a "filter by payload SHAPE" cannot tell them apart — **the discriminator is the run_id.**

Two defects both followed from this (fixed #1159):
- **run_id auto-detect picked the burn.** The strih burn 911002 is present on all on-air inputs,
  so it TIES the painter on screenshot-count, and `mv_skew_snapshot.dominant_run_id` breaks ties to
  the **SMALLEST** id — 911002 << the painter's ~1.8e9 epoch — so the burn won. Combined with cv2's
  flaky multi-QR decode (it intermittently drops the burn on some shot every round), the chosen
  (burn) id was absent somewhere every round → "0 fully-decodable measurement rounds". Fix:
  `painter_run_id()` strips `NODE_BURN_RUN_IDS` before delegating to `dominant_run_id`.
- **The decode recovery-ladder guard was fooled.** `decode_qr_texts`'s `any(t.startswith("P"))`
  guard treated a decoded burn as "painter found" and skipped the upscale/threshold pass that would
  still recover a missed painter. Fix: `has_painter_payload()` (parse + non-burn run_id).
- `pick_painter_tick` also excludes burn run_ids (defense in depth).

**mv_skew_snapshot's `dominant_run_id`/`tick_map` were left UNTOUCHED** — the burn-exclusion lives
in `qr_align_pins.py` only (scope). If mv-skew is ever run WHILE measurement burns are ON, it has
the same latent bug and would need the same `painter_run_id`-style exclusion.

**cv2 multi-QR decode is genuinely flaky** — with 3 QRs in a frame it drops symbols
non-deterministically (measured: the burn missed on ~1/12 shots; larger QRs missed MORE, not less).
So a composited-image test CANNOT be a reliable RED; the deterministic reproduction is a **pure
`ticks_from_raw()` test over synthetic decoded-text lists**. A composited painter+burn PNG decoded
through cv2 is fine as a GREEN integration proof (the painter itself decodes reliably), but assert
the burn genuinely coexisted (`burn_seen`) so the test still exercises the condition.

**FAIL diagnostics:** every abort path now prints `format_round_table()` (round × camera decoded
frame_id, `--` = undecoded, per-camera `decoded N/R`) so the operator can tell undecodable from
unstable-spread from one-dead-camera. Since #1160 it takes an optional `tail_start` and marks the
STABLE-TAIL rounds used for the verdict with a `tail` column (2-arg callers get the old format).

## Measure to a STABLE TAIL, never the convergence transient (#1160)

The rig backlog (issue 1145) drains at ~0.3 frame/s, so a fresh restart / receiver reconnect / burn
toggle in the earlier E2E steps leaves a camera MINUTES over the align bound while it catches up. The
OLD aligner measured a FIXED 9-round window and medianed the WHOLE window → it judged the transient
(spreads decaying 10,10,11,12,9,9,9,7,2 id → median 9, worst delta 75 ms > the 66 ms sanity bound →
abort) though steady state was ≤2 id seconds later. **Never judge a fixed window of a converging
system.**

- `measure_stable_tail()` (replaced the fixed-count `measure_rounds`) loops barrier rounds until the
  last **K** (`--stable-tail-rounds`, default 3) rounds are MUTUALLY stable, then judges the STABLE
  TAIL only. "Mutually stable" = the tail spreads' **max−min ≤ `--stable-tol-ids`** (default 1) — the
  pairwise form, which subsumes the ticket's "round-to-round ≤1" AND rejects a slow monotonic ramp
  (spreads 1,2,3 have round-to-round ≤1 but max−min 2 → still diverging → correctly not stable).
- **Stop decision (`measure_tail_status`, PURE):** stability is judged over the maximal contiguous
  suffix of **FULL** rounds (a decode-miss round breaks the suffix — `_stable_tail_start`). Then:
  (a) tail already at parity (median spread ≤ the ≤1-id gate, ≥ `min_parity_rounds` rounds) →
  `converged-aligned`, PASS fast on just K rounds; (b) tail stable but NOT at parity AND ≥
  `min_valid_rounds` (5) rounds → `converged-stable`, re-derive floor-3 from the tail; (c) stable but
  < min_valid rounds → `stable-need-more`, keep measuring. **K=3 < min_valid=5 by design:** the
  cheap already-aligned confirm needs only 3, but the re-derive path keeps measuring to accumulate 5
  — no threshold is weakened, each gate (66 ms sanity, ≤1-id parity, min-valid/parity rounds) is
  applied UNCHANGED to the tail.
- **Bounded (`--measure-budget-s` ~90 s + `--max-measure-rounds` 30):** a rig that never stabilizes
  (a degraded/over-rate grabber) still FAILS within the bound with the full table printed. The verify
  (post-apply) re-measure is stable-tail too, so a pin-change transient is not re-caught.
- **Wiring:** `qr-align.sh` remaps `QR_ALIGN_ROUNDS` → `--max-measure-rounds` and adds
  `QR_ALIGN_BUDGET_S` → `--measure-budget-s`. The ~90 s bound is INTERNAL to the python, so the E2E
  step needs no outer `timeout` and `recording-e2e.sh` is UNTOUCHED (its static anchors stay intact).
- Tier-0: `_stable_tail_start` / `measure_tail_status` are PURE (no rig); `measure_stable_tail` + the
  `align()` flow are tested against a monkeypatched `barrier_screenshot`
  (`tests/python/test_qr_align_tail_1160.py`).

## A pin BELOW the arrival floor is inert — pin ABOVE it (the floor-aware fix, #1161)

The genlock FIFO is `latency = max(pin, transport)`, NOT `pin + transport`. So raising a source's
`genlock_latency_ms` moves the presented frame ONLY when the new pin exceeds that source's arrival
TRANSPORT floor (how old frames already are when they reach strih). In the transport-dominated regime
the live rig sits in — frames arrive ~59-66 ms old (head_skew 76/59) while the cross-camera deltas
are ~1 canvas frame — the OLD `floor(3) + delta` plan (≈ 3-50 ms) lands BELOW the floor, so it is
structurally INERT: the FIFO cannot present a frame younger than what arrived, and a reserve below the
arrival edge has no leverage (live E2E 32556463012: cam3 17→50 read-back OK, frame did not move).
This is NOT a settle-time issue and NOT fixable by the rejected wall-clock frame-grid pin (issue 1003,
2026-08-17/18/20).

**The frame-mover LANDED (sibling genlock-C, `genlock_relock_acquire_should_hold` in
`src/genlock_backlog.rs` + `vendor/obs-studio/libobs/obs-source.c`):** on a pin RISE the setter zeroes
the conveyor boundary to force a bounded re-acquire, and the ACQUIRE branch (N>=2) HOLDs until the
oldest queued frame ages to the raised reserve, then re-anchors via the history-anchored
`genlock_relock_select_nearest` (a fail-open cap prevents a new hold-collapse). It moves the frame
ONLY when the reserve sits ABOVE the arrival floor — so the aligner MUST compute above-floor pins.

**The aligner's floor-aware fix (#1161, `scripts/qr_align_pins.py`):**
- **Absolute floor from the strih genlock audit, NOT the painter QR.** The painter-QR `gen_ts` is
  CLOCK_REALTIME (painter box, DanteSync-synced) while dev1's `t_send` is CLOCK_MONOTONIC — cross-clock,
  so painter-QR gives only RELATIVE cross-camera deltas, never an absolute floor. `arrival_floors_from_jitter`
  reconstructs each source's ABSOLUTE floor `latency_ms + mean_head_skew_ms` from `genlock-jitter-report
  --json` (the pin's own DanteSync-synced OBS clock — comparable to the pin), reusing
  `prerecord_phase_calibrate.measured_by_camera` (no re-implementation). `qr-align.sh` fetches it into
  `--jitter-json` (the same OBS-log fetch `[4g/8]` uses; best-effort — see below).
- **`floor_aware_pins(arrival_floors, deltas, floor_ms, max_abs_latency_ms)`:** the slowest (min-delta)
  camera → `floor_ms` (inert, stays at its natural floor); every faster camera → `round(arrival_floor_i
  + pure_delta_i)` (its floor + the PURE present-age hold it must add — `deltas` come from `round_deltas`
  over ZERO pins so the cross-clock offset still cancels), which sits ABOVE its floor so the genlock-C
  frame-mover engages. FAILs LOUD per-camera when a target exceeds `max_abs_latency_ms` (94), and when
  a faster camera has no arrival floor (never a fabricated floor). Pins reach the SLOWEST's NATURAL
  floor only — no net latency beyond the physical transport, never the rejected deep 90/160/184.
- **Off-parity attribution splits by which plan ran.** If above-floor pins were applied (jitter JSON
  present) but the re-measured tail STILL stays off-parity → `floor_aware_stuck_abort_reason` (the
  genlock-C frame-mover did not engage: its build is not deployed on strih, or the transport floor
  shifted mid-run). If no arrival floor was available (fallback floor+delta, possibly below-floor) →
  the pre-fix `hold_inert_abort_reason` (`pins_requiring_more_hold` + `format_pin_apply_report`).
  Either way parity tolerance is NEVER widened — the run FAILS the owner's same-frame bar. The verify
  re-measure is the live acceptance instrument; a wrong computed pin still FAILS it.
- **Best-effort audit fetch → graceful fallback.** When `qr-align.sh` cannot fetch the audit (standalone
  call, unreachable log, no audit lines), the aligner falls back to the (inert-prone) floor+delta plan
  with a loud WARNING rather than aborting — so a missing audit degrades to the pre-fix behavior, never
  a hard stop. Wire the audit (the normal E2E path does) for the actual fix.
- **Tier-0:** `arrival_floors_from_jitter`, `floor_aware_pins`, `floor_aware_stuck_abort_reason` and
  the align() floor-aware flow (a FIFO-modelling barrier: present_age = max(pin, arrival_floor)) are
  pure/monkeypatched — `tests/python/test_qr_align_pinapply_1161.py`. The `qr-align.sh` audit fetch is
  best-effort bash (verified by `bash -n` + shellcheck + an arg-passthrough smoke; the live fetch is
  supervisor-verified on the rig).
