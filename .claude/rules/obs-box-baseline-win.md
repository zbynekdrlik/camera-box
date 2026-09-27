---
paths:
  - "scripts/lib/obs-box-baseline-win.sh"
  - "scripts/win-baseline-check.sh"
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/version-integrity-gate.sh"
  - "scripts/rig-health-audit.py"
  - "tests/python/test_obs_box_baseline_win_1357.py"
  - "tests/python/fixtures/win_baseline_1357/**"
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
  logs `Crash report written to <path>` and exits, so the box's launcher can restart OBS. This
  holds once the FULL bundle with that change is deployed; an older build on a box still shows
  upstream's task-modal "OBS has crashed!" box.
  `DontShowUI` still matters for obs64 in one case: an `abort()` (for example a C++ exception out
  of the handler) goes to WER, not to the OBS dialog. Only the HKLM value is read, so a
  `DontShowUI` set by Group Policy or in HKCU still reads DRIFT here.
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
  (`win_baseline_report_rows`) and never touches bad/unknown/ok. `recording-e2e.sh` does NOT feed it
  yet. Wiring the gather into `[0/8]` means an ssh + scp to stream on every run, plus the static-anchor
  discipline for that file.
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
- `deploy-genlock-fleet.sh` is at 975 of its 1000-line budget (asserted in `deploy_genlock_fleet.rs`).
- `version-integrity-gate.sh` was over its 1000-line budget before this facet (1013 lines). That is
  why the facet's rendering lives in the lib and the gate only adds the wiring (1022 lines). A split
  of the gate is a separate refactor for the supervisor to schedule, not part of this facet.
- **rig-health:** a reader rc outside 0/11/20 is a crashed reader and always emits a NOTE row,
  even after some box rows. An empty verdict is read as UNKNOWN, and the reader output is decoded
  with `errors="replace"` (OEM-codepage scheme names must not crash the audit).
- **The reader's box summary line** lists only DRIFT items (`drift=`) and unread items
  (`unknown=`), never OK ones. The tests anchor the whole line (`re.M`, `^…$`), because an `in`
  substring check passed while every OK item was listed as unknown.
