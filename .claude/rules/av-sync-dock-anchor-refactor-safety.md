---
paths:
  - "vendor/av-sync-dock/src/sync-test-output.cpp"
  - "vendor/av-sync-dock/src/sync-test-output-*.cpp"
  - "vendor/av-sync-dock/src/sync-test-output-internal.hpp"
  - "vendor/av-sync-dock/test/dock-output-source.ps1"
  - "tests/support/av_sync_dock_output.rs"
  - "tests/av_sync_dock_output_sources_1386.rs"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "vendor/av-sync-dock/src/camera-box-audio-worker.hpp"
  - "tests/genlock_preload.rs"
  - ".github/workflows/windows-genlock.yml"
  - ".github/workflows/windows-genlock-fast.yml"
---

# Refactoring the dock-lock decision chain (`sync-test-output-audio.cpp` since issue 1386) breaks stale text-anchor
# gates in THREE places, not one — and TWO of the three are invisible to local Tier-0 checks (#955)

**Before restructuring the `if (act.apply && ...) { ... } else if (act.apply) { ... } else if
(...) { ... } else { ... }`-shaped dock-lock decision chain in
`vendor/av-sync-dock/src/sync-test-output-audio.cpp` (e.g. #955's switch-on-`cb_dock_lock_outcome()`
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
runner) — a session touching only the dock output sources and passing every local Tier-0 check has
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
dock output sources read straight off disk (the union, see the issue-1386 section below). This costs one throwaway script and a few
seconds, versus a full CI round-trip (the `CI` job alone took ~4 minutes to reach the failing
test; `Windows genlock FAST` circles back separately) to discover the same mismatch. Confirmed
effective in the #955 fix-up: the script caught that all three redesigned anchors matched BEFORE
the second push, which then went green on the first try.

## A NEW guard added to a camera-box emit site needs its OWN pwsh mirror too — check for a sibling precedent first (#999/#1005, 2026-08-11)

Adding a brand-new `if (camerabox::cb_<something>(...))` guard around a camera-box `sync_found`
emit site (not a decision-chain refactor — a genuinely NEW check, e.g. #1005's
`cb_corrected_video_ts_is_valid`) is easy to prove locally with a Rust structural text-anchor test
(`tests/av_sync_dock_qr_patch_guard.rs`-style: assert the guard call text appears exactly N times,
assert the OLD unguarded form is gone) — but that Rust test alone is NOT the double coverage this
file's own existing pattern already establishes. The dock output compiles ONLY via the
`windows-genlock*.yml` pwsh gate (no local compile path at all, not even syntax), so the FULL
established convention for any guard added here is BOTH a Rust structural test AND a matching pwsh
`[regex]::Matches(...).Count` anchor in **both** `windows-genlock.yml` and
`windows-genlock-fast.yml` — see the existing `si.gate_convention = true;` count==2 check (#999)
sitting right next to where a #1005-style new guard would go. A review caught this gap live: the
#1005 wiring landed with its Rust-side proof but no pwsh-side one, an inconsistency with its own
sibling in the exact same source region.

**Before considering a new-guard change complete: grep the SAME source region's pwsh block in both
workflow files for an existing sibling guard's anchor shape, and add the matching pair (a
`Count -ne 2` presence check for the new guard text, plus a negative check that the OLD/replaced
form is gone) to both files — verified with the throwaway-script technique above before trusting
it.**

## The audio callback's per-channel decode has its own two-language anchor pair (issue 1367)

`st_raw_audio_camera_box` no longer mixes channels: it calls `cb_ensure_audio_picker`, pushes every
channel's plane to `camerabox::ChannelMarkerPicker` (`camera-box-channel-pick.hpp`), logs a pick
switch through `cb_note_channel_switch` and ends with `cb_audio_diag_tick`. Its anchors live in
`tests/av_sync_dock_channel_pick_1367.rs` AND the pwsh step "Assert dock decodes the marker per
channel (issue 1367)" in both windows-genlock workflows, which check the same list over the same
three brace-balanced function bodies (the helper calls and their order, the plane hand-off, the
picker lifecycle, the switch-log call into the pure `CbChannelSwitchLog`,
the banned mixdown forms, the diag line's appended `marker_channel=%zu channel_clusters=%s
channel_switches=%llu` and its argument order).
Moving or renaming any of those means editing all three places; the Rust test uses the shared
`tests/support/cpp_source.rs` (comment-stripped `body_of`), while the pwsh slice keeps comments.

## The audio decode worker's anchors (issue 1381)

`tests/av_sync_dock_audio_worker_1381.rs` + the pwsh step "Assert dock audio decode runs off the audio
thread (issue 1381)" in BOTH windows-genlock workflows pin the gate + FIFO copy in `st_raw_audio` /
`cb_audio_gate_and_publish`, the worker handlers (`st_audio_block_run`, `st_audio_block_gap`,
`st_audio_session_end`, `cb_audio_session_begin`, and `cb_audio_forget_lock` that both session
handlers call), `cb_refresh_measure_source`, the ONE declaration of `CAMERA_BOX_MEASURE_SOURCE_NAME`
(in `camera-box-audio.hpp`, with `sync-test-dock.cpp` defining its ASRC name from it and none in
the dock output sources), the worker lifecycle and the diag line's appended `decode_ms_max=
decode_ms_sum= audio_dropped= decode_resets= audio_publish_max_us=`. The per-channel step's
diag-ARGUMENT anchor now ends at `(unsigned long long)st->cb_switch_log.total,` (the new arguments
follow it) in the Rust test and both pwsh copies. Replay all of them from the YAML text before pushing: a python squish
(`re.sub(r"\s+", " ", ...)`, comments kept) + the brace-balanced `Get-DockBody`, reading the needle lists
out of each step's `foreach (... in @(...))`.

## The dock output is split over several files; every anchor reads them as ONE source (issue 1386)

`sync-test-output.cpp` passed 2000 lines, so the output is now four files:

| File | What lives there |
|---|---|
| `sync-test-output.cpp` | the obs_output_info callbacks (`st_create` / `st_destroy` / `st_start` / `st_stop`) and `register_sync_test_output` |
| `sync-test-output-video.cpp` | the video path: `st_raw_video`, the decode worker (`st_video_decode_job_run`), the QR decodes, the marker search, `cb_video_qr_record` / `cb_refresh_measure_source`, norihiro's `sync_index_found` list |
| `sync-test-output-audio.cpp` | the audio path: `st_raw_audio`, `cb_audio_gate_and_publish`, the audio worker handlers, `st_raw_audio_camera_box` (the pairing, the lock audit, the #942/#955 dock-lock chain), `cb_audio_diag_tick`, `cb_apply_pairing_recovery`, norihiro's demod |
| `sync-test-output-internal.hpp` | the includes, the tunable `#define`s, `struct sync_test_output` and the other shared structs, and the declarations of the cross-TU functions |

- **Linkage.** Every TU wraps its code in ONE namespace, `av_sync_output`. Only a function another
  TU calls has external linkage, and it is declared once, at the bottom of the internal header,
  with UNNAMED parameters, so an anchor keyed on the named signature can only find the definition.
  Everything else stays `static`. A cross-TU function's definition has no `static`, so its body
  anchor is `void st_raw_audio(void *data, struct audio_data *frames)`, not `static void …`.
  Making another function cross-TU means dropping its `static`, adding its declaration, and
  editing its signature anchors in the Rust test and in BOTH workflows.
- **One reader per language.**
  - Rust: `tests/support/av_sync_dock_output.rs` (`source()` returns the raw union; the tests
    squish and strip it themselves), and `cpp_source::unique_body_of` for a body.
  - pwsh: every dock step in both workflows dot-sources
    `vendor/av-sync-dock/test/dock-output-source.ps1`, then calls `Get-DockOutputSource` (the
    union, whitespace collapsed, comments kept) and `Get-DockBody $output '<sig>'` (brace-balanced).
  - Both read `sync-test-output.cpp` plus every `sync-test-output-*.cpp` / `-*.hpp`, in ordinal
    file-name order. The public `sync-test-output.hpp` (the dock UI's interface) is not read.
  - Both body extractors FAIL when a signature occurs more than once in the text they are given,
    so a second spelling can never silently win over the definition. They count in different
    text: `unique_body_of` in what the Rust test passes (the comment-stripped union for the tests
    that strip comments), `Get-DockBody` in the union with comments KEPT. So the pwsh one is the
    stricter: a named signature spelled in a comment fails only there. Declarations are safe in
    both because they carry no parameter names.
  - A function MOVING between the output files therefore never breaks an anchor: the pwsh
    mutation run moved `cb_audio_forget_lock` to the video file and every step stayed green.
- **Never read one output file on its own.** A new anchor uses the helpers.
  `tests/av_sync_dock_output_sources_1386.rs` enforces it:
  - fails: a non-comment workflow line naming one output file (judged by the union's own file
    rule, in any quote style or API); a glob over them (`sync-test-output-*.cpp`, which skips the
    lifecycle file and the internal header); a Rust path literal to one output file;
  - allowed: a `#` comment pointer, the public `sync-test-output.hpp`.

  It also pins:
  - the union's `.cpp` set equals the `src/sync-test-output*.cpp` entries of `PLUGIN_SOURCES` in
    `vendor/av-sync-dock/CMakeLists.txt`, so a new TU is compiled AND read;
  - every TU includes the internal header, then `plugin-macros.generated.h` (the `blog` wrapper),
    then opens the namespace;
  - the pwsh twin's whole `Where-Object { … }` file filter, verbatim, next to the Rust
    `is_output_file` table: a clause changed on one side only fails.
- **Verify locally (Tier-0)**, since the first compile of this C++ is CI's "Compile-check
  av-sync-dock":
  - `g++ -std=c++17 -fsyntax-only -Wall -Wextra` each TU with the two stub headers
    (`av-sync-dock-decode-worker.md`);
  - `g++ -c` each TU to an object and cross-check with `nm -C`: every `av_sync_output::` symbol one
    object needs is defined by exactly one object, and `register_sync_test_output` stays unmangled.
    Under `pipefail`, `nm | grep -q` SIGPIPEs on a big symbol table and reads "not defined": write
    the `nm` output to a file first;
  - run the REAL pwsh steps, not a replica. Extract each dock step's `run:` block from the YAML,
    prefix `$ErrorActionPreference = 'stop'` (what GitHub prepends), and run
    `pwsh -NoProfile -Command ". '<step.ps1>'"` from the tree root with a portable pwsh 7 unpacked
    in the scratchpad. The worktree guard refuses a direct `pwsh` call, so drive it from a script
    FILE. Before trusting a green run, run the OLD steps against the OLD tree: that must be green
    too, and the OLD steps against the NEW tree must fail;
  - compile and run the one or two Rust anchor tests that cover the change with plain `rustc --test`
    / `clippy-driver -D warnings`, and cover the others with a Python replica of their exact
    needles (the owner's limit on local test runs). The av_sync_dock tests read
    `env!("CARGO_PKG_VERSION")` at compile time too: set `CARGO_PKG_VERSION=0.0.0` beside
    `CARGO_MANIFEST_DIR`. A `fmt --check && rustc …` chain that stops at fmt leaves the OLD binary
    to run: compile in its own call, to a new output name.
  - **Prove a new anchor in BOTH languages without touching `vendor/` (issue 1381).** Copy only
    `vendor/av-sync-dock/{src,test}` into a scratch tree, apply one mutant there (a python script
    with a count-1 `replace`), then run the extracted pwsh step with that tree as cwd
    (`dock-output-source.ps1` resolves `../src` from its own path, so it reads the mutant) and
    compile the Rust test with `CARGO_MANIFEST_DIR=<scratch tree>` (the union helper reads the
    mutant). Every mutant must fail both; the real tree must pass both. Worked set: the eight
    pre-fault wiring mutants in `av-sync-dock-decode-worker.md`.
  The dock change is live only after a FULL-bundle Windows deploy (`rig-state-inspection.md`).
- **Size budget.** At the split `sync-test-output-audio.cpp` is 1021 lines and
  `st_raw_audio_camera_box` about 297 (their bodies unchanged). The next addition to the audio
  path first moves norihiro's demod into its own TU: `operator-`, `int16_to_complex`,
  `identify_audio_index_max`, `crc4_check`, `st_raw_audio_decode_data`,
  `st_raw_audio_test_preamble` and the non-camera-box tail of `st_raw_audio`.
