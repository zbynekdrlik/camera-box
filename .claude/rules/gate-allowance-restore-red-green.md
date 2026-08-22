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
