---
paths:
  - "scripts/launch-obs-genlock.sh"
  - "scripts/obs-guarded-launch.ps1"
  - "scripts/obs-self-heal-install.sh"
  - "scripts/strih/**"
  - "tests/launch_obs_genlock.rs"
  - "tests/strih_ahk_respawn_774.rs"
---

# OBS launch-path contract (.lnk primary, per-box params) — #774/#775

Every automated OBS (re)launch on strih/stream goes through the box's **Start-Menu shortcut**
`OBS Studio.lnk`, NOT a bare `obs64.exe` — the shortcut carries the box's per-box params (strih:
`--enable-media-stream --verbose`, needed by the interkom VDO.ninja Browser source; a bare launch
drops them → "Permissions denied" rendered on program output, live incident 2026-07-15). This
contract is now **test-pinned across every path** — do not silently revert it:

- **`scripts/launch-obs-genlock.sh`** (`build_launch_program`): `.lnk` primary
  (`if (Test-Path $lnk) { Start-Process -FilePath $lnk }`), bare `$exe -WorkingDirectory bin\64bit`
  ONLY in the `else` fallback with a LOUD "params will be MISSING" warning — in BOTH the initial
  launch and the #786 redraw. Pinned by `tests/launch_obs_genlock.rs`
  (`program_launches_lnk_as_primary_bare_exe_only_as_fallback_775`,
  `redraw_relaunch_also_prefers_lnk_775`). The older `program_launches_with_bin64_cwd` pins only
  the FALLBACK — it is NOT the primary-path guard.
- **`scripts/obs-self-heal-install.sh`** (#411, ships disabled): reuses `build_launch_program`
  VERBATIM, so it inherits `.lnk`. Pinned by `self_heal_reuses_wrapper_launch_program_775`. Never
  fork a second launch path here.
- **strih AHK respawn** is versioned at **`scripts/strih/NL_STARTUP.ahk`** (was live-only, #774).
  `app1_path` = the `.lnk`; window match is PROCESS-based (`ahk_exe obs64.exe`, never a title, so
  an OBS title change can't stop respawn); `#SingleInstance Force` is intentional (clean
  double-start replace + re-arms `SafeLoop=1` on relaunch). Deploy is a **win-\* MCP** step per
  `scripts/strih/README.md` (never ssh for the GUI/AHK) — read that README before deploying it,
  and diff the committed copy vs the live `D:\_APPS\NL_STARTUP.ahk` for fidelity FIRST.

**`scripts/obs-guarded-launch.ps1` is the exception, and it is CORRECT:** it launches bare
`obs64.exe` with **NO per-box args**. That is fine because it is what STREAM's `OBS Studio.lnk` is
retargeted to (stream does not need `--enable-media-stream`; the script's job is the #786 ASIO
audio-buffering launch gate, not param-carrying). Do NOT "fix" it to add args blind — it is a
latent footgun ONLY if it were ever made **strih's** launcher (it would drop
`--enable-media-stream`). If strih ever needs the guarded launcher, teach it strih's params first.

Standing docs already covering related facts: obs-ops SKILL §144 ("every relaunch MUST go through
`launch-obs-genlock.sh`; do NOT hand-roll a Start-Process"); `.claude/rules/rig-state-inspection.md`
(the per-box `.lnk` TargetPath+Arguments live-resolve, the box-specific `.lnk` locations differ).

## AHK stop-first/restart-last MUST wrap EVERY obs64-kill site, not just the redraw loop (#1272)

`build_launch_program`'s AHK bracket (`$ahkStopped`/`ahk_stop_ps`/`ahk_restart_ps`, has_ahk=1 only)
used to wrap ONLY the #786 audio-buffering redraw loop's own kill+relaunch — the `--force`
kill_block at the very TOP of the program had zero AHK involvement. On strih, right after
`scripts/deploy-genlock-fleet.sh`'s own step 8 restarts+verifies AHK, a `--force` relaunch killed
obs64 with AHK still watching: `NL_STARTUP.ahk` respawned a duplicate obs64 within seconds via the
same `.lnk`, racing this wrapper's own relaunch and failing the `(3c)` `#978` session-visibility
gate ("expected exactly 1 obs64 process, found 2") — the live 2026-09-02 incident.

**Fixed contract, test-pinned by `tests/launch_obs_genlock.rs`
(`force_stops_ahk_before_obs64_kill_and_restarts_after_verify_1272`,
`ahk_stopped_declared_once_and_restart_emitted_once_1272`):** `$ahkStopped = $false` is declared
ONCE, at the very top of the program (right after `$ErrorActionPreference = 'Stop'`) — never
re-declare it deeper in the file, a second declaration silently wipes out an earlier stop. The
`--force` kill_block prepends the existing `ahk_stop_ps` snippet (has_ahk=1 only) before its own
obs64 kill line. The AHK restart (`ahk_restart_ps`) fires exactly ONCE, after the WHOLE
launch+audio-verify sequence closes (covering both the `$guardedLnk` verify-only branch and this
wrapper's own redraw-loop branch), before the `(3c)` session-visibility gate — which itself asserts
AHK's own SessionId and needs AHK back by then. The pre-existing per-iteration `ahk_stop_ps` call
inside the redraw loop stays as a harmless idempotent no-op safety net (its own
`if (Get-Process AutoHotkey64 ...)` guard).

**Follow-up fix (same ticket, review finding):** moving the AHK-stop to the top widened the window
where an EARLY failure exit (exe not found = `exit 5`, obs64 never starting on the INITIAL launch =
the first `exit 6`) would now permanently disable the AHK respawn watcher — a regression those two
exits never had before (only the pre-existing redraw-loop exit paths had that accepted limitation).
Fixed with `ahk_best_effort_restart_ps` — reuses ONLY `ahk_resolve_and_relaunch_ps` (never the
fail-loud/`exit 9` wrapper, so the real exit 5/6 code stays the one reported) — emitted immediately
before each of those two exits. Pinned by
`early_exit_failures_attempt_best_effort_ahk_restart_before_exiting_1272`.

**Gotcha this surfaced — reusing a shared PS primitive at a NEW call site can retroactively make a
PREVIOUSLY-unique `.find()` anchor ambiguous (the same class `ci-testing-gotchas.md` already
documents for #867, now a third instance).** `ahk_relaunch_ps` (embedding the literal
`"$ahkRelaunchVerified = $false"`) is reused by `ahk_restart_ps` (the real, success-logging,
fail-loud restart) AND now also by every `ahk_best_effort_restart_ps` call site — so a test that
used to `.find()` that string (there was only ONE occurrence) had to switch to `.rfind()` (the real
restart block is always the LAST occurrence, since the early-exit sites sit earlier in the text)
PLUS an explicit check that its own unique success marker (`"AHK watchdog restarted via"` — never
duplicated, since the best-effort snippet deliberately uses a different `Write-Warning` line) sits
between the anchor and the fail branch, proving `.rfind()` genuinely landed on the intended
occurrence rather than merely the last of several by coincidence
(`program_ahk_restart_is_verified_and_fails_loud_867`, updated in the same PR). **Before reusing
ANY existing PS/bash snippet at a new call site, grep the whole file for every LITERAL string that
snippet contains and check every test's `.find()`/`.rfind()` anchor on those strings still targets
the intended occurrence — not just "does the assertion still technically pass".**

## Verifying a launch-obs-genlock.sh / obs-self-heal-install.sh change from a worktree-isolated worker: EXECUTE the script, don't just source it

`.claude/rules/ci-testing-gotchas.md`'s "worktree-isolated worker cannot run a sourced-bash-lib
test" section already documents that `bash -c '…source lib…'` is refused inside a worktree.
**What that section does NOT call out: running the script FILE DIRECTLY as an executable — `bash
scripts/launch-obs-genlock.sh --box strih --force` (or `scripts/obs-self-heal-install.sh --box
strih`) — is NOT refused**, and both scripts have a real CLI `main()` that prints their FULL emitted
plan/program to stdout (they need no rig, no MCP, no network — pure string builders). This is a much
stronger local verification path than the `python3 -c` fallbacks the ci-testing-gotchas.md section
lists: capture the plan to a file, then Python-simulate every `.find()`/`.rfind()`/`.count()`
assertion in the corresponding `tests/*.rs` file against the REAL emitted text (extract the raw
program block between the plan's own two `# ----...` delimiter lines to get exactly what
`run_sourced("build_launch_program ...")` would return). This proved BOTH a genuine pre-fix RED
(exact byte offsets showing the bug) and a genuine post-fix GREEN for issue 1272 without any cargo
compile, and caught the `.find()`-anchor-ambiguity gotcha above before it ever reached CI. Also
works to diff an OLD committed version against the new one: `git show <sha>:scripts/foo.sh >
/tmp/old.sh && bash /tmp/old.sh --box strih` runs the historical version standalone (if it sources
a sibling lib via a `HERE`-relative path, recreate that relative layout under `/tmp` first — copy
the lib file(s) to the same relative subpath before running).
