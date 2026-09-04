---
paths:
  - "src/optical_floor.rs"
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
---

# The #881 optical undecodable floor: report-only (issue 915) → RE-GATED (issue 905 item 3)

> **SUPERSEDED — issue 905 item 3 (2026-09-04): the floor is LIVE-GATING again.**
> `optical_floor::gates_overall_pass()` is now hardcoded **`true`** (was `false`), and
> `RUN_UNDECODABLE_FLOOR` was recalibrated **8 → 6** (per-window kept 4). All the physical
> blockers issue 915 waited on are closed: issue 909 (cam1 grabber card replaced), issue 881
> (120Hz monitor — owner ruled it will NEVER be installed), issue 1179 (100Hz declined). The 60Hz
> baseline — and its irreducible optical temporal tear — is PERMANENT, so the floor is no longer
> "temporary until 120Hz"; it is a permanent, data-calibrated gate. Data: 31 post-cam1-fix dev1
> verdicts, steady run-wide max 4 / mean 1.3 / p90 3 (residual cam2-only), one genuine cam2 fault
> outlier 27; floor 6 = 50% headroom over steady max, below the pre-#707 regression level 10.
> **Everything BELOW documents the report-only ERA (issue 915) — it is HISTORY.** Read
> "gates_overall_pass() is false / report-only / temporary until 120Hz" throughout as the pre-905
> state; today the seam is `true` and a nonzero over-floor run FAILS `overall_pass`. Re-disarm
> (a future new artifact class) is the inverse one-line flip back to `false`.

## The seam — `optical_floor::gates_overall_pass()`, mirrors issue 914 exactly

`window_within_floor`/`run_within_floor` (both in `src/optical_floor.rs`) are **UNCHANGED** —
still fully computed, still feed `CamboxSegment::pass` (the STRICT verdict) byte-for-byte. Only
the CALLERS decide whether the floor's result folds into the RELAXED verdict that actually
decides `overall_pass`, gated on `optical_floor::gates_overall_pass()` (hardcoded `false`):

- `window_gate::decide()`: `relaxed_pass = frame_count > 0 && (undecodable_ok ||
  !gates_overall_pass())`. `strict_pass` is computed INDEPENDENTLY (`frame_count > 0 &&
  undecodable_ok && copies == 0 && gaps == 0`) so it keeps its exact pre-889-AND-pre-915
  byte-for-byte meaning — it is no longer *derived from* `relaxed_pass` the way it used to be,
  because `relaxed_pass` no longer implies the floor held.
- `probe::recording_segments::segment_continuity()`: the run-wide fold is `overall_pass &=
  run_wide_undecodable_within_floor || !gates_overall_pass()`. Two new always-serialized fields on
  `SegmentedContinuity` (`total_undecodable`, `run_wide_undecodable_within_floor`) keep the
  run-wide reading visible even though it no longer gates — mirrors `windows_failed_report_only`'s
  issue-889 visibility precedent (a nonzero/over-floor value with `overall_pass == true` is the
  relaxation visibly doing its job, not a hidden regression).

**Root cause this exists for:** cam1's ShadowCast 2 grabber hardware defect (issue 909) trips the
run-wide floor on hardware noise unrelated to the chain under test — a real optical/monitor
artifact would spread evenly across every cambox sharing the splitter (`rig-one-camera-splitter`
in memory), so 100% concentration of `undecodable` in CAM1 windows is the signature of issue 909,
not a real regression. Restore path on issue 905: flip `gates_overall_pass()` back to `true` once
cam1 is physically replaced AND issue 881 (120Hz monitor) lands.

## What is STILL strict — don't confuse this with a blanket "undecodable never gates"

`CamboxSegment::pass` (the STRICT field, drives `windows_failed_report_only`) is untouched — a
per-window over-floor `undecodable` still fails `pass`. `frame_count > 0` and a non-empty schedule
are UNTOUCHED and still gate BOTH verdicts unconditionally (an absent cambox proves nothing either
way — see `zero_frames_fails_both_verdicts_889` in `window_gate.rs`). Only the RELAXED/overall
fold changed.

## Reporting a partially-decoupled term needs a SCOPED JSON key, not a blanket one

`all_cambox_continuity` (the JSON object) still gates on `frame_count`/schedule emptiness even
though its `undecodable` floor term is now report-only — a blanket `"gates_overall_pass": false`
on the WHOLE object (the shape issue 861/914 use for a term that is FULLY decoupled) would be
misleading here. `recording-verdict.rs` instead adds `undecodable_floor_gates_overall_pass` /
`undecodable_floor_gate`, scoped by name to the specific term that changed. When decoupling only
PART of a JSON object's gating, name the flag after the specific term, not the object.

## The per-window WARN print now needs to distinguish MULTIPLE independent reasons (updated for the 2026-08-05 re-gate)

Before issue 915, `!s.pass && !s.relaxed_pass` could ONLY mean `frame_count == 0` or an
over-floor `undecodable` (the two were coupled in one `if/else`). Issue 915 made the optical floor
independently report-only, so a window could trip it while still passing `relaxed_pass` (`!floor_ok`
prints the #915 line). **The 2026-08-05 RE-GATE (ticket 889 comment 5196190653) then took
`copies`/`gaps` back OUT of "always report-only"**: above the per-window tolerance
(`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`, recalibrated 1 → 2 on 2026-08-06, ticket 889
comment 5198131539 — three valid hardware runs measured a per-window max of `{1, 1, 2}`, so
tolerance=1 was flaky by construction; recalibrated again 2 → 3 later the same day, ticket 889
comment 5200533407 — a post-#998-fix run measured a healthy per-window max of `{1, 1, 2, 3}` with
run-total burden identical to clean runs, so tolerance=2 was flaky by construction the same way)
they are a real, loud, gating failure again
(`SegmentedContinuity::windows_over_copies_gaps_tolerance`) — only AT OR UNDER the tolerance do
they stay absorbed (the `copies != 0 || gaps != 0` "#889 WITHIN TOLERANCE" print, still inside the
`s.relaxed_pass == true` branch).

Consequently the `else` branch (BOTH verdicts fail) is no longer just `frame_count == 0` — it is
`frame_count == 0`, OR `copies`/`gaps` over the tolerance (the dominant real-world cause
today, since `gates_overall_pass()` is still hardcoded `false`), OR — once issue 905 restores
`gates_overall_pass()` to `true` — an over-floor `undecodable`. `recording-verdict.rs` derives
these via the pure, Tier-0-testable seam `crate::window_gate::relaxed_failure_reasons` /
`RelaxedFailureReason` (`EmptyWindow` / `OverCopiesGapsTolerance` / `FloorExceededGating` /
`FloorWithinReportOnly`) rather than re-deriving the conditions inline — a window can carry more
than one reason at once (e.g. over-tolerance copies/gaps AND a merely-report-only over-floor
undecodable count), and `frame_count == 0` always short-circuits to `EmptyWindow` alone (an
empty window's other counts carry no meaningful signal). The deep-review that found the OLD
inline version's bug (it consulted the optical floor BEFORE checking `frame_count == 0`, so an
empty window was misclassified as an exceeded floor — `window_within_floor`'s defensive
`frame_count == 0` clause always reads `false`, which looks identical to a genuine floor breach
unless checked first) is issue 889's re-gate deep-review findings 1+2.

## Testing pattern: a test written to prove the OLD strict fold may already be neutralized by an earlier commit in the same PR

When decoupling more than one gating term in sequence (window-level via `window_gate.rs`, then
run-wide via `recording_segments.rs`), a test aimed at the SECOND term can accidentally already
pass once the FIRST term's fix lands — check what the test's fixture numbers actually exercise
before assuming a rename+flip is "the RED commit for term 2". Concretely:
`single_window_five_undecodable_exceeds_per_window_floor_...` and
`undecodable_over_floor_combined_with_a_copy_...` both use undecodable=5, which is UNDER the
run-wide floor (8) — so both already read `overall_pass == true` the moment `window_gate.rs`'s
per-window fix lands, regardless of whether `recording_segments.rs`'s run-wide fold was touched
yet. Only `pre_707_regression_level_...` (sum=10, over the run-wide floor of 8) is a genuine RED
against the run-wide fold specifically — verify which of your renamed tests actually probes which
term before citing them as RED evidence in a commit message.
