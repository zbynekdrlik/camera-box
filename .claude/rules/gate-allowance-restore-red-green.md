---
paths:
  - "src/bin/recording-verdict.rs"
---

# Restoring a deliberately-relaxed verdict-gate constant (#905-style) — RED→GREEN even with no local compile path

`recording-verdict.rs` carries several deliberate, tracked, LOUDLY-reported relaxations of an
originally-strict verdict gate — `REAL_DROPS_ALLOWANCE_DEFAULT` (issue 904, restored to 0 by
issue 905 item 1); issue 905 items 2/3 still pending (`frozen_leg`/`self_heal_reset` back to
blocking, per issue 914; the optical undecodable floor's `gates_overall_pass()` back to `true`,
per issue 915 — see `optical-undecodable-floor-report-only.md`); and the per-segment `<=1/<=1`
SINGLETON allowance `window_gate::segment_singleton_allowance_gates_overall_pass()` (issue 1169,
owner 2026-08-22 — the segment-bar copies/gaps soft-release; re-tighten to absolute zero = flip
that one fn to `false`, the consts `SEGMENT_SINGLETON_{COPIES,GAPS}_ALLOWANCE` stay as the band —
see `window-gate-tolerance-walkdown.md` seam 4; issue 1169 stays OPEN as the trail, closed only by
a zero-singleton green run). Each of these follows the SAME
shape: a relaxation constant/flag was introduced to work around a KNOWN, NAMED root cause while it
was still unfixed, with an explicit **restore condition** written into its own ticket (usually
"root cause fixed" + "N consecutive clean runs with the allowance never consumed"). When a
restore ticket's precondition is finally met, the fix is almost always a ONE-CONSTANT flip with
zero other production-code changes — but it still deserves the full RED→GREEN discipline, even
though this file has **no local compile path at all** (`required-features = ["probe"]` — see the
project CLAUDE.md's Local Build Policy section; `cargo check`/`clippy`/`test` on default features
never touch it, so there is genuinely nothing to compile-verify locally, not even a syntax check
beyond `cargo fmt --all --check`).

## The pattern that worked (issue 905 item 1, `REAL_DROPS_ALLOWANCE_DEFAULT` 2→0)

1. Write/update the tests FIRST, against the CURRENT (still-relaxed) constant value — commit them
   as `[red]`. Since the tests can't be executed locally, "RED" here means: reason through what
   the test's assertion would evaluate to against the OLD constant, and confirm it would genuinely
   fail (e.g. a test asserting `!pass` for a single real drop, when the still-2 allowance would
   actually make `pass == true` — a real, if unexecuted, contradiction).
2. Flip ONLY the constant (and its doc comment) in a SEPARATE, immediately-following commit —
   `[green]`. Diff-verify with `git show --stat` that this commit really is a 1-line value change
   and nothing else snuck in.
3. Do NOT rip out the relaxation's whole MECHANISM (the env-override read, the per-node/run-level
   JSON fields, the loud print lines) just because the DEFAULT reverts to strict — leave it dormant
   and available (e.g. `CAMERA_BOX_REAL_DROPS_ALLOWANCE`) for a future genuinely NEW artifact
   class. Ripping it out is a much larger diff for zero behavioral gain, since `allowance(0)` is
   already proven byte-identical to the pre-relaxation strict check (there is usually already a
   dedicated test proving this — for #904/#905 it's
   `allowance_zero_matches_pre_904_is_zero_exactly_904`). Keep ONE test at the pure-method level
   (`NodeVerdict`/`node_verdict_json` called directly with an EXPLICIT nonzero allowance, bypassing
   the env-based default entirely) so the mechanism itself stays regression-tested even though the
   DEFAULT no longer exercises it — this also avoids ever needing `std::env::set_var` in a test
   (unsafe since Rust 1.82, and a real parallelism-race risk with `cargo test`'s default threading).
4. A fixture built around "N+1 gaps, one past the OLD allowance" almost always simplifies back down
   to its ORIGINAL pre-relaxation single-gap shape once the allowance returns to 0 — check nearby
   SAFETY-invariant tests (e.g. `#356`/`#571` never-mask tests) for a `_multi_gap`-with-inflated-
   count fixture that can be restored to the simpler `window(...)` helper's single-gap call.

Items 2 and 3 on issue 905 are excellent candidates to follow this exact recipe when their own
preconditions land.

## The pattern INVERTED (issue 1169, `REAL_DROPS_ALLOWANCE_DEFAULT` 0 → 1)

Issue 1169 (owner, 2026-08-22) RE-WIDENED `REAL_DROPS_ALLOWANCE_DEFAULT` from the issue-905
restored 0 back to the LOUD `<=1` SINGLETON band — the SAME recipe run BACKWARDS (a re-relaxation
with a re-tighten trail, not a restore). Current state:

- **DEFAULT = 1** — a single per-frame delivery singleton (issue-1167 v3 paced-trickle absorption +
  a FIFO stale_replay in the same event; `burn_unreadable` stays 0) PASSES within the allowance and
  is reported LOUDLY (per-node + full_chain notes now say "real-drops singleton allowance consumed:
  N — issue 1169 re-tighten trail"; the JSON already carries `real_drops_allowance` +
  `real_drops_allowance_consumed_nodes`). `>=2` of anything still FAILS; `burn_unreadable` stays an
  unconditional hard fail.
- **Re-tighten = the ONE constant flip back to 0**, landed once a zero-singleton green run holds
  (e.g. after the issue-1168 floor reduction and/or the cam1-card swap). Issue 1169 stays OPEN as
  that trail.
- The inverse of recipe step 4: the issue-356/#571 never-mask SAFETY test's part (b) fixture was
  LIFTED from 1 gap back to 2 (one PAST the singleton band) so it keeps proving loss BEYOND the
  allowance — mirroring what issue 904 did for its allowance of 2, re-scaled to band 1.
- The DORMANT re-tighten proof (`re_tightening_the_1169_allowance_to_zero_restores_the_strict_bar`)
  is the inverse of `single_real_drop_...` above: an EXPLICIT `is_zero_within_allowance(0)` on the
  same singleton node still FAILS, so flipping the constant back is a proven one-line change.
- **Blast-radius gotcha when MOVING this default (either direction): the adjacent, untouched
  sibling test region's narrative COMMENTS go factually stale — refresh them IN-BRANCH.** These
  comments are load-bearing in this file, and a default flip silently makes lines like "the DEFAULT
  is now strict" or "restored zero default" outright FALSE (review caught 4 such spots + 1 false
  one on the 1169 flip). The ASSERTIONS in the sibling explicit-allowance tests
  (`real_drops_within_..._905`, `allowance_zero_matches_pre_904...`) stay correct (they pass
  explicit allowances, never the default) — only the framing prose rots. Audit every `#905`/`#904`
  comment near the const + the `MECHANISM` test section and fix the stale ones in the same branch
  (the adjacent-stale-comment norm), not the assertions.

NOTE: this is a DIFFERENT seam from the segment-bar `<=1/<=1` singleton allowance
(`window_gate::segment_singleton_allowance_gates_overall_pass()`), which is issue 1169's FIRST
seam. Both share the ticket and the re-tighten trail; this one is the full_chain `real_drops` path
in `recording-verdict.rs`, that one is the per-segment copies/gaps path in `window_gate.rs`.

## The pattern INVERTED, THIRD instance (issue 1169, cam-leg V4L2 capture-drop band `CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT` 0 → 2)

Issue 1169's THIRD and LAST seam gives the RAW cam-leg V4L2 capture-drop counter
(`full_chain.loss.cam2_*.zero_loss` in `recording-verdict.rs`) the SAME loud singleton band. This
was the last binding `all_pass &= …` red: the strict `capture_zero = v4l2_dropped == 0` fold
(`all_pass &= capture_zero`) failed `overall_pass` on exactly `v4l2_dropped:2` over
`frames_captured:35961` (0.0056%) while `full_chain.zero_loss` + `all_cambox_continuity` were
already green. These raw drops are UPSTREAM camera-leg buffer drops (the kernel `sequence` gap
`capture.rs` tracks) that the merged issue-1167 v2–v5 paced-trickle + FIFO emit-fill absorbs by
design — a strict-zero bar on the RAW counter double-reds what the presented layer compensated.

- **DEFAULT = 2** (`CAMLEG_V4L2_DROP_ALLOWANCE_DEFAULT`, env-overridable via
  `CAMERA_BOX_CAMLEG_V4L2_DROP_ALLOWANCE`) — justified from the live data: healthy cam2/cam3
  routinely log 0–2 capture-dropped per ~10-min run window. The fold became
  `all_pass &= within_band` where `within_band = camleg_capture_band(v4l2_dropped, allowance)`
  (a pure Tier-0-testable scalar fn returning `(within_band, band_consumed)`). A `<=2` count PASSES
  with the node JSON carrying `zero_loss=true` + `camleg_singleton_band_consumed=true` + a loud
  `note` ("cam-leg V4L2 singleton band consumed: N/2 — absorbed by the issue-1167 emit fill; issue
  1169 re-tighten trail") + a `>>> ⚠ #1169 CAM-LEG V4L2 SINGLETON BAND` stderr line. `>2` still
  FAILS unchanged (`zero_loss=false`, band NOT consumed).
- **Re-tighten = the ONE constant flip back to 0**, landed once a zero-singleton green run holds
  (issue 1168 floor reduction and/or the cam1-card swap). Dormant proof:
  `re_tightening_the_camleg_v4l2_band_to_zero_restores_the_strict_bar` (an EXPLICIT
  `camleg_capture_band(2, 0) == (false, false)`), the inverse of
  `camleg_v4l2_singleton_band_absorbs_two_drops_into_overall_pass_1169`.
- **Blast-radius:** the adjacent `#861` `zero_loss_capture_drop_still_fails_overall_pass_...` test
  (fixture `v4l2_dropped=7`, deliberately WAY over the `<=2` band) stays a hard FAIL, but its
  narrative comment referenced the old `all_pass &= capture_zero` fold — refreshed IN-BRANCH to
  `all_pass &= within_band` (the adjacent-stale-comment norm; the ASSERTIONS were already correct).
- **Downstream mirror:** the Python `scripts/e2e_discord_report.py` needed NO change — its
  physical-fault blocker (`_blocking_failures` item 2) keys on `cam2_* zero_loss is False`, so a
  band-consumed `zero_loss=true` auto-drops out of the blocker list, while `_stream_drop_total`
  keeps counting the raw `v4l2_dropped` honestly. Verified against the sweep.

This one is the RAW capture-leg counter path in `recording-verdict.rs`; distinct from seam 1
(per-segment copies/gaps in `window_gate.rs`) and seam 2 (per-node `real_drops` delivery counter in
`recording-verdict.rs`). All three share the ticket + the single re-tighten trail (issue 1169 stays
OPEN until a zero-singleton green run flips every band back to 0).
