---
paths:
  - "src/cold_cut.rs"
  - "scripts/lib/cold-cut-step.sh"
---

# Cold-cut gate — the keepalive-bypass step + the LIVE-flip prerequisites (#768 → #1086)

`src/cold_cut.rs` is the crate-root REPORT-ONLY seam that measures the onset (first ~1s) of each
program cut to a cambox that was hidden `>= COLD_HIDDEN_SECS`. It STAYS report-only
(`gates_overall_pass()` hardcoded `false`) until a full-authority session flips it LIVE with real
data. This rule is the map for that follow-up.

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

## Prerequisites before flipping `gates_overall_pass()` LIVE

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
- **Prereq 2 (a genuine-cold run) — STILL OPEN.** No verdict anywhere used `COLD_CUT_BYPASS_CAM`;
  it needs a rig write + a live E2E run (full-authority). In the CURRENT sweep only the re-appearing
  boxes CAM1/CAM2/CAM3 get a 2nd cut, so the bypass target MUST be one of those (onset is measured
  on the 2nd cut).
- **Prereq 4 caveat — the onset-undecodable bound is NOT warm-calibratable.** A WARM cut can carry
  a 1/30 optical-glitch undecodable onset frame (run 156174349 CAM2: a healthy 39 ms warm cut
  flagged `genuine_cold_cut_miss`), so the current 0-tolerance criterion would FALSE-RED on a warm
  glitch. The LIVE gate needs an onset-undecodable ALLOWANCE, and because a genuine cold onset's
  first frame(s) are legitimately undecodable during rebind, that allowance is coupled to the cold
  wake-up — calibrate it WITH the genuine-cold run, never from warm-only data (same trap as the
  wake-up ceiling). The sustained-fps floor (27.0 = 30 − 3) is warm-safe: healthy min is
  CAM3/CAM7 ~29.2-29.5, so keep the floor ≤ ~29.2.
