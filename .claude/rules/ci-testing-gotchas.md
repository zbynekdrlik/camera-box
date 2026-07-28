---
paths:
  - ".github/workflows/*.yml"
  - "tests/*.rs"
  - "scripts/version-integrity-gate.sh"
  - "scripts/drift-guard.sh"
  - "scripts/dantesync-gate.sh"
  - "scripts/clock-offset-guard.sh"
---

# CI + bash-test-harness gotchas (#826)

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
