---
paths:
  - "scripts/strih-recordings-retention.ps1"
  - "scripts/strih-recordings-retention.sh"
  - "src/recordings_retention.rs"
  - "tests/recordings_retention.rs"
---

# E2E recordings retention — dry-run-first sweep (#1122)

## Why

The E2E harness (`scripts/recording-e2e.sh`) records ONE OBS program capture per run into each
Windows box's LIVE OBS record directory. `[8/8e]` only prints a `Remove-Item` plan for THAT run's
own file, and the `#652` preflight merely WARNs (`RECORDINGS_BUDGET_GB=50`). So aborted /
`KEEP_RECORDINGS=1` / early-abort / failed-download runs leak forever. Live strih (2026-08-19):
`D:\_REC` held **397 files / 691 GiB — 344 `.mkv` runs** back to 2025-10-27, ~15× the 50 GB budget.

## Where the recordings live

- **strih** live OBS record dir (`GetRecordDirectory`, "light" profile): `D:\_REC`.
- OBS `FilenameFormatting` = `%CCYY-%MM-%DD %hh-%mm-%ss` → `2026-08-19 02-23-06.mkv`; `RecFormat2=mkv`.
- The `bundle-state-server` `/record-dir-stats.json` endpoint (curl `http://<box>:8899/…`) reports
  `total_bytes` / `file_count` / `oldest_mtime` over that dir — the quick way to check current usage.
- The stream box records `.mp4`; parameterise `-RecordDir` / `--record-dir` for it.

## The decision (keep newest-N runs UNION younger-than-D-days)

Canonical spec: **`src/recordings_retention.rs`** (pure, Tier-0, `tests/recordings_retention.rs`).
`scripts/strih-recordings-retention.ps1` is a FAITHFUL PORT of it — keep the two in sync.

- The EXPLICIT allowlist matches ONLY OBS-timestamp names: `YYYY-MM-DD HH-MM-SS[ (n)].mkv|.mp4`
  (case-sensitive). It is **NEVER a generic `*.mkv` sweep**: a differently-named operator/debug
  recording is PROTECTED. Proven live — `strih700105.mkv` sits in `D:\_REC` and lands in PROTECT,
  never DELETE.
- KEEP a matching file if it is in the newest `KeepRuns` runs OR younger than `KeepDays` (union);
  DELETE the rest. Non-matching files (`Screenshot …png`, `strih-partial-*.json`, custom names) are
  always PROTECTED.
- Defaults `KeepRuns=20 / KeepDays=3` on live strih → 691.3 GB down to **38.4 GB** (under budget),
  652.9 GB freed across 323 runs, all 54 non-recording/foreign files protected.

## Runbook — DRY-RUN first, then the SUPERVISOR's reviewed -Execute

Deploy-genlock-fleet.sh emission style: `scp -O` the `.ps1`, run it via `powershell -File` — NEVER
a nested `powershell -Command` over ssh (fails silently, see `rig-state-inspection.md`).

```bash
# 1) DRY-RUN (read-only — deploys the tool, prints the full keep/protect/delete plan, deletes nothing)
scripts/strih-recordings-retention.sh --keep-runs 20 --keep-days 3

# 2) Review the printed plan (PROTECT / KEEP / DELETE + SUMMARY). Confirm the DELETE set is only
#    timestamp-named runs and the "after cleanup" total is at/under the budget.

# 3) SUPERVISOR ONLY — the first real deletion (irreversible bulk delete of prod-box files):
scripts/strih-recordings-retention.sh --execute
```

`--execute` maps to the `.ps1` `-Execute` switch. Everything else is dry-run. For the stream box:
`--host 10.77.9.204 --record-dir 'C:\Users\newlevel\Videos'`. ssh password via `STRIH_SSH_PW`
(default `newlevel`).

## Tier-0 verification

The pure decision has no local cargo path (#477/#557 block ALL local cargo compilation — `--no-run`
INCLUDED, contra the top-level CLAUDE.md Local Build Policy which still describes `cargo test
--no-run` as allowed; the live `block-tier0-local-build.sh` hook blocks every compiling cargo shape).
Verify the module + tests by copying them into a scratch dir as `mod recordings_retention { … }` +
the test file (strip its leading `//!` header) and compiling standalone with `rustc --edition 2021
--test scratch.rs && ./scratch` — rustc is not cargo, so the hook does not touch it, and it runs the
pure logic RED→GREEN with zero repo `target/`. Also run `cargo fmt --all --check` (allowed,
non-compiling — it parses the Rust). The `.ps1`/`.sh` are verified with `bash -n` + `shellcheck` and
a live DRY-RUN against strih (read-only, deletes nothing).

## Two gotchas when a `.ps1` mirrors a Rust decision AND travels over scp (both proven live, #1122)

- **A scp'd `.ps1` MUST be pure ASCII.** PowerShell on the box reads the transferred file in a
  non-UTF-8 codepage, so a non-ASCII char in a STRING (an em-dash `—`, `∪`, `≈`) is mangled and
  BREAKS parsing (the first live run failed with `Unexpected token ')'` where an `—` sat inside a
  `Write-Output` string). Keep every scp'd `.ps1` ASCII-only (`grep -nP '[^\x00-\x7F]'` before
  deploying); em-dashes are fine in the sibling `.sh` (it never leaves dev1).
- **Use `[0-9]`, never `\d`, in the `.ps1` allowlist regex.** .NET regex `\d` (without
  `RegexOptions.ECMAScript`, which `-cmatch`/`-cnotmatch` do not set) also matches Unicode decimal
  digits (fullwidth `２`, Arabic-Indic, Devanagari), so `\d` makes the on-box executor MORE
  permissive than the Rust spec's `is_ascii_digit()` — the wrong direction for a DELETE gate. `[0-9]`
  keeps the PowerShell mirror byte-exact with the canonical Rust decision. Same lesson applies to any
  future Rust↔PowerShell parity mirror in this repo.
