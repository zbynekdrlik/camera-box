---
paths:
  - "src/switch_in_transient.rs"
---

# Switch-in transient classifier (issue 1144) — a fail-closed excusal that must never hide real loss

`src/switch_in_transient.rs` is a PURE crate-root classifier the imag per-segment content sweep
(`recording-verdict.rs`) calls to recognize an imag NDI-receiver **spin-up transient**: burns
missing for ~10 s right after the imag program cuts to a camera, then clean. A classified transient
is ATTRIBUTED to the cold-cut measurement (excused from `imag_overall_pass`) instead of failing the
content gate on it. The whole point is to excuse THAT one artifact without a blind boundary trim
that would also hide REAL leading loss (issue 1144 branch a, rejected).

## It is REPORT-ONLY and folds ONLY through the content seam

The excusal is `content_pass = raw_pass || sit.is_transient`, folded via
`imag_leg_gate::content_folds_into_overall_pass(imag_overall_pass)` — the CONTENT seam, whose
`content_gates_overall_pass()` is `false`. So excusing a segment changes NO blocking outcome today.
NEVER wire the excusal into the blocking presence/verification side of the #1142 split (the
whole-recording `node_verdict_for_imag` fold, or the stream sweep's own `seg.overall_pass`). The raw
per-segment `pass` stays honest in the JSON; the imag facet also carries `content_overall_pass_raw`
(the un-excused AND) so the raw signal is never lost, and every classified transient is surfaced on
its segment (`switch_in_transient: true`) AND under `cold_cut_onset.imag_switch_in_transients`
(never silently dropped).

## The discriminator is FAIL-CLOSED by construction — do not loosen it

`classify` is a CONJUNCTION; ANY criterion failing leaves the segment a content failure (the cheap
direction while report-only). The criteria (see the constants + doc in the module): cut-adjacent
window; burns present + `undecodable==0`; optical not drifting (`|avg_step-expected| <=
AVG_STEP_DEV_MAX`, a DRIFT guard only — NOT freeze protection); loss ONSET at the cut
(`first_missing - first_id <= ONSET_OFFSET_MAX_IDS`); a leading BURST (maximal prefix run within
`RECOVERY_GAP_IDS`) that is SUBSTANTIAL (`>= MIN_TRANSIENT_MISSING`, so a few real drops are NOT
excused — the #583 single-frame-drop shape stays a failure), BOUNDED (`<= MAX_TRANSIENT_SPAN_FRAC`
of the id-span, so a half-window-dead camera is NOT excused), and DENSE (`>= BURST_DENSITY_MIN`, so
a sparse leading loss is NOT excused); a clean RECOVERY (`residual <= MAX_RESIDUAL_AFTER_BURST`, so
a double-burst / non-recovering loss is NOT excused); and the optical stuck EXPLAINED by the burn
transient (`stuck_density*span_frames <= missing*(1+STUCK_VS_MISSING_TOL)`, so an independent
optical freeze is NOT excused). The adversarial unit tests pin each of these — keep them green.

## n=1 calibration — the constants are conservative and MUST be re-validated at the flip

The thresholds were calibrated from exactly ONE real positive (verdict-276174336 CAM3 window 1) +
the 38 healthy zero-loss segments across the .611/.613/.614 baseline runs. That is thin. Before the
content gate flips blocking (issue 1144 item 2, a supervisor/rig-ops **sick-camera discrimination
run** — cam1 1/60 shutter via bkshading relay, or a deliberately-dropped imag burn), RE-VALIDATE:
the sick run's mid-window / sustained / frozen fault MUST read `is_transient == false` (not
excused). NEVER widen a constant so a red simply passes — that is the exact "calibrate on the
artifact, not the chain health" trap the imag-leg work already burned on.

## Tier-0 — a pure module RED->GREENs via a rustc replica; a small add to a BIG module uses a FOCUSED replica

The whole `probe` module (and `recording-verdict.rs`) is CI-only, so put the pure decision at the
crate root and unit-test it locally with `rustc --edition 2021 --test <copy>.rs` (no cargo — Tier-0
#557 blocks even `cargo test --no-run`). `switch_in_transient.rs` has no deps, so its replica is a
direct copy. For a SMALL addition to a LARGE existing module (e.g. `summarize_projection_leg` in the
63 KB `tear_detect.rs`, issue 1144 item 3), do NOT replicate the whole file — assemble a FOCUSED
replica with ONLY the pieces the new code touches (the consts + `tear_gate_pass` + the enum + a
minimal struct + the new fn + its tests). The RED->GREEN discipline still applies: commit the tests
+ a STUB that returns not-transient/not-backed (`[red]`, verify the positive test fails against it),
then the real impl (`[green]`). The probe-gated WIRING has no local compile — verify it with
`cargo fmt --all --check` (rustfmt parses cfg-gated files) + a hand type-audit of the field
accesses; CI is the first type check.
