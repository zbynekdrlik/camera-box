---
paths:
  - "src/cold_cut.rs"
  - "scripts/lib/cold-cut-step.sh"
---

# Cold-cut gate — the keepalive-bypass step + the LIVE flip (#768 → #1086, LIVE since 2026-09-17)

`src/cold_cut.rs` is the crate-root seam that measures the onset (first ~1s) of each program cut to
a cambox that was hidden `>= COLD_HIDDEN_SECS`. It is **LIVE since #1086** — `gates_overall_pass()`
returns `true` and the fold reds `overall_pass` on a genuine cold-cut miss. This rule keeps the
keepalive-bypass runbook (still the way to MANUFACTURE a genuine cold cut) + the calibration that
backs the flip.

## Why every natural cold cut is WARM (the whole reason #1086 exists)

The strih OBS runs the **#767 keep-alive DistroAV build** — every NDI receiver carries
`PROP_BEHAVIOR_KEEP_ACTIVE` and keeps decoding even when its source is OFF-program
(`.claude/rules/distroav-receiver-lifecycle.md`). So the all-cambox sweep hides each camera `>= 60s`
between windows but the receiver stays WARM the whole time — a revert of issue 767 (a receiver that
never rebinds from cold) would NOT redden the seam. To make a cut GENUINELY cold you must tear the
receiver DOWN: clear its `ndi_source_name` (the same idle discipline `obs_phase2.py`
`_quiesce_probe_input`/teardown use — DistroAV tears it down cleanly), hold it cold, then re-point.

## The keepalive-bypass step (`scripts/lib/cold-cut-step.sh`, #1086) — OFF BY DEFAULT

- `obs_phase2.py idle-receiver --input <NDI input> [--restore <name>]` is the primitive: idle
  clears `ndi_source_name` (+ `genlock_fifo` off) and prints `PREV_NDI_NAME=<name>`; `--restore`
  re-points it. **`overlay: True`** keeps the per-source `genlock_latency_ms_src` pin intact, so
  only those two keys change and the input ends exactly as it started. Restore hardcodes
  `genlock_fifo: True` — CORRECT for the pinned-genlock strih deployment (every strih input is
  genlocked); it would be wrong only on a non-genlocked input.
- The step is wired into the recording-e2e.sh sweep as two gated call sites
  (`cold_cut_before_segment` before each switch, `cold_cut_after_segment` after) + a
  `cold_cut_cleanup_restore` in `cleanup()`. State machine: `none → appeared` (target's 1st cut)
  `→ idled` (first off-target segment after it appeared — receiver torn down cold) `→ restored`
  (before its 2nd cut, topping up the cold hold to `COLD_CUT_HOLD_SECS >= 60`). Produces EXACTLY ONE
  genuine cold cut.
- **Opt-in:** `COLD_CUT_BYPASS_CAM=<sweep label>` (empty ⇒ every function is an inert no-op that
  always `return 0`, so a normal E2E is byte-for-byte unchanged and never trips the sweep's
  `set -e`). When active, `COLD_CUT_BYPASS_INPUT=<strih NDI input, e.g. "NDI cam1">` is REQUIRED
  (`reset_state` fails loud — never guess which live receiver to idle).
- **Safety nets:** `cleanup_restore` re-points an idled-but-never-restored receiver on EXIT (run
  interrupted mid-hold, or a single-appearance sweep). The restore REFUSES an empty captured name
  (`--restore ""` is falsy → would re-idle the input black) — it warns + marks the run skipped.

## Arming it in CI — repository variables + the arm-check guard (`scripts/lib/cold-cut-bypass-guard.sh`, #1086)

`.github/workflows/full-path-e2e.yml` sources both env vars from **repository variables** onto the
recording step: `COLD_CUT_BYPASS_CAM: ${{ vars.COLD_CUT_BYPASS_CAM }}` /
`COLD_CUT_BYPASS_INPUT: ${{ vars.COLD_CUT_BYPASS_INPUT }}`. So obtaining the genuine-cold run is a
**repository-variable flip, no code change** — set `COLD_CUT_BYPASS_CAM=CAM1` +
`COLD_CUT_BYPASS_INPUT="NDI cam1"` (the strih NDI input feeding CAM1's window) in the repo's
Actions → Variables, let the pull_request gate run, then CLEAR them. Unset = empty string = the
runtime lib is an inert no-op, so a normal gate run is byte-for-byte unaffected.

- **Loud + fail-closed pre-flight:** a `Cold-cut keepalive-bypass arm check` step runs FIRST (before
  the rig-busy poll + artifact fetch — it is pure, no rig), sourcing `cold-cut-bypass-guard.sh`'s
  `cold_cut_bypass_arm_check`: silent when both vars empty; a LOUD `ARMED` banner naming both values
  when armed on the sweep run; and it FAILS CLOSED (`::error::`, non-zero) on a
  `COLD_CUT_BYPASS_CAM` outside the current-sweep 2nd-cut set (`cold_cut_bypass_valid_targets`,
  default `CAM1 CAM2 CAM3`, env-overridable via `COLD_CUT_BYPASS_VALID_TARGETS`) or a set CAM with
  an empty INPUT — catching a stuck repo variable before any rig time is spent.
- **ALL_CAMBOX-INERT gotcha:** the cold-cut hooks fire ONLY inside `recording-e2e.sh`'s
  `if [ "$ALL_CAMBOX" = "1" ]` fused sweep (a `pull_request` gate run). On a `workflow_dispatch`
  soak `ALL_CAMBOX='0'`, so an armed bypass is **INERT** (nothing idled, NO cold cut measured) — the
  arm-check step passes `ALL_CAMBOX` (same ternary as the recording step) and prints an INERT
  `::warning::` instead of the ARMED banner there. So the genuine-cold run MUST be a `pull_request`
  gate run, never a manual dispatch.

## The LIVE calibration (#1086, flipped 2026-09-17)

Flipped after all four prerequisites held. The gate blocks on `cold_cut_gate_pass =
!any_genuine_cold_cut_miss` — a `!clean` onset (late/missing wake-up OR onset undecodable `>
ONSET_UNDECODABLE_ALLOWANCE`) whose switch is late enough to rule out the issue-793 startup segfault
(`GenuineColdCutMiss`). Mined across 81 local verdicts (59 with cold transitions, 203 cold
transitions):

| term | LIVE bound | worst observed (green) | disposition |
|---|---|---|---|
| wake-up latency | `WAKEUP_LATENCY_MAX_NS` = 66.67 ms (2f@30) | 47.38 ms (p95 39.4 ms); genuine-cold CAM1 1.76 ms | BLOCK via genuine miss (1.41x margin) |
| `any_wakeup_over_max` / `any_wakeup_missing` | flagged if over ceiling / no decodable onset frame | 0 / 0 runs | BLOCK via genuine miss |
| onset undecodable / transition | `ONSET_UNDECODABLE_ALLOWANCE` = 1 (allow ≤1, block ≥2) | 80/81 runs = 0; 1 run = 1 (156174349, benign warm glitch, wake-up 39 ms, overall_pass=false for UNRELATED gates) | BLOCK ≥2 via genuine miss (1-frame headroom; a black onset = ~30, 30x) |
| `any_genuine_cold_cut_miss` (segfault-ruled-out) | the LIVE fold | 0 runs after the allowance | **LIVE — `gates_overall_pass()=true`** |
| `any_miss_possibly_segfault` (switch < 90 s into run) | attribution-only | 0 runs | REPORT-ONLY — data can't separate the issue-793 segfault; every production cold cut lands past the window |
| sustained-receive-fps / `ReceiveHealth` (floor `30 − 3` = 27) | steady-state whole-window | green min 29.18 | REPORT-ONLY — double-jeopardy with the LIVE continuity gates (copies/gaps/frozen_leg); no cold-restore-degraded datapoint to calibrate a cold-specific floor. Re-entry: a genuine cold-restore-degraded case the continuity gates miss. |

Fold-replication over all 81 verdicts: **0 runs newly red** (the lone old-glitch run passes under
the allowance); zero green runs affected — zero-FP. Tier-0 RED→GREEN via a standalone `rustc --test`
replica of `src/cold_cut.rs` (strip the serde derive) — 4 new-spec tests fail on the old code, 23
pass on the new; the fold is not unconditional (synthetic clean=pass, late-black=red,
early-black=report-only). Consumer sweep: `recording-verdict.rs` gate string report-only → LIVE;
`scripts/e2e_discord_report.py` cold-cut added to `_blocking_failures` (guarded `gates_overall_pass
is True`) + its `_report_only_tripped` branch guarded `is not True` so the two historical fixtures
(`gates_overall_pass=false`) keep meaning report-only.

## Prerequisites (all held before the #1086 flip — kept for the record)

1. A WARM baseline from real E2E runs (the seam serializes `all_cambox_continuity.cold_cut_onset`).
2. At least one GENUINE-cold run using the bypass step (so the wake-up-latency / onset-undecodable
   bound is calibratable against a real cold gap, not a warm-only distribution).
3. Re-confirm per-cambox onset tick-decodability on the target rig
   (`.claude/rules/cambox-tick-decodability.md`) — a box that genuinely can't decode the Vernier at
   onset would read a healthy cold cut as black and false-red.
4. Calibrate the report-only phase-2 constants: `WAKEUP_LATENCY_MAX_NS`, `TARGET_RECEIVE_FPS ±
   SUSTAINED_FPS_TOLERANCE` (the sustained-receive-fps health field — "warm cut works" vs
   "steady-state receive healthy", the issue #1/#799 class), and use the issue-793 discriminator
   (`onset_miss_attribution`: a miss whose switch is `< SEGFAULT_WINDOW_MAX_SECS` into the run is a
   PossibleSegfaultWindow, not a genuine cold-cut miss) to exclude a startup-segfault confound.
   All of this is Tier-0 unit-tested in `src/cold_cut.rs`; the flip itself is a one-line change to
   `gates_overall_pass()`.

## Status of the prerequisites (2026-09-01, issue #1086 data-mine)

Mined all 44 local `verdict-*.json` incl. the first 3-run green 7-cam series (1363366080 /
1168855508 / 674135238):

- **Prereq 1 (warm baseline) — DONE.** Every cold transition across all 44 verdicts is WARM
  (keep-alive receiver never goes cold): worst wake-up 16.09-47.38 ms, never `any_wakeup_over_max`
  / `any_wakeup_missing`. So the report-only `WAKEUP_LATENCY_MAX_NS = 66.67 ms` is validated
  warm-safe — but UNvalidated for the genuine-cold direction it guards.
- **Prereq 3 (per-cambox tick-decodability) — DONE.** All 7 camboxes decode the shared cam2 Vernier
  tick in the green series (`undecodable` 0-1 of ~847 per window, populated `presentation_cadence`).
  No box reads a healthy cold cut black.
- **Prereq 2 (a genuine-cold run) — DONE (17.9.2026).** Release PR 1328 E2E 35166086465 /
  RECORDING_E2E_RUN_ID 557465489 ran with `COLD_CUT_BYPASS_CAM=CAM1` / `COLD_CUT_BYPASS_INPUT="NDI
  cam1"`: strih "NDI cam1" torn down cold for 182 s, restored 0.18 s before the 2nd cut — onset
  30/30 decodable, wake-up 1.76 ms, receive healthy, `clean=true`, no `any_*` flag. Even a genuinely
  cold receiver rebinds inside the ceiling here (the DistroAV reattach is fast). The two other
  transitions (warm CAM2/CAM3) were clean too.
- **Prereq 4 caveat — the onset-undecodable bound is NOT warm-calibratable.** A WARM cut can carry
  a 1/30 optical-glitch undecodable onset frame (run 156174349 CAM2: a healthy 39 ms warm cut
  flagged `genuine_cold_cut_miss`), so the current 0-tolerance criterion would FALSE-RED on a warm
  glitch. The LIVE gate needs an onset-undecodable ALLOWANCE, and because a genuine cold onset's
  first frame(s) are legitimately undecodable during rebind, that allowance is coupled to the cold
  wake-up — calibrate it WITH the genuine-cold run, never from warm-only data (same trap as the
  wake-up ceiling). The sustained-fps floor (27.0 = 30 − 3) is warm-safe: healthy min is
  CAM3/CAM7 ~29.2-29.5, so keep the floor ≤ ~29.2.


## GOTCHA — a `gh run rerun` of a MERGED PR's E2E cannot be the genuine-cold run (17.9.2026)

Once the PR is merged, the `#646` docs-only-diff detector at the top of `full-path-e2e.yml` sees an EMPTY diff
(base already contains head) and every downstream step — the cold-cut arm check, the rig-busy gate, the
recording harness, the fail-closed guard — is SKIPPED; the attempt completes green in ~15 s having measured
nothing (live: run 35152458072 attempt 2 with both `COLD_CUT_BYPASS_*` variables set). The genuine-cold run
must therefore ride an OPEN pull_request run: set the two repository variables immediately BEFORE opening
the next dev→main PR, harvest the verdict, unset them. A `workflow_dispatch` run is plan-only
(`E2E_EXECUTE_VERDICT=0`) and equally useless for it.
