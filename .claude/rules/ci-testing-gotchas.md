---
paths:
  - ".github/workflows/*.yml"
  - "tests/*.rs"
  - "scripts/version-integrity-gate.sh"
  - "scripts/drift-guard.sh"
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
