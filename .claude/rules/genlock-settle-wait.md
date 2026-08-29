---
paths:
  - "scripts/lib/genlock-settle.sh"
  - "tests/harness_genlock_settle_1221.rs"
---

# genlock-FIFO settle-wait (issue 1221) — wait on a MEASURED steady-state signal, never a blind sleep

`scripts/lib/genlock-settle.sh` is the E2E harness's SETTLE-WAIT between the `[4i/8align]`
per-source latency-pin writes and `[5/8] StartRecord`. Each pin write re-parameterises that input's
genlock FIFO → a relock/drain/regain era; recording immediately after measured the transient, not
steady-state (verdict 950927573: per-window `derived_uniform_fraction` 0.644→0.967 monotone
convergence, faults concentrated in the head windows). The cure is to WAIT until the FIFO is quiet,
using the FIFO's OWN signal.

## The signal + the #797-immune decision

- The `genlock-fifo audit '<src>':` line (`src/jitter_audit.rs`) appends ~every 5.017 s per source
  with cumulative `relocks` / `underruns` / `dropped_due` / `late_holds` counters. "Quiet for this
  pass" = all four **DELTAS are zero** between two consecutive snapshots of that source's LATEST
  line. `settle` proceeds once every input SEEN in the log has been quiet for N consecutive passes
  (default 2).
- **No rate, no division anywhere in the quiet decision** — it compares raw cumulative counter
  VALUES (delta == 0?), so the `#797` phantom-rate trap (a single-tick window ÷ wall-clock reads
  ~50 at a true 60) structurally cannot apply. This is the OPPOSITE approach to
  `cadence-health.sh`, which DOES measure a rate and therefore must divide by the two lines' OWN
  timestamps — settle needs no timestamps at all.

## Two hard rules a NEW poll-loop-that-waits helper must follow (both bit here)

1. **BOUNDED by TWO independent termination bounds — never one.** A wall budget
   (`E2E_GENLOCK_SETTLE_BUDGET_S`, ~180 s) AND a hard pass ceiling (`GENLOCK_SETTLE_MAX_PASSES`).
   The ceiling terminates the loop even if the clock seam ever fails to advance (a wedged
   `date +%s`, a broken test clock) — a wall-budget-only loop hangs forever on a stuck clock. On
   exhaustion: **FAIL-OPEN with a loud WARN** and proceed (report-only — downstream gates judge the
   recording), never abort, never wait unbounded (`no-timeout-band-aids`: this is not a band-aid
   because it waits on the measured signal, but the BOUND still must exist).

2. **It runs as a BARE statement under recording-e2e.sh's `set -euo pipefail`, so it must ALWAYS
   exit 0 (the #1133 class).** The lib is SOURCED, so `set -e` is active inside it. EVERY path in
   `genlock_settle_wait` returns 0, and every internal command that could fail-abort is neutralised:
   `_genlock_settle_now` ALWAYS exits 0 and emits a valid integer (a failed/garbage clock read → 0)
   so `start="$(_genlock_settle_now)"` can never abort; `_genlock_settle_read_snapshot` ends
   `|| true; return 0`; the awk parse always exits 0; `genlock_settle_pass_verdict` /
   `genlock_settle_all_settled` end on a `printf`/explicit `return 0`. A `set -uo`-only test harness
   is STRUCTURALLY BLIND to this — add a test that sources under the caller's EXACT
   `set -euo pipefail` and asserts the line AFTER the bare call is reached
   (`runner_never_aborts_the_caller_under_set_euo_pipefail`).

## Tier-0 testing a poll loop with zero ssh + zero real waiting

The runner takes three injectable seams so the whole loop is exercised offline:
`GENLOCK_SETTLE_READER_CMD` (a command whose stdout is one snapshot — a test scripts a per-pass
fixture sequence via an idx counter file), `GENLOCK_SETTLE_NOW_CMD` (a fake clock), and
`GENLOCK_SETTLE_SLEEP_CMD` (`:` in tests). The pure functions
(`genlock_settle_latest_counters` / `_pass_verdict` / `_all_settled`) are split from the ssh runner
so they unit-test directly. Default reader = `win_ssh_run` (`scripts/lib/win-ssh-exec.sh`) tailing
strih's newest OBS log — the SAME read the `[4g/8]` calibration block already does (reuse, not new
machinery). Local net (no cargo, #557): the bash replica sourcing the lib (RED before it exists,
GREEN after) + `bash -n` + `shellcheck -S warning` + `cargo fmt --all --check` + the anchor sweep.

## Wiring is anchor-safe (issue 675 pattern)

The `[4j/8settle]` block in `scripts/recording-e2e.sh` is NEW lines only (sourced-helper + a call),
after the `[4i/8align]` `fi` and before the freeze-watch arm; gated `E2E_GENLOCK_SETTLE=1 &&
ALL_CAMBOX=1`; watched inputs = `camera_align_ndi_sources_excluding_csv "${PREFLIGHT_EXCLUDED_CAMS:-}"`
(the exact set align pinned, acked-offline dropped). It never edits an existing anchored line and
never duplicates the `[5/8] StartRecord` literal (comments included).
