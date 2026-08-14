---
paths:
  - "src/presentation_cadence.rs"
  - "src/optical_floor.rs"
  - "src/e2e_latency_gate.rs"
  - "src/av_window.rs"
  - "src/lipsync_cross_check.rs"
  - "src/self_heal_attribution.rs"
---

# Calibrating + wiring a NEW verdict gate seam (the one-line-restorable `gates_overall_pass()` pattern)

Several fused-verdict terms follow ONE convention: a PURE decision module at the crate root
(`optical_floor.rs`, `e2e_latency_gate.rs`, `presentation_cadence.rs`, `av_window.rs`,
`lipsync_cross_check.rs`, `self_heal_attribution.rs`) exposing `const <THRESHOLD>` +
`fn <x>_gate_pass(...) -> bool` + `fn gates_overall_pass() -> bool`, and a THIN consumer in the
probe-gated `recording-verdict.rs` that computes the measured value, emits a JSON term, and folds
`all_pass &= gate_pass || !gates_overall_pass();`. When adding the NEXT such gate (e.g. #1036 added
the `presentation_cadence` paired-judder gate), follow this playbook — it is the shape that reviews
cleanly.

## 1. Calibrate from LOCAL verdict JSONs — no fresh E2E run
`/tmp/recording-e2e-*/verdict-*.json` on dev1 already carry every per-window metric under
`all_cambox_continuity.segments[N].<field>` (and top-level nodes like `latency`). Mine the
distribution directly (`python3` + `glob`), filtering to `overall_pass == true` runs for the
healthy baseline (mirrors `phase-sync-calibrator-testing.md`'s #893 "reuse a green run's verdict
JSON" — a full hardware E2E is ~1h and collides with the `full-path-e2e.yml` concurrency group).
Report the per-run baseline table on the ticket as part of the design comment.

## 2. Pick the signal with the TIGHTEST GREEN CEILING, not a noisy one
The #1036 lesson: the metric may expose several fields; gate on the one whose green-run
distribution has a tight ceiling far below the pathology. `paired_fraction` (the specific
15fps-judder signature) had green max **0.00473** vs pathology ~0.966 — a ~200x separation —
while `evenness_score` (0.655–0.994) and `duplicate_fraction` (spikes to 0.053) were far too noisy
to gate on. A green run sitting at 0.655 means no gate-able threshold exists on that field.

## 3. Threshold = honest margin that passes EVERY recent green run (gates-green-first)
A bound that would have failed a recent green run is wrong (the standing philosophy). #1036 set
`0.05` = 10.6x the worst green window and ~19x below the pathology. State the margin math on the
ticket.

## 4. Single per-window-max term vs a run-wide second term — RATE vs COUNT
- A per-window **RATE** metric (e.g. `paired_fraction`) needs only a per-window-MAX term: the
  pathology saturates every affected window, so there is no "spread the budget across windows"
  loophole → one term is honest (#1036).
- A per-window **COUNT** metric (e.g. `optical_floor`'s `undecodable`) needs BOTH a per-window AND
  a run-wide summed term — else N windows x a per-window allowance tolerates MORE than the
  regression the gate was written to catch (`optical_floor.rs` "Two terms, not one").

## 5. LIVE vs report-only — and the cam1-grabber (issue 909) test
`gates_overall_pass()` returns `true` (LIVE, folds into `overall_pass`) ONLY if the bound passes
every green run with margin AND is not tripped by an unrelated hardware fault. The cam1 ShadowCast
grabber defect (issue 909) is why `optical_floor`/`frozen_leg`/`av_window` are report-only. Verify
your metric empirically survives it before going LIVE: `presentation_cadence` is LIVE because the
worst green `paired_fraction` (0.00473) INCLUDES CAM1 windows that carry the defect — do NOT argue
LIVE-safety from a mechanical "that defect can't manufacture this signature" claim (a capture-side
drop next to a duplicate CAN complete a paired event — #1036 review finding); argue it from the
empirical margin.

## 6. Gotchas
- The pure module compiles on DEFAULT features (Tier-0 unit-testable); the probe consumer
  (`recording-verdict.rs`, `required-features=["probe"]`) is CI-first-compile — hand-type-check the
  wiring, it never compiles locally.
- A multi-line string inside `serde_json::json!({...})` or `println!` MUST use `\` line-continuation
  escapes, or it embeds literal whitespace runs; `cargo fmt` does not reformat inside string
  literals, so a broken one passes fmt silently.
- Mirror an existing seam's arms EXACTLY (`e2e_latency_gate::cam_strih_latency_gate_pass` is the
  freshest template) — incl. the `None`-measurement arm. One justified divergence: a `None`
  measured value is FAIL for latency (a missing sample is anomalous + unguarded elsewhere) but PASS
  for cadence (a zero-cadence run is already hard-failed by copies/gaps/undecodable — no
  double-jeopardy). State which you chose and why in the fn doc.
