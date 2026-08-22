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

## 5. The walk-down stays on ITS ticket
If the step does not reach 0, the ticket stays OPEN and carries the remaining steps; the PR must NOT
close it. Gate the next step on the concrete evidence the running gate produces (N>=2 consecutive
green post-fix runs at the lower value). `window_gate.rs` is crate-root (default features, locally
testable with `# airuleset:build-ok`); `recording_segments.rs` is probe-gated (CI-only compile).
