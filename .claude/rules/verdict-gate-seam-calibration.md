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
  - "src/own_burn_absent.rs"
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

**SUPERSEDED (#1250): the uniformity gate now reads `beat_corrected_uniform_fraction`, and the mined
`cadence_uniformity_gate.worst_uniform_fraction` key CHANGED SEMANTICS across that boundary.** #1250
found the "0.67–0.78 sick rig" was mostly a benign sampling-phase BEAT (balanced 1↔3 net-zero pairs
around the mode), not FIFO churn — `derived` counted each 1 and 3 as non-uniform. `beat_corrected_
uniform_fraction` collapses the ±1-refresh pair `min(count(mode-1), count(mode+1))` back to uniform,
so the gated worst is 0.916/0.947 on the two mined post-fix runs (PASS 0.90) instead of 0.566/0.769
derived. The verdict JSON `worst_uniform_fraction` key now carries the BEAT-CORRECTED value (pre-#1250
runs carry the derived value under the SAME key); `worst_derived_uniform_fraction` (pre-beat) +
`worst_raw_uniform_fraction` are diagnostics. **Cross-era mining caveat:** when comparing
`worst_uniform_fraction` across historical verdicts, a run from before the #1250 deploy carries the
derived reading and a run after carries the beat-corrected reading under the identical key — read
`worst_derived_uniform_fraction` (present only post-#1250) to compare apples-to-apples, or segregate
by the #1250 deploy time. The "gate on derived, not raw" rule above generalizes: gate on the
BEAT-CORRECTED field, surface derived + raw as diagnostics.

**Calibration lessons from #1250 (reusable for any beat-aware / count-collapse metric):**
- **When a ticket's fix-SHAPE prose contradicts its own DATA-CHECK number, reconstruct the REAL
  ORDERED per-frame data and gate toward the acceptance number, not the prose.** #1250's ticket said
  collapse "ADJACENT" complementary pairs, but its own data-check `(479+360)/846 ≈ 0.99` is a COUNT
  collapse. The per-segment histogram in the verdict JSON is order-BLIND, so it can't tell the two
  apart — you must rebuild the ordered tick sequence from the `stream-partial-*.json` `frames[].tick`
  (the STREAM partial reproduces the per-cambox `presentation_cadence` exactly; the STRIH partial does
  NOT — it is a different tap with a different beat phase). That reconstruction showed only 137/180
  ones are strictly adjacent to a 3 → strict-adjacent yields 0.864 (RED, fails the ticket's own
  acceptance) while count-based yields the ticket's 0.9917. Document the resolution + flag it.
- **Bound a beat collapse to the PHYSICAL ±1-refresh pair `(mode-1, mode+1)`, not the general
  `x+y == 2*mode` family.** A one-refresh-early/late capture is the exact beat mechanism and the only
  complementary pair the rig produces (mode 2 → (1,3)); a ±2+-refresh jump is a bigger artifact that
  SHOULD stay visible. Behavior-identical on-rig, strictly safer off-mode (the #1250 review finding).

### Owner-mandated RED-on-current-rig OVERRIDES gates-green-first (§3)
The standard "a bound that would fail a recent green run is wrong" is REVERSED when the owner
declares the green runs FALSELY green (hiding visual degradation). #1142's cadence-uniformity 0.95
floor is RED on today's 0.67–0.78 rig BY DESIGN. Document the deviation loudly in the seam doc + the
ticket; recalibrate from the first genuinely-clean post-fix run as a named TODO.
## 12. A content/pixel metric needs a VIABILITY cross-check before any threshold calibration (#1101)

§7's reachability trap (does the term EMIT?) is only the first gate. The SECOND, learned calibrating
the #1088 dup-cadence surface (#1101): a content/pixel metric can emit real per-window data that is
STRUCTURALLY DEGENERATE — an all-zero distribution that means "signal blind", not "rig clean". The
#1088 surface hashes the STREAM box's LOSSY `.mp4` recording with a BYTE-EXACT row-sampled FNV-1a;
byte-exact frame identity does not survive lossy encode+decode, so the content hash observes almost
none of the duplication that is genuinely present. Mined across 18 production verdicts + their
`stream-partial-*.json`: **147 tick-proven copies (a repeated Vernier `tick` = a byte-duplicate
camera frame, exactly what `copies` counts) produced only 2 content-hash duplicates ≈ 1.4%.**

Lessons for the NEXT content/pixel gate:

- **Calibrating a LIVE threshold on an all-zero green distribution is NOT automatically safe.** For a
  count/rate metric, `green_max == 0` looks like the tightest possible ceiling — but if the signal is
  blind (can never produce a positive value even under the real defect), a LIVE gate is a permanent
  FALSE-GREEN: it passes the exact pathology it was written to catch. A gate that can never fire is
  worse than no gate. This is the OPPOSITE failure mode from §2's "green sits at 0.655, no threshold
  exists" — here green sits at exactly 0.0 for the wrong reason.

- **Cross-check the metric against an INDEPENDENT ground-truth signal that the SAME recording already
  carries.** For dup-cadence that is the Vernier tick: a repeated tick is a byte-duplicate camera
  frame (`copies`), so the content-hash SHOULD register a duplicate on those exact frames. It doesn't
  → the signal is blind. The self-diagnosis is a pure crate-root classifier
  (`dup_cadence::copy_observation` / `signal_viability` / `signal_promotable`, #1101): per window,
  cross-check content-hash duplicates against tick-copies over the SAME frames; `Blind` when ≥
  `MIN_TICK_COPIES_FOR_VIABILITY` copies occurred but the hash observed < `COPY_OBSERVATION_RATE_MIN`
  of them. Emit `signal_viability`/`signal_promotable` in the report-only node so an all-zero
  `duplicate_fraction` can never be mistaken for a promotable green.

- **Make promotion-readiness a COMPUTED, machine-checked property, not a guess.** The LIVE-flip
  precondition is `signal_promotable(signal_viability(..)) == true` on real runs — the seam's
  `gates_overall_pass()` flip is gated on that, not merely on a calibrated bound. On the current lossy
  tap it reads `blind`, so the flip stays blocked; the signal FIX (a codec-tolerant near-duplicate
  hash, or re-tapping a lossless stage — the strih partial carries no content_hashes today) is a
  cross-cutting follow-up gated on a real 50→60-pulldown run to validate.

- **A hardware SEGREGATION step (per the dispatch/`window-gate-tolerance-walkdown.md`) is MOOT when
  the signal is dead.** cam1's known ~61.5fps grabber wobble (issue 1145) could in principle inflate a
  dup-cadence reading, but with the signal blind, cam1's windows read 0.0 like every other — there is
  nothing to segregate. Confirm a signal is VIABLE before spending effort segregating known-faulty
  boxes out of its distribution.

## 13. FIXING a blind content/pixel signal — codec-tolerant near-duplicate, validated on retained real pixels (#1166)

§12 diagnosed the #1088 dup-cadence content hash as `Blind` on the lossy stream tap. #1166 FIXED it —
the reusable playbook for turning a blind pixel signal Viable:

- **A byte-exact per-frame HASH can never survive a lossy encode; a codec-tolerant NEAR-duplicate
  measure can.** Replace the exact-equality test with a per-pair row-sampled mean-abs-luma-DIFFERENCE
  (MAD) to the recording predecessor, thresholded (`NEAR_DUP_MAD_MAX`). A byte-duplicate source frame
  survives the lossy round-trip as a LOW-MAD pair (only global quantization noise); genuine motion is
  far higher. `frame_content_hash`->`frame_row_sampled_mad`; the classifier consumes an
  `Option<f64>` per-window sequence and `is_near_duplicate(mad)` instead of `hash[i]==hash[i-1]`.

- **DOWNSCALING DESTROYS the separation — sample FULL-WIDTH rows, never a thumbnail.** Measured on the
  retained diagnostic PNGs: 8×8/16×16 thumbnail MAD ranges OVERLAP copy-vs-motion (averaging washes
  out the localised motion that distinguishes a real frame from a duplicate), while FULL-resolution
  MAD separates cleanly. Full-width row-sampling (~64 of 1080 rows) keeps each sampled row at full
  horizontal resolution → it preserves the full-res separation at ~6% of the pixel cost. Consequence:
  the MAD is a PAIRWISE quantity that MUST be computed on the box between consecutive full-res decoded
  frames (`probe::recording::frame_prev_diffs`) and carried per frame — it cannot be reconstructed in
  the merge from a compact per-frame thumbnail.

- **The retained diagnostic frame PNGs (`<partial>-pixels/frame-*.png`) ARE a real-lossy validation
  corpus — use them.** They are dumped around copy/gap events, so adjacent pairs include genuine
  tick-proven copies AND genuine motion, on the ACTUAL lossy recording. Correlate each adjacent PNG
  pair's `frame_index` with the partial's per-frame `tick` (a strict-adjacent tick repeat = a
  tick-proven copy) and compute the candidate metric on the real pixels. #1166: 32 copy pairs vs 381
  motion pairs across 12 runs → MAD ≤ 10.0 observes 81% of copies at 0% motion false-positive, where
  the byte-exact hash observed 0/32. This is a genuine local RED→GREEN of the SIGNAL, not just the
  detector mechanics — embed the measured copy/motion MAD scalars as a locked crate-root fixture test.

- **The signal fix does NOT unblock the LIVE flip — the promotion gate is still DATA-gated.** The
  PNG-dump corpus is BIASED (frames near events), and the existing partials carry the OLD byte-exact
  hashes, so the full per-run `signal_viability` distribution + `DUP_RATE_PULLDOWN_MIN` recalibration
  cannot be produced from retained data — they need a FRESH green run emitting the new signal. Ship
  the fixed signal REPORT-ONLY (`gates_overall_pass()` stays `false`); the LIVE flip stays gated on
  `signal_promotable == true` on ≥2 consecutive real runs + a recalibrated bound. State this
  precisely; keep the ticket OPEN carrying the promote step (window-gate-tolerance-walkdown.md §5).

- **A near-duplicate signal (unlike byte-exact) CAN fire on a genuinely-static-but-distinct scene**
  (every consecutive pair low-MAD → high rate, regular, window-spanning → the classifier would
  mis-flag it a pulldown). Harmless while report-only, but the full-run recalibration must judge
  whether an UPPER rate bound is needed to separate a ~16.7% pulldown from ~100% static (frozen_leg's
  domain). Not a concern on the animated Vernier/burn test pattern, but note it for the calibration.

## A PRESENCE seam is the same shape but skips §1–§3 (calibration) — `own_burn_absent` (issue 1247)

Not every new `gates_overall_pass()` seam gates a CALIBRATED THRESHOLD mined from the verdict-JSON
distribution. `src/own_burn_absent.rs` (issue 1247) is a PRESENCE seam: it flags a SCHEDULED cambox
whose OWN digital burn was entirely absent from the recording (`full_chain.burn_ids_present.<cam> ==
0`) — the issue-1246 symptom where a cam's leg is live but served by production `camera-box.service`
(no digital burn), so the per-segment optical-tick verdict can overstate it as a clean pass. It keys
off the `--switch-schedule` DEPLOYED set (`SwitchWindow.cambox`), NOT a mined metric distribution, so
§1–§3 (calibrate-from-JSONs, tightest-green-ceiling, honest-margin-threshold) DO NOT APPLY — there is
no threshold, only present-vs-absent. Everything else is identical: a pure crate-root decision (no
probe deps, std-only, unit-tests Tier-0 on default features — the pure-module + shell-replica pattern
below), a thin consumer in `recording-verdict.rs` computing the counts from the same `camN_ids.len()`
that build `full_chain.burn_ids_present` (so the gate can never disagree with that field), a JSON term
under `full_chain.own_burn_absent_gate`, and the report-only fold `all_pass &= gate_pass ||
!gates_overall_pass();` with `gates_overall_pass()` hardcoded `false` (the LIVE `[7b/8]` run-integrity
check already fails such a run; a one-line flip makes it blocking). The e2e_discord_report branches
use `is True` / `is not True` on the seam's serialized `gates_overall_pass` so a future flip
auto-routes report-only → blocking without double-counting (the same convention as
`e2e-discord-report.md`).

**Lesson (CYCLE-6 review 🔵) — a verdict-JSON field name MUST describe what it ACTUALLY carries.**
The first cut named the field `scheduled_cams` but it held the ASSESSED subset (scheduled ∩ cams that
HAVE a burn-count key) — a scheduled-but-unassessed cambox (e.g. `imag`, measured by its own leg
gate) silently vanished from a field named "scheduled". Because this whole gate family's PURPOSE is
preventing durable-artifact misreads, a misleadingly-named field in the artifact is itself the bug
it exists to catch. Renamed to `assessed_cams` + a clarifying `note` clause. General rule for any new
verdict-JSON key: name it for the set it actually contains, and if that set is a filtered subset, say
so in the `note`.

## 14. A CONTENT/pixel gate can be VIABLE yet still NOT promotable — check redundancy + the upper bound (#1101 / #1166 promote cycle)

§13 turned the #1088 dup-cadence content signal from `Blind` to `Viable` (the codec-tolerant MAD
fix). When the promote cycle finally had real green-run data (the 2026-09-01 green 7-cam series:
`signal_viability="viable"`, `signal_promotable=true`, and the signal correctly caught a real
2026-08-25 30→60 duplication — CAM3 `duplicate_fraction`≈0.50, `gap_mean`≈2, `coverage`≈0.999,
`inferred_source_fps`≈30), the LIVE flip STILL did not happen. Two calibration gaps that a bare
"viable ⇒ flip" plan misses, both learned mining all 44 post-fix verdicts:

- **A tick-based `copies` gate SUBSUMES a content near-dup gate for PURE duplication — so calibrate
  where the content gate is UNIQUE, not where it is redundant.** The dup-cadence classification floor
  `DUP_RATE_PULLDOWN_MIN = 0.10` = ~85 near-dup pairs in an ~845-frame window; the per-segment
  `copies` gate fails at `copies_gaps_tolerance` = 3–5. For a PURE-duplication pulldown a duplicated
  frame is BOTH a content near-dup AND a repeated Vernier tick, so ~85 near-dups ⇒ ~85 tick-copies ≫
  tolerance → `copies` already hard-fails it (the only masked event in the corpus, the 08-25 30→60,
  was independently failed by `copies` + `cadence_uniformity_gate` + `cadence_judder_gate`). The
  content gate is UNIQUE only where near-dups do NOT coincide with tick-copies — a content freeze with
  a LIVE tick, or a blended/interpolated pulldown (`content_near_dup_pairs` ≫ `copies_observed_by_content`;
  e.g. run 2034201093 CAM7 had 59 near-dups vs 6 tick-copies). Verify from the JSON whether a masked
  window's near-dups are tick-copies (redundant with `copies`) or not (the real unique value) BEFORE
  arguing a LIVE flip adds coverage. And note: the floor cannot simply be lowered to add unique
  coverage — the green worst raw fraction (~0.007 ≈ the copies tolerance) sits right where lowering
  would start false-flagging green windows on the rate floor alone.

- **A near-dup signal fires on a FREEZE too — an upper RATE bound (or a report-only hold) is needed
  before LIVE, else you re-expose the frozen_leg cam1-grabber (issue 909) false-positive class.** The
  `DUP_COVERAGE_MIN` veto excludes a LOCALIZED freeze, but a window-SPANNING content freeze
  (`duplicate_fraction`→1.0, regular, spanning) is masked exactly like a pulldown — there is no upper
  bound separating a ~0.167 pulldown from a ~1.0 static freeze (frozen_leg's domain). Promoting LIVE
  without it means dup-cadence hard-fails a spanning freeze, which is precisely why the freeze-adjacent
  gates (`frozen_leg`/`optical_floor`/`av_window`) stay report-only (§5). With no spanning-freeze
  datapoint to calibrate the boundary (0/44 green runs masked, incl. all cam1 windows), the honest
  disposition is NO-FLIP-WITH-DATA: keep report-only (observability is retained in the verdict JSON),
  correct the seam's own now-stale doc (a `gates_overall_pass()` doc that still cites a since-satisfied
  precondition MISLEADS the next worker into a naive flip), and record the re-entry condition (viable
  on ≥2 green runs + `worst_masked` null on green + the upper-bound resolved on real spanning data or
  frozen_leg promoted first + any owner supersede direction, e.g. issue 1196's aux-tick-pair signal,
  resolved). "Viable" is necessary, not sufficient, for a content/pixel LIVE flip.

## Flipping a report-only seam LIVE: re-audit for a LATENT report-only false-positive that becomes a live false-FAIL (issue 1196)

A gate can ride REPORT-ONLY for weeks producing a wrong reading on a scene shape that just does not
occur in the recent green window — and the moment you flip `gates_overall_pass()` LIVE that latent
wrong reading turns into a false-FAIL on the next run of that shape (the #1127 "❌ on a passing run"
trap the owner hates). The report-only era's all-green history is NOT proof the gate is safe LIVE —
it only proves the WRONG shape was absent. Before any report-only→LIVE flip:

1. **Re-run the LIVE gate over EVERY historical verdict, incl. the runs the calibration EXCLUDED**
   (multi-tile, dead-painter, convergence-transient — `window-gate-tolerance-walkdown.md`). If any
   would fail, that shape is a live false-fail waiting to happen. Issue 1196: the tear gate's own
   module doc admitted a count-2 multi-tile skew residual scores as a single-source "tear", but the
   green window was all single-tile (`multi_path_suspect_fraction` 0.0), so it read clean report-only
   — the LIVE gate would have failed 4/10 windows on the real multi-tile run 1859005342.
2. **The LIVE gate must EXCLUDE the same UNSCOREABLE class the promotion property excludes.** If
   `signal_promotable`/`window_promotable` already guards on a "this window is untrustworthy" flag
   (a suspect fraction, a coverage floor), the gate (`*_gate_pass`) MUST carry the SAME guard — a
   promotion property that's stricter than the live gate is a bug: the gate fails windows the
   promotion property already declared unscoreable.
3. **A noise floor that is a COUNT (N frames irrespective of window length — an occasional 1–3-frame
   artifact) needs a COUNT term, not just a rate ceiling** (§4). A rate-only ceiling calibrated on
   long windows false-fails a short window with the same few artifact frames. Issue 1196's tear gate
   is TWO-TERM: `Observed AND single-tile AND tear_fraction > rate_ceiling AND tear_frames >=
   count_floor`.
4. **Verify a verdict field's SEMANTICS from the per-frame data before building the flip on its
   NAME.** A grading that reads `max_spread` as "the primary-band span" when the field is actually the
   primary∪aux UNION span can invert the operative-mechanism conclusion (issue 1196: the tear fires
   via the AUX single-mark cross-band, not the primary band which is structurally blind — mining the
   partial's per-frame `payloads` reversed the grading). Mine the raw partial, don't trust the field
   name.

## 15. Zero-FP-over-the-whole-distribution CAN outweigh an unvalidated upper bound — the #1166 promote's actual flip (2026-09-02)

§14 held dup-cadence report-only because two calibration gaps were unresolved: no upper RATE bound
separating a pulldown from a spanning freeze, and an owner aux-tick supersede fork (issue 1196). The
LIVE flip that finally happened did NOT resolve either gap with new data — it happened because a
THIRD, independently-sufficient signal accumulated: the measured FALSE-POSITIVE risk of flipping,
across the ENTIRE retained corpus, stayed at zero. This is a distinct, reusable calibration mode from
§3's "gates-green-first" (a threshold bound) and §5's "LIVE vs report-only" cam1-grabber test (a
SPECIFIC false-positive CLASS) — here the whole-distribution FP count is the promotion evidence, even
though a SPECIFIC known-risky shape (a spanning freeze) was never independently cleared.

**The re-entry event + the re-mine (named in a prior W-park comment, closed by this one):**

| run | overall_pass | masked_windows | worst_raw | copies_by_content / tick_proven | viability | promotable |
|---|---|---|---|---|---|---|
| 1363366080 | ✅ | 0 | 0.0012 | 2/3 (0.67) | viable | true |
| 1168855508 | ✅ | 0 | 0.0024 | 7/7 (1.0) | viable | true |
| 674135238 | ✅ | 0 | 0.0071 | 9/9 (1.0) | viable | true |
| 1973834759 | ✅ | 0 | 0.0036 | 8/8 (1.0) | viable | true |
| 1556876186 | ✅ | 0 | 0.0012 | 3/3 (1.0) | viable | true |
| 1574770780 | ✅ | 0 | 0.0047 | 8/8 (1.0) | viable | true |
| 269576128 (post-issue-1260, .610) | ✅ | 0 | 0.0095 | 12/13 (0.92) | viable | true |
| 255477892 (post-issue-1260, .611) | ✅ | 0 | 0.0024 | 5/5 (1.0) | viable | true |
| 1347045170 / 659887078 / 300823397 | ❌ (other gates) | 0 | 0.012 / 0.039 / 0.022 | 16/17, 50/51, 21/21 | viable | true |
| 1326320314 / 1700989544 / 722076375 | ❌ (other gates) | 0 | 0.006 / 0.0024 / 0.0012 | 8/44, 2/14, 3/20 | blind | false |

14 runs total; **0/14 masked** on the DISCRIMINATED signal (`worst_masked_duplicate_fraction` — see
§8, gated on windows the classifier flagged `duplication_masked`, never the raw worst); the 3
`viability=blind` rows are FAILED runs with frozen/torn content, where there is nothing left for the
content signal to observe (not a signal defect — see §12's `SignalViability::Indeterminate`/`Blind`
split). `signal_promotable` (§12's `signal_viability(...) == Viable`) reads `true` on 11/14, including
BOTH post-issue-1260 runs, so the promotion precondition holds on the freshest data too, not just the
older mine.

**Why zero-FP-over-the-distribution is sufficient evidence here, even with an unvalidated upper
bound:** the two §14 gaps are about a HYPOTHETICAL shape (a window-spanning freeze) that has never
actually appeared masked in 44+ mined runs across weeks of rig operation — including every cam1
ShadowCast-grabber window and every known frozen/torn run in the corpus (the `blind` rows above are
frozen/torn and STILL read `masked_windows=0`, because a frozen window's near-dups fail
`DUP_COVERAGE_MIN`/`DUP_GAP_CV_MAX` exactly as designed). A bound that is theoretically unvalidated
but has produced ZERO false positives over the FULL observed operating range is a materially
different risk than an untested bound on a shape the rig actually produces often. The gate's whole
PURPOSE is also asymmetric: it exists to catch a FUTURE masked halving that issue 1203's `received=`
rate tap is structurally blind to — sitting report-only produces zero protection against that future
event, so the downside of NOT flipping (an undetected future pulldown) is weighed against a
downside (a freeze false-fail) that has never once materialized.

**What did NOT change:** `DUP_RATE_PULLDOWN_MIN=0.10` / `DUP_GAP_CV_MAX=0.35` / `DUP_COVERAGE_MIN=0.5`
are untouched — only the one-line `gates_overall_pass()` seam flipped. The upper-bound gap from §14
is still genuinely open (a real spanning-freeze datapoint would still be the right way to close it
properly); this promotion is a risk-accepted flip on empirical zero-FP evidence, not a claim that the
gap is resolved. The aux-tick supersede fork (issue 1196) is also still open and independent of this
flip — the content MAD signal stays the live tap unless/until that migration happens.

**Consumer-side lesson (generalizes the delivery-spread/own-burn-absent/tear pattern from §11):** the
Python `e2e_discord_report.py` classifier must ALSO be updated at the same flip — it is a SEPARATE
consumer of the same `gates_overall_pass` field with its OWN hardcoded report-only routing
(`_report_only_tripped`'s unconditional `masked_windows > 0` check, with no `gates_overall_pass`
guard at all before this flip, unlike the delivery-spread branch which already had one). A gate flip
in `recording-verdict.rs` alone is NOT sufficient — grep `tests/`/`scripts/` for the seam's field name
before declaring a promote cycle done; see `e2e-discord-report.md` for the routing rule this fix
brought into line with its siblings.
