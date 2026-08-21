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

## The floor-3 pin lever CANNOT ADD hold — a pin INCREASE is inert on a live rig (#1161)

The floor-3 model floors the slowest camera to 3 and RAISES the faster ones' pins to delay them into
parity. That raise is **structurally inert on a live rig** — the aligner cannot move a source's
presented frame to an OLDER one:

- `obs_source_set_genlock_latency_ms` (`vendor/obs-studio/libobs/obs-source.c`, ~7851) on a value
  change clears `genlock_phase_anchor_ns` and re-arms the (ms-path-inert) `genlock_filled` latch, but
  NEVER clears `genlock_locked_next_boundary_ns` (the conveyor) and NEVER forces a re-acquire (the
  ACQUIRE branch runs only when that boundary `== 0`).
- The conveyor is a pure DOWNWARD-only FOLLOWER; `should_converge_phase` (`src/genlock_backlog.rs`)
  only sheds toward `max(reserve, floor)`. Raising `reserve` (= the pin) only raises that shed
  threshold — it never deepens the hold.

So a per-source pin INCREASE moves only the CONFIG value (read-back confirms it), never the presented
frame → a one-canvas-frame residual can survive apply, and the re-measured residual reads INFLATED
(the delta metric `m_i = pin_i − latency_i` folds in the raised pin while the frame stayed put). This
is NOT a settle-time issue (no upward mechanism to wait for) and NOT fixable by the wall-clock
frame-grid pin (issue 1003 REJECTED it three ways). The frame-mover is issue 1003's Stage-2 ACQUIRE
bracketing gate — a genlock-C change, live-only, gated on issue 1004 — OUT of the aligner's reach.

**What the aligner does instead (#1161):** when the re-measured tail STABILIZED but stayed off-parity
AND the plan asked a source to add hold, `align()` attributes the abort PRECISELY (via
`pins_requiring_more_hold` + `hold_inert_abort_reason`) — naming the genlock FIFO limit + issue 1003
— and emits per-source before/after pin+residual telemetry (`format_pin_apply_report`), instead of a
generic "did NOT hold" that reads as flakiness/settle. It NEVER widens the same-frame parity bar; the
run still FAILS. All three helpers are pure (Tier-0, `tests/python/test_qr_align_pinapply_1161.py`).
