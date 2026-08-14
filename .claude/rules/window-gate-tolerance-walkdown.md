---
paths:
  - "src/window_gate.rs"
  - "src/probe/recording_segments.rs"
---

# Walking WINDOW_COPIES_GAPS_TOLERANCE down (or up) is a DATA-FIRST step, gated on rig-verified deploy segregation

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
