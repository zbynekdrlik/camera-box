---
paths:
  - ".github/workflows/*.yml"
  - "tests/*.rs"
  - "tests/python/*.py"
  - "scripts/version-integrity-gate.sh"
  - "scripts/drift-guard.sh"
  - "scripts/dantesync-gate.sh"
  - "scripts/clock-offset-guard.sh"
  - "scripts/*_calibrate.py"
---

# CI + bash-test-harness gotchas (#826)

## Splitting ONE combined multi-`--box`-style gate call into TWO calls silently stales a window-bounded "all boxes in this window" test (#888)

When a gate step invokes the same tool once with several `--box`/similar repeated args in one
call (e.g. `render-budget-gate.py --box strih=... --box stream=... --box imag=...` in
`scripts/recording-e2e.sh`'s `[4d/8]`), and you deliberately split it into TWO separate
invocations (one term needs different pass/fail handling than the others — #888's imag-report-
only relaxation is the concrete case), any EXISTING test that anchors on the first arg and reads
a fixed byte window (`&s[call.saturating_sub(200)..(call + 500).min(s.len())]`) expecting to find
ALL the args in that ONE window will not error loudly — it will either still pass by coincidence
(if the window happens to be wide enough to still reach the second call) or fail with a confusing
"missing box" message that reads like the split broke wiring, when really the test just encodes
the OLD combined-call invariant. **Before splitting a multi-arg call like this, grep every test
file for the FIRST arg's literal (e.g. `--box "strih=`) and re-read what its window assertions
actually check** — a positive assertion for an arg that moved to the OTHER call needs to become
either a negative assertion (`!window.contains(...)`, proving the split, as in
`tests/harness_imag_topology.rs`'s `render_budget_gate_strih_stream_call_site_no_longer_includes_imag_888`)
or a brand new test scoped to the NEW call's own region (bounded from the first call's closing
`fi` to the next step's banner, as in `tests/harness_render_budget_imag_report_only_888.rs`) —
never left as a stale assertion that happens to still pass.

## Raising a shared formula constant (a `PHASE_SYNC_FLOOR_MS`-style floor/cap) breaks EVERY hardcoded literal test expectation that assumed the old value -- across BOTH languages (#707)

Several "pure kernel" constants in this repo are deliberately duplicated across THREE places:
the Rust kernel (`src/phase_sync.rs`, `src/qpsk_marker.rs`, ...), a CLI-boundary parity-check test
file that pins the SAME numeric vectors the kernel's own unit tests use
(`tests/harness_phase_sync_gate.rs` etc.), and a Python mirror + its own pytest suite
(`scripts/phase_sync_calibrate.py` / `tests/python/test_phase_sync_calibrate.py`). Changing the
constant's VALUE (not just its doc comment) means every one of those three layers has hardcoded
LITERAL expected offsets computed from the OLD value, and all of them go RED the moment the
constant changes -- not because the fix is wrong, but because the fixture data is now stale.

**Before bumping a shared constant like this, grep for the constant name across the WHOLE repo**
(`grep -rn CONST_NAME --include=*.rs --include=*.py`), not just the two files that define it --
`#707` needed literal updates in `src/phase_sync.rs`'s OWN test module, `src/bin/phase-sync-gate.rs`'s
doc-comment JSON examples, `tests/harness_phase_sync_gate.rs` (5 tests), AND
`tests/python/test_phase_sync_calibrate.py` (the `ApplyLatencyHappyPath`/`ApplyLatencyRollback`
direct-call tests AND the `TestCLI` tests that mock the compiled gate binary's return value with
old-floor literals like `{"NDI cam5": 3, "NDI cam1": 13}`). Missing any ONE of these means `cargo
test`/`pytest` goes red on a file you never touched, which reads like a regression when it's
actually just a stale fixture.

**When only the ABSOLUTE value moved and the underlying FORMULA/invariant is unchanged** (e.g. a
"slowest camera pinned to floor, others get floor+deficit" additive formula), add ONE
floor-agnostic invariant test (asserts a DIFFERENCE or RATIO, never an absolute literal) alongside
the value-specific ones -- it never needs updating again on the next floor bump, and it is exactly
the test that catches a REAL regression (the formula itself breaking) as opposed to a cosmetic
one (the floor's absolute value changing on purpose).

## Getting a genuine pre-fix RED commit when you already edited a file straight to GREEN

`regression-test-first.md`'s RED-before-GREEN commit order still applies even when you (or a
tool like `cargo fmt`) already produced the fully-fixed file in one pass. Two-step recovery, no
history rewrite needed: (1) temporarily `Edit` the file back to the OLD value/behavior while
KEEPING the new test(s) you just added, run the test suite to confirm the new test(s) actually
fail against the old code (a genuine RED, not just "the test wasn't run yet"), commit that state;
(2) `Edit` the file forward to the real fix again, re-run, confirm GREEN, commit. This is the
same "recreate the pre-fix state, commit RED, re-add the fix, commit GREEN" pattern the top-level
CLAUDE.md GOTCHA documents for a different failure mode (accidentally staging a later hunk early)
-- reusable any time the fix was written before the RED commit was made.

## A pushed commit can silently CANCEL the in-flight hardware E2E gate — check the concurrency group before your second push

`full-path-e2e.yml` has:

```yaml
concurrency:
  group: full-path-e2e-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true
```

keyed on the PR number, not the commit SHA. Once dev→main PR #704 (the long-lived dev-train PR
this repo pushes through) has an in-flight `pull_request`-triggered "Full-path E2E" hardware run,
**any further push to `dev` — even a docs-only commit with zero code changes — cancels that
in-flight run** the instant GitHub registers the new HEAD SHA on the PR. This is the SAME class of
gotcha CLAUDE.md's `linux-genlock.yml` GOTCHA already documents (a later push cancels a run keyed
on `github.ref` "even if the later push doesn't itself touch vendor/\*\*") — but it applies
separately to `full-path-e2e.yml`, keyed on the PR number instead of the ref, and it is easy to
forget because a "just a docs commit" push feels harmless.

**Before any push while a hardware E2E run might be live:** `gh run list --branch dev --json
databaseId,workflowName,status,event | jq` and check whether a `"Full-path E2E..."` run for
`"event": "pull_request"` is `"status": "in_progress"`. If it is, and your pending commit is not
itself needed to fix that run, HOLD the push until the hardware run reaches a terminal state — a
worker dispatch does not own the hardware gate (the supervisor does, per the autopilot-worker
brief), and cancelling real rig time for an unrelated docs/log commit wastes it for nothing.

**In a two-(or-more)-worker shared checkout (see the top-level CLAUDE.md's shared-checkout GOTCHA),
your OWN just-triggered E2E run can get cancelled again moments later WITHOUT you pushing anything
further (2026-07-30, issue 889 session).** Confirmed live: worker A held its push until an
in-flight E2E (SHA X) reached a terminal state, then pushed its own final commit (SHA Y) once
that was true — correctly following the rule above. The fresh E2E run for SHA Y still got
`cancelled` within ~10 minutes with `git log origin/dev` showing NO new commit on top of Y at all.
The cause: worker B (a different ticket, same shared PR) pushed its OWN commit in the same window,
which per the concurrency group above (keyed on the PR NUMBER, so every worker on this PR shares
ONE slot) cancels whichever `pull_request` E2E run is currently occupying it, whether or not that
run belongs to "your" SHA. **This is expected turbulence in a multi-worker PR, not a sign your
push broke something** — do not `gh run rerun` it yourself to "fix" it (that is still the
supervisor's call, per the same brief), and do not treat the cancellation as evidence of a defect
in your own commit. The regular (non-hardware) `CI` workflow run for your SHA is the one that
actually proves your code — if IT shows `success`, your work is verified regardless of how many
times the shared PR's hardware slot gets bounced around by concurrent pushes before the
supervisor gets a clean run through it.

## `run_sourced`-style bash test harnesses inherit the sourced script's OWN `set -e`

`tests/version_integrity_gate.rs`'s `run_sourced` helper (and any similar harness in this repo
that does `. "$SCRIPT"` then appends a test body) sources the target script into the SAME shell —
so if the script itself starts with `set -euo pipefail` (as `version-integrity-gate.sh` and
`drift-guard.sh` both do), that `-e` **persists into the harness's own remaining script** even
though the sourced file's own "stop here when sourced" guard returns before running `main`. A test
body that calls a sourced function and expects to inspect its NON-ZERO return code (`func ...;
echo RC=$?`) never reaches the `echo` — the leaked `-e` aborts the whole harness the instant the
called function returns non-zero, and the harness's own `assert!(out.status.success(), ...)` then
mis-reports "sourced harness exited non-zero" with the function's OWN print already on stdout
(easy to misread as "the function itself failed" rather than "the test harness died before it
could report").

**Fix (already applied): put `set +e` immediately after the source, before the caller's body** —
`format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}")`. This only matters for a test body
that exercises a NON-zero-return scenario (a DRIFT/UNKNOWN assertion); every prior test in this
file only ever exercised return-0 scenarios, so the bug was silent until #826's verdict-function
tests (which deliberately assert DRIFT/UNKNOWN) hit it. Any future sourced-harness test in this
repo that asserts a specific non-zero exit code from a sourced function should sanity-check this
the same way (a bare `func; echo RC=$?` that never prints `RC=` is the tell).

## A "bounded loop" content-assert scans the WHOLE script text — a genuinely-bounded NEW loop can still trip it (#830)

`tests/harness_rig_busy_gate.rs`'s `rig_busy_gate_is_bounded_never_infinite` asserts the ENTIRE
`scripts/rig-busy-gate.sh` text never contains the literal substrings `"while true"` or
`"while :"` — a crude but repo-wide guard against an unbounded poll loop, not scoped to any one
function. Adding a SECOND loop anywhere later in the same file (#830: a lease-acquire wait/retry
loop, added before the pre-existing OBS busy-check loop) trips this test even when the new loop
IS genuinely bounded by some other mechanism (a `SECONDS`-based deadline, in the first attempt at
this) — the assertion doesn't parse semantics, only greps text. Fix: bound the new loop with an
explicit iteration-count `for ((i = 0; i <= max_attempts; i++))` (matching this repo's own
established idiom for the pre-existing loop) instead of `while true`/`while :` + an internal
break/deadline check — genuinely bounded AND passes the text scan. Before adding ANY loop to a
file covered by this kind of content-assert test, `grep -n "while true\|while :"` the target
test file first to know whether the literal keywords are banned outright.

## Isolate ANY shared-host-path (lockdir/heartbeat/lease file) to a per-test tempdir — never the real path (#830)

A script that reads/writes a well-known SHARED filesystem path outside the test's own tempdir
(a lockdir like `/var/tmp/rig-lease/`, a heartbeat file, anything with a fixed default location)
MUST accept an env override for that path (`RIG_LEASE_DIR` for `scripts/lib/rig-lease.sh` /
`scripts/rig-busy-gate.sh`, mirroring the existing `CAMERA_BOX_RIG_HEARTBEAT` override on
`scripts/lib/rig-heartbeat.sh`) — and EVERY test that exercises the script MUST set that override
to its own `tempfile::tempdir()`. Without it, parallel `cargo test` threads (or two different test
FILES, run in the same or different binaries) race on the SAME real path, causing flaky
false-refusals ("held by a foreign holder") that have nothing to do with the scenario under test —
and on this repo's shared dev1 checkout (see the top-level CLAUDE.md GOTCHA on two workers sharing
one clone), a test run could even collide with a REAL in-progress CI job's lease. When adding a
test that invokes an EXISTING script via `Command::new("bash")`, check whether that script touches
any shared host path and add the override to the test's own command-builder helper (deriving the
tempdir from a fixture path already unique per test, e.g. `fake_py`'s own parent dir, keeps the
helper's signature stable — see `run_gate` in `tests/harness_rig_busy_gate.rs` /
`tests/harness_rig_busy_gate_self_heal.rs`).

## Testing a "read N times" polling loop offline: make the fixture-injection env var accept an EXECUTABLE, not just a static file (#836)

Several gate scripts read a live status once via an env-overridable `cat "$FIXTURE_PATH"` seam
(`DANTESYNC_GATE_WIN_HTTP_<NAME>` etc.) — fine for a single read, but a gate that now SAMPLES a
node multiple times (`dantesync-gate.sh`'s `gather_http_samples`, #836) needs a fixture that
returns DIFFERENT content on each successive call, and a static file can't do that. The fix,
reusable for any future "poll N times" gate: extend the read function so the override may point
at either a plain file (`cat`'d every call, unchanged pre-#836 behavior) OR an EXECUTABLE script
(`[ -x "$path" ] && "$path" || cat "$path"`) — a test then writes a tiny generated bash script
that reads/increments a counter file and prints the Nth line of a pre-written responses file
(clamped to the last line once the caller's real call count exceeds it). This proves the whole
sampling loop end-to-end (not just the pure grading functions) with **zero real network and zero
real sleep** — pair it with a `--window-s 0` (or equivalent no-delay) override so the test doesn't
also pay the gate's real inter-sample spacing. See `write_multi_read_fixture` in
`tests/dantesync_gate.rs` for the worked pattern.

**A "gather N samples" loop should still short-circuit to the old single-read failure mode when
the VERY FIRST read is empty** (endpoint simply not there) — don't blindly attempt all N reads
regardless. This keeps a genuinely-down node's "unreachable" verdict exactly as fast as before
multi-sampling was added, and means most pre-existing single-read "unreachable"/"fallback" tests
need NO `--samples`/`--window-s` override at all — only tests whose fixture IS reachable (so the
loop proceeds to try further reads) need the override to stay fast and to satisfy the new
min-distinct-samples requirement.

## Parallelizing a `set -euo pipefail` script's loop body with `&`/`wait` (#836)

`dantesync-gate.sh` moved from grading N independent nodes ONE AFTER ANOTHER to grading them
CONCURRENTLY (`grade_http_node ... > "$outfile" &` per node, then `wait`) once sampling a single
node started taking real wall-clock time. Two `set -e` traps to know about before doing this in
ANY script here that starts with `set -euo pipefail`:

1. **A bare `wait` (no args) returns the exit status of the LAST job it waited for — and under
   `set -e` a non-zero return from a bare `wait` statement WILL abort the script**, even though
   the job's own failure already happened safely in the background and you don't actually care
   about its raw exit code (you read the real per-job outcome back from a file). Always write
   `wait || true` when you don't need to react to which job failed via its process exit status.
2. **`[ cond ] && assignment` / `[ cond ] && do_something` as a STANDALONE statement does NOT
   trip `set -e` when `cond` is false** — this is a real, easy-to-misjudge bash exemption (any
   command that is part of an AND-OR list OTHER THAN the list's last command is exempt from `-e`,
   and a short-circuited `&&` never reaches its second half, so the first half is treated as "not
   the last command run"). Verified empirically: `bash -c 'set -e; [ 0 -gt 0 ] && x=1; echo ok'`
   prints `ok` and exits 0. Don't "fix" this pattern defensively (e.g. rewriting it to an
   `if`/`fi`) out of a mistaken belief it's an `-e` hazard — it isn't. The genuine hazard is #1
   above (`wait`), not this one.

**When backgrounding a loop body:** each job needs its own OUTPUT channel (a per-job tmp file for
stdout, redirected at the `&` call site) and its own VERDICT channel if the caller needs to know
pass/fail per job (a per-job tmp file the job function `printf`s its result into directly — never
try to smuggle a verdict back through the backgrounded function's own exit CODE, since `wait`'s
exit-status reporting for multiple backgrounded jobs is awkward and, per #1, actively dangerous
under `set -e`). Replay the per-job output files in the SAME deterministic order they were
launched (not the order jobs happen to finish) so the report stays byte-for-byte stable regardless
of which node's HTTP endpoint answers fastest. Prove the concurrency is REAL, not just structured
to look parallel, with a timed test: give N≥2 jobs a real multi-second delay each and assert total
wall-clock stays close to ONE job's delay, not N× it (see
`gate_samples_multiple_nodes_concurrently_not_sequentially` in `tests/dantesync_gate.rs`).

## Extracting a shared PS/bash snippet into a `scripts/lib/*.sh` helper can make it appear TWICE in one generated output — a bare-text `.find()` anchor becomes ambiguous (#867)

When a generator script REUSES another generator's full output wholesale (e.g.
`obs-self-heal-install.sh`'s `build_recovery_script` embeds `launch-obs-genlock.sh`'s
`build_launch_program` output verbatim for its kill+relaunch step — a deliberate "one launch
path" reuse, not a bug), and BOTH the outer and the reused inner program independently embed the
SAME shared helper (e.g. a new `scripts/lib/*.sh` snippet both now source), the shared snippet's
own text appears MULTIPLE times in the outer script's final generated output. A test that did
`.find("<some literal from the shared snippet>")` to anchor an ORDERING assertion (e.g. "the
restart must happen after the verify step") silently grabs the FIRST occurrence — which may be
the wrong one (the reused inner program's copy, not the outer script's own step). This bit
`tests/obs_self_heal_install.rs` in #867 when a bare-exe-name literal (previously unique because
only ONE of the two restart mechanisms used that exact string) was replaced by a shared
`ahk_resolve_and_relaunch_ps` helper both mechanisms now call — the literal that used to be
unique now appears twice.

**Fix: anchor on a comment/log-line that is UNIQUE to the specific call site you mean** (e.g. the
outer script's own `# --- Step 4/4: RestartAhk ---` marker comment, never present inside the
reused inner program), not on any part of the shared helper's own body. Before trusting a
`.find()`-based ordering test after extracting shared logic into a lib, grep the FULL generated
output for your anchor string and confirm it is genuinely unique — `grep -c` on the harness's
captured stdout, not just eyeballing the source.

## Piping the FULL local `cargo test` run through `tail` hides an early failure AND makes the exit code lie (#895)

The top-level CLAUDE.md's "one failing test binary makes `cargo test` SKIP the remaining
binaries" gotcha means the mandatory FULL-suite re-run (after touching `recording-e2e.sh` /
`rig-mode.sh`) must genuinely see the WHOLE run, not a tail of it. Two independent traps stack
here: (1) `cargo test 2>&1 | tail -100` silently discards every line BEFORE the last 100 — if the
run has ~180 test binaries, an EARLY failure and everything after it up to the last 100 lines is
invisible, so "no FAILED in what I can see" is not proof of a clean run; (2) the shell reports the
exit code of the LAST command in an unguarded pipe (`tail`, which itself almost always exits 0) —
not `cargo test`'s own exit code — so even a background-task "completed exit code 0" summary can be
misleading when the command was piped through `tail` without `set -o pipefail`.

**Fix: redirect the full run to a FILE (never pipe through `tail`), then check the exit code of the
`cargo test` command itself, separately** — e.g. `cargo test > run.log 2>&1; echo "EXIT:$?" >>
run.log`. Grep the untruncated file for `test result: FAILED` (must be zero hits) and count `test
result: ok` lines against the expected total binary count, and treat the trailing `EXIT:0`/`EXIT:1`
line as the authoritative signal, not a background-task summary of a piped command. A long run may
need `run_in_background` (the default 120s foreground timeout backgrounds it anyway) — that's fine,
just read the FULL log file once the notification arrives, never a `tail`-truncated one.

## A single `verify_imag_pure_functions` test can FAIL under full-suite parallel `cargo test` load, yet pass every time in isolation (#912)

Observed live (#912 session): a full `cargo test` run (~178 binaries) failed exactly one test,
`imag_obs_log_checks_on_a_healthy_capture` in `tests/verify_imag_pure_functions.rs` — a
bash-harness test that sources a script and greps a static fixture log, nothing timing-sensitive
about its OWN logic. Re-running `cargo test --test verify_imag_pure_functions` alone (all 40 tests
in that binary) passed clean, twice. CI (`gh run list --branch dev --workflow CI`) was green on
the exact same commit both before and after. This is resource contention from running MANY test
binaries concurrently on a shared dev1 box (shell/process-spawn pressure), not a real regression —
the same class of flake `ci-testing-gotchas.md`'s "shared host path" and `set -e`/`wait` sections
already describe, just observed for a new file.

**Before treating a full-suite failure as a real regression:** re-run the SPECIFIC failing test
FILE alone (`cargo test --test <file_stem>`); if it passes clean there AND the same commit is
green on CI, it's a local parallel-load flake — note it, don't file a ticket for an unreproduced
single flake, and don't let it block an otherwise-verified change. Only escalate if the failure
reproduces in isolation or CI itself goes red.

## A red hardware Full-path E2E on YOUR PR can be caused by ANOTHER concurrent ticket's LIVE FLEET STATE drift — not your diff, and not a git-checkout race (#690/#923)

The top-level CLAUDE.md's shared-checkout GOTCHA covers a `git`-level race (another worker's
not-yet-fixed RED commit riding along on a shared push). This is a DIFFERENT, hardware-level
variant with zero git interleaving involved: the `Full-path E2E (recording-based · hardware ·
self-hosted dev1)` workflow's `[0/8]` preflight includes a **cross-box genlock-parity check**
(`genlock_parity`, #756) that refuses the WHOLE run if the fleet's boxes (strih/stream/imag) are
not all on the SAME genlock build SHA — a condition that can flip between two runs with **zero
code change on either side**, purely because a completely unrelated ticket's rig-ops deploy
hot-swapped a build onto one box but not the others in the meantime.

**Live incident (2026-08-01, PR #922 / issue #690):** a PR touching ONLY `vendor/av-sync-dock/`
(A/V-sync dock, unrelated to genlock builds) failed its required Full-path E2E with
`genlock_parity DRIFT (... strih=b986152... stream=b986152... imag=9948ed8...)`. The immediately
PRIOR Full-path E2E run (a different, unrelated PR) had passed ~1h45m earlier — proving the drift
appeared in that window, from some OTHER process's rig deploy, not from this PR's diff.

**Before debugging your own diff over a Full-path E2E failure:** read the failing preflight
step's OWN log line — `genlock_parity` (or any of the other `[0/8]` drift-guard/DanteSync/
version-integrity facets) failing means the RIG STATE is the problem, not your code, and no
amount of re-reading your diff will explain it. Check whether the immediately-prior E2E run (same
workflow, `gh run list --workflow "Full-path E2E..." --limit 5`) passed — if it did, and your diff
doesn't touch genlock/build/deploy scripts, the drift happened independently of your work.

**What to do:** file it as ITS OWN issue (never bundle it into your ticket's PR — a rig-ops
fleet redeploy is categorically different work, per `drive-rig-steps-in-supervisor.md`-class
scoping), reference the standing durable-fix ticket if one exists (#789 tracks "one canonical
build fleet-wide" as the actual fix for this recurring class — see the closed #818 for the
pattern's history), and leave your own PR open/unmerged pending that separate fix. Do NOT
attempt the rig hot-swap yourself from an autopilot-worker session scoped to code+CI — that is a
live rig-ops action outside a code ticket's scope, same as any other rig deploy decision.

**SUPERVISOR resolution recipe (how #923 was actually converged, 2026-08-01):** when the
Windows boxes sit on a MAIN-merge SHA (a full windows-genlock build on main) but
`linux-genlock.yml` has no run at that SHA (it push-triggers only on dev; by the time you'd
dispatch `--ref main`, main HEAD may already have moved past the deployed SHA via a later
vendor-untouched merge), you can still build the EXACT parity SHA for imag: `git tag
genlock-parity-<short> <full-sha> && git push origin <tag>`, then `gh workflow run
linux-genlock.yml --ref <tag>` — the artifact stamps `git rev-parse HEAD` at the checked-out
ref, so a tag dispatch yields a byte-exact `GENLOCK_BUILD_SHA.txt` match with the deployed
Windows bundle. Deploy to imag via `setup-imag.sh --yes` with `GENLOCK_RUN_ID=<that run>`
(the canonical hot-swap path, all manifest verifies included), then verify per the #912
restart-race gotcha (`ps -o pid,lstart -C obs` newer than the swap + `render tick ENABLED` in
the newest log). And when a LATER merge will redeploy the Windows boxes anyway (e.g. shipping a
new plugin DLL), converge ALL THREE boxes in that one pass — windows bundle to strih/stream +
`linux-genlock` dispatch at the same ref for imag — so parity never has a stale-box window.
Post-E2E leftover: aborted runs can leak `genlock_burn=true` on strih camera inputs, which the
[0/8] preflight then refuses one input per attempt; clear ALL of them in one sweep
(`obs_burn_filter.py check`+`remove` over `NDI cam1..7`) instead of chasing the gate's
one-at-a-time errors (#924 tracks making the preflight normalize this itself).
