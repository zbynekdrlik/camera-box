---
paths:
  - "src/switch_latency.rs"
  - "src/delivery_spread_gate.rs"
---

# Changing `SPREAD_THRESHOLD_MS` — the SHARED constant + its full lock-step consumer list (#1120)

`SPREAD_THRESHOLD_MS` (`src/switch_latency.rs`) is the cross-camera latency-spread bound
(`max(p50) − min(p50) > bound` = FAIL). It started at #624's 16ms half-frame; issue 1120
recalibrated it to **24ms** (honest margin above the CAM1 ShadowCast grabber residual, issue 1110);
issue 1121 is the re-tighten follow-up (walk it back toward 16.0 from fresh green data after the
grabber SWAP, per `window-gate-tolerance-walkdown.md`).

## It is ONE constant feeding TWO gates — raising it moves BOTH

- **SOURCE-side** (`all_cambox_latency.spread_gate_pass`) — the cam2→camera `d_X` photon-to-capture
  spread. Folds UNCONDITIONALLY into `overall_pass` (`all_pass &= sv.pass`, recording-verdict.rs
  ~L5378). This is the BLOCKING gate #1120 fixed.
- **DELIVERY-side** (`all_cambox_delivery_latency`) — `src/delivery_spread_gate.rs` re-exports the
  same constant as `DELIVERY_SPREAD_BOUND_MS = crate::switch_latency::SPREAD_THRESHOLD_MS` (by
  design: "no second, drifting constant"). Its fold was REPORT-ONLY (issue 1033) but **#1142 flipped
  it BLOCKING** (`gates_overall_pass()==true`, owner mandate 2026-08-19 — the phase lottery, a
  good-phase 3.97ms vs a bad-phase 85ms, was hiding a real delivery-spread failure behind a green
  gate). So BOTH the source AND the delivery spread now gate at the SAME `SPREAD_THRESHOLD_MS` bound.
  Raising the shared constant now moves BOTH blocking behaviours (a delivery spread in the old..new
  band that was failing flips to pass) — its DOC comments name the bound and must move in lock-step.
  The delivery block now also surfaces `gates_overall_pass` so `e2e_discord_report.py`'s classifier
  auto-follows the seam (delivery moved from `_report_only_tripped` to `_blocking_failures`, #1142).

Both spreads are driven by the SAME CAM1 grabber, so keeping ONE constant is correct — do NOT
decouple into two constants (that reintroduces the drift the design forbids).

## The FULL consumer list — grep `16\.0\|16 ms\|16ms\|SPREAD_THRESHOLD\|> 16\|spread_gate_pass\|cross_camera_spread` across `src/` AND `tests/`, then update EVERY hit that names the old value

A naive sweep MISSES two sites (both #1120 + the review agent initially missed them):

1. `src/switch_latency.rs` — the const + module doc + const doc + the `#[cfg(test)] mod tests`
   boundary/constant tests (Tier-0, locally RED→GREEN via compile-then-run-the-binary-directly, #477).
2. `tests/delivery_spread_gate.rs` — a literal `assert_eq!(DELIVERY_SPREAD_BOUND_MS, N)` pin (Tier-0).
3. `src/delivery_spread_gate.rs` — the re-export + several "16 ms" doc comments.
4. `src/bin/recording-verdict.rs` (**probe-gated, CI-only compile — NO local type-check, verify by
   `cargo fmt --all --check` + hand-audit of the spread arithmetic**): the two log-print sites pass
   the const BY REFERENCE (auto-track, no literal), BUT its `#[cfg(test)] mod tests` fixtures pin
   spread VALUES — any fixture injecting a spread in the OLD..NEW band that asserts
   `spread_gate_pass=false`/`overall_pass=false` SILENTLY FLIPS to pass and must be widened. Also
   the "20 > 16" vacuity-guard doc + "within 16ms" test names/messages.
5. **`src/lib.rs`** — the `pub mod switch_latency` and `pub mod delivery_spread_gate`
   module-declaration COMMENTS name the bound. Easy to miss.
6. **`tests/recording_verdict_merge_gate_exit_code.rs`** — a probe-gated SUBPROCESS test proving the
   merge binary exits non-zero on a wide spread. Its fixture (50ms) fails at both old+new bounds so
   its BEHAVIOUR is unchanged, but its "16ms threshold" doc + assertion MESSAGES are stale.

The flip risk is one-directional: RAISING the bound only makes spreads PASS, so the only hazard is a
fixture that asserted a FAIL because of a spread in the (old, new] band. LOWERING it (the #1121
re-tighten) is the opposite — a PASS-asserting fixture with a spread in the [new, old) band flips to
fail. Either way, grep the band and check every PASS/FAIL-asserting fixture.

## Python report fixtures (`tests/python/fixtures/e2e_discord_report/*.json`) carry pre-computed
`spread_gate_pass` values (not re-derived) — they only need touching if a fixture's
`cross_camera_spread_ms` sits in the changed band. At the 16→24 change none did (all ≤15.3ms).
