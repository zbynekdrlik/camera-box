---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/rig-mode.sh"
  - "tests/rig_mode.rs"
  - "tests/harness_*.rs"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

## GOTCHA — editing `scripts/recording-e2e.sh` (OR `scripts/rig-mode.sh`) can silently break OTHER test files' static anchors

Many separate `tests/harness_*.rs` / `tests/rig_mode.rs` files independently `.find()`/`.split()`
literal substrings/adjacency in `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh` (a banner like
`"[5/8] StartRecord"`, a bare function-name anchor like `.split("do_event()")`, or structural
adjacency like `fi\ntrap cleanup EXIT`) to pin ordering/structure — the same static-string-assertion
model `tests/harness_recording_e2e_cleanup_resilient.rs` (#328) established, now reused across many
unrelated features in BOTH files (`#137` av-restart-sync, `#286` all-cambox burn targets, `#649`
StopRecord ordering, `#524`'s `event_mode_calls_stop_stray_recordings_guard`, etc.). A new comment
or code line you add to EITHER file CAN accidentally (1) duplicate a literal anchor another test's
`.find()`/`.split()` relies on — `.find()` returns the FIRST match in the WHOLE file, and
`.split(X).nth(1)` grabs the segment AFTER the SECOND occurrence of `X`, not the occurrence near
your own edit — or (2) break a textual adjacency two unrelated tests hard-code (e.g. inserting a
line between an `if` block's closing `fi` and the following `trap cleanup EXIT HUP INT TERM` line).

**Confirmed live (#649, 2026-07-10, recording-e2e.sh):** a new `cleanup()` comment containing the
literal text `[5/8] StartRecord` broke 3 tests in `tests/harness_av_restart_sync_gate.rs` (the
`#137` gate, which anchors on that exact string to slice "everything before the main record
step"); and adding new variable declarations directly before `trap cleanup EXIT HUP INT TERM` broke
`recording_e2e_all_cambox_extends_burn_targets_to_every_strih_input_286` in
`tests/harness_recording_e2e_paths.rs` (which hard-codes that the `#286` ALL_CAMBOX block's
closing `fi` is immediately followed by `trap cleanup EXIT`).

**Confirmed live (#722, 2026-07-13, rig-mode.sh — the SAME class, a DIFFERENT file):** a new
comment reading "...sent by `do_event()` AFTER this function returns..." — placed INSIDE
`event_mode_assert()`, BEFORE the real `do_event() {` definition — broke
`event_mode_calls_stop_stray_recordings_guard` in `tests/rig_mode.rs`, which extracts the function
body via `s.split("do_event()").nth(1)`. With TWO occurrences of the literal text `do_event()` (the
new comment, then the real definition), `.nth(1)` grabbed the segment BETWEEN them (a few lines of
comment) instead of the real function body — the test's failure message printed the WRONG slice
(the comment text), which is the tell: if a `.split()`/`.find()`-based test's failure output looks
like the wrong region of the file, suspect a duplicated anchor, not a logic regression. Fix: reword
the comment so it never contains the bare function-name-with-parens text (e.g. "the EVENT-mode
caller" instead of "`do_event()`") when that text sits BEFORE the real definition in the file.

**Confirmed live (#832, 2026-07-27) — the anchor you break can be a test YOU are writing IN THE
SAME PR, not just a pre-existing one.** Adding an explanatory comment right before a call site
(`# #832: recording-verdict-on-imag.sh has its OWN independent IMAG_BOX default...` right before
`"$HERE/recording-verdict-on-imag.sh"`) created a SECOND occurrence of the literal script name —
a NEW test's own `s.find("recording-verdict-on-imag.sh")` then latched onto the comment (the FIRST
occurrence) instead of the real invocation a few lines later, so its assertion window never
reached the actual call. Same failure shape hit a second, unrelated anchor in the same PR:
`rig-mode.sh`'s pre-existing explanatory comment already said `` `scripts/drift-guard.sh
--check-imag` `` (backticked, no `bash ` prefix) several lines above the REAL
`bash scripts/drift-guard.sh --check-imag ...` call — a naive `.find("drift-guard.sh
--check-imag")` grabbed the comment, not the call. Fix in both cases: anchor on a substring that
can ONLY appear at the real call site (the quoted `"$HERE/...").sh"` invocation form, or a
`bash ` / other prefix the comment never uses) — never a bare script/flag name that a nearby
comment could also contain. **The general rule: when you write a NEW static-anchor test against
one of these two files in the SAME commit/PR that adds explanatory prose near the call site,
verify your OWN anchor is unique too — you can self-collide, not just collide with someone else's
existing test.**

**Mitigation:** after ANY edit to `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh`, run the FULL `cargo test` suite —
not just your own new/targeted test file — before pushing. A failure elsewhere in the suite right after touching this
file is very likely a textual collision, not a real regression — grep the failing test's
`.find(...)` argument (or the surrounding slice logic) to see which literal string or adjacency
moved, then reword your new text (or relocate it) so it no longer matches/breaks that anchor.

**Prevention pattern (#675) — ADD new behavior via a sourced helper, never edit the literal
anchor line itself.** When the new logic needs to run right after an EXISTING pinned line (e.g.
"verify camera-box came back active after `systemctl restart camera-box`"), don't touch that
line's text at all — append a call to a NEW function in a NEW `scripts/lib/*.sh` file via command
substitution on the line(s) immediately after it (`$(my_new_helper_cmds "label")`). The static
anchor test suite reads ONLY `scripts/recording-e2e.sh`'s own text, never a sourced lib's — the
function CALL is invisible there (compile-time text), but its expanded OUTPUT still lands in the
final remote command at actual runtime. This adds a whole new capability with ZERO risk to any
existing `.find()`/adjacency assertion, and keeps the new logic in ONE sourced source of truth
(mirrors `rig_test_dropin_clear_cmds` in `scripts/lib/rig-test-dropin.sh`, #309) instead of
duplicating it inline at every call site. See `scripts/lib/camera-box-restart-verify.sh` for a
worked example — 3 call sites (cam1, the ALL_CAMBOX loop, cam2/painter) each gained a poll+retry
step with the ORIGINAL restart lines byte-for-byte unchanged, verified by the full `cargo test`
suite staying green (115/115 binaries, no anchor collisions).

**Variant (#712) — WRAPPING an anchored line's execution mode (not just appending after it) is
ALSO safe, PROVIDED you check every sibling test uses SUBSTRING `.find()`, never a full-line/exact
match.** #712 needed the cam3/4/5/6 ALL_CAMBOX restore loop's ssh call to run BACKGROUNDED
(`( timeout ... ssh ... ) &` instead of a bare foreground call) so 4 boxes restore concurrently
instead of sequentially — this touches the anchor line itself, not just text after it. Before
doing this: `grep -rn '\.find(' tests/*.rs` for every string that could live inside the region
being touched, and confirm each is a `body.contains(...)`/`region.find(...)` SUBSTRING check
(unaffected by a `(`/`) &` wrapper on the same logical command) rather than something that
requires the anchor to be the literal FIRST token on its line or hard-codes exact whitespace. The
new PID-collection + wait logic itself went into a new sourced lib
(`scripts/lib/cambox-parallel-restore.sh`), same as the #675 pattern — only the wrap-in-parens
touched the anchored region directly, and it was verified safe (grep first, then the full
`cargo test` suite green after) rather than assumed safe.
