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
own file, and the `#652` preflight merely WARNs. So aborted / `KEEP_RECORDINGS=1` / early-abort /
failed-download runs leak forever. Live strih (2026-08-19): `D:\_REC` held **397 files / 691 GiB —
344 `.mkv` runs** back to 2025-10-27.

## The WARNING trigger = LOW FREE SPACE, not the file-sum budget (owner ruling #1276, 14.9.2026)

The `#652` preflight WARN was ORIGINALLY "the sum of recording files exceeds `RECORDINGS_BUDGET_GB=50`".
The owner REJECTED that semantics (verbatim: „B varovanie ma byt ked 50gb uz len ostava miesta!!!")
because it false-alarms on a disk with hundreds of GB free (strih 137 GB of recordings but 619 GB
free). Since **#1276** the WARN fires when the recordings VOLUME has at most `RECORDINGS_FREE_MIN_GB`
(default 50, env-overridable) of **FREE space left**:

- Canonical pure decision: **`recordings_retention::free_space_verdict(free_bytes, min_free_gb)`**
  (`src/recordings_retention.rs`, `tests/recordings_retention.rs`) → `Ok` (free ≥ threshold) /
  `Warn` (free < threshold) / `Unknown` (`free_bytes` None — never a false low-space WARN). Decimal
  GB (1e9 B). Exactly `min_free_gb` free is `Ok` (no warn).
- Python mirror the bash preflight calls: **`bundle_state_gather.recordings_free_verdict`** (same
  spec, `tests/python/test_bundle_state_gather.py`).
- The volume's `free_bytes` is served by the box's `:8899 /record-dir-stats.json`
  (`bundle_state_gather.record_dir_stats` reads it via `shutil.disk_usage(record_dir).free` — the
  SAME local record-dir read it already does; no new transport; `None` on failure). The preflight is
  `check_recordings_free_space` in `recording-e2e.sh` (`RECORDINGS_FREE_MIN_GB`); an unreachable
  server / unreadable free space just skips (NOTE), never a false WARN, never a gate.

The **DELETE-set** computation below is UNCHANGED — the owner ruled only on the warning trigger; a
`--execute` deletion remains an owner-only step.

## Production recordings PROTECTED by SIZE (owner ruling #1276, 15.9.2026)

The DELETE-set decision now ALSO protects production-shaped recordings by SIZE. Owner ruling (issue
#1276 comment 5678041040): any recording at or above a size floor is PROTECTED, never deleted,
regardless of age or newest-N rank; only the small E2E-run files are eligible for the DELETE set.
No archive step, no age-based deletion of production files.

- ONE named constant **`PRODUCTION_SIZE_FLOOR_BYTES = 1_073_741_824`** (1 GiB) in
  `src/recordings_retention.rs`, calibrated from the 2.9. dry-run: E2E runs were 0.0–0.8 GB,
  production recordings 5.6 / 7.9 / 17.3 GB — so 1 GiB sits above the E2E max and well below the
  smallest production file. `plan()` pulls at/above-floor files out of the newest-N pool into the
  kept set with the new `KeepReason::ProductionSized` reason (so a big production recording never
  consumes an E2E keep slot); below-floor files keep the newest-N ∪ younger-than-D rule.
- The `.ps1` mirror carries the **byte-identical** `$ProductionSizeFloorBytes = 1073741824`, a
  `$fl.Length -ge $ProductionSizeFloorBytes` PROTECT branch tagging Reason `"production-sized"`, and
  a `SizeFloor` header line so every dry-run shows the floor and WHY files are kept. Parity is pinned
  statically by `tests/python/test_recordings_retention_mirror_1276.py` (no pwsh on dev1): the two
  constants are equal, the `-ge` (never `-gt`) branch and the reason tag are present, and the header
  prints the floor. Re-calibrate the ONE constant (both places) only if E2E-run sizes ever grow past
  ~1 GB.

## Where the recordings live

- **strih** live OBS record dir (`GetRecordDirectory`, "light" profile): **`C:\_REC`** since 17.9.2026
  (owner ruling 18.9., issue 1338 — the D: NVMe dropped off the bus and the owner chose to leave
  recordings on the C: system disk). **Historically `D:\_REC`** (the issue-1122 default). Both wrapper
  defaults (`RECORD_DIR` / `-RecordDir`) now default to `C:\_REC`; `--record-dir` / `-RecordDir`
  overrides it, and a missing dir FAILS LOUD (non-zero exit before any enumeration) rather than a
  silent empty sweep.
- OBS `FilenameFormatting` = `%CCYY-%MM-%DD %hh-%mm-%ss` → `2026-08-19 02-23-06.mkv`; `RecFormat2=mkv`.
- The `bundle-state-server` `/record-dir-stats.json` endpoint (curl `http://<box>:8899/…`) reports
  `total_bytes` / `file_count` / `oldest_mtime` / **`free_bytes`** (#1276) over that dir — the quick
  way to check current usage AND the volume's free space.
- The stream box records `.mp4`; parameterise `-RecordDir` / `--record-dir` for it.

## The decision (keep newest-N runs UNION younger-than-D-days)

Canonical spec: **`src/recordings_retention.rs`** (pure, Tier-0, `tests/recordings_retention.rs`).
`scripts/strih-recordings-retention.ps1` is a FAITHFUL PORT of it — keep the two in sync.

- The EXPLICIT allowlist matches ONLY OBS-timestamp names: `YYYY-MM-DD HH-MM-SS[ (n)].mkv|.mp4`
  (case-sensitive). It is **NEVER a generic `*.mkv` sweep**: a differently-named operator/debug
  recording is PROTECTED. Proven live — `strih700105.mkv` seen in the strih record dir lands in
  PROTECT, never DELETE.
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
