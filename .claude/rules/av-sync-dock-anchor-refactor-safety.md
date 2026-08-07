---
paths:
  - "vendor/av-sync-dock/src/sync-test-output.cpp"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "tests/genlock_preload.rs"
  - ".github/workflows/windows-genlock.yml"
  - ".github/workflows/windows-genlock-fast.yml"
---

# Refactoring the dock-lock decision chain in `sync-test-output.cpp` breaks stale text-anchor
# gates in THREE places, not one — and TWO of the three are invisible to local Tier-0 checks (#955)

**Before restructuring the `if (act.apply && ...) { ... } else if (act.apply) { ... } else if
(...) { ... } else { ... }`-shaped dock-lock decision chain in
`vendor/av-sync-dock/src/sync-test-output.cpp` (e.g. #955's switch-on-`cb_dock_lock_outcome()`
extraction), grep for its literal OLD source text in ALL of these locations — not just the one
Rust test file the top-level CLAUDE.md GOTCHA already warns about for `recording-e2e.sh`/
`rig-mode.sh`:**

1. `tests/genlock_preload.rs::vendored_source::dock_lock_corrector_is_monitor_only_by_build_default_942`
   — a `#![cfg(feature = "probe")]`-gated Rust test. **Invisible to every local Tier-0 check**
   (`cargo check`/`clippy`/`test --no-run` on default features never compiles this file at all —
   confirmed live: `cargo test --lib` shows nothing for it, no error, no warning). Only CI's
   `ci.yml` `Test` job (which runs `--all-features`) catches a stale anchor here.
2. **`.github/workflows/windows-genlock-fast.yml`'s own pwsh "Assert dock lock corrector is
   monitor-only by default" step.**
3. **`.github/workflows/windows-genlock.yml`'s IDENTICAL pwsh step** (the two workflow files
   duplicate this gate verbatim — always update both together).

Steps 2 and 3 are ALSO invisible locally (PowerShell/pwsh gates only run on the Windows CI
runner) — a session touching only `sync-test-output.cpp` and passing every local Tier-0 check has
**zero local signal** that any of these three are now stale. Live incident (#955, 2026-08-06):
extracting the chain into `switch (cb_dock_lock_outcome(...)) { case ...::Write: ... }` passed
`cargo fmt`/`check`/`clippy`/`test --no-run` and the full local `cargo test` suite (197/197 `ok`)
locally, then failed BOTH `CI` (the probe-gated Rust test, job "Test") and `Windows genlock FAST`
(the pwsh gate) on the very first push — the pwsh error was line-for-line the same stale-anchor
class as the Rust test's failure, just in a completely different language/file.

## The fix pattern (reusable for the NEXT such refactor)

Each of the three anchors is built the same way: collapse ALL whitespace to single spaces
(Rust: `s.split_whitespace().collect::<Vec<_>>().join(" ")`; pwsh:
`(Get-Content -Raw) -replace '\s+', ' '` — functionally equivalent for substring-containment
purposes, even though pwsh's version can leave a stray leading/trailing space the Rust join()
never does), then either `.contains(...)` a literal multi-token substring or slice a region
between two literal markers. When the OLD chain is replaced by a `switch`, rebuild each anchor
around the NEW structure's own literal tokens:

- A **positive, ordering-sensitive check** ("the write happens INSIDE the gated branch, as its
  FIRST statement") becomes one contiguous squished string spanning from the outcome-derivation
  call through the write case's opening brace and first statement — e.g.
  `"camerabox::CbDockLockOutcome outcome = camerabox::cb_dock_lock_outcome( act,
  camerabox::cb_dock_lock_may_actuate(), ...); switch (outcome) { case
  camerabox::CbDockLockOutcome::Write: { const double delta_ms = ...; cb_apply_lock_latency_ms(...);"`.
  Verify NO comment sits between the pieces you're concatenating (a doc comment placed BEFORE the
  span is fine; one placed INSIDE it breaks a raw/non-stripped `output.contains(...)` check).
- A **uniqueness-count + branch-slice check** (old: `MONITOR_BRANCH_START = "else if (act.apply)
  {"`, sliced to the next `"} else if"`) becomes `"case camerabox::CbDockLockOutcome::Suggest: {"`
  sliced to the next `"case camerabox::CbDockLockOutcome::RailWarn:"` (or whichever case follows).
  Keep using the comment-stripped variant for this one — the branch's own explanatory comment can
  legitimately mention the banned function NAME in prose ("no cb_apply_lock_latency_ms()").
- A **"reachable from exactly one place" invariant** is STRONGER when anchored on the exact CALL
  FORM with its real argument (`cb_apply_lock_latency_ms(act.new_delay_ms)`) and counted
  everywhere in the file (`.matches(...).count() == 1` / pwsh
  `([regex]::Matches($text, [regex]::Escape(...))).Count`), rather than a bare function name —
  the function's own DEFINITION and a comment's bare mention (`cb_apply_lock_latency_ms()`, no
  args) won't collide with the parameterized call form, so no comment-stripping is needed for
  this one either.

## Verify offline BEFORE re-pushing — a throwaway Python script, no compiler needed

Since two of the three anchors can't be exercised locally (the probe feature is Tier-0-banned to
build; the pwsh gate only runs on the Windows CI runner), write a short Python script that
replicates the EXACT string algorithm (`" ".join(s.split())` for the Rust `squish()`;
`re.sub(r'\s+', ' ', s)` for pwsh's `-replace '\s+', ' '`; the line/block-comment stripper for
`strip_cpp_comments()`) and run your candidate anchor strings against the REAL current
`sync-test-output.cpp` content read straight off disk. This costs one throwaway script and a few
seconds, versus a full CI round-trip (the `CI` job alone took ~4 minutes to reach the failing
test; `Windows genlock FAST` circles back separately) to discover the same mismatch. Confirmed
effective in the #955 fix-up: the script caught that all three redesigned anchors matched BEFORE
the second push, which then went green on the first try.
