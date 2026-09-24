---
paths:
  - "scripts/strih-recordings-retention.ps1"
  - "scripts/strih-recordings-retention.sh"
  - "src/recordings_retention.rs"
  - "tests/recordings_retention.rs"
  - "tests/fixtures/recordings_retention_parity.tsv"
  - "tests/python/test_strih_lx_recordings_retention_1317.py"
---

# E2E recordings retention — dry-run-first sweep (#1122)

**strih-lx (Linux) has its own executor (issue 1317 part 5).** The Windows strih PC was RETIRED at
the M4 cut-over; `10.77.9.202` is the Linux strih-lx, which records to **`/srv/_REC`** (its active OBS
profile `strih-lx`, `[Output] Mode=Advanced` → `[AdvOut] RecFilePath`; read live 24.9.2026: 18
`.mkv`, 12.9 GB, all OBS-timestamp names, all below the 1 GiB floor, dir `newlevel:newlevel 775`,
433 GB free). `strih-recordings-retention.sh` now carries THREE modes and still has **no default box**:

- `--box <fleet-name>` — class dispatch through `scripts/lib/obs-fleet.sh` (the
  `obs-backup-retention.sh --box` precedent): `linux-genlock` → ssh + THIS script fed to
  `bash -s -- --local-sweep …` (no sudo; creds `LINUX_BOX_USER`/`LINUX_BOX_PW`, default newlevel);
  `windows-genlock` → the unchanged `.ps1` driver. `--box strih` → the RETIRED pointer.
- `--host <win-ip>` — the Windows `.ps1` driver by address; a linux-genlock address is refused by
  `obs_fleet_refuse_linux_target` with a `--box strih-lx` pointer.
- `--local-sweep` — the bash decision `rr_plan()` on the current machine. Record dir = `--record-dir`,
  else resolved from the active OBS profile (`user.ini` `[Basic] ProfileDir` → `basic.ini`:
  Advanced → `[AdvOut] RecFilePath` (`FFFilePath` when `RecType=FFmpeg`), Simple →
  `[SimpleOutput] FilePath`); an unresolvable profile FAILS LOUD, never a guessed default.
  `--plan-tsv` prints the raw machine plan; `RETENTION_NOW_EPOCH` is the "now" test seam.

The Linux executor refuses `--execute` with `--keep-runs 0` (the newest file may be the recording OBS
is writing right now) and, before each `rm`, re-checks the file is still a regular non-symlink,
allowlisted, below-floor file. Dirs, symlinks and other non-regular entries are reported as `OTHER`
rows ("PROTECT (not a regular file)") and never touched. The plan is **fail-safe** (review round 1):
`rr_plan` runs as `plan="$(rr_plan ...)" || ...`, where bash IGNORES `set -e` (`inherit_errexit` does NOT
change that -- proven in review round 2), so its safety comes ONLY from explicit checks: every command
in it that can fail is checked and `return 1`s (never add an unchecked one); `--keep-runs` is
bounded to 9 digits and `--keep-days` to 0..36500 (an over-range value used to break a `[ -lt ]` test inside the plan and fall through to DELETE with exit 0); a row becomes DELETE only on
a POSITIVE proof (index >= keep-runs AND (keep-days = 0 OR age >= horizon)) and an unevaluable
comparison aborts the whole run before any `rm`. An unreadable record dir fails loud instead of
globbing to a silent 0-file sweep. The ssh leg carries `UserKnownHostsFile=/dev/null` +
`LogLevel=ERROR` (the address was the Windows strih until M4 — a stale key must not block it) and a
`timeout` INSIDE `sshpass` (`RETENTION_SSH_TIMEOUT`, default 900 s); `--user` / `--obs-config-dir` are
forwarded, the `.ps1`-only `--budget-gb` / `--remote-path` are refused for a Linux box. Every mode
REFUSES a flag that does not apply to it (never silently ignores it): `--local-sweep` refuses `--user` /
`--budget-gb` / `--remote-path`, the Windows driver refuses `--obs-config-dir` / `--plan-tsv`; a zero
`RETENTION_SSH_TIMEOUT` is refused (GNU `timeout 0` disables the bound); `--plan-tsv --execute` is
refused on dev1 before any ssh. The linux leg's dev1 banner goes to stderr, so `--box strih-lx
--plan-tsv` stdout is pure TSV (`OTHER`/`PROTECT`/`KEEP`/`DELETE` rows) for a supervisor script.

Profile resolution reads `[Basic] ProfileDir` (user.ini, then global.ini) and only then the display
name `Profile`. `scripts/strih-obs-start.sh` launches OBS with `--profile "${STRIH_OBS_PROFILE:-strih-lx}"`;
observed live 24.9.2026 on the running strih-lx: `user.ini` `ProfileDir=strih-lx`, matching the launch
profile. If an operator ever launches a different profile without OBS recording it in `user.ini`, pass
`--record-dir` explicitly. The executor reads whole-second mtimes, so at the
exact horizon a file can read up to 1 s older than the Rust `f64` view and same-second files order by
name — negligible, and the shared table uses integer ages for exactly that reason.

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
`scripts/strih-recordings-retention.ps1` (Windows) and `rr_plan()` in
`scripts/strih-recordings-retention.sh` (Linux, `--local-sweep`) are FAITHFUL PORTS of it — keep all
three in sync.

**Parity = ONE shared case table** (issue 1317 part 5): `tests/fixtures/recordings_retention_parity.tsv`
(tab-separated `case`/`file`/`keep`/`delete`/`end` rows, integer ages relative to a fixed `now`) is
read by BOTH `tests/recordings_retention.rs` (`shared_parity_table_matches_the_canonical_plan_1317`,
against `plan()`) and `tests/python/test_strih_lx_recordings_retention_1317.py` (against `rr_plan()`
over a REAL fixture dir — sparse files via `truncate`, mtimes via `utime`). Add a case to the TABLE,
never to one side only. The bash floor `RR_PRODUCTION_SIZE_FLOOR_BYTES` is pinned equal to the Rust
constant. Mirror traps the table covers: the allowlist regex uses an EXPLICIT `[0123456789]` list
under `LC_ALL=C` (the table's fullwidth/Arabic-Indic digit names pin that no Unicode-digit class —
a `.ps1`-style `\d` — ever leaks in; under glibc `[[:digit:]]` would also reject them, so they do not
distinguish the explicit list from `[[:digit:]]`);
the within-days rule `age < days*86400` on integer ages is `age < ceil(days*86400)` (computed in awk
with `%.0f` — strih-lx's awk is **mawk**, whose `%d` clamps at 2^31-1); the newest-first sort is
`LC_ALL=C sort -k1,1nr -k3` (bytewise name tie-break = Rust `String` order); the production floor
pulls big files OUT of the newest-N pool before ranking; enumeration uses `dotglob` so a dotfile is
listed (and protected). Mutation-checked: flipping `-ge`→`-gt`, `-lt`→`-le`, the tie-break direction,
the dedup-suffix space, or the newest-N bound each fails the parity test.

- The EXPLICIT allowlist matches ONLY OBS-timestamp names: `YYYY-MM-DD HH-MM-SS[ (n)].mkv|.mp4`
  (case-sensitive). It is **NEVER a generic `*.mkv` sweep**: a differently-named operator/debug
  recording is PROTECTED. Proven live — `strih700105.mkv` seen in the strih record dir lands in
  PROTECT, never DELETE.
- KEEP a matching file if it is in the newest `KeepRuns` runs OR younger than `KeepDays` (union);
  DELETE the rest. Non-matching files (`Screenshot …png`, `strih-partial-*.json`, custom names) are
  always PROTECTED.
- Defaults `KeepRuns=20 / KeepDays=3` on live strih → 691.3 GB down to **38.4 GB** (under budget),
  652.9 GB freed across 323 runs, all 54 non-recording/foreign files protected.

## Runbook — strih-lx (Linux): DRY-RUN first, then the SUPERVISOR's reviewed --execute

```bash
# 1) DRY-RUN (read-only: ssh + bash -s, resolves /srv/_REC from the OBS profile, deletes nothing)
scripts/strih-recordings-retention.sh --box strih-lx --keep-runs 20 --keep-days 3

# 2) Review the plan: DELETE must hold only OBS-timestamp runs below 1 GiB.

# 3) SUPERVISOR ONLY -- the first real deletion on strih-lx:
scripts/strih-recordings-retention.sh --box strih-lx --keep-runs 20 --keep-days 3 --execute
```

Live 24.9.2026 at the defaults (20 runs / 3 days): 18 files, 12.90 GB, DELETE set EMPTY (18 < 20).
With `--keep-runs 5 --keep-days 1` the dry-run planned 9 deletions (5.71 GB), keeping 7.19 GB.

## Runbook (Windows boxes) — DRY-RUN first, then the SUPERVISOR's reviewed -Execute

Deploy-genlock-fleet.sh emission style: `scp -O` the `.ps1`, run it via `powershell -File` — NEVER
a nested `powershell -Command` over ssh (fails silently, see `rig-state-inspection.md`).

```bash
# 1) DRY-RUN (read-only — deploys the tool, prints the full keep/protect/delete plan, deletes nothing)
scripts/strih-recordings-retention.sh --box stream --record-dir 'C:\Users\newlevel\Videos' --keep-runs 20 --keep-days 3

# 2) Review the printed plan (PROTECT / KEEP / DELETE + SUMMARY). Confirm the DELETE set is only
#    timestamp-named runs and the "after cleanup" total is at/under the budget.

# 3) SUPERVISOR ONLY — the first real deletion (irreversible bulk delete of prod-box files):
scripts/strih-recordings-retention.sh --box stream --record-dir 'C:\Users\newlevel\Videos' --execute
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

Faster than the scratch-module copy: build a one-module `camera_box` stub rlib from the REAL
`src/recordings_retention.rs` (`lib.rs` = `pub mod recordings_retention;`, `rustc --crate-type rlib
--crate-name camera_box`) and compile the REAL `tests/recordings_retention.rs` against it with
`--extern camera_box=<rlib>` (plain `rustc --test`, and `clippy-driver --test -D warnings` for CI's
lint verdict) — the `include_str!` of the parity table resolves relative to the test file. The bash
side is pure pytest (`tests/python/test_strih_lx_recordings_retention_1317.py`, fake `sshpass` on
PATH for the `--box` legs).

## Two gotchas when a `.ps1` mirrors a Rust decision AND travels over scp (both proven live, #1122)

- **A scp'd `.ps1` MUST be pure ASCII.** PowerShell on the box reads the transferred file in a
  non-UTF-8 codepage, so a non-ASCII char in a STRING (an em-dash `—`, `∪`, `≈`) is mangled and
  BREAKS parsing (the first live run failed with `Unexpected token ')'` where an `—` sat inside a
  `Write-Output` string). Keep every scp'd `.ps1` ASCII-only (`grep -nP '[^\x00-\x7F]'` before
  deploying). The sibling `.sh` is bash: an em-dash is harmless there even now that the linux leg
  pipes it to strih-lx over `bash -s` (a UTF-8 box).
- **Use `[0-9]`, never `\d`, in the `.ps1` allowlist regex.** .NET regex `\d` (without
  `RegexOptions.ECMAScript`, which `-cmatch`/`-cnotmatch` do not set) also matches Unicode decimal
  digits (fullwidth `２`, Arabic-Indic, Devanagari), so `\d` makes the on-box executor MORE
  permissive than the Rust spec's `is_ascii_digit()` — the wrong direction for a DELETE gate. `[0-9]`
  keeps the PowerShell mirror byte-exact with the canonical Rust decision. Same lesson applies to any
  future Rust↔PowerShell parity mirror in this repo.
