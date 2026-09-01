---
paths:
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
---

# Walking WINDOW_COPIES_GAPS_TOLERANCE down (or up) is a DATA-FIRST step, gated on rig-verified deploy segregation

## #1132 (owner mandate, 2026-08-19) — the tolerance NO LONGER folds into `overall_pass`

The relaxed copies/gaps RESCUE was removed from the BLOCKING fused verdict (a hardware-sick CAM1 leg
with copies=1/gaps<=3 passed green for a week, masking the defect). There are now FOUR independent
copies/gaps-related seams in `window_gate.rs`, do not conflate them:

1. **`WINDOW_COPIES_GAPS_TOLERANCE`** (the const this rule is about) — still walked 0..3, still used
   by `relaxed_pass`, but `relaxed_pass` is now **REPORTED-ONLY** (observability), it no longer gates.
2. **`copies_gaps_tolerance_gates_overall_pass() -> false`** (#1132, mirrors
   `optical_floor::gates_overall_pass`) — the DISARM flag for the `<=3` rescue. While `false`, the run
   fold uses `WindowGateDecision::overall_pass_term`, which — under seam 4 (#1169) — absorbs a
   `<=1/<=1` SINGLETON and otherwise requires `copies==0 && gaps==0`. `segment_continuity()` folds
   `overall_pass_term`, NOT `relaxed_pass`.
3. **`optical_floor::gates_overall_pass()`** — the SEPARATE optical undecodable floor seam (issue
   915/905). `overall_pass_term` shares `relaxed_pass`'s floor term EXACTLY, so #1132 did NOT re-gate
   the floor — a window over the floor but clean on copies/gaps still passes the blocking verdict.
4. **`segment_singleton_allowance_gates_overall_pass() -> true`** + the consts
   `SEGMENT_SINGLETON_{COPIES,GAPS}_ALLOWANCE=1` (#1169, owner 2026-08-22) — a DISTINCT, strictly
   TIGHTER (`<=1/<=1`) allowance folded into `overall_pass_term` while seam 2 is disarmed. A `<=1/<=1`
   singleton (the designed issue-1167 v3 paced-trickle + FIFO stale_replay residual, post cam1 card
   swap) is ABSORBED into the blocking verdict; `>=2` of EITHER still fails. It is LOUD, never silent:
   `strict_pass`/`pass` stay false (visible), a `CamboxSegment::singleton_allowance_note` fires, and
   `SegmentedContinuity::windows_singleton_allowance_consumed` counts it — addressing #1132's masking
   concern (the note) while honoring its multi-frame intent (`>=2` fails). **NEVER conflate this with
   seam 2:** the `<=3` rescue stays disarmed; #1169 is a separate seam with its OWN re-tighten trail.

**Consequence for a future walk-down:** stepping `WINDOW_COPIES_GAPS_TOLERANCE` (seam 1) now only
moves the REPORTED `relaxed_pass` verdict + `windows_over_copies_gaps_tolerance` count; it does NOT
change what gates `overall_pass`. REACTIVATING the `<=3` tolerance rescue (seam 2 → `true`, e.g. the
upstream residual is genuinely bounded again and the sick leg is fixed/excluded — #1110/#1134) makes
`overall_pass_term == relaxed_pass` and the walked const governs the blocking fold again. #1132's
disarm, #1031's walk-down, and #1169's singleton allowance are orthogonal.

## #1220 (owner mandate, 2026-08-29) — seam 2 RE-ARMED by OVERRIDE, not by meeting the precondition above

The paragraph above stated seam 2's restore precondition as "the sick leg is fixed/excluded"
(#1110/#1134). **That precondition was NOT met when seam 2 was re-armed** — #1220 is the owner's
2026-07-31 standing soft-release directive ("jemne uvoľniť gate na zelenú + tickety na pritvrdenie,
potom ticket po tickete") applied directly to this seam: two same-day full-cycle runs (1989954227,
797081170) both passed `all_cambox_av_sync` for the first time, and `all_cambox_continuity` failed
purely on windows sitting within the ALREADY-CALIBRATED `<=3` channel but over the tighter #1169
`<=1/<=1` band. The owner directive overrides the stated precondition outright, rather than being
evidence the precondition was satisfied — do not read `copies_gaps_tolerance_gates_overall_pass()
== true` as proof the CAM1 leg is fixed; check #1110/#1134's own status separately if that matters.
`WINDOW_COPIES_GAPS_TOLERANCE` is now LIVE again (gates `overall_pass`, not just `relaxed_pass`) and
the walk-down commitment stays open on #1220 (not #1031) going forward.

**#1243 (relax-to-green) + #1242 (walk-back) — VERIFY the fold state BEFORE a "relax/restore the
strict per-segment fold" ticket; its premise is easily stale.** Because #1220 re-armed seam 2,
`segment_continuity()` ALREADY folds `overall_pass_term == relaxed_pass` (NOT strict `pass`). So a
ticket that says "switch the cambox per-segment blocking fold from strict → relaxed" (#1243 change 1)
can describe a state that already holds — grep `copies_gaps_tolerance_gates_overall_pass()` (== true
today) and read the run's `all_cambox_continuity.overall_pass` in the verdict JSON (true when every
window is within the `<=3` tolerance; segments show `pass=false` report-only via
`windows_failed_report_only` while `relaxed_pass=true`) BEFORE assuming the fold is strict. In #1243
the fold was already relaxed, so change 1 was a comment-only walk-back annotation and the only real
RED→GREEN was a SEPARATE gate — the #1142 uniformity floor (`presentation_cadence::UNIFORM_FRACTION_MIN`,
a crate-root Tier-0 const, NOT this rule's window_gate seams) walked 0.95 → 0.93 (run 1629895310
worst derived 0.9397 was the sole blocking red). The paired RESTORE ticket #1242 therefore restores
strict `copies==0` by DISARMING seam 2 (`copies_gaps_tolerance_gates_overall_pass() -> false`), never
by "changing the fold" — the fold already reads relaxed via the seam.

**Second walk step (still #1243, 2026-08-31): 0.93 → 0.90.** A SECOND complete 7-cam verdict (run
1230380558) landed with worst derived_uniform_fraction 0.9221 — below 0.93, RED-ing an otherwise
steady run. Same mechanism, same data-first walk-down doctrine: two-run combined observed steady-rig
range is 0.9221–0.9953, so `UNIFORM_FRACTION_MIN` moved to 0.90 (margin under both runs' minimums,
still far above the sick-rig band 0.67–0.78). A future worker reading THIS rule after a third data
point lands should re-derive the tightest supportable value the same way, not assume 0.90 is final —
#1242 is still the ticket that root-causes the residual churn and eventually restores 0.95.

**Reusable shape for a FUTURE seam like this one — "precedence supersession", distinct from every
flag-flip pattern in `gate-allowance-restore-red-green.md`:** re-arming `copies_gaps_tolerance_
gates_overall_pass()` needed ZERO change to seam 4's own flag (`segment_singleton_allowance_gates_
overall_pass()`, still hardcoded `true`) — `decide()`'s existing `if`/`else if` chain already made
seam 2 take precedence the moment it returned `true`, and seam 4 became an automatic GRADUATED
FALLBACK (if a future step disarms seam 2 again, seam 4 resumes governing on its own, with no code
change needed there either). When a NEW seam is layered on top of an existing tighter one via an
`if`/`else if` chain (rather than a flat boolean OR), re-arming the OUTER (wider) seam is a
ONE-FUNCTION flip that transparently supersedes the inner one — verify this by testing the inner
seam's pure helper fns DIRECTLY (not just through the combined decision fn) to prove they read as
permanently dormant, the way `src/window_gate.rs`'s `singleton_helper_fns_are_dormant_while_the_
1220_tolerance_channel_is_armed` test does.

**Even a "doc/test-only, self-adjusting, zero production logic change" claim still needs the review
step.** The #1220 gated adversarial review caught 3 factual-accuracy findings — none in old code,
all in freshly-written prose describing the SAME live-run evidence the worker itself had just
gathered: miscounting which of four live-run windows were genuinely OVER a band vs already ABSORBED
by it, an unconditional print line whose neighboring guard had gone stale, and a JSON branch that
checked only ONE of the two now-relevant flags. Hand-written narrative citing specific live numbers
is exactly the kind of claim a second pass catches that `cargo fmt --all --check` (a pure syntax
check) never can.

## #1169 (owner, 2026-08-22) — the `<=1/<=1` SINGLETON allowance (seam 4) + its re-tighten to zero

Soft-release to green with a re-tighten trail, exactly the owner's 2026-07-31 doctrine ("jednu
stratenú snímku netreba riešiť" — one lost frame per window is acceptable). The FIRST full verdict of
the 2026-08-22 series (859647390) measured 8/10 segments at absolute zero and 2 windows at a `<=1/<=1`
singleton (seg[3] CAM3 copies=1 gaps=1, seg[4] CAM2 copies=1) — the designed absorption cost, plus the
matching `frozen_leg.stale_replay` (CAM2×1/CAM3×1, the same event surfaced twice). The mechanical bar:

- `overall_pass_term` absorbs a segment iff `copies <= SEGMENT_SINGLETON_COPIES_ALLOWANCE && gaps <=
  SEGMENT_SINGLETON_GAPS_ALLOWANCE` (both `1`) **while seam 2 is disarmed**; `>=2` of either fails.
- The pure decision is `window_gate::segment_singleton_allowance_consumed(copies, gaps)` + the note via
  `segment_singleton_note(copies, gaps)` (both crate-root, Tier-0-testable; `recording_segments` +
  `recording-verdict.rs` only CALL them). JSON self-describes via
  `segment_singleton_allowance_gates_overall_pass` + the two allowance consts + `segment_singleton_gate`.
- **Re-tighten to absolute zero = flip `segment_singleton_allowance_gates_overall_pass()` to `false`**
  (one line, the `gate-allowance-restore-red-green` dormant-mechanism pattern — the consts stay as the
  band definition). Gated on the issue-1168 floor reduction and/or the cam1 card swap landing + N
  consecutive zero-singleton green runs. **Issue 1169 stays OPEN as that trail; the PR does NOT close
  it** — the zero-singleton green run closes it.

---

`WINDOW_COPIES_GAPS_TOLERANCE` (`src/window_gate.rs`) is the per-window `copies`/`gaps` tolerance the
RELAXED verdict applies. It is walked between 0 and 3 as the upstream genlock residual changes
(issue 889 walked it UP 0→3; issue 1031 walked it back DOWN 3→1). Each step is decided from the
per-window distribution in the retained verdict JSONs, never from taste. The procedure:

## 1. Mine the distribution from `/tmp/recording-e2e-*/verdict-*.json`
Per-window `copies`/`gaps`/`undecodable` live in `.all_cambox_continuity.segments[]`. The tolerance
gates each term INDEPENDENTLY (`copies <= TOL && gaps <= TOL`), so the binding metric is
`max(maxCopies, maxGaps)` across the run's windows. A useful one-liner:
```bash
jq -r '.all_cambox_continuity as $c | ($c.segments//[]) as $s |
  "\($c.overall_pass) maxC=\($s|map(.copies)|max) maxG=\($s|map(.gaps)|max) \
   totUndec=\($s|map(.undecodable)|add) winOverTol=\($c.windows_over_copies_gaps_tolerance)"' verdict-*.json
```

## 2. Segregate pre-fix vs post-fix by the RIG-VERIFIED deploy time — this is load-bearing
- **The verdict JSON records NO version.** Verdict mtime = run END. You cannot tell which binary a
  run used from the verdict alone.
- **Verify the deploy time on the RIG, not from the commit.** The genlock fixes that change this
  burden live in the OBS build (imag-nb `libobs.so.30`/`distroav.so`, strih/stream `obs.dll`) — NOT
  the cam-box binary. Check the deployed `.so`/`.dll` mtime AND the OBS process start time via MCP
  (`mcp__linux-imag-nb__Shell` / `mcp__win-strih__Shell`). Example incident (issue 1031): cam1's
  `camera-box` binary was still dev.432 from days earlier and completely irrelevant; the real deploy
  was imag-nb + strih OBS at 09:17-09:18 CEST.
- **A run START time inside ~10 min of a genlock OBS restart records the CONVERGENCE TRANSIENT, not
  steady state — EXCLUDE it.** Run START ≈ verdict mtime minus the run length (~20 min). The
  issue-1049 phase convergence needs settle time after a fresh OBS start; a run that started 2 min
  after the restart measured 14 copies/gaps/window while the very next run (started +27 min) measured
  1/1. Only fully-converged post-restart runs count as post-fix data.
- **Exclude dead-painter / no-signal runs**: all windows `frames=0` (optical black) or an undecodable
  storm — they carry no meaningful copies/gaps burden signal. Say so explicitly.

## 3. Pick the tightest value the STEADY post-fix data supports — never below it
`TOL_min = max(maxCopies, maxGaps)` over healthy, converged, post-fix windows. Step to exactly that
(issue 1031: steady post-fix max was 1/1 → step to 1). Do NOT step below it (would fail the first
green run); do NOT leave it looser (contradicts the zero-loss intent). **0 is special**: it is only
reachable when the residual is GENUINELY zero AND it structurally changes the "at-tolerance still
fails strict" contract tests (`decide(100,0,0,0)` PASSES strict) — treat 0 as a distinct milestone,
gated on the shared-duplicate root cause (issue 859) landing, not a mere number change.

## 4. Both-directions test update
Most boundary tests already track the const (`TOL`, `TOL+1`, `TOL+2`) and self-adjust. Only two kinds
of site pin a literal and must move WITH the const:
- the literal pin `assert_eq!(WINDOW_COPIES_GAPS_TOLERANCE, N)` (`window_gate.rs`).
- **at-boundary integration fixtures** that hardcode a window AT the old tolerance and assert it
  PASSES (e.g. `recording_segments::windows_at_copies_gaps_tolerance_still_pass...`, cam2 built with
  the old tolerance count). Recalibrate the fixture's count to the new tolerance — this is
  boundary-tracking, NOT weakening (the intent "a window AT tolerance still passes" is preserved),
  the same op every prior recalibration did.
Over-tolerance fixtures with a literal well above the new tolerance (4, 9) stay green; fix their
stale "tolerance+1" comments. `recording-verdict.rs` reads the const dynamically (self-adjusting).
Before pushing, grep `\.(copies|gaps),\s*[0-9]` and `decide(` literals across `src/`+`tests/` for any
PASS-asserting fixture carrying a count that would flip at the new value.

**Correction, issue 1243 (2026-08-31): the "PASS-asserting" framing above is INCOMPLETE — a walk
UP (widening the tolerance) breaks a THIRD, easy-to-miss site the same way: a FAIL-asserting
over-tolerance fixture pinned at the OLD `tolerance+1` (the minimal over value at the time it was
written) silently flips to PASS once the const walks past it, unless it too is bumped to the NEW
`tolerance+1`.** The initial recalibration sweep on this walk grepped for the literal OLD const
value (`= 3`) and for test NAMES mentioning "tolerance" — that missed TWO such fixtures
(`non_adjacent_freeze_hiding_a_real_drop_still_fails_strict`'s companion, and
`benign_delivery_reorder_*_missing_ticks_still_fail_625`) because neither fixture's own test name
references the tolerance at all; they exist to prove an UNRELATED property (a hidden freeze, a
benign delivery reorder) and only INCIDENTALLY sat at the old over-tolerance boundary as a
"this still fails, not silently absorbed" companion assertion. A gated-Fable review pass caught
both — the sweep for a future step on THIS const must grep the actual FIXTURE VALUES
(`\.gaps, N`/`\.copies, N`/`decide(.*, N,`/`decide(.*, N)` for every literal `N` in
`[old_tolerance+1 .. new_tolerance]`), never rely on the test's own name or its stated purpose to
find every site that needs recalibrating.

## 5. The walk-down stays on ITS ticket
If the step does not reach 0, the ticket stays OPEN and carries the remaining steps; the PR must NOT
close it. Gate the next step on the concrete evidence the running gate produces (N>=2 consecutive
green post-fix runs at the lower value). `window_gate.rs` is crate-root (default features) —
**note (#557, 2026-08-18): the `# airuleset:build-ok` local-compile bypass this section used to cite
is now DISABLED repo-wide** (see the project CLAUDE.md's Local Build Policy) — verify a
`window_gate.rs` change locally via `cargo fmt --all --check` + hand-derivation only, exactly like
`recording_segments.rs` (probe-gated, CI-only compile either way).

## Third walk step (issue 1243, 2026-08-31): `WINDOW_COPIES_GAPS_TOLERANCE` 3 -> 5
Same doctrine, a DIFFERENT seam than the two `UNIFORM_FRACTION_MIN` steps documented above (that
constant lives in `src/presentation_cadence.rs`; this one lives in `src/window_gate.rs` — see this
file's own header for the full ownership split). Three complete post-fix 7-cam verdicts
(1629895310, 1230380558, 1142514714) gave per-run worst `max(copies,gaps)` of `{1, 1, 4}` — run
1142514714's seg3 CAM4 measured 4 separate single-frame duplicates over ~14s, the run's sole
blocking-gate failure. Stepped to 5 (one event of margin above the observed ceiling, not the bare
ceiling itself — the n=3 variance already showed the same "flaky at its own ceiling" pattern every
earlier step on this const hit). Full evidence: `src/window_gate.rs`'s own "2026-08-31
RE-CALIBRATION" module-doc section + the design-addendum comment on issue 1243. Walk-back trail
stays on issue 1242, unchanged.

## Per-CAMBOX override seam — a FIFTH, orthogonal seam (issue 1251, 2026-09-01)

Distinct from every seam above: those all move ONE global `WINDOW_COPIES_GAPS_TOLERANCE` (or a
global on/off flag) applied uniformly to every box. Issue 1251 added a per-CAMBOX OVERRIDE so ONE
sick box can be relaxed without touching the global default — CAM2's grabber HW (issue 1249) starves
in bursts (copies/gaps up to 18) while cam3+cam5 (same card, same splitter) stay within 5, so a
single global tolerance cannot tell the sick box from the healthy ones.

- **The seam:** `WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX: &[(&str, u32)] = &[("CAM2", 25)]` +
  `copies_gaps_tolerance_for_cambox(cambox)` (both `src/window_gate.rs`). `decide` is now a thin
  wrapper over `decide_with_tolerance(..., tolerance)`; `decide_for_cambox` applies the override;
  `relaxed_failure_reasons` wraps `relaxed_failure_reasons_with_tolerance`. Behaviour is
  byte-identical to the pre-1251 `decide` at the default (locked by
  `decide_with_tolerance_matches_decide_at_the_default_1251`).
- **EXACT-MATCH on the label — the load-bearing gotcha.** Production emits UPPERCASE `CAMN`
  (`camera_active_sweep_pairs` → `Cam N:CAMN`; `switch_schedule.py` writes it; the verdict's
  `cambox` field is `CAM2`), but the `recording_segments.rs` UNIT fixtures use lowercase `cam2`. The
  match is exact (`*name == cambox`), so the override hits production and DELIBERATELY MISSES the
  lowercase fixtures — that is what keeps the existing lowercase-`cam2` boundary tests (e.g.
  `gap_of_six_exceeds_tolerance_fails_overall_pass_1243`) valid. A case-INSENSITIVE match would break
  them. If you add a NEW per-cambox test, use the UPPERCASE label (`win("CAM2", …)`) or it won't
  pick up the override.
- **Per-window applied tolerance is carried on the segment.** `CamboxSegment.copies_gaps_tolerance`
  (auto-serialized JSON `copies_gaps_tolerance`) is the tolerance ACTUALLY applied to that window;
  `SegmentedContinuity.copies_gaps_tolerance` (run-wide) stays the DEFAULT for back-compat. The
  `overall_pass` fold and `windows_over_copies_gaps_tolerance` count read each window's OWN field,
  NOT the run-wide value; the verdict self-describes the policy via `copies_gaps_tolerance_per_cambox`.
  `e2e_discord_report.py`'s continuity classifier reads the per-segment field (run-wide fallback for
  old verdicts). When adding a consumer, read the per-window field, never the run-wide one.
- **Walk-back = set the map to `&[]`** (one line) when issue 1249's HW swap lands — tracked on issue
  1242, step recorded on issue 1243. Everything else (the lookup fn, the per-window field, the
  `decide_with_tolerance` core, the JSON key, the report prose) stays wired and resolves to the
  default for every box; the map-empty state is the tested walk-back state. Never masks a real
  defect: a non-overridden box over its default still fails the run (proven by
  `per_cambox_override_absorbs_cam2_starvation_but_not_other_boxes_1251`).
