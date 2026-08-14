---
paths:
  - "scripts/lib/e2e-failure-stage.sh"
  - ".github/workflows/full-path-e2e.yml"
---

# Full-path E2E failed-gate Discord alert — derive the stage, never hardcode a verdict (#844)

The `if: failure()` Discord step in `full-path-e2e.yml` fires on EVERY non-zero job exit — a
`[0/8]` preflight abort records no frame. So the alert MUST report the stage that actually failed
and MUST NOT claim a frame-loss/latency verdict for a run that produced none.

- The message is composed by the pure sourced helper `scripts/lib/e2e-failure-stage.sh`
  (`e2e_failure_stage_content OUTDIR RUN_ID SHA_SHORT RUN_URL`). Keep it PURE (no ssh/network/
  writes) so it stays shellcheck-clean and unit-testable.
- The durable per-run truth is the RUN_ID-scoped artifact set under
  `/tmp/recording-e2e-<RUN_ID>/`: `verdict-<RUN_ID>.json` (the SAME file the #703 fail-closed guard
  trusts) + the downloaded `strih-*.mkv` / `stream-*.mp4` / `painter-*.csv`. `RECORDING_E2E_RUN_ID`
  reaches the alert step via `GITHUB_ENV` (written by `recording-e2e.sh` at its very top, before any
  stage) — do NOT invent a mutable phase file: a killed run leaves it stale, which is the exact bug
  class this ticket family (#844) is about. Artifacts are the ground truth.
- ONLY `overall_pass=false` yields a "breach" claim. A missing/malformed/keyless verdict is
  "no trustworthy verdict", a downloaded-recordings-but-no-verdict is a decode-stage abort, and
  neither-present is a pre-recording (preflight/deploy/record-setup) abort — none claim a breach.

## Local verification of a shell helper — cargo tests DON'T run locally here

`cargo test` (even targeted, even with `# airuleset:build-ok`) is BLOCKED in camera-box — the marker
+ `AIRULESET_ALLOW_LOCAL_BUILD` are DISABLED for this repo (airuleset #477); tests run in CI only.
Locally you get `cargo check` / `cargo clippy` / `cargo test --no-run` (compile-check) and nothing
that RUNS a test. To get real RED→GREEN evidence on a pure shell helper, run bash DIRECTLY against
it — the exact thing the Rust harness does internally:
`bash -c '. scripts/lib/<lib>.sh; <fn> <args>'` (and the strict production shape
`bash -c 'set -euo pipefail; . <lib>; <fn> …'` to prove `set -e` never swallows it). The full
`cargo test` suite is the supervisor's job at integration.
