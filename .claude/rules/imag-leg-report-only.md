---
paths:
  - "src/imag_leg_gate.rs"
  - "scripts/lib/imag-leg-marker.sh"
  - "src/bin/recording-verdict.rs"
---

# imag leg recording verdict — report-only seam + its TWO gating paths (issue 798)

The imag leg's frame-by-frame recording verdict is REPORT-ONLY today via
`camera_box::imag_leg_gate::gates_overall_pass()` (returns `false`), mirroring
`optical_floor` / `e2e_latency_gate` / `burn_hold`. Two non-obvious facts a future change MUST know:

## The imag leg gates `all_pass` in TWO independent places — flip BOTH via the ONE seam

In `recording-verdict.rs::build_and_print_verdict` the imag leg folds into `all_pass` TWICE:

1. **Whole-recording node fold** — `all_pass &= imag_leg_gate::folds_into_overall_pass(nv.is_zero() && span_ok)` inside `if let Some(imag_frames) = &imag_frames_opt` (`node_verdict_for_imag`: optical tick contiguity + `imag_burn_ok` digital-burn contiguity + optical-beat freeze/copy). Surfaced at `full_chain.loss.imag`.
2. **All-cambox per-segment sweep** — `all_pass &= imag_leg_gate::folds_into_overall_pass(imag_overall_pass)` in the `--switch-schedule` sweep. Surfaced at `all_cambox_continuity.imag`.

Both route through the SAME `imag_leg_gate::folds_into_overall_pass()`, so the issue-798 follow-up
flips ONE function (`gates_overall_pass()` → `true`) to make the imag leg blocking again — do NOT
hunt for two separate toggles. The per-cambox (stream) sweep's own `all_pass &= seg.overall_pass`
fold is a DIFFERENT term and stays blocking — never touch it when changing imag gating.

## The imag verdict only FLOWS when recording-e2e.sh `[8/8c]` succeeds — check `imag_leg_verified`

Both imag blocks are `if let Some(imag_frames)`-guarded: they run ONLY when the merge got
`--merge-partials imag=<json>`. Historically that happened in 0 of 76+ runs — `[8/8c]` degrades
gracefully on any imag-side StopRecord / reachability / decode failure and `[8/8d]` silently omits
the flag. Signals that tell you whether the imag leg was actually verified this run:

- `full_chain.imag_leg_verified` (bool, verdict JSON) — the durable, mineable answer to "did the
  imag leg actually run?". Mine it before assuming a green run proved imag.
- The `IMAG-LEG-VERIFIED` / `IMAG-LEG-NOT-VERIFIED` run-log marker (`scripts/lib/imag-leg-marker.sh`,
  emitted at `[8/8c]`) — names the skip REASON (no-recording-path vs extract-failed).

## Before flipping it blocking (the follow-up)

Per `verdict-gate-seam-calibration.md`: you need a GREEN imag-verdict distribution to calibrate
against, and there were ZERO imag runs at report-only-land time. The follow-up must (1) confirm the
rig-side extract is healthy so imag partials flow (a live E2E — supervisor/rig-ops scope), (2)
accumulate green imag runs, (3) flip `gates_overall_pass()` to `true`, and (4) fold in the issue-887
produced-vs-presented ~7% deficit as a blocking term. Do NOT flip it blind.

## The 0/76 root cause was the decode CPU-PIN, not StopRecord/reachability/decode (issue 1094, FIXED)

The `[8/8c]` extract ran `recording-verdict-on-imag.sh`, whose `build_onimag_command` hardcoded
`nice -n 19 taskset -c 12-15 …`. That range was the E-cores of the RETIRED 16-thread imag notebook.
The box was swapped to an **i5-13420H (12 threads, online cpus 0-11, E-cores 8-11)** — cores 12-15
do not exist, so `taskset -c 12-15` exits **rc=1 "Invalid argument" BEFORE it can exec
recording-verdict**. No partial was ever produced → `[8/8c]` "extract failed" → `[8/8d]` omitted
`--merge-partials imag=…` → `imag_leg_verified=false` on EVERY run. The setup-imag.sh CPU-isolation
plan was already made topology-agnostic for this same swap (`imag_cpu_isolation_plan`, #833/#841),
but the DECODE pin in recording-verdict-on-imag.sh was a SEPARATE hardcoded core reference the swap
sweep missed.

Fixed (1094): `onimag_decode_core_range <ncpus>` (pure, Tier-0-tested) → top min(4,ncpus) cores
(8-11 here, 12-15 on the old box); `main()` resolves it from the LIVE box (nproc over ssh + a
`taskset` probe) and FAILS OPEN (empty range → decode runs unpinned, nice 19 only), so a pin error
can never again silently zero the imag leg. The stale on-box binary is NOT a factor — it supports
`--extract-partial imag` and emits `schema_version=3`, exactly what the current merge expects.

Diagnostic tells: an imag extract that fails with **no decode-progress log line at all** (no
`recording decode progress frames_read=…`) points at the PREFIX (`taskset`/`nice`), not the binary
or the recording — a failing `taskset` aborts the whole command before recording-verdict starts.
And when a box is swapped, sweep EVERY hardcoded core reference (`grep -rn 'taskset -c'`), not just
the isolation plan.

Latent (not a current defect, no ticket): recording-verdict-on-imag.sh STEP 1 re-uploads the binary
only when it is absent/non-executable (`[ -x ]`) — it does NOT compare versions, so a FUTURE
`PARTIAL_SCHEMA_VERSION` bump would leave imag on a stale binary whose partial the fresh dev1 merge
rejects. Harmless while the schema stays at 3; if you bump the schema, add a checksum/version gate
to STEP 1 (or have recording-e2e.sh pass `--force-upload`).
