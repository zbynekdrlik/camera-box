---
paths:
  - "src/optical_floor.rs"
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
---

# The #881 optical undecodable floor became report-only (issue 915, 2026-08-01)

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

## The per-window WARN print now needs to distinguish TWO independent report-only reasons

Before issue 915, `!s.pass && !s.relaxed_pass` could ONLY mean `frame_count == 0` or an
over-floor `undecodable` (the two were coupled in one `if/else`). After issue 915, copies/gaps
(issue 889) and the optical floor (issue 915) are BOTH independently report-only, so a window can
trip either or both while still passing `relaxed_pass` — `recording-verdict.rs`'s per-window WARN
block checks each condition separately (`!floor_ok` prints the #915 line, `copies != 0 || gaps !=
0` prints the #889 line, both can fire on the same window) instead of a single either/or branch.
Only the `else` (both verdicts fail) narrows down to `frame_count == 0` alone now.

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
