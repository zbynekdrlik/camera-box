# Strict Gate Coverage Map (EPIC #406 audit)

Refs #406 ("EPIC: Strict automatic CI gates for zero-loss + bounded-latency + zero
A/V-desync (+ render budget) — the standing #1 requirement").

This document is the answer to "for each strict gate, does it run AUTOMATICALLY, and if
so, on what?". It replaces re-deriving this from memory every time the question comes
up. **Audited 2026-07-03** by reading the actual CI workflow, the actual kernel/bin/test
source, AND a real CI run's log (`gh run view <id> --log`, run 28638701425 on
`d0b8e60`) to confirm every claim below against what CI *actually executed*, not just
what the code implies.

## The two-tier model (from the EPIC)

- **Tier A — off-rig, runs on EVERY push/PR ("the 5-min PR CI gate"), no rig needed.**
  A pure-Rust kernel (a `classify`/decision function with no I/O) that encodes the
  PASS/FAIL logic for one of the north-star properties, unit-tested and run by
  `ci.yml`'s `test` (`cargo nextest run --all-features`) and `coverage`
  (`cargo llvm-cov --all-features ...`) jobs on every push to `dev`/`main`. This proves
  the *decision logic* can't regress — it does NOT run against a live recording.
- **Tier B — on-rig, the REAL full-flow gate.** `scripts/recording-e2e.sh` (dispatched by
  `.github/workflows/full-path-e2e.yml`, self-hosted `camera-lan` runner) drives the real
  cam2→cam1→strih→stream chain, feeds live measurements into the Tier-A kernels via thin
  CLI binaries, and decides PASS/FAIL on an actual recording. **This is
  `workflow_dispatch`-only — it does not run on push, PR, merge, or a schedule.**

## Coverage table

| Gate (north-star signal) | Kernel (pure, Tier-0) | Unit tests | CI-automatic? | Rig wiring (Tier B) | Applied live only via |
|---|---|---|---|---|---|
| Render budget (fps/frame-time) — #405/#404 | `src/render_budget.rs:62 classify` | 6 (`:100`) | ✅ `test`+`coverage` jobs | `src/bin/render-budget-gate.rs` ← `scripts/render-budget-gate.py` (OBS-WS GetStats) ← `recording-e2e.sh` `[4d/8]`, hard-fails BEFORE recording starts | manual rig E2E dispatch |
| Colour correctness — #364 | `src/colour_verify.rs:188 classify_patch` / node summary | 27 (`:731`) | ✅ `test`+`coverage` jobs | consumed inside `src/bin/recording-verdict.rs` (`--colour-gate`); pixel sampling in `src/probe/colour_sample.rs` (probe-feature, also run under `--all-features`) | manual rig E2E dispatch (or ad-hoc `recording-verdict --colour-gate` on a recorded file) |
| Frozen-camera freshness — #365 | `src/frozen_camera.rs:51 frozen_cameras` | 11 (`:103`) | ✅ `test`+`coverage` jobs | `src/bin/frozen-camera-gate.rs` ← `scripts/frozen-camera-gate.py` (OBS GetSourceScreenshot hashing) ← `recording-e2e.sh` `[4c/8]`, hard-fails BEFORE recording | manual rig E2E dispatch |
| Delivery contiguity — #186/#216/#226 | `src/probe/burn_contiguity.rs:80/187/285` | 42 (`:592`) | ✅ `test`+`coverage` jobs (probe feature, included in `--all-features`) | consumed inside `src/bin/recording-verdict.rs` (the headline PASS/FAIL, `is_zero()`) | manual rig E2E dispatch, or ad-hoc `recording-verdict` on any recorded file |
| Analyzed-span duration floor — #373 | `src/recording_span_gate.rs:29/41/60` | 6 (`:69`) | ✅ `test`+`coverage` jobs | called from `src/bin/recording-verdict.rs:476-483,2384-2421` (ANDed into the headline so a collapsed optical read can't vacuously pass) | same as above |
| Zero-loss restart survival — #109 | `src/zero_loss_restart_survival.rs:128 classify` | 11 (`:162`) | ✅ `test`+`coverage` jobs | `src/bin/zero-loss-restart-gate.rs` ← `recording-e2e.sh` optional `ZERO_LOSS_RESTART_GATE=1` step (OFF by default — opt-in, brackets a real OBS/PC restart) | manual rig E2E dispatch with the env flag set |
| A/V-sync restart survival — #137 | `src/av_restart_sync.rs:139 classify` | 13 (`:170`) | ✅ `test`+`coverage` jobs | `src/bin/av-restart-sync-gate.rs` ← `recording-e2e.sh` optional `AV_RESTART_GATE=1` step (OFF by default, brackets a real OBS restart) | manual rig E2E dispatch with the env flag set |
| 4-camera mutual phase-sync offsets — #286 | `src/phase_sync.rs:69 compute_phase_sync_offsets` | 9 (`:100`) | ✅ `test`+`coverage` jobs | `src/bin/phase-sync-gate.rs` ← `scripts/phase_sync_calibrate.py` (`compute_phase_sync_offsets` shells out via stdin/stdout JSON, #438) — single source of truth, same shape as `render-budget-gate.rs` | operator-run `phase_sync_calibrate.py --apply` |
| Genlock arrival-jitter audit report — #272 | `src/jitter_audit.rs` (parser + summarizer, not a PASS/FAIL verdict) | 10 (`:243`) | ✅ `test`+`coverage` jobs | `src/bin/genlock-jitter-report.rs`, run ad hoc by an operator investigating reserve/loss trade-offs | N/A — this is a diagnostic/report tool, not a gate; no PASS/FAIL semantics to automate |
| Cam1 cross-recording loss reconciliation — #356 | `src/burn_reconcile.rs:39 cam1_real_drops_proven_delivered_downstream` | 4 (`:53`) | ✅ `test`+`coverage` jobs | called from `src/bin/recording-verdict.rs:2361` (re-classifies a proven-delivered cam1 "loss" as burn-unreadable rather than a real drop) | manual rig E2E dispatch, or ad-hoc verdict merge |
| Broadcast-OBS liveness/wedge — #391 | `src/obs_watchdog.rs:122 classify` | 15 (`:197`) | ✅ `test`+`coverage` jobs | `src/bin/obs-watchdog-gate.rs` ← `scripts/obs-liveness-probe.py`; independently scheduled/standing watchdog (NOT part of `recording-e2e.sh`) | runs continuously as its own scheduled watchdog, not gated to a push/E2E |
| NDI sender re-announce trigger — #297 | `src/reannounce.rs:127 should_reannounce` | 4 (`:205`) | ✅ `test`+`coverage` jobs | called directly from `src/ndi.rs` inside the live capture loop — exercised on every appliance run, not a separate "gate" | N/A — runtime resilience behavior, not a pass/fail verdict |

Every kernel above is a **crate-root module with no `#[cfg(feature = "probe")]` /
`#[cfg(target_os = "linux")]` gate** (verified in `src/lib.rs:12-137`) — i.e. it always
compiles, on any target, under default features. The probe-gated kernel
(`burn_contiguity`) is under `--all-features`, which `ci.yml`'s `test`/`coverage` jobs
already pass. Confirmed directly against a real CI run's log
(`gh run view 28638701425 --log`): all 1600 nextest cases ran, including every
`<kernel>::tests::*` case and every `harness_<gate>_gate` integration-test binary that
invokes the real compiled `*-gate` CLI (`harness_render_budget_gate`,
`harness_frozen_camera_gate`, `harness_av_restart_sync_gate`,
`harness_zero_loss_restart_gate`, `harness_obs_liveness_watchdog`,
`harness_phase_sync_gate`, …) and asserts `scripts/recording-e2e.sh` still wires each one
(`phase-sync-gate` is the one exception — `phase_sync_calibrate.py` is a standalone
operator-run calibration tool, not part of the `recording-e2e.sh` E2E chain, so its
harness test proves CLI-boundary parity with the kernel instead of a wiring guard).

**Conclusion: Tier A is complete.** No kernel named in the EPIC has unit tests excluded
from CI, no gate binary is unbuilt or unwired, and no wiring-guard test is missing. There
is no mechanical "test not compiled" gap to fix — see "Why no code gate was wired" below.

## The real gap — Tier B (unchanged from the EPIC body)

`full-path-e2e.yml` is `workflow_dispatch`-only. It does not run on push, PR, merge, or a
schedule. This means:

- A regression in the *decision logic* (e.g. a wrong threshold, an off-by-one in
  `burn_contiguity`) is caught automatically (Tier A, every push).
- A regression in the *actual system* the decision logic measures (a burn choking render
  fps, a camera losing frames, an A/V desync introduced by an OBS/DistroAV update) is
  caught ONLY when a human dispatches the rig E2E. This is exactly the 2026-07-02
  incident the EPIC opens with — the kernel-level tests all still pass because the bug
  was in `vendor/distroav`, not in any of the kernels above.

Turning this into an automatic gate (merge-to-main and/or nightly, per the EPIC's Tier-B
plan) requires the self-hosted `camera-lan` runner to safely reroute + restore
strih/stream's live OBS program on every trigger — this is infrastructure/scheduling
work, not a source change, and is explicitly called out in the EPIC as the standing
follow-up. **Not attempted in this pass** — automating a rig-disrupting trigger without
the operator's sign-off on timing/frequency is exactly the kind of change this audit was
told to be cautious about (`no new blocking CI gate that could break the pipeline for
everyone`), and it is supervisor/operator-driven work per the project's existing
decision (`full-path-e2e.yml`'s own header: "operator is the guard").

## Findings filed as follow-up work — now closed

- **#438 (closed)** — `phase_sync.rs` / `phase_sync_calibrate.py` were two independent
  implementations of the same math with no shared golden-vector test and no CLI wrapper
  tying them together (the one kernel in the table above without a thin `*-gate` Rust
  binary as the single source of truth). Fixed by adding `src/bin/phase-sync-gate.rs`
  (mirroring `render-budget-gate.rs`'s stdin/stdout-JSON shape) and changing
  `phase_sync_calibrate.compute_phase_sync_offsets` to shell out to it instead of
  reimplementing the formula — there is now exactly ONE implementation, so the two can
  never silently drift. `tests/harness_phase_sync_gate.rs` proves the compiled binary
  reproduces the kernel's own unit-test vectors exactly.

## Why no code gate was wired in this pass

The task was: wire in a gap ONLY if a landed kernel's tests are not actually run in CI
(an excluded `[[bin]]`, a test file not compiled, a module gated out). The audit above
found none — every kernel, every gate binary, and every wiring-guard test already runs
automatically on every push (confirmed against a real CI log, not just static reading).
The standing gap is Tier B (rig-automation infra), which is out of scope for a "safe,
non-blocking code wiring" fix and is already tracked by this EPIC itself.
