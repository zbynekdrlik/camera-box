---
paths:
  - "src/bin/recording-verdict.rs"
---

# Restoring a deliberately-relaxed verdict-gate constant (#905-style) — RED→GREEN even with no local compile path

`recording-verdict.rs` carries several deliberate, tracked, LOUDLY-reported relaxations of an
originally-strict verdict gate — `REAL_DROPS_ALLOWANCE_DEFAULT` (issue 904, restored to 0 by
issue 905 item 1), and issue 905 items 2/3 still pending (`frozen_leg`/`self_heal_reset` back to
blocking, per issue 914; the optical undecodable floor's `gates_overall_pass()` back to `true`,
per issue 915 — see `optical-undecodable-floor-report-only.md`). Each of these follows the SAME
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

NOTE: this is a DIFFERENT seam from the segment-bar `<=1/<=1` singleton allowance
(`window_gate::segment_singleton_allowance_gates_overall_pass()`), which is issue 1169's FIRST
seam. Both share the ticket and the re-tighten trail; this one is the full_chain `real_drops` path
in `recording-verdict.rs`, that one is the per-segment copies/gaps path in `window_gate.rs`.
