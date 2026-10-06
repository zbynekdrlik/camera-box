---
paths:
  - "scripts/lib/obs-box-baseline-win.sh"
  - "scripts/win-baseline-check.sh"
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/version-integrity-gate.sh"
  - "scripts/rig-health-audit.py"
  - "tests/python/test_obs_box_baseline_win_1357.py"
  - "tests/python/fixtures/win_baseline_1357/**"
  - "scripts/lib/e2e-win-baseline.sh"
  - "tests/python/test_e2e_win_baseline_1357.py"
---

# The ONE Windows OBS-box baseline (issue 1357)

**Why it exists.** On 26.9.2026 RESOLUME-SNV ran the Windows `Balanced` power plan. stream and mbc
ran `Bitsum Highest Performance`, fohabl `High performance`. Balanced caused 10-22 ms host stalls,
and the FOH VB-Matrix underran on both resolume VBAN senders (U+58/+50 per 5 min, then U+1/+16 after
the switch to High performance, issue 1372 comment 5849221193). Nothing graded the difference. This
is the Windows sibling of the Linux baseline (`obs-box-baseline.md`).

## The list, and who may change what

`scripts/lib/obs-box-baseline-win.sh` holds the list. Each item grades OK / DRIFT / UNKNOWN, and an
unread item is UNKNOWN, never OK:

| item | OK when |
|---|---|
| `power_scheme` | the stock High performance (`8c5e7fda-…`) or Ultimate (`e9a42b02-…`) GUID, or a scheme NAMED High performance / Ultimate Performance / Bitsum Highest Performance (case-insensitive) |
| `sleep_ac` | STANDBYIDLE AC index 0 |
| `hibernate_ac` | HIBERNATEIDLE AC index 0 |
| `usb_selective_suspend` | USB selective suspend AC index 0 (Disabled) |
| `wer_dontshowui` | `HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting` `DontShowUI` = 1. The value being absent means the WER crash dialog shows, so absent = DRIFT |

- **Only the power plan is ever SET**, by the Windows genlock deploy program's step `(0b)`
  (`win_baseline_power_plan_ensure_ps`, embedded by `build_windows_deploy_program` for stream and
  resolume). The step runs before any OBS stop. When the active scheme is not max-performance class,
  it activates the INSTALLED High performance scheme (the stock GUID, else the first scheme NAMED High
  performance) and reads it back. On failure it prints a FAIL line and exits 11. It uses `Write-Host`,
  not `Write-Error`: the deploy program runs under `$ErrorActionPreference = 'Stop'`, where
  `Write-Error` throws and the `exit 11` after it never runs.
- Sleep, USB suspend and WER are owner machine settings. They are reported, never written.
- **What `DontShowUI` does and does not cover.** It stops the WER dialog for a process that has no
  crash handler of its own (the obs-browser-page / CEF subprocesses, other rig tools). It never
  covered OBS's own crash handler (`vendor/obs-studio/frontend/obs-main.cpp`, OBS installs its own).
  Since issue 1378 the genlock build's handler shows no dialog at all: it writes the crash file,
  logs `Crash report written to <path>` and exits. The box's respawner can then start a fresh OBS,
  where one runs: the `camera-box-obs-self-heal-stream` task on stream (ships disabled), the
  owner's AHK safe-loop on resolume. This holds once the FULL bundle with that change is deployed;
  an older build on a box still shows upstream's task-modal "OBS has crashed!" box.
  `DontShowUI` still matters for obs64: an `abort()` or a second fault inside the crash path (an
  exception out of the handler, a fault in `exit()`'s static destructors) goes to WER, not to the
  OBS dialog. Only the HKLM value is read, so a `DontShowUI` set by Group Policy or in HKCU still
  reads DRIFT here.
- The timer resolution and MMCSS are not graded (they cannot be set persistently).

**Match by NAME too.** stream's High performance scheme is a DUPLICATE with its own GUID
(`659aca3b-…`). The stock `8c5e7fda-…` is not installed there, and Process Lasso's Bitsum GUID is
per install. A GUID-only list would grade stream's High performance as DRIFT, and a hard-coded
`/setactive 8c5e7fda-…` would fail on stream. The accepted trade-off: the NAME is trusted, so a
user scheme named "High performance" that was duplicated from Balanced would grade OK, and could
become the `/setactive` target when the stock GUID is absent.

**Live state 27.9.2026:** stream = Bitsum, resolume = stock High performance. Sleep, hibernate and
USB are OK on both. `DontShowUI` is absent on BOTH, so `wer_dontshowui` reads DRIFT on both until the
owner sets it. That is an expected report-only row, not a bug.

## Gather, grade, consumers

- **Gather** = `win_baseline_gather_ps1`, a read-only `.ps1`. Run it as a FILE
  (`powershell -NoProfile -ExecutionPolicy Bypass -File …`), never as nested PowerShell over ssh
  (`rig-state-inspection.md` §2).
- **Output format:** `==WINBASELINE-SECTION== <item>`, the raw stdout+stderr lines, then
  `==WINBASELINE-EXIT== <rc>`. A section without its EXIT marker is UNKNOWN. CRLF is tolerated.
- **`reg query` stderr is kept on purpose.** The grader reads "unable to find the specified registry
  key or value" as "value absent". Any other text there is UNKNOWN.
- **The grader is English-label based** (`Power Scheme GUID:`, `Current AC Power Setting Index:`).
  A localized Windows reads UNKNOWN (fail-safe), never OK. The setting GUID line must match the
  section's setting, so a value read from the wrong setting is UNKNOWN.
- **The grader runs under the CALLER's `set -euo pipefail`** (both consumers set it). Every local it
  tests must be initialised. An unset one aborted the grade into an empty verdict on the LIVE state
  (DontShowUI absent), and a pytest that sourced the lib without `-u` stayed green. The pytest
  `_grade` helper now grades under `set -euo pipefail` and requires a real verdict on every row.
- **`scripts/win-baseline-check.sh`** is the dev1 reader. It scp's the gather to
  `C:/camera-box-win-baseline-gather.ps1`, runs it by path and grades each box of the obs-fleet
  facet `win-baseline` (stream, resolume). resolume is SKIPPED when away. The home gate is the
  obs-fleet one (OBS-WS :4455), so a resolume that is up with OBS down is also skipped. That is
  deliberate: the `resolume.lan` / `bridge` .201 address collision (`obs-fleet-list.md`) makes a
  bare ssh-reachability probe unsafe to read as "this is resolume". `--out-dir DIR` keeps
  `DIR/<box>.txt`. The test seam is `WIN_BASELINE_FETCH_CMD <box> <host> <out>`.
- **`version-integrity-gate.sh --win-baseline NAME=FILE`** prints report-only rows
  (`win_baseline_report_rows`) and never touches bad/unknown/ok.
- **The full-path E2E feeds it in `[0/8]`** through `scripts/lib/e2e-win-baseline.sh`, right before
  the version-integrity gate. This is the #675 sourced-helper pattern: `recording-e2e.sh` gains only a
  comment, the source line, the call, and one array line per gate invocation.
  - `e2e_win_baseline_gather "$OUTDIR/win-baseline"` runs `win-baseline-check.sh --out-dir` once.
    It runs under `timeout --kill-after=5 <bound>`, with stdin `/dev/null`, and the output goes to
    `$OUTDIR/win-baseline/win-baseline-check.log`.
  - It prints the check's `box=… win_baseline=…` summary lines, then ONE verdict line:
    `Windows OBS-box baseline: OK | DRIFT | UNKNOWN | TIMEOUT after N s | FAILED (rc N) …`, marked
    `(report-only, does NOT block the run …)`.
  - It always returns 0, so the caller's `set -euo pipefail` never aborts on it. That includes the
    stale-file `rm`: it is guarded, because an unguarded failing `rm` would kill the whole E2E.
- **What it hands the gate:** `WIN_BASELINE_GATE_ARGS` holds `--win-baseline <box>=<dir>/<box>.txt`
  for every facet box whose gather is a regular file, in facet order.
  - The check creates a box's file BEFORE its fetch, so an unread or timed-out box still goes to the
    gate and grades UNKNOWN there.
  - A resolume that is away is SKIPPED by the check and gets no arg.
  - The helper deletes each box's old file first, so a reused `OUTDIR` never grades a stale gather.
    An old path it cannot remove (a directory, a read-only dir) keeps that box out of the array for
    the run, with one line naming it.
  - Both gate invocations (imag acked offline / normal) expand the array with
    `${WIN_BASELINE_GATE_ARGS[@]+"${WIN_BASELINE_GATE_ARGS[@]}"}`, which is safe under nounset.
  - That line sits BEFORE each invocation's `${STRIH_LINUX_GATE_ARG:+--strih-linux}` tail. The pinned
    `--win-state` / `--imag-acked-offline` / `--genlock-sha` sequences and the `--strih-linux` count of
    2 (`tests/harness_strih_platform_1351.rs`) are untouched.
- **Which machine answered:** per graded box the helper prints `box=<b> gathered from host=<name>`,
  the COMPUTERNAME on the gather's `==WINBASELINE-BEGIN==` line (`<none>` for an empty gather). Live
  names: `STREAM`, `RESOLUME-SNV`. `resolume.lan` can resolve to the .201 address `bridge` also holds
  (`obs-fleet-list.md`); when that PC answers :4455 the check reads it as resolume. The check does
  not compare the name yet, so read this line before trusting a resolume row.
- **Where the rows appear in a run log:** first the helper's lines, just before the gate header; then
  the gate's `-- Windows OBS-box baseline (issue 1357: report-only, NEVER gates the run) --` block
  after its fleet rows and before `GATE PASS` / `GATE FAILED` / `GATE INCOMPLETE`.
- **The bound is sized from the check's own per-box bounds**, never guessed:
  boxes × (2 × `WIN_BASELINE_SSH_TIMEOUT` (scp + ssh, 20 s each) + `OBS_FLEET_RESOLVE_TIMEOUT` 2 s +
  `OBS_FLEET_STATUS_TIMEOUT` 4 s) + 10 s = 102 s for stream + resolume.
  - `E2E_WIN_BASELINE_TIMEOUT` overrides it.
  - The three defaults are pinned to their sources by a test, so raising one in
    `win-baseline-check.sh` / `obs-fleet.sh` fails there instead of turning a slow box into TIMEOUT.
  - Values are read base 10, so a leading zero is never read as octal. An octal error inside `$(…)`
    would abort the caller.
- **When the bound hits:** GNU `timeout` TERMs its whole process group.
  - The check is bash with an EXIT trap, so it catches the TERM, removes its own mktemp work dir and
    exits. `timeout` returns 124 at once, read as TIMEOUT (137, a needed `--kill-after` KILL, too).
  - The fetch child the check was waiting on is TERMed with it.
  - Each inner `timeout 20 scp/ssh` runs in its own process group, so it ends within its own 20 s.
  - A child that IGNORES TERM would outlive the run: `timeout` exits with the check, before any
    KILL. The real chain stays bounded anyway, because every scp/ssh runs under its own inner
    `timeout 20`.
- **Cost per run:** one scp + one ssh + one `powershell -File` per home box, a few seconds when the
  boxes answer. The only write is the check's own gather file on the box
  (`C:/camera-box-win-baseline-gather.ps1`, overwritten every run).
- **Tests:** `tests/python/test_e2e_win_baseline_1357.py`, run by CI's `pytest tests/python`.
  - Each outcome runs under a `set -euo pipefail` caller, through the fetch seam. That includes a hung
    fetch whose process must be gone and whose work dir must not leak, and a stale gather that cannot
    be removed.
  - The REAL gate `if … fi` block text is run against a stub gate on both branches, with the array
    set, unset and empty.
  - The gate subprocesses seed `VERSION_INTEGRITY_GATE_VENDOR_NEWEST` / `_PENDING`, so no test runs a
    live `git fetch origin` (the same default `tests/version_integrity_gate.rs` uses).
- **`rig-health-audit.py`** `check_win_baseline` emits one NOTE row per read box, naming every
  non-OK item. NOTE rows are never counted.

## Verify without touching a setting

- **Tier-0 net:** the pytest file (plus the no-network run from `ci-testing-gotchas.md`),
  `bash -n` + `shellcheck -S warning -x` on the touched scripts, and the std-only Rust files through
  `rustc --test` with the tempfile stub (`deploy_genlock_fleet`, `version_integrity_gate`,
  `harness_obs_fleet_list_1296`).
- **Live, read-only:** paste the gather body (without its final `exit 0`) into the box's win-* MCP
  Shell and grade the output. The real resolume output is the fixture
  `resolume_gather_program_live_2026-09-27.txt`.
- **Check the deploy block without running it:**
  `[System.Management.Automation.Language.Parser]::ParseInput($src, [ref]$tok, [ref]$errs)` on the
  block text. That is a parse, it executes nothing. For the logic, run only the `Get-WbActiveScheme`
  / `Test-WbMaxPerf` / target-pick part, never the `/setactive` line.

## Traps when editing the deploy block

- `tests/deploy_genlock_fleet.rs` finds the FIRST `# (1b)`, `# (8)`, `# (8b)`,
  `Get-Process obs64,obs-browser-page` and `Stop-Process -Name AutoHotkey64`, and counts
  `$ErrorActionPreference = 'Stop'`. The block's comment is labelled `# (0b)` and must never contain
  any of those strings.
- No error message may contain the literal `powercfg /setactive`. The pytest counts the powercfg
  mutating verbs in the whole deploy program and expects exactly one.
- `deploy-genlock-fleet.sh` is at 985 of its 1000-line budget (asserted in `deploy_genlock_fleet.rs`).
- The facet's rendering lives in the lib (`win_baseline_report_rows`); the gate only adds the wiring
  (one option + one call line in main()). The gate was since split (issues 1377 + 1384: facet libs,
  `vig_row_*` row functions), so a gate row belongs in its lib, never back in main()
  (`version-integrity-gate.md`).
- **rig-health:** a reader rc outside 0/11/20 is a crashed reader and always emits a NOTE row,
  even after some box rows. An empty verdict is read as UNKNOWN, and the reader output is decoded
  with `errors="replace"` (OEM-codepage scheme names must not crash the audit).
- **The reader's box summary line** lists only DRIFT items (`drift=`) and unread items
  (`unknown=`), never OK ones. The tests anchor the whole line (`re.M`, `^…$`), because an `in`
  substring check passed while every OK item was listed as unknown.
