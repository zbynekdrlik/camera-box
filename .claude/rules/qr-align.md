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

- **ADDITIVE pins (#1253 — corrects the #1161 formula; the FIFO is `present_age = transport + pin`).**
  `m_i = current_pin_i − latency_i`; the OLDEST-present (min-delta) camera anchors to its current pin
  (floor 3 after the reset). Every YOUNGER camera is pinned to `round(current_pin_i + pure_delta_i)` —
  its CURRENT pin plus the RELATIVE present-age gap it must add to match the oldest — so the additive
  FIFO delays it into parity. The pre-#1253 formula pinned `round(arrival_floor_i + pure_delta_i)` (an
  ABSOLUTE present-age target), which OVERSHOT because the FIFO is `transport + pin`, not
  `max(pin, transport)` (run 1899055119: `post ≈ pre + pin_delta` — see the #1252/#1253 section
  below). Still RELATIVE-only in effect (the younger cameras reach the OLDEST present age — no net
  latency beyond the physical transport, never the rejected 90/160/184 absolute depth). The arrival
  floors are still fetched, but now ONLY to budget-check the RESULTING present age
  (`arrival_floor_i + delta_i`) against the 94 ms ceiling, never to compute the pin.
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
- **`CAMERA_ALIGN_SET` is a deliberate SUPERSET of the ALIGNABLE part of `CAMERA_ACTIVE_SET`**
  (on-air alignment incl. cam4, which the measurable E2E sweep excludes) — with ONE named
  exclusion: **cam2 (the projection probe) NEVER derives into the align set** (issue 1216,
  2026-08-28). cam2's grabber captures imag-nb's HDMI output, so its painter-QR view arrives
  through painter → cam1 camera → strih → imag → HDMI → grabber — structurally ~8 painter ids
  (~130 ms) behind the direct splitter family; the floor-3 MUTUAL align cannot equalize it by
  design, and its bimodal decode (twice-rescaled optical image, 4/17 rounds) flips the measured
  spread 2-3 ↔ 6-9 ids so the stability criterion fails (run 33166543288's 'unstable' abort).
  The aligner also drops acked-OFFLINE boxes via
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
  TAIL only. "Mutually stable" = the tail spreads' **max−min ≤ `--stable-tol-ids`** (default 1, the
  tight CLEAN band) — the pairwise form, which subsumes the ticket's "round-to-round ≤1" AND rejects a
  slow monotonic ramp (spreads 1,2,3 have round-to-round ≤1 but max−min 2 → still diverging →
  correctly not stable).
- **OUTLIER-TOLERANT since #1161 (the measurement-window robustness lane).** A HEALTHY rig with no
  convergence transient is stationary noise around a center (2-3) with occasional near-band 4/5/1
  blips; the width-1 band kept truncating the suffix on every blip, so the tail formed late and the
  window ended before 5 clean rounds accrued (a healthy rig wrongly FAILED, live E2E 32568491541 —
  spreads `[2,3,2,3,4,3,5,1,3,3,1,3,1,2,2,3,1,2,2,4,1,3,2,2]`, tail only 3). Fix (`_stable_tail`): a
  round that widens the CLEAN band beyond `--stable-tol-ids` is a SKIPPABLE outlier (the span
  continues across it, it never extends the band and never counts as a clean round) iff BOTH
  (a) MAGNITUDE — within `--stable-outlier-tol-ids` (default 2) of the clean band (a near-band blip /
  measurement-cadence hiccup, NEVER a far convergence transient or a large swing); and (b) COUNT —
  after skipping, outliers stay STRICTLY FEWER than clean rounds (the in-band core stays the
  majority). A far/over-budget outlier or a non-FULL round STOPS the walk. `_stable_tail_start` is now
  a thin wrapper over `_stable_tail` (one algorithm, no mirror-drift).
- **Stop decision (`measure_tail_status`, PURE):** stability is judged over the maximal contiguous
  span of **FULL** rounds (a decode-miss round breaks the span — `_stable_tail`). Then:
  (a) tail already at parity (median spread ≤ the ≤1-id gate, ≥ `min_parity_rounds` rounds) →
  `converged-aligned`, PASS fast on just K clean rounds; (b) tail stable but NOT at parity AND ≥
  `min_valid_rounds` (5) **CLEAN** rounds → `converged-stable`, re-derive floor-3 from the tail;
  (c) stable but < min_valid CLEAN rounds → `stable-need-more`, keep measuring. **K=3 < min_valid=5 by
  design:** the cheap already-aligned confirm needs only 3, but the re-derive path keeps measuring to
  accumulate 5. **`min_valid` is judged on the CLEAN (in-band) count, NEVER the span length** — an
  outlier round never counts toward the 5, so the LENGTH strictness is unchanged; each gate (66 ms
  sanity, ≤1-id parity, min-valid/parity rounds) is applied UNCHANGED to the tail.
- **Bounded (`--measure-budget-s` ~150 s + `--max-measure-rounds` 40, extended by #1161):** sized from
  the data — a late tail (~r21 on the live run) plus a possible issue-1145 backlog transient need room
  for a transient-drain + 5 clean rounds (~3.75 s/round). A rig that never stabilizes (a
  degraded/over-rate grabber, a sawtooth ±5-11 whose swings are magnitude-rejected, a near-band
  high-frequency 2-cycle whose outliers are count-rejected) still FAILS within the bound with the full
  table printed. The verify (post-apply) re-measure is stable-tail too, so a pin-change transient is
  not re-caught.
- **Wiring:** `qr-align.sh` remaps `QR_ALIGN_ROUNDS` → `--max-measure-rounds` and adds
  `QR_ALIGN_BUDGET_S` → `--measure-budget-s`. The ~90 s bound is INTERNAL to the python, so the E2E
  step needs no outer `timeout` and `recording-e2e.sh` is UNTOUCHED (its static anchors stay intact).
- Tier-0: `_stable_tail` / `_stable_tail_start` / `measure_tail_status` are PURE (no rig);
  `measure_stable_tail` + the `align()` flow are tested against a monkeypatched `barrier_screenshot`
  (`tests/python/test_qr_align_tail_1160.py` — incl. the #1161 outlier-tolerance + extended-window
  cases: the exact live 24-round sequence converges-stable, a lone near-band blip is skipped not
  reset, and the sawtooth / [10,3] / [1,3] / ramp / backlog regressions still FAIL).

## A pin BELOW the arrival floor is inert — pin ABOVE it (the floor-aware fix, #1161)

> **CORRECTED by issue 1253 — the FIFO is ADDITIVE (`present_age = transport + pin`), NOT
> `max(pin, transport)`.** This whole section's max-model premise (a below-floor pin is "inert", so
> the aligner must pin ABOVE the arrival floor via the genlock-C ACQUIRE frame-mover) was a
> MISDIAGNOSIS. Run 1899055119 proved `post ≈ pre + pin_delta`: EVERY pin adds hold, so `floor + delta`
> is NOT inert and writing an absolute `arrival_floor + delta` target as the pin OVERSHOOTS. The live
> plan is now the additive `new_pin_i = current_pin_i + delta_i` (arrival floors kept ONLY for the
> budget check). The text below is retained for the #1161 history + the two-phase reset + partial-audit
> fallback mechanics (all still used); read the pin FORMULA + the FIFO model from the #1252/#1253
> section, not from the max-model claims here.

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
  reconstructs each source's `latency_ms + mean_head_skew_ms` from `genlock-jitter-report --json` (the
  pin's own DanteSync-synced OBS clock — comparable to the pin), reusing
  `prerecord_phase_calibrate.measured_by_camera` (no re-implementation). `qr-align.sh` fetches it into
  `--jitter-json` (best-effort — see below).
- **The audit "arrival floor" is the PRESENT AGE `max(pin, transport)`, NOT the raw transport — so it
  is only a true transport from an UN-PINNED start.** Pins PERSIST across runs; a prior aligned run
  leaves them elevated (`{3,66,66,66}`), so run 2+ would read pin-HELD ages, not transports (review 🔴).
  Hence the **TWO-PHASE RESET (`qr-align.sh` PHASE 0 → `qr_align_pins.py --reset-to-floor` /
  `reset_pins_to_floor`):** reset every align pin to the floor, settle so the genlock sheds DOWN to the
  transport, then RE-FETCH the audit (scoped to ONLY the post-settle log lines — the [4g/8]
  Correction-2 line-count discipline, never a blind `-Tail` that averages `latency_ms` across two pin
  regimes) so the floors are TRUE transports and the SLOWEST returns to pin 3 EVERY run (the owner's
  floor-3 doctrine, preserved). The `win_ssh_run` fetches are `timeout`-bounded (win-ssh-exec.sh's own
  doc: the caller must bound it). `QR_ALIGN_RESET_SETTLE_S` / `QR_ALIGN_AUDIT_WINDOW_S` tune the waits.
- **`floor_aware_pins(arrival_floors, deltas, floor_ms, max_abs_latency_ms, current_pins)`:** the slowest
  (min-delta) camera → `floor_ms` (inert, stays at its natural floor); every faster camera →
  `max(floor_ms, round(arrival_floor_i + pure_delta_i))` (its floor + the PURE present-age hold it must
  add — `deltas` come from `round_deltas` over ZERO pins so the cross-clock offset still cancels; the
  `max(floor_ms, …)` clamp never emits a sub-floor pin), which sits ABOVE its floor so the genlock-C
  frame-mover engages. FAILs LOUD per-camera when a target exceeds `max_abs_latency_ms` (94), and when
  a faster camera has no arrival floor (never a fabricated floor). `current_pins` is a belt-and-suspenders
  for a DIRECT (no-reset) call: a pin-dominated co-slowest (current_pin ≥ its present age → held by its
  OWN pin, true transport unobservable below) is NOT torn down to the floor (that would drop it to that
  lower transport → misaligned); it keeps its pin. On the reset path every pin is at the floor, so this
  never triggers and the true slowest floors correctly. Pins reach the SLOWEST's NATURAL floor only — no
  net latency beyond the physical transport, never the rejected deep 90/160/184.
- **The SPREAD sanity gates the PURE present-age deltas when floors are available** (`sanity_ok(pure_deltas)`),
  same 66 ms bound — the pin-FOLDED `deltas` over-read by ~the pin elevation from a pinned steady state
  and would spuriously FAIL a legit drift as a "degraded grabber" (review 🔴); the folded-delta sanity
  stays for the no-floors fallback (unchanged there).
- **A PARTIAL audit degrades gracefully, never a hard abort (review 🟡):** if a FASTER camera is missing
  its floor, `align()` falls back to the (inert-prone) floor+delta plan with a loud warning (the verify
  re-measure still FAILs a genuine misalignment) — a partial fetch must never be strictly worse than no
  fetch.
- **Off-parity attribution splits by which plan ran.** If above-floor pins were applied (jitter JSON
  present) but the re-measured tail STILL stays off-parity → `floor_aware_stuck_abort_reason` (the
  genlock-C frame-mover did not engage: its build is not deployed on strih, or the transport floor
  shifted mid-run). If no arrival floor was available (fallback floor+delta, possibly below-floor) →
  the pre-fix `hold_inert_abort_reason` (`pins_requiring_more_hold` + `format_pin_apply_report`).
  Either way parity tolerance is NEVER widened — the run FAILS the owner's same-frame bar. The verify
  re-measure is the live acceptance instrument; a wrong computed pin still FAILS it.
- **Best-effort reset+fetch → graceful fallback.** When `qr-align.sh` cannot run the reset+audit
  (standalone call with no `win_ssh_run`/`PROBE_BIN_DIR`/`OUTDIR`, a reset failure, an unreachable log,
  no audit lines), the aligner falls back to the (inert-prone) floor+delta plan with a loud WARNING
  rather than aborting — so a missing audit degrades to the pre-fix behavior, never a hard stop. The
  normal E2E path runs the reset+fetch for the actual floor-aware fix.
- **Tier-0:** `arrival_floors_from_jitter`, `floor_aware_pins` (incl. the clamp + don't-tear-down),
  `floor_aware_stuck_abort_reason`, `reset_pins_to_floor`, and the align() flow (a FIFO-modelling
  barrier: #1253 additive present_age = transport + pin; pinned-state sanity + partial-audit fallback) are
  pure/monkeypatched — `tests/python/test_qr_align_pinapply_1161.py`. The `qr-align.sh` two-phase
  reset+fetch is best-effort bash (verified by `bash -n` + shellcheck + a 3-case reset/arg smoke; the
  live reset+fetch is supervisor-verified on the rig).

## Three outcomes for a STABLE tail — WITHIN-BUDGET aligns, UNSTABLE/degraded FAILs, BUDGET-BOUND soft-releases (#1161 final lane / issue 1168)

Once the tail is proven STABLE and within the 66 ms spread sanity, the floor-aware plan has THREE
outcomes, not two. The split lives in `align()` via the pure `floor_aware_partition(arrival_floors,
deltas, floor_ms, max_abs_latency_ms, current_pins) -> (plan, over_budget, missing)` (the shared core
`floor_aware_pins` also delegates to — one copy of the loop, no mirror-drift):

1. **WITHIN-BUDGET correctable — APPLY + verify (byte-unchanged).** Every faster camera's target
   `arrival_floor_i + delta_i` ≤ the 94 ms ceiling → the plan is applied, re-measured, and PASSes iff
   parity holds. The pre-#1161-final behaviour, untouched.
2. **UNSTABLE / degraded / HOLD-INERT — FAIL (byte-unchanged).** A never-stabilizing tail, a spread
   above the 66 ms sanity, a faster camera missing its arrival floor, or a within-budget pin that is
   applied but whose frame does NOT move (`floor_aware_stuck_abort_reason` / `hold_inert_abort_reason`)
   → the run ABORTS with the per-camera named reason. Requirement 4: **HOLD-INERT stays a FAIL** — a
   real defect is NEVER folded into budget-bound.
3. **BUDGET-BOUND — SOFT-RELEASE, apply NONE, exit 0 (NEW).** The tail is STABLE and sanity-clean, but
   ≥1 faster camera's target exceeds the 94 ms ceiling — the constant per-box arrival-floor offset
   whose correction is physically budget-impossible (a pin above the ceiling is forbidden by the
   deep-pin doctrine). `align()` sets `status="budget-bound"`, applies NOTHING (the two-phase reset
   already floored the align set), persists `over_budget` (per-camera `floor/delta/target/bound`) +
   `report_only_residual_ms` into the result JSON, emits the loud `budget_bound_report()` marker
   (`arrival floor X + delta Y = Z > bound 94 … REPORT-ONLY RESIDUAL: cross-camera spread ~N ms
   survives — tracked in issue 1168`), and returns; `main()` exits 0 so the E2E proceeds. Basis: the
   supervisor's judgment + the owner's 2026-07-31 revision ("zelený gate najprv, pritvrdenie cez
   tickety; zakázané je len TICHÉ obídenie") — a stable, budget-impossible tail PASSES with a LOUD
   report-only residual; instability keeps FAILING.

**Why APPLY NONE, not a within-budget partial apply.** The existing verify model requires FULL
cross-camera parity (`post_ok` over every camera), which can never hold while the over-budget camera
stays at its floor — so a partial apply cannot be certified by it without new verify logic and a live
re-measure round, and would risk mis-attributing a genuine hold-inert defect as budget-bound. Apply-
none is the floor-3 doctrine's honest baseline (every camera at its natural floor), needs no new
verify, cannot mask a hold-inert defect, and the reported residual IS exactly the per-box floor spread
issue 1168 reduces. `floor_aware_partition` still clamps the over-budget cameras to the floor (a pin
we cannot afford is never written up), so the pure plan is ready if a future ticket ever wants partial
apply.

**Residual visibility.** The align step's channel into the per-run report is its RUN LOG: the result
JSON (stdout, now carrying `budget_bound`/`over_budget`/`report_only_residual_ms`) + the loud
`budget_bound_report()` stderr marker — the same greppable-run-log-marker convention
`IMAG-LEG-NOT-VERIFIED:` uses for a report-only preflight state. The exit-code contract (0 = success)
flows unchanged through `qr-align.sh` and `recording-e2e.sh`, so neither needs a change (folding the
residual into the composed Discord report TEXT would need the report composer / verdict JSON — a
separate report lane).

**Re-tighten (issue 1168).** When the per-box arrival floors are reduced so the max cross-camera floor
delta drops under the correction budget (94 − floor), REVERT this soft-release: turn the BUDGET-BOUND
branch back into a hard-FAIL on unalignment. Tracked there.

Tier-0: `floor_aware_partition`, the budget-bound `align()` flow (FIFO-modelling barrier), and the
byte-unchanged within-budget/unstable/hold-inert regression guards are pytest-verified with no rig
(`tests/python/test_qr_align_pinapply_1161.py`); the live soft-release is the supervisor's E2E
acceptance instrument.

## A one-source-frame spread is the N=2 lock-phase QUANTUM, not a lag — never pin it (#1252)

Run 1899055119 aborted `[4i/8align]`: the floor-aware plan raised +83 ms pins on cam1/4/5/6/7 and
the re-measured residual DOUBLED (16.7 ms → ~84–100 ms), with `post_residual ≈ pre_residual +
pin_delta` on every camera. The genlock-C frame-mover was EXONERATED (the setter cleared anchor +
boundary; the ACQUIRE-bracket logged HOLD then ACQUIRE at oldest ≥ reserve) — it moved each frame
exactly as far as the (wrong) pin told it to.

**Root cause — the plan chased a phantom quantum.** ONE camera → splitter → every box sees the
IDENTICAL image, so there is no real cross-cambox transport spread. The ~16.7 ms cross-camera
"residual" is exactly ONE 60 fps source frame (1000/60 = 16.67 ms) = the N=2 (60-into-30) lock-phase
quantum: which of the 2 source frames a 30 fps canvas frame latches shifts a camera's measured
present age by an integer number of source frames, and over a short (N=2) audit the mean reads one
frame off. cam3's arrival floor read 84 (= 67 + 16.7) from only `samples=2` — a PHANTOM "slowest"
reference. The pin lever provably cannot close such a quantum: (1) it is presentation-phase jitter
around zero — the frame_id tail shows the "slowest" camera alternately AHEAD and behind, so there is
no consistent slowest to pin against; (2) the lever only ADDS delay (`post = pre + pin_delta`), so it
can only GROW a sub-frame spread; (3) no pin can go below the 3 ms floor to pull a camera earlier;
(4) the measurement itself carries the same ±one-source-frame quantum, so a "correction" could not be
verified. So `within_aligned_quantum(deltas)` (spread < `DEFAULT_ALIGNED_QUANTUM_MS` = 1.5 source
frames ≈ 25 ms) classifies the rig ALREADY ALIGNED at the floor-3 achievable limit and applies NO
pins — a new PASS status `already-aligned-quantum`, gated on the PURE present-age spread AFTER the
66 ms degraded-grabber sanity gate (a degraded card still FAILs; a real ≥ 2-source-frame spread is
still planned). This does NOT widen the same-frame parity bar — it suppresses the ABOVE-FLOOR PIN
PLAN when its own input is a sub-frame phantom, nothing else. `1.5` source frames is the
discriminator: a spread that rounds to ≤ 1 source frame is the quantum, ≥ 2 is real.

**Two quantities that DISAGREE at this precision — do not reconcile them.** The pin plan's "residual"
comes from `round_deltas` (the `gen_ts_ns` + `t_send` compensated present-age delta, ms); the
per-round table prints raw `frame_id`. At the sub-frame precision that matters here they are
DIFFERENT quantities and DISAGREE (the run's frame_id tail spread was 2–4 painter ticks with cam3
sometimes AHEAD, while the present-age residual was one source frame with cam3 the "slowest"). That
disagreement is itself the tell that there is no real offset to resolve — only noise around zero. A
future worker chasing a mismatch between the frame_id table and the ms residual is chasing the
quantum; do not "fix" it by widening the parity bar or deepening pins.

**Fixture note:** the issue-1161 / issue-1160 align tests that used a 2-id (~16.7 ms) or 19 ms spread
to exercise the plan / hold-inert / re-derive paths were superseded — a sub-quantum spread is now
already-aligned, so those fixtures were bumped to a CLEARLY-real spread (4 ids ≈ 33 ms = 2 source
frames; the pinned-state sanity test to 27 ms, target 93 ≤ the 94 ms ceiling). When writing a NEW
align test that must reach the plan, use a spread ≥ 2 source frames (≥ ~34 ms), never one source
frame. Tier-0: `within_aligned_quantum` + an `align()` flow test reproducing the run from its REAL
recorded deltas + arrival-floor audit (`tests/python/test_qr_align_quantum_1252.py`), pytest, no rig.

**Aliasing blind band (16.7 ms, 25 ms) — the one honest cost.** The lock phase is PERSISTENT while
locked (it does not average out over the tail), so a measured spread = the real spread ± < 1 source
frame. A GENUINE 2-source-frame lag (33.3 ms) whose phase biases it DOWN can therefore alias into
(16.7, 25) ms and be quantum-suppressed — a PASS with a real 2-frame offset left standing. This is
inherent to ANY threshold between 1 and 2 frames (1.5 frames is the max-separation cut); the backstop
is DOWNSTREAM — the recording-verdict SOURCE cross-camera spread gate still blocks a large real
spread. So if a run PASSES `[4i/8align]` but later FAILs the recording SOURCE-spread gate, suspect a
real 2-frame lag aliased low here — do NOT read the align PASS as "cameras are frame-perfect".

**FIXED in issue 1253 — the FIFO is ADDITIVE, so the plan ADDS hold to the current pin.** The
measured `post ≈ pre + pin_delta` means the live FIFO is `present_age = transport + pin`, NOT the
`max(pin, transport)` model the issue-1161 above-floor formula (`pin_i = arrival_floor_i + delta_i`)
assumed — so writing an ABSOLUTE present-age target as the pin adds it ON TOP of the transport and
OVERSHOOTS by ~the arrival-floor baseline (the +83 ms doubling). For run 1899055119 the quantum gate
catches it first (already-aligned), but a GENUINE ≥ 2-source-frame spread reaches the formula and
overshoots into a loud hold-inert abort. The supervisor confirmed additivity from run 1899055119
(a de-facto controlled experiment: pin +84 → residual shifted +84 additively on every pinned camera)
and ruled option (1): the **additive-correct plan `new_pin_i = current_pin_i + hold_i`** (the PURE
present-age gap from `round_deltas` over zero pins). The oldest-present camera (hold 0) keeps its pin
(floor after the reset); every younger camera is delayed by exactly its present-age gap so all present
ages converge to the OLDEST (relative-only — no net latency beyond the physical floor, exact and
independent of current-pin uniformity: new present age = present_age_i + (max_present − present_age_i)
= max_present). The `max`-model "pin-dominated co-slowest don't-tear-down" special case is SUBSUMED
(hold 0 → keeps the pin). The issue-1161/1168 BUDGET-BOUND soft-release is preserved and re-expressed:
`arrival_floor_i + hold_i` is now the RESULTING present age (= max_present), so `> 94` still means
"aligning UP to the oldest present age blows the ceiling" (the oldest camera cannot be brought DOWN —
pins only add) → apply none, report-only residual, exit 0. Arrival floors are now used ONLY for that
budget check, never the pin value. The `samples=2` PHANTOM arrival floor is treated at the SOURCE:
`arrival_floors_from_jitter` DROPS a floor whose explicit `samples < MIN_FLOOR_SAMPLES` (3) — run
1899055119's cam3 "84" = 67 + 16.7 came from `samples=2` (a MISSING samples count is trusted; only an
explicit low count is the known phantom). The `within_aligned_quantum` gate is unchanged and still
runs before any plan. Tier-0: `tests/python/test_qr_align_pinapply_1161.py` (the `_FifoBarrier` model
migrated `max` → additive, an additive-overshoot RED→GREEN proof) + `test_qr_align_quantum_1252.py`
(the phantom-floor filter).
