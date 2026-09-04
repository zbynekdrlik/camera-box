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
  - "scripts/recording-e2e.sh"
  - "scripts/rig-mode.sh"
  - "scripts/*-alert-watchdog.sh"
  - "scripts/lib/rig-lease.sh"
  - "scripts/lib/rig-lease.sh"
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

## Appending a word to a `scripts/recording-e2e.sh` banner: the occurrence-count anchor sweep is BLIND to NEGATED (must-not-contain) region assertions (#1263)

The CLAUDE.md static-anchor discipline for `recording-e2e.sh`/`rig-mode.sh` edits recommends a
python occurrence-count sweep (old vs new: flag any test string-literal whose count went 1→0 or
1→2). That sweep only catches POSITIVE anchors — it is structurally BLIND to a test that asserts a
region must **NOT** contain a phrase (`assert!(!window.contains("X"))` / a forbidden-region check).
Adding a common word to a banner (`REPORT-ONLY`, `WARN`, `ABORT`, `strict`) can newly SATISFY such a
must-not-contain assertion and break CI even though every positive anchor count is unchanged.
Concrete: `tests/harness_render_budget_imag_report_only_888.rs` FORBIDS `REPORT-ONLY` inside the
`[4d/8]` render-budget region (its window slices FORWARD from the first `--box "strih=` / the
`[4d/8] #405` banner). #1263 appended `REPORT-ONLY` to the EARLIER `[4d1/8]` banner (line ~2871) —
safe only because both 888 forbidden regions slice forward from lines AFTER it. **So when you add a
word to a banner, ALSO grep every recording-e2e-reading test for negated
`.contains(...)`/`!window.contains(...)`/`!s.contains(...)` assertions and confirm the region each
forbids starts AFTER (or is otherwise disjoint from) your edit** — the positive occurrence-count
sweep alone cannot prove this, and a full `cargo test` is CI-only under Tier-0. (The sibling
report-only decoupling seam that added the `[4d1/8]` word is `scripts/lib/mv-fps-preflight.sh`'s
`mv_fps_preflight_term_is_report_only` per-box term predicate — strih report-only while issue 1260
open, imag strict, flipped back to strict in the PR closing issue 1260.)

## Adding a STEP between the merge call and `exit "$GATE"`: the occurrence-count sweep is BLIND to the 703 byte-DISTANCE window too (#1265)

Sibling blind-spot to the #1263 negated-region one above, hit live on #1265. `tests/harness_e2e_execute_verdict_703.rs` does NOT anchor on a literal — it slices a FIXED BYTE WINDOW from the
merge call (`"$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?`) and asserts `exit "$GATE"` is *inside*
it (`&s[exec_merge_block..(exec_merge_block + 9600).min(s.len())]`). Any NEW step you insert between
the merge call and that exit grows the DISTANCE, which is not a literal-count change at all — so the
occurrence-count anchor sweep (old-vs-new literal counts) passes completely clean while the 703
window silently goes RED at CI (cargo blocked → Tier-0-invisible). #1265's `[8/8g]` loop-gain damp
block pushed the distance 8949 → 10106 > the then-current 9600 window; the count sweep flagged
nothing, a fresh-context reviewer caught it. **So when you add/remove ANY step in the
`recording-e2e.sh` region between the `[8/8]` merge call and `exit "$GATE"` (a report step, a
combine step, a gain damp, a snapshot), explicitly re-measure that distance and WIDEN the 703
window** — the sweep and the harness anchor simulations do NOT cover it (it is a distance, not an
anchor). Measure it directly:
`python3 -c "s=open('scripts/recording-e2e.sh').read(); mb=s.find('\"\$VERDICT_BIN\" \"\${MERGE_ARGS[@]}\" || GATE=\$?'); print(s.find('exit \"\$GATE\"', mb)-mb)"`
then set the window above that with headroom + the file's own `// #NNNN: widened from A to B bytes
... (measured distance N)` convention comment. `recording-e2e-cleanup-composition.md` documents the
widening itself; THIS entry is the reason the count sweep won't remind you to.

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
outer script's own `RestartAhk backstop` marker/log lines — issue 1273 restructured the outer AHK
step into a failure-path backstop and RETIRED the old `# --- Step 4/4: RestartAhk ---` marker, so
that phrase no longer exists in the emitted text; anchor on `RestartAhk backstop` instead, never
present inside the reused inner program), not on any part of the shared helper's own body. Before
trusting a
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

**A SECOND, DIFFERENT log signature for the same class of "not your diff" failure — `UNKNOWN`
(incomplete read), not `DRIFT` (mismatched SHAs) (#942, 2026-08-02).** The `[0/8]` version-
integrity gate's `strih` read specifically depends on an ACTIVE win-\* MCP session writing its
observed state to a file (`stream`'s equivalent read succeeds automatically via an HTTP
`:8899` endpoint; `strih`'s apparently does not have that same automated path). When no such
session is populating it — observed live when the rig was mid a supervisor-owned live
measurement across two consecutive pushes on the same PR (#943) — the exact log shape is:

```
NOTE: could not fetch version-integrity state from 10.77.9.202 (http :8899) — the
      win-* MCP holder must write the drift-guard observed values to .../version-strih.json,
      else the gate refuses.
...
strih          UNKNOWN  (no state file ... — win-* MCP fetch missing)
...
genlock_parity UNKNOWN  (cross-box genlock parity INCOMPLETE — read [stream=..., imag=...]; UNREAD: strih)
!! GATE INCOMPLETE: 2 box(es) UNKNOWN: strih genlock_parity — NOT clean.
```

Same response as the DRIFT variant above: this is rig-state, not your diff (confirmed here by a
PR touching ONLY `src/av_sync_dock.rs`/its tests/the C++ mirror header — nothing genlock/version-
pin related). Do NOT touch win-\* MCP yourself if the dispatch says the rig is mid a live
measurement you must not disturb — that is exactly what's blocking the automated populate step.
Leave the PR open, comment the evidence on the ticket, and let whoever owns the rig moment (the
supervisor, or the live measurement concluding) get the state populated; then `gh run rerun
<run-id>` (never a fresh `gh workflow run`, per the `linux-genlock.yml`/`full-path-e2e.yml`
GOTCHA in the top-level CLAUDE.md) on the SAME commit gets a real verdict with no new push.

## A branch's own explanatory comment can DEFEAT a negative-substring anchor test written against it (#942 hardening session, 2026-08-02/03)

When writing a NEGATIVE check ("this branch must never call X") against one of the vendored
`av-sync-dock` C++ files by slicing a region and asserting `!region.contains("X(")`, check whether
that exact region's own comment ALREADY explains what the branch does NOT do, using the same
literal call text — e.g. a monitor-only branch's comment reading "no cb_apply_lock_latency_ms(),
no rebase() (rebase assumes a real actuator move happened, which this is not)". A naive
`squish()`ed (whitespace-collapsed, comments left in) substring check on that region finds the
PROSE, not a real call, which can mask a genuine future regression (the check still "passes" for
the wrong reason) or, worse, misfire on an innocent comment edit.

**Fix: strip `//` and `/* ... */` comments from the vendored text BEFORE slicing for a
negative-substring check** — a small line/block-comment stripper preserving newlines (so
`//`-comment scope stays correct) is enough; it doesn't need string/char-literal awareness for
this repo's vendored C++, since it's only ever used to bound an ASCII substring search, never to
reconstruct compilable source. See `strip_cpp_comments()` in
`tests/genlock_preload.rs::vendored_source` and its use in
`dock_lock_corrector_is_monitor_only_by_build_default_942`'s branch-scoped
`cb_apply_lock_latency_ms(`/`rebase(` check. A POSITIVE anchor (pinning that code X DOES exist at
a specific location) does not need this — comments don't accidentally satisfy a positive check the
same way, though it's still worth grepping the count including comments to confirm uniqueness.

**Before trusting ANY new anchor (positive or negative) in this file class: prove it bites.**
Temporarily mutate a SCRATCH COPY of the vendored/workflow text (never `vendor/` itself, and never
leave a real file mutated — `cp` to a backup, mutate the REAL non-vendor file in place if it isn't
`vendor/`, run the specific test, confirm the expected FAIL, then restore from the backup and
`md5sum`-verify it matches the pre-mutation hash byte-for-byte) or run the exact same
squish/strip/slice logic in a throwaway `python3` one-liner against the file content read via
`open()` — either proves the anchor actually catches the regression it exists to prevent, rather
than merely "looking right". An anchor you never watched fail is not a proven anchor.

## A commit message merely MENTIONING the airuleset hooks PATH in prose can false-trip `block-foreign-airuleset-write.sh`

The hook scans the whole `git commit` command TEXT (including a heredoc `-m` body) for anything
that looks like a write to `~/devel/airuleset/**` — it does not distinguish "this command writes
to that path" from "this commit message's PROSE happens to mention that path" (e.g. correcting a
doc pointer that wrongly said a hook lives in this repo, when it actually lives in
`~/devel/airuleset/hooks/`). This is a genuine false positive in a foreign (non-airuleset) repo's
own commit — the fix is the documented escape hatch, not rewording the message to hide the
path: append `# airuleset:foreign-ok <reason>` to the `git commit` command. Confirmed harmless
here (nothing was written to the airuleset repo; the commit only touched
`.claude/rules/rig-state-inspection.md` inside camera-box).

## Restoring a temporarily-relaxed STRICT gate term: mine historical CI runs, and check whether the ticket's own "closes when" premise redirects to a DUPLICATE (#888, 2026-08-03)

When a RE-GATE-style ticket says "restore to strict once issue N lands", checking `gh issue view N
--json state` alone is not enough — **N may be CLOSED as a duplicate of a DIFFERENT, still-open
canonical ticket** (here: #886 was closed 2026-07-30 as "Duplicate of #865"; #865 carried the real
root-cause discussion and was still OPEN with no matching fix commit in `git log`). Reading only
the referenced ticket's `state` field would have wrongly concluded the precondition was met, or
wrongly concluded it was NOT met and left a coin-flip gate report-only forever. **Always follow a
closed issue's own close comment for a "Duplicate of #M" redirect and check M's state too before
trusting a "closes when X lands" premise.**

Once the redirect is found and the literal fix commit still doesn't exist, that does NOT
automatically mean "don't restore" — check the ACTUAL, CURRENT, MEASURED state instead of the
premise's literal wording. The technique (same family as `.claude/rules/phase-sync-calibrator-
testing.md` and `.claude/rules/gap-metric-reconciliation.md` — recalibrate/verify from a recent
CI run's own data rather than triggering a fresh soak — here applied specifically to RESTORING a
previously-relaxed term):

```bash
gh run list --workflow "Full-path E2E (recording-based · hardware · self-hosted dev1)" \
  --json databaseId,conclusion,createdAt,event -L 60 \
  --jq '.[] | select(.event=="pull_request") | "\(.databaseId) \(.createdAt) \(.conclusion)"'
# then per run:
gh run view <id> --log | grep -E "imag (PASSED|MISSED) its render budget"   # the term's own verdict
gh run view <id> --log | grep -E "imag burn-check.*burn_on=True"           # confirm the condition
#                                                                             being measured (burns
#                                                                             ON) was actually true
```

Pull EVERY reachable `pull_request`-triggered hardware-gate run since the relaxation landed, not
just the 2-3 most recent — a wide, multi-day sample (#888 used 10 runs across 2.5 days) is far
stronger evidence than one more manual soak, and it also reveals WHERE the transition happened
(here: a clean MISS→PASS boundary the same evening `issue 884`'s imag-obs.service supervision
deployed — consistent with `issue 799`'s documented "restart clears render degradation" pattern,
though this was recorded as a hypothesis, not a verified causal claim, since no #865/#886-titled
commit exists). **`gh run view --log` retention is finite** — very old runs in the list may return
an empty log; treat those as "unreachable", not as evidence either way, and lean on the runs that
DO return data.

## `gh run view --log` can return ZERO lines for a freshly-completed run — fall back to the jobs API

Reading a hardware-gate run's log minutes after it finished, `gh run view <id> --log` and
`gh run view --job <job-id> --log` both returned **completely empty output** (`wc -l` = 0) with no
error and exit 0 — indistinguishable from "the run printed nothing". The run was `completed/success`
and its log was genuinely there; only the `gh run view` path failed to produce it. The REST endpoint
worked immediately on the same run:

```bash
jid=$(gh run view <run-id> --json jobs --jq '.jobs[0].databaseId')
gh api repos/zbynekdrlik/camera-box/actions/jobs/"$jid"/logs | grep -aE '<pattern>'
```

Note the `grep -a` — the API streams the log as raw bytes that grep may otherwise treat as binary
and silently reduce to "Binary file matches". This is distinct from the finite-RETENTION case
documented above (an OLD run whose log is genuinely gone): here the log exists and is current, and
an empty `gh run view --log` must NOT be read as "the step produced no output" or, worse, as
evidence a gate step did not run.

## A static-anchor test that SLICES a region leaves everything outside that region unguarded — including the step's own log banner

`tests/harness_render_budget_imag_report_only_888.rs` slices its asserted region from
`--box "strih=` to `[4e/8]` and asserts hard on the strictness of the code inside it. When issue 888
flipped imag's `[4d/8]` term from report-only back to STRICT, every in-region assertion was updated
and the full suite went green — but the step's `echo "[4d/8] ..."` BANNER sits a few lines ABOVE
`--box "strih=`, i.e. outside the slice, and kept printing `imag is measured but REPORT-ONLY
(issue 888, temporary — see below)` into every gate run's log while the code aborted on failure.
The gate was strict; its own log said it was advisory.

Two rules follow, both generalizing past this one file:

1. **A step's log banner is part of its contract, not decoration.** Whatever a `[N/8]` banner claims
   about strictness/scope is what the next person debugging an abort will believe. When you change a
   term's strictness, the banner is part of the change — grep the step's `echo` lines, not just the
   branch you edited.
2. **When writing a slice-based anchor test, deliberately decide where the region STARTS.** If the
   contract you are pinning includes prose printed by the step, the region must begin at the banner
   (or a second assertion must cover it). A region chosen for convenience — "start at the first
   unique anchor I could find" — silently excludes everything before it, and green tests will then
   certify a lie.

## A trailing `///` doc-comment block at the end of a `.rs` test file fails to compile if nothing follows it

`error: expected item after doc comment` — a doc comment (`///`) must document an ITEM (a
function, struct, etc.); a closing explanatory note at the END of a test file with no more `#[test]`
functions after it needs a plain `//` comment, not `///`. Easy to trip when replacing/removing the
last test function in a file and leaving a trailing note behind.

## Writing a NEW block-comment header for a group of new functions self-collides with `.split("fn_name()")`-style slicing anchors -- three times in one session (issue 901)

The duplicate-anchor class this file already documents (the `.find()`/`.split()` traps above)
also bites the AUTHOR of a brand-new test, not just an editor of an existing one. Adding a NEW
Rust static-anchor test file that slices `do_test()`'s body the same way `tests/rig_mode.rs`
already does (`s.split("do_test()").nth(1).split("do_event()").next()`) is a SEPARATE occurrence
of that exact split-delimiter text — if the NEW test file's own helper is written before checking
whether the SAME literal `"funcname() {"` already occurs earlier in the source as a bare mention
(not the real definition), `.find()`/`.split()` silently grabs the wrong occurrence and produces
a confusing false pass/fail unrelated to the real assertion. This happened THREE times writing
`tests/harness_rig_mode_chain_verify_901.rs` + the code it drove in `scripts/rig-mode.sh` in one
sitting:

1. My own new test used `.find("enforce_strih_ndi_mapping()")` to slice the function body — grabbed
   an EXISTING header comment ~500 lines earlier that says "...enforce_strih_ndi_mapping() below
   passes it..." instead of the real `enforce_strih_ndi_mapping() {` definition.
2. My own new explanatory block comment ABOVE the newly-added functions (inserted before
   `do_test()`'s own definition) literally wrote the words "do_test()" in prose — a NEW, earlier
   occurrence of the exact split anchor `tests/rig_mode.rs` (a PRE-EXISTING sibling test file) AND
   my own new test file both key on, silently truncating the sliced body to nothing.
3. The same new block comment quoted the OLD hint text being removed ("NEXT: confirm the
   PHASE2-PROBE scene...") verbatim, inside a sentence explaining what it replaces — which broke
   my own new test asserting that exact string is now ABSENT from the file (the explanatory
   comment itself still contained it).

**Fix pattern, reusable:** (a) when writing a `.find()`/`.split()` anchor for a function body,
anchor on `"funcname() {"` (the real definition's opening brace), never the bare `"funcname()"` —
a bare call/mention can appear anywhere in prose; (b) when writing a NEW explanatory comment near
an anchored function, NEVER spell out that function's call-syntax (`funcname()`) or the literal
text of anything a negative-assertion test checks is now absent — describe it in prose without
reproducing the exact banned/anchored string (e.g. "the old confirm-the-scene hint" instead of
quoting it verbatim). Catch this class immediately by re-running the NEW test right after writing
it, before trusting a green result — a false-negative slice often still "passes" a
`!body.contains(...)` assertion for the wrong reason.

## `full-path-e2e.yml` (the hardware gate) triggers ONLY on `pull_request` targeting main -- a plain `git push origin dev` with no open PR never re-runs it

Confirmed from the workflow's own `on:` block: `pull_request: branches: [main]` +
`workflow_dispatch` only -- no `push` trigger at all. This is the OPPOSITE of `linux-genlock.yml`
(triggers on every push to `dev`, per the top-level CLAUDE.md GOTCHA) and means a small trailing
commit pushed to `dev` with NO open PR (e.g. a docs-only autopilot-log entry, or any fix landed
after a PR already merged) costs only the fast `CI` workflow (~7 min), never the ~30 min hardware
gate -- useful when you need to push something to `dev` cheaply without burning rig time, and a
reminder that the hardware gate's absence from a plain push run is NORMAL, not a sign anything is
broken or skipped.

## `set-ndi-mapping.py`'s plain (non `--verify-only`) run ALREADY re-reads + hard-fails on mismatch -- the gap was SCOPE (active-only), not a missing verify step

Before assuming `enforce_strih_ndi_mapping()`'s call into `set-ndi-mapping.py` needs a "verify
after set" step bolted on, read `set-ndi-mapping.py`'s own `main()`: even the plain (non
`--verify-only`) path already re-reads every targeted input's binding AFTER setting it
(`bindings = {inp: _get_binding(ws, inp) for inp, _ in want}`) and exits 1 on any mismatch
(`wrong`). The real gap behind issue 901 gap 4 (an INACTIVE camera's mangled 'NDI cam5' pin going
uncaught) was that `active_map()`/`--active "$CAMERA_ACTIVE_SET"` FILTERS `want` down to only the
currently-active cameras BEFORE any of this runs -- an inactive camera's input is never even
looked at, set OR verified. The fix is a SEPARATE, wider, report-only `--verify-only` sweep across
the full 7-camera table, not a "second verify pass" on the same active-only set (which already
verifies itself).

## Re-arming (or relaxing) a `gates_overall_pass()`-style seam: grep the WHOLE repo, not just the file that OWNS the seam (issue 861 re-arm, 2026-08-06)

Flipping a report-only seam back to blocking (or vice versa) means EVERY pre-existing test that
asserted the OLD JSON shape (`gates_overall_pass: false`, a "report-only" gate string, an
unaffected exit code) now describes wrong behavior — the `bf12c1b84`/#889 precedent already
documents finding and fixing these INSIDE the one file that owns the seam
(`src/bin/recording-verdict.rs`). What that precedent under-warns about: **the SAME term can ALSO
be exercised through a completely SEPARATE test file that spawns the compiled binary as a
subprocess** (this repo's `tests/recording_verdict_merge_gate_exit_code.rs`, which proves the REAL
`recording-verdict --merge-partials` PROCESS exits non-zero — a different assertion surface than
the in-process `#[cfg(test)] mod tests` block, see that file's own doc comment for why the
subprocess form exists). A `grep -n "gates_overall_pass\|<json-field-name>"
src/bin/recording-verdict.rs` alone missed it entirely on the first pass; CI's `Test` job caught it
on the SECOND push, costing a full CI round-trip that a repo-wide grep would have caught for free.

**Before declaring a `gates_overall_pass()` re-arm complete:** `grep -rln "<the JSON field name>"
--include="*.rs" --include="*.py" --include="*.sh" --include="*.yml" .` across the WHOLE repo, not
scoped to the one `.rs` file that defines/consumes the seam — include subprocess-spawning test
files, Python report/combine scripts (`scripts/*_report.py`, `scripts/*_combine*.py`), and CI
workflow pwsh/bash assertions. Distinguish readers of the MEASURED value (`gate_pass` — unaffected
by a re-arm, since the measurement itself never changed) from readers of the GATING flag/behavior
(`gates_overall_pass`, an unconditional `overall_pass`/exit-code assertion on a fixture that fails
the term) — only the latter class needs updating.

## A test-harness temp dir hand-rolled from pid+timestamp is a latent collision (#975)

`std::process::id()` is CONSTANT across every test thread in one test binary, so a per-call temp
dir named `temp_dir()/foo_{pid}_{nanos}` keys its uniqueness SOLELY on the timestamp. Two
concurrent calls that read the same clock tick share one dir — and if that dir holds PATH-injected
stubs (a `sshpass`/`sha256sum`/`camera-box` mock), one call's stub silently contaminates the other
(live: harness_deploy_fleet.rs's `run_fleet`, `local aaaa != remote bbbb` under the coverage
runner's heavier `--all-features` parallel schedule). SystemTime has true-ns resolution here so the
collision is RARE, but it is real and undeterministic. Fix: `tempfile::tempdir()` (already a
dev-dep) — a kernel-atomic O_EXCL random name that CANNOT collide, whose Drop cleanup also removes
the trailing manual `remove_dir_all` race. Rule: never hand-roll a unique temp path from
pid+timestamp in a test; use `tempfile::tempdir()`.

## A shared JSON/state file read FIELD-BY-FIELD (N separate parses) tears when a peer deletes it mid-read; read it ATOMICALLY (#970/#980)

`scripts/lib/rig-lease.sh`'s `rig_lease_holder_summary` read holder.json via FIVE separate
`python3 open()+json.load()` calls (one per field). When a releasing foreign holder `rm`s the
lockdir mid-summary, the reads TORE — the first field succeeded, then the file vanished and the
rest read empty, logging a garbled `restreamer# run_url= job=` (and a `[ -z "$repo$run_id" ]`
corrupt-guard did not catch it because the first field was non-empty). Fix: read the file in ONE
`open()+json.load()` that formats the whole line — a concurrent `unlink` after `open()` cannot
truncate an already-open FD on Linux, so the result is all-or-nothing (full consistent line, or a
clean placeholder). Any "read several fields separately from a file another party can delete" is
this bug; collapse it to a single parse.

## A flaky gate test that races a wall-clock releaser: tie the trigger to the gate's OWN cadence, not a sleep (#970/#980)

`foreign_lease_released_within_wait_budget_lets_us_proceed` released the seeded foreign holder from
a background thread after a fixed `sleep(600ms)`. Under CPU load the gate's first poll could land
after the 600ms release (or mid-delete), so it never observed the holder and the assertion
("logged held by #888") flaked — a TEST-timing race, not a production bug. Fix: drive the release
from the gate's OWN poll cadence via a pluggable seam — here `RIG_LEASE_RUN_STATUS_CMD` (invoked
once per lease poll) removes the lockdir on the gate's 2nd poll, AFTER poll 0 has observed+logged
the live holder. Deterministic, load-independent, and no assertion weakened. General pattern: when
a test needs "state X changes AFTER the code-under-test has observed it once", hook the change to a
seam the code invokes, never to a wall-clock sleep.

## Make a shared lockdir's TEARDOWN atomic too, not just the read — rename-then-delete (#857)

The #970/#980 entry above made the rig-lease READER atomic (one `open()+json.load()`). The
RELEASE side (`rig_lease_release` in `scripts/lib/rig-lease.sh`) was still `rm -rf "$d"`, whose
recursive delete removes the lockdir CONTENTS (holder.json, heartbeat) BEFORE the inode — so a
concurrent observer can still see `$d` present but holder.json gone. That window is not just a
cosmetic "unnamed holder" log: a concurrent `rig_lease_acquire`'s `mkdir "$d"` guard fails EEXIST
against the holder-less dir, falls to `rig_lease_is_stale` (which treats no-holder.json as
reclaimable), reclaims into `$d`, and has that fresh lease DELETED by the departing release's
still-running `rm -rf "$d"` — two runs momentarily both believing they hold the rig.

**Fix pattern for ANY teardown of a shared lockdir/state-dir a peer may be reading:** rename the
whole dir aside in ONE atomic syscall, THEN delete the renamed copy — `mv "$d" "$d.releasing.$$"
&& rm -rf "$d.releasing.$$"`. `$d` goes complete→absent in one step; a reader/acquirer only ever
sees a COMPLETE lease or none. Pair it with a `*.releasing.*` sweep (call it from acquire-entry —
the guaranteed GC point — and from release) so a crash between the rename and the rm cannot leak a
dir; the sweep only ever touches already-detached `.releasing.*` copies, never the live `$d`, so
it is safe against a concurrent release rm-ing the same copy (double-delete is a `rm -f` no-op).
Do NOT add an `rm -rf "$d"` fallback when the `mv` fails — it reintroduces the exact window; a
failed rename means `$d` already vanished (a peer released/reclaimed), and the stale-reclaim
heartbeat backstop self-heals any pathological fs-error leftover. Test it deterministically with a
PATH `rm` shim that reproduces `rm -rf`'s contents-first order and probes whether the active path
is ever `[ -d "$d" ] && [ ! -f "$d/holder.json" ]` (see
`tests/harness_rig_lease_release_atomicity_857.rs`).

## A RED test that intentionally leaks a SIGTERM-immune child will HANG a `cargo test ... | tail` pipeline (#850)

When you write a RED test that deliberately leaks a child process (proving a `Drop`/cleanup gap —
e.g. `tests/harness_rig_test_ledger_723.rs`'s `Fixture` without its `impl Drop`, or any
process-reaping fixture), the leaked child INHERITS the test binary's stdout file descriptor — the
WRITE end of the pipe when you run `cargo test ... 2>&1 | tail`. Even after the test binary itself
exits and the child reparents to init (`ppid=1`), that child keeps the pipe's write end open, so
`tail` never sees EOF and the whole pipeline BLOCKS FOREVER (the wrapper shell stays alive, the
output file reads 0 bytes, no `test result:` line ever flushes). It looks like a stuck/blocked
build; it is actually the leak holding the pipe. **Fix: `kill -9` the leaked orphan** (a
SIGTERM-immune `trap "" TERM` fixture needs `-9`) — the pipe then closes, `tail` flushes the
`FAILED` line, and the wrapper exits. Better: for a test that may leak on the RED path, run it
WITHOUT a `| tail` pipe (redirect to a file: `cargo test ... > run.log 2>&1`), so a leaked child
holding stdout can't wedge the reader. Clean up the orphan before observing the next state.

## GOTCHA — the #675 sourced-helper pattern also covers a LOCAL (dev1-side) command, and a NEW `.find()` marker must be unique across the WHOLE file (#716)

Two extensions of the recording-e2e.sh anchor discipline, both confirmed on #716 (persist each
cam-box burn-run fps log to dev1):

- **#675 extends beyond `$(...)`-embedded REMOTE builders to a LOCAL runner function.** When the
  new behaviour is a command that runs LOCALLY on dev1 (e.g. an `scp` PULLING an artifact BACK from
  a box, mirroring the `cam1-capture-stats.txt` sidecar), put the whole runner in a NEW
  `scripts/lib/*.sh` and call it as a plain function line — `cbox_burn_log_persist "$CAM_PW"
  "$CAM1_IP" cam1 "$RUN_ID" "$OUTDIR"` — NOT `$(...)`-embedded (a local command must not be
  command-substituted, and the trailing-newline-glue gotcha above never arises). The static anchor
  tests read only recording-e2e.sh's own text, so the runner body is invisible; recording-e2e.sh
  gains only a `. "$HERE/lib/..."` source line + the call line(s). Worked example:
  `scripts/lib/cbox-burn-log-persist.sh` (pure remote-path/dest-name builders + a best-effort
  `... 2>/dev/null || echo WARNING >&2; return 0` runner) with `tests/harness_cbox_burn_log_persist.rs`
  exercising it via a fake `sshpass` stand-in on PATH.
- **A NEW static-anchor test's `.find()` marker must be unique across the WHOLE file — including the
  comment you add near the top-of-file `. "$HERE/lib/..."` source block.** The #832 self-collision
  class bites even between two comments YOU add in the SAME PR: #716's persist-block marker
  (`#716: persist each cam-box burn-run fps log`) initially also appeared verbatim in the
  source-block comment ~3400 lines earlier, so the test's `s.find(marker)` would have latched onto
  the top comment, not the real block (all block-content asserts still passed by accident, since
  the slice ran to EOF — a silent wrong-anchor, not a failure). Fix: reword the source-block comment
  so ONLY the real call site carries the exact marker phrase; verify with
  `grep -c '<marker>' scripts/recording-e2e.sh` (must be 1).

## NEVER update the shared rustup toolchain mid-round (live incident 2026-08-17)

A worker lane ran a background `rustup update` on dev1 while (a) the self-hosted Full-path E2E
was compiling its probe binaries and (b) sibling lanes had warm `target/` caches. The E2E died
mid-compile with `failed to build archive from rlib .../libstd-<hash>.rlib: No such file or
directory` (the std rlib was replaced under the running rustc) — a hardware-slot run wasted on a
toolchain race — and sibling lanes hit phantom clippy/linker errors from artifacts compiled by
the pre-update rustc. Rules: (1) NO session (worker or supervisor) updates rustup/rustc on dev1
while any lane or CI run is active — a version-parity concern is NOTED in the evidence block for
the supervisor, never self-fixed mid-round; (2) after any toolchain change, every warm `target/`
(worktrees included) needs `cargo clean` — mixed-rustc artifacts produce misleading errors, not
real code failures; (3) an E2E/CI failure whose log shows a missing-rlib / mixed-artifact shape
is re-run after confirming `rustc --version` + the rlib exists — it is not a code regression.

## A report-only bash-lib probe called as a BARE statement aborts the run on a grep no-match under the CALLER's `set -euo pipefail` — and a `set -uo`-only harness is structurally blind to it (#1133)

The `run_sourced` `set -e`-leak entry above is the harness INHERITING a sourced script's `-e`.
This is the INVERSE, and it bit a fresh report-only probe: a new `scripts/lib/*.sh` "just log a
WARN, never gate" function (`leg_health_cap1s_band_warn`) built its value with a grep pipeline —
`caps="$(printf '%s\n' "$text" | grep -oE 'cap-1s: \[…\]' | …)"`. `grep -oE` exits 1 on ZERO
matches; under `pipefail` the whole pipeline returns non-zero; because this is an **assignment**
(not an `if`-condition), the CALLER's `set -euo pipefail` (here `scripts/recording-e2e.sh:51`)
`set -e`-ABORTS the entire E2E run — before the step's own `ok:` line — on exactly the empty /
timed-out ssh read (or a just-restarted box whose instance-scoped journal window has no matching
line yet) the probe is meant to treat as benign. A silent phantom-fail, not a warn.

Two things make this a recurring trap for ANY new sourced-lib helper that greps:

1. **A report-only helper must be called as a bare statement, so it MUST return 0 on every input.**
   Guard it: an early `[ -n "$text" ] || return 0`, AND a trailing `|| true` on any `grep`/`sed`
   pipeline whose no-match/SIGPIPE exit would otherwise propagate (the same "grep must survive zero
   matches under pipefail" discipline the self-heal-attribution rule states, but here it is
   load-bearing for the WHOLE-RUN exit, not just one line). A `sed -n …p` extractor is already
   safe (no-match = exit 0); a `grep -oE …` or a `… | head -1` (head closes the pipe → SIGPIPEs the
   upstream) is NOT.

2. **A `run_sourced`-style harness that sources with `set -uo pipefail` (NO `-e`) can never catch
   this** — the probe always reaches its final `return 0` without `-e`, so the production abort is
   invisible to every test. Add a SECOND helper that sources under the caller's EXACT
   `set -euo pipefail` (`run_under_set_e` in `tests/harness_leg_health_guard_1133.rs`) and assert
   the full per-box sequence (`extract → classify → band_warn`) reaches its `ok`/sentinel line on
   an EMPTY read, plus that a genuinely-bad read still aborts. Verify the same locally by running
   the lib under `bash -c 'set -euo pipefail; . lib; …'`, never only `-uo` — a `-uo`-only local
   check reproduces the harness's blind spot. (Caught here only by a fresh-context reviewer running
   the lib under the real `-e` context; the `-uo` runner + harness both passed while the bug was live.)

## A NEW real state mutation added BEFORE `cleanup()`'s own EXIT trap installs needs its OWN temporary trap (issue 808, 🔴 review finding)

`scripts/recording-e2e.sh`'s `trap cleanup EXIT HUP INT TERM` doesn't install until far down the
file (behind ~1400 lines of `[0/8]` preflight, all of it before `cleanup()` is even armed). That
region has ~30 ordinary `exit 1` sites (reachability, DanteSync, version/parity, clock-offset,
leg-health, and more — common expected failure modes, not edge cases). Adding a NEW step in that
region that performs a REAL, must-be-undone state mutation (issue 808: `systemctl stop
bkshading-relay` on two boxes) silently breaks the restore promise the moment ANY of those 30+
sites fires — every prior pre-trap-declared variable in this file (`IMAG_PREV_SCENE`,
`AV_SYNC_APPLY_OFFSET_MS`, `STRIH_PROG_SOURCE`/`STREAM_PROG_SOURCE`) only ever gets its actual
MUTATION performed AFTER the trap installs; issue 808's pause was the first to mutate real
external state (not just declare a variable) before line ~2100, and a fresh-context reviewer
caught the gap a self-review missed.

**Fix pattern, reusable for any future pre-trap mutation:** install a TEMPORARY, single-quoted
`trap '...' EXIT HUP INT TERM` immediately after the mutating step, whose body undoes exactly
that mutation. A later `trap ... EXIT` on the SAME signal set completely REPLACES the earlier
handler (standard bash semantics) — so this temporary trap is automatically superseded the
instant `cleanup()`'s own real trap installs further down, and it needs no explicit teardown.
Pin the ordering with a static-anchor test (`s.find("bkshading_e2e_pause_stop ") < s.find("trap
'\n") < s.find("' EXIT HUP INT TERM") < s.find("trap cleanup EXIT HUP INT TERM")` — see
`tests/harness_bkshading_e2e_pause_808.rs`'s two ordering tests for the worked pattern), not just
a functional test of the mutation/restore pair in isolation.

**What this does NOT fix, and doesn't need to:** a genuine SIGKILL of the whole harness stays
structurally untrappable by ANY mechanism (the file's own `#878`-area comment already documents
this as an accepted risk for other pre-trap state, e.g. `camera-box.service` itself) — recovery
from THAT class of loss is the existing NEXT-RUN startup-self-heal pattern, not something an
in-run trap can ever cover. Don't over-scope a pre-trap-mutation fix into also solving SIGKILL
recovery; that is separate, pre-existing, accepted scope.

## Generating a `.rs` test file's contents via a Python script: a plain `'\t'`/`'\n'` inside a Python (non-raw) triple-quoted string silently becomes a REAL tab/newline BYTE in the written Rust source (#1216 completion)

Since Tier-0 blocks all local cargo compilation, editing a large `tests/*.rs` file in this repo
often goes through a small `python3 <script>.py` that does a string `.replace()` on the file's
text (the pattern this whole CLAUDE.md/rules-file family already recommends for surgical,
`old.count(...) == 1`-verified edits). When the NEW Rust text you're inserting itself contains a
Rust string literal meant to hold `\t`/`\n` (e.g. a `printf 'ACTIVE\t%s\n'` line inside an `r#"..."#`
raw-string bash harness, or a `line.split_once('\t')` char literal), writing that text as a
PLAIN Python string (`"printf 'ACTIVE\t%s\n' ..."` or a non-`r`-prefixed triple-quoted block) has
Python itself interpret `\t`/`\n` as escape sequences and write the ACTUAL tab/newline BYTE into
the `.rs` file — not the two-character sequence `\` + `t` the Rust source is supposed to contain.

Two different failure shapes result, and only ONE of them is caught by `cargo fmt --all --check`:

1. **Inside a Rust CHAR LITERAL** (`'\t'`) — a real tab byte breaks Rust syntax outright
   (`character constant must be escaped: \`\t\``), so `cargo fmt --all --check` (the Tier-0-legal
   syntax-check net this repo relies on when `cargo build`/`test` are blocked) DOES catch it —
   but only because char literals are strict; this is the lucky case.
2. **Inside an `r#"..."#` RAW STRING** (e.g. a bash heredoc's own `printf 'FOO\t%s\n' "$VAR"`
   line) — a raw string accepts ANY byte including a literal tab/newline, so `cargo fmt` reports
   NOTHING wrong; the file "compiles clean" while silently embedding the wrong shell text (a
   multi-line single-quoted bash string with an embedded raw newline instead of the intended
   `\n` escape sequence functions similarly in bash today, since printf still copies a literal
   newline through unchanged — but it is fragile, differs from every sibling helper's own style
   in the same file, and the NEXT accidental Python-side round-trip through this same bug could
   land the raw byte somewhere printf-semantics do NOT tolerate it).

**Fix: when a Python `.replace()` script's `new` string must contain a LITERAL `\t`/`\n` destined
for the Rust source (not an actual tab/newline you want Python itself to act on), write it as a
Python RAW string** (`r"printf 'ACTIVE\t%s\n' ..."` or `r'''...'''`) so Python passes the two
characters `\` + `t` straight through unmodified. **Verify after writing, every time:** `cat -A
<file> | grep -n '\^I'` (shows a literal tab as `^I` under `cat -A`) must return NOTHING for any
line that is supposed to hold a Rust/bash `\t` escape sequence — a hit means the Python script
wrote a raw byte instead of the two-char escape, exactly this bug. `cargo fmt --all --check`
alone is NOT sufficient proof the generated Rust text is correct; it only catches shape 1 above.

## `$GITHUB_SHA` on a `pull_request`-triggered job is the SYNTHETIC merge commit, never the PR's head — any commit-scoped `gh run list --commit` resolution wired into a `pull_request` workflow needs `github.event.pull_request.head.sha` instead (issue 1244 review catch)

Any script that resolves a CI artifact "for THIS run's own commit" via `gh run list --commit
"$GITHUB_SHA" ...` is silently WRONG the moment it runs inside a `pull_request`-triggered job
(this repo's `full-path-e2e.yml` is exactly that: `on.pull_request: branches: [main]`). GitHub
Actions sets `$GITHUB_SHA` on a `pull_request` event to the **synthetic merge commit**
(`refs/pull/N/merge`), not the PR's real head commit — and a workflow that (like `ci.yml`) triggers
only on `push: [dev, main]` NEVER produces a run whose `headSha` is that merge sha. So a bare
`$GITHUB_SHA` fallback in a commit-scoped resolution doesn't just occasionally miss — it resolves
NOTHING, ever, on every automatic `pull_request` run, turning what might have been an intermittent
gap into a 100% permanent one.

**Confirmed live (issue 1244, 2026-08-31):** a fresh commit-scoped fix to
`scripts/lib/camera-box-parity-align.sh`'s `cambox_align_deploy()` (replacing a "newest on branch"
`gh run list --branch dev` resolution — itself proven non-deterministic inside the E2E job's own
runner environment) added a `$GITHUB_SHA` fallback with the comment "set by every GitHub Actions
job … recording-e2e.sh … inherits it with no explicit wiring". A fresh-context adversarial review
caught this before merge: `gh run view` on the two incident runs showed `event: pull_request`;
`gh run list --commit <merge_sha>` on the then-open PR #1211 returned EMPTY (`gh run list --commit
<head_sha>` found the run). The existing `#703` step in the SAME `full-path-e2e.yml` ALREADY solves
this exact problem — `SHA="${{ github.event.pull_request.head.sha }}"` in its own shell env, wired
explicitly in the calling step's `env:` block, precisely because the same anomaly bites there too.

**Fix + the rule going forward:** any NEW (or edited) commit-scoped `gh run list --commit`
resolution that a `pull_request`-triggered workflow step invokes MUST have its candidate sha wired
EXPLICITLY from that step's own `env:` block — `SOME_VAR: ${{ github.event_name == 'pull_request'
&& github.event.pull_request.head.sha || github.sha }}` (the `|| github.sha` arm keeps a
`workflow_dispatch`/`push` trigger correct, where `$GITHUB_SHA` already IS the real candidate) —
never rely on a bare `$GITHUB_SHA`-reading fallback inside the sourced script/lib itself to cover
the `pull_request` case. Pin the wiring with a static-anchor test reading the workflow YAML text
(the existing `tests/harness_full_path_e2e_workflow.rs` `step_block` pattern — slice between the
step's `name:` and its `run:` line, assert the new env var name AND
`github.event.pull_request.head.sha` both appear inside that slice) so a future edit that drops the
wiring fails loudly instead of silently reintroducing the 100%-refuse trap.

## A `.log` test fixture is GITIGNORED (`*.log`) — it silently won't commit; use `.txt` (#1265)

`.gitignore` has `*.log`, so a committed test fixture named `<name>.log` (e.g. an anonymized OBS-log
sample a `tests/python/*.py` parser test reads at runtime) is NEVER added — `git status` shows it as
neither `??` nor staged (it's ignored), the RED/GREEN passes LOCALLY (the file is on disk), then CI
fails file-not-found because the fixture was never in the commit. Confirmed live (#1265: a
`fixtures/audio_ref_band_mbc_1265.log` sample). Fix: name any committed log-shaped fixture `.txt`
(not gitignored) and reference that from the test. Before trusting a new fixture is committed,
`git check-ignore <path>` (a hit = it will be silently dropped) — never assume `git add` took it.

## A worktree-isolated worker CANNOT locally run a sourced-bash-lib test or a PATH-stubbed dry-run (#1265)

The worktree-isolation guard refuses `bash -c '…source lib…'`, `ENV=x bash <script>`, and any
`PATH=<stub> … | grep` pipeline ("what it reads/is handed as shell text cannot be shown not to run
git"). So the two established local Tier-0 recipes for this repo's `scripts/lib/*.sh` + watchdogs —
(a) sourcing a lib under `set -euo pipefail` and calling its functions, and (b) the manual
stubbed-`curl` `--dry-run` of a `*-alert-watchdog.sh` — are BOTH unrunnable from a worktree worker
(they DO run for the SUPERVISOR / a non-isolated session). What a worktree worker CAN still do
locally, and should rely on instead: (1) run the load-bearing python one-liners STANDALONE via
`python3 -c '…' <args>` (the guard allows `python3 -c`, only not `bash -c`), proving the real logic;
(2) `python3` a `.find`/`.split`/window SIMULATION of each Rust static-anchor assertion against the
edited script (reproduce the harness's exact slice logic — this is the authoritative anchor-safety
proof when `cargo test` is Tier-0-blocked); (3) `bash -n` + `shellcheck -S warning` on the `.sh`;
(4) `cargo fmt --all --check` (parses the `.rs`); (5) the notify-dedup sweep pytest. The Rust
harness (`run_under_set_e` sourcing the lib) then runs at CI / for the supervisor. Note this in the
evidence block so the supervisor runs the sourced-lib harness + any stubbed dry-run at integration.

## Clippy `doc_lazy_continuation` is a CI-only failure under Tier-0 — a doc line starting with `+ ` (or `- `/`* `/`1. `) is a Markdown LIST ITEM (issue 1196 integration, 2026-09-02)

`cargo clippy -D warnings` cannot run locally (Tier-0 #557), `cargo fmt --check` does not lint doc
prose, so this class surfaces only in CI's Lint job — and it blocked the whole release PR's E2E
(the E2E fetches the CI artifacts and fails closed when `ci.yml` is red). The trap: a `//!`/`///`
paragraph that WRAPS so a line begins with `+ exactly ONE aux mark …` — clippy reads `+ ` as a
Markdown bullet and every following unindented line as a "list item without indentation". Fix by
rewording (`plus …`), never by indenting prose that is not a list. Pre-push local net (cheap,
run over every touched `.rs`): `grep -nE '^\s*//[/!] ?([-+*]|[0-9]+\.) ' <files>` and check that
each hit is a REAL list item whose continuation lines are indented by 2+ spaces.

## Inserting a NEW line right after an existing `# shellcheck disable=SC2XXX` directive silently REBINDS it to the wrong statement (issue 1260)

A shellcheck inline directive comment applies ONLY to the command on the IMMEDIATELY FOLLOWING
line — not "somewhere nearby". Adding a genuinely-needed new statement (e.g. `local prio;
prio="$(some_resolver)"`) between an EXISTING `# shellcheck disable=SC2016  # <reason>` comment and
the `printf`/command it was written for silently detaches the directive from its intended target:
the directive now suppresses SC2016 on the NEW line (which may not even trigger it), and the
ORIGINAL line it was meant to protect starts firing the warning again. `shellcheck -S warning`
(CI's gate level here) stayed clean either way in the live case (SC2016 is style-tier), so this is
easy to miss — it only surfaced under `shellcheck -S style` in a fresh-context adversarial review.
**Fix:** when inserting ANY new line between a `# shellcheck disable=...` comment and its target,
move the directive down to sit immediately above the target again. **Prevention:** after editing
near a shellcheck directive, run `shellcheck -S style <file> | grep SC<the-disabled-code>` — a hit
means the directive is no longer covering what it says it covers.

## Calling a validate-and-warn resolver function TWICE (once to build a value, once to log it) double-prints its side effect (issue 1260)

A pure-looking resolver like `onbox_decode_priority_class()` (reads an env var, validates it,
prints a `WARNING` to stderr on a bad value) is NOT side-effect-free on the invalid path — calling
it a second time purely to obtain the SAME value for a log line (rather than reusing the value the
FIRST call already produced) re-runs the validation and re-prints the warning, so an operator sees
the same typo'd override reported twice in one run's log. Caught by a fresh-context adversarial
review, not by any test (the RED/GREEN test suite only asserted the value was correct, never that
stderr appeared exactly once). **Fix pattern:** compute the value ONCE (here: `main()` extracts the
already-resolved class back out of the built command string via a pure parameter-expansion parse,
`${ONBOX_CMD#*PriorityClass = \"}` / `${...%%\"*}`, rather than calling the resolver again) and
reuse it for both the command AND the log line. When adding a NEW test for a validate-and-warn
function, assert the WARNING's exact occurrence count somewhere in the full flow, not just that it
appears — this class of bug is invisible to a "does it contain WARNING" check.

## A dev1-watchdog ssh probe wrapped in `timeout <cmd>` BYPASSES a sourced test's `<cmd>` FUNCTION stub → the "hermetic" test hits the LIVE rig (#1290)

The dev1 alert-watchdogs' driver tests (`harness_splitter_port_watchdog_*`, the sibling pattern)
source the watchdog in `--dry-run` and stub `sshpass()` (and `probe_box()`) as BASH FUNCTIONS to
stay hermetic — the function stub intercepts the probe so no real network call is made. But
`timeout N sshpass …` (the form `optical-chain-alert-watchdog.sh` uses for ITS cam2 probe) runs
`timeout` (a real binary) which execs the REAL `sshpass` binary — a shell FUNCTION is not in
timeout's exec environment, so the stub is BYPASSED and the probe reaches the live rig from a unit
test. Confirmed live (#1290): a new `rig_mode_probe` written as `timeout … sshpass … ssh cam2 …`
made the pre-existing splitter-port 739 driver tests non-hermetic AND rig-state-dependent — a real
cam2 probe returned TEST/EVENT/UNKNOWN depending on the live rig, which would flip a `WOULD alert`
assertion.

Two ways the sibling watchdogs avoid it, pick per situation:
- **optical-chain** overrides the HIGHER-LEVEL function wholesale in its test (`measure()`), so the
  `timeout sshpass` line is never reached — fine when the whole measure step is one overridable fn.
- **splitter-port (#1290)** keeps a fine-grained `rig_mode_probe` that the test overrides wholesale,
  AND writes the production form as `sshpass -p PW timeout N ssh …` — `sshpass` stays the OUTER
  command (still intercepted by the function stub → hermetic), while `timeout` bounds the ssh
  itself. NEVER `timeout … sshpass …` on a watchdog whose driver test relies on an `sshpass()`
  function stub. The rule: any wrapper that must exec the stubbed command (`timeout`, `stdbuf`,
  `nice`, `env`) put INSIDE the stubbed command, never outside it, or the stub is bypassed.

## Testing git-ancestry-dependent logic: build a throwaway synthetic repo, never pin against THIS repo's own live history (issue 1292)

A function that reasons about `git merge-base`/ancestry ranges against `origin/main`/`origin/dev`
(the `imag_genlock_range_log`/`imag_genlock_ahead_log`/`imag_genlock_on_dev` family in
`scripts/drift-guard.sh`) needs a REAL git repo to exercise honestly — a mocked/stubbed `git`
binary can't reproduce genuine DAG topology (merge commits, TREESAME collapse, `--is-ancestor`).
The naive move is to test against THIS repo's own checkout (`tests/drift_guard.rs`'s existing
`imag_genlock_range_log_rejects_option_shaped_box_sha_never_a_false_ok_531` already does this, for
a check that only needs "some real history exists"). But for a test whose EXPECTED RESULT depends
on the specific relationship between a fixed commit and `origin/main`'s CURRENT tip, pinning
against live history is a ticking time bomb: `origin/main` only ever GROWS (this repo's two-branch
workflow), so a box that reads "ahead" today can genuinely become "behind" the moment a new PR
merges — exactly what made the incident's own reproduction commit (`box=3ffe2fbc5`) only
transiently ahead. A test asserting `range == empty` against that live sha would eventually flip to
a real, unrelated CI failure with no code regression behind it.

**Fix: build a small, throwaway, fully-isolated two-branch repo per test** — a bare "origin" (`git
init --bare`) + a working clone with a real `origin` remote, `tempfile::tempdir()` for both (never
a hand-rolled pid+timestamp path, per the `#975` entry above), driven via `std::process::Command`.
See `build_two_branch_ahead_repo()` in `tests/drift_guard.rs` for the worked pattern: commit on
`main`, branch `dev`, commit twice on `dev` (touching the SAME paths the function under test
filters on — `vendor/obs-studio`/`vendor/distroav` here), `git merge --no-ff dev` back into `main`
(reproducing a real PR-merge commit with two parents), push both branches, `git fetch origin` so
`origin/main`/`origin/dev` are real remote-tracking refs (NOT local branches of the same name — the
function under test reads the remote-tracking refs, so the test must produce those, not a
same-named local branch). This is deterministic FOREVER (the DAG shape drives the result, not wall
time), and it directly reproduces the exact BUG shape (a box on `dev` past the point that got
merged into `main`) without any dependency on the live repo's ever-advancing tip.

**Before trusting your OWN expected values, verify them empirically in `bash -c`, not by reasoning
about git internals from memory.** git's default `git log A..B -- <pathspec>` history-simplification
is genuinely subtle (git 2.43 `revision.c`'s BOTTOM-flag/TREESAME-collapse rule — a merge commit
that is TREESAME to one parent for the given paths is dropped from the listing in favor of the
non-treesame parent's own commits). A first draft of this exact test file assumed a 2-commit range
would collapse to the ONE merge commit; empirically it printed the TWO underlying non-merge commits
instead. Build the synthetic repo in a throwaway `bash -c` shell first (`git init --bare`, the
merge, `git log --oneline A..B -- paths`), read the ACTUAL output, THEN write the Rust assertion —
never assert a count/SHA you haven't seen printed. (And never add `--first-parent`/`--full-history`
to a `git log` call whose whole point is this collapse — either flag reintroduces the exact false
positive the collapse exists to avoid.)
