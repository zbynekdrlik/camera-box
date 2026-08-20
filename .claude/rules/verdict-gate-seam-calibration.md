---
paths:
  - "src/presentation_cadence.rs"
  - "src/optical_floor.rs"
  - "src/e2e_latency_gate.rs"
  - "src/av_window.rs"
  - "src/lipsync_cross_check.rs"
  - "src/self_heal_attribution.rs"
  - "src/dup_cadence.rs"
  - "src/burn_hold.rs"
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

## 7. A CONTENT/pixel-based metric belongs in the OFFLINE recording-verdict pass (#1088)

A per-frame metric that needs PIXELS (a content hash, a luma stat, a colour read) has a THIRD home
beyond the two obvious live taps — and it dominates both. cam-box-side hashing is a RIG WRITE
(supervised deploy, out of scope for a dev1-side read-only measure); receiver-side (strih/stream)
hashing PERTURBS a live broadcast render. But `probe::recording::read_frames` ALREADY streams every
gray8 frame of the offline recording (dev1 CI, once per verdict) — so compute the pixel metric THERE.
It is neither a rig write nor a live-box perturbation, and it matches "sample on the worker" exactly.
`hash_recording_frames` (a SEPARATE luma-only ffmpeg pass) is the pattern; index the returned vec by
`RecordingFrame::frame_index` and slice it per cambox window with `partition_frames_by_window` (the
same attribution the sweep uses — no `recording_segments.rs` churn). Before writing a new pixel hash,
check `dupe_decimation::dupe_content_hash` (#889) — a proven row-sampled FNV-1a already exists for the
OPPOSITE (over-rate) phenomenon (investigate-existing-first).

**TRAP (issue 1101) — "the offline pass" that computes `all_cambox_continuity` is the dev1 MERGE,
which has NO recording. A pixel/content metric gated behind `stream_rec` (the fused `--stream` path)
is STRUCTURALLY UNREACHABLE in the production gate and produces ZERO data.** The production ALL_CAMBOX
gate runs `VERDICT_ON_STREAM=1` (the default, `recording-e2e.sh`): each Windows box `--extract-partial`s
its LOCAL recording (the partial carries per-frame ticks/`gen_ts`/`frame_index` — NOT the recording,
NOT content hashes), and the dev1 `--merge-partials` step COMPUTES `all_cambox_continuity` with no
recording on dev1. So `stream_rec` (set only from `--stream <recording>`) is `None` in the merge, and
any block written as `match stream_rec.as_deref() { Some(rec) => hash_recording_frames(rec) ... None =>
skip }` takes the skip arm — it fires ONLY in the legacy `VERDICT_ON_STREAM=0` fused path, which the
rig does not use. The issue-1088 dup-cadence surface shipped this way and appeared in 0 of 81 retained
`verdict-*.json` (healthy runs included) — not a thin sample, structurally none. So §7's "compute it
in the offline pass THERE" is only half the pattern: `read_frames`/`hash_recording_frames` do decode
every frame, but the pass that actually decodes the recording in production is the ON-BOX
`--extract-partial`, NOT the dev1 merge. A content metric that must reach `all_cambox_continuity` has
to be computed on the box during extract and CARRIED in the partial's schema (like the per-frame ticks
already are), so the merge can slice it per window without the recording. Verify a new content/pixel
term actually emits on a real production merge-gate `verdict-*.json` BEFORE building a calibration or a
LIVE flip on it — a report-only term nobody can see is not "shipped", it is dormant.

## 8. GATE on the DISCRIMINATED signal, never the raw metric (#1088 review)

When the metric builds a multi-condition classification (e.g. #1088 `duplication_masked` = rate ∧
regularity ∧ coverage), the GATE must key on the DISCRIMINATED result, not the raw underlying number.
#1088's first cut bounded the raw worst `duplicate_fraction` across ALL windows — which would
double-jeopardy a localized freeze (high raw fraction, coverage-vetoed → not the target defect,
already `frozen_leg`'s domain) the moment `gates_overall_pass()` flips LIVE. Fix: a pure
`worst_masked_duplicate_fraction` that folds only the windows the classifier flagged, and the gate fn
doc STATES it must receive the discriminated value. Report BOTH (the discriminated gate signal + the
raw value, informational) in the JSON. General rule: if you built a veto to exclude a sibling gate's
domain, feed the gate the post-veto signal or the veto was pointless.

## 9. The additive-separate-pass vs return-type-churn tradeoff for a CI-only probe fn (#1088)

`recording-verdict.rs` + `src/probe/*` have NO local compile path (CI is first compile). When a new
metric needs data from a widely-called probe fn (`analyze_recording_*` has ~15 callers), threading it
through that fn's RETURN TYPE churns every caller — all CI-first-compile, high blind-change risk. An
ADDITIVE separate pass (a new fn with zero existing-call-site edits) is the lower-risk trade even at
the cost of an extra offline decode, for a report-only first cut. Note the fold-into-main-pass
optimization as a follow-up (gated on the metric proving its value), don't do it blind.

## 10. Two ZERO-churn plumbing moves for a merge-carried metric (#1112)

§7 says a content/pixel metric must be computed ON the box during `--extract-partial` and CARRIED to
the dev1 merge. Two techniques land that carry WITHOUT the blind CI-first-compile churn §9 warns
about — both proven wiring the #1088 dup-cadence surface into the production merge gate (#1112):

- **Carry a per-box value via a `RecordingPartial` builder — ZERO caller churn.** `RecordingPartial`
  is constructed ONLY through `from_frames(...)` + chained `.with_colour()`/`.with_av_sync()`
  builders (never a struct literal outside `from_frames`). Add an optional field
  (`content_hashes: Option<Vec<u64>>`, `#[serde(default)]`) + a `with_content_hashes` builder;
  `from_frames` defaults it to `None`, so every existing `from_frames` call site is unchanged and the
  new value is opt-in. Bump `PARTIAL_SCHEMA_VERSION` per the file's convention (grep tests for any
  hardcoded old-version JSON literal — the strict `from_json` check rejects a stale version). The
  struct derives `PartialEq` not `Eq`, so a `Vec`/`f64`-bearing field is fine.
- **Thread a new value into a many-call-site verdict fn via a same-signature WRAPPER — ZERO
  call-site churn.** `build_and_print_verdict` has ~37 call sites (mostly probe-gated tests, all
  CI-first-compile). Adding a param directly = 37 blind edits. Instead: rename the body to
  `build_and_print_verdict_with_<x>(..., new_param)` and keep a thin `build_and_print_verdict`
  wrapper with the ORIGINAL signature that delegates with the neutral default (`None`). Only the ONE
  new caller (the merge) calls the `_with_<x>` form; every existing caller (tests + the fused main)
  is byte-for-byte unchanged. This is the call-site-churn analogue of §9's return-type-churn trade.

Both keep the risk surface to the FEW lines that genuinely change, which is the whole game on a file
with no local type check. Verify with `cargo fmt --all --check` (parses the probe files) + a hand
type-audit of the new arg types and EVERY call site's arity (`grep -n 'build_and_print_verdict'` — a
stray 8-arg call to the 9-arg fn is a CI-only red).

## 11. Flipping a report-only seam BLOCKING, or adding a new blocking gate (#1142 lessons)

Three hard-won patterns from flipping three seams blocking at once (imag leg + cadence uniformity +
delivery spread):

### RED→GREEN for a PURE crate-root gate module under #557 — `rustc --test` scratch, NEVER cargo
`#557` (2026-08-18) BANS every compiling cargo shape locally — including `cargo test --no-run`, so
the "compile with --no-run then run the binary directly" pattern in CLAUDE.md's Local Build Policy is
STALE. A pure crate-root gate module (`presentation_cadence.rs`, `imag_leg_gate.rs`,
`delivery_spread_gate.rs`, `partial_schema_gate.rs`) is self-contained enough to compile STANDALONE:
```bash
# strip a `serde::Serialize` derive the module carries (serde isn't linkable standalone; the gate
# tests never serialize), then compile+run as a --test binary:
sed 's/, serde::Serialize)]/)]/' src/presentation_cadence.rs > /tmp/pc.rs
rustc --edition 2021 --test /tmp/pc.rs -o /tmp/pc_t && /tmp/pc_t
```
For a module with a `crate::` cross-ref (e.g. `delivery_spread_gate` re-exports
`crate::switch_latency::SPREAD_THRESHOLD_MS`), stub the referenced const in a tiny scratch that
inlines just the pure fns + the tests. This gives a GENUINE observable RED (seam still false) →
GREEN (flip) locally, which the probe-gated `recording-verdict.rs` consumer can never (CI-first).
`cargo fmt --all --check` is still the syntax proof for the probe-gated wiring (it parses cfg-gated
files); a python replication over `/tmp/recording-e2e-*/verdict-*.json` proves the FOLD flip's real
effect end-to-end (model each flipped term's pass + whether it now folds; include a synthetic clean
verdict to prove the gates are not UNCONDITIONALLY red).

### A NEW blocking fold breaks every unit test that builds a verdict WITHOUT that node — scope it with a `--require-<x>` flag
The imag_leg_verified honesty flip (a missing imag leg REDs) was UNCONDITIONAL at first and reded
~10 in-process/subprocess verdict tests that build a verdict with `imag: None` (isolated
strih/stream/cam scenarios) + `merge_gate_exit_code` subprocess controls. Fix: a CLI flag
(`--require-imag-leg`, `#[arg(long, default_value_t = false)]`) that gates the fold; the PRODUCTION
path sets it (`recording-e2e.sh` ALL_CAMBOX `[8/8d]` merge, appended via the #675 `MERGE_ARGS+=(...)`
pattern — NOT the strih+stream-only zero-loss-restart merge), every unit test defaults off. Verify
the recording-e2e.sh edit anchor-safe: `bash -n` + `shellcheck` + the OLD-vs-NEW literal
occurrence-count sweep (`camera-active-set.md`'s Tier-0 net). Pin the flag's A/B with a probe-gated
test reusing a KNOWN-clean imag-None fixture (`window_cam2`): off=pass, on=red, on+offline-ack=pass.

### Gate cadence uniformity on `derived_uniform_fraction`, NOT raw `uniform_fraction`
The raw `uniform_fraction` (fraction of deltas == the caller's `expected_step`) FALSE-REDS a clean
window whose per-frame step MODE differs from `expected_step` — several synthetic switch-schedule
fixtures advance the tick +1 under `--switch-expected-step 2`, so raw reads 0.0 on a perfectly clean
window (`derived_uniform_fraction`, the #726 mode-based field, reads 1.0). On the REAL rig the two are
EQUAL (mode IS 2; verified across every mined verdict, worst 0.67–0.78 on both), so gating on
`derived` REDs the sick rig IDENTICALLY without the synthetic false-red. General rule: when a cadence
metric offers a caller-`expected_step` field AND a data-mode-`derived` field, gate on the derived
one and surface the raw as diagnostic.

### Owner-mandated RED-on-current-rig OVERRIDES gates-green-first (§3)
The standard "a bound that would fail a recent green run is wrong" is REVERSED when the owner
declares the green runs FALSELY green (hiding visual degradation). #1142's cadence-uniformity 0.95
floor is RED on today's 0.67–0.78 rig BY DESIGN. Document the deviation loudly in the seam doc + the
ticket; recalibrate from the first genuinely-clean post-fix run as a named TODO.
