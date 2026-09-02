---
paths:
  - "scripts/vb-matrix-alert-watchdog.sh"
  - "scripts/vb_matrix_decision.py"
  - "systemd/vb-matrix-alert-watchdog.*"
  - "tests/python/test_vb_matrix_*.py"
  - "tests/harness_vb_matrix_*.rs"
---

# VB-Matrix presence facet + dev1 alert watchdog (#1227)

Closes the detection gap behind the **2026-08-30 → 09-02 VB-Matrix outage**: VB-Audio Matrix
(`VBAudioMatrix_x64.exe`) was NOT running on the stream box for 3 days after a reboot, because the
Scheduled Task `StartVBMatrix` has only a stale one-shot TIME trigger (no AtLogon). Its virtual
"VB-Matrix VASIO-8" ASIO driver had no host, so both stream OBS ASIO inputs (`ASIO Input Capture`,
`test-audio`) starved (`asrc: … starved_blocks≈2940/min`) while `mbc` (Dante VSC) stayed healthy —
and nothing alarmed (the #1023 asio-starve watchdog ships DISABLED; #1226 audio-lag reads
`ts_lag_ms`, not process presence). This adds process-presence observability.

## The facet is 3-STATE — a FAILED read must NEVER become a false "0" (the load-bearing invariant)

`bundle_state_gather.vb_matrix_process_from_listing(tasklist_csv)` is TRI-STATE, and this is the
whole correctness of the facet (a review 🔴 caught the first cut collapsing it):
- `None` — the listing is UNREADABLE (empty/whitespace = a tasklist subprocess FAILURE, since a
  live box always lists SOME processes; or a `csv.Error`). → the caller omits the facet → UNKNOWN.
- `("", "")` — a VALID listing with no VB-Matrix HOST row (genuinely absent).
- `(name, pid)` — a host found.

`vb_matrix_running_facet(install_present, proc)` then maps: not-installed OR `proc is None` →
`("","","")` (facet OMITTED → UNKNOWN, never a page); install + `("","")` → `("0","","")` (DOWN);
install + `(name,pid)` → `("1",name,pid)`. `"0"` is a TRUTHY string so `build_bundle_state`'s
`{k:v for … if v}` KEEPS it (DOWN surfaces); `""` is dropped. The **disk install-present gate**
(`vb_matrix_install_present_under`, scanning `C:\Program Files (x86)\VB\VBAudioMatrix`) is what
distinguishes "installed but dead" (stream → DOWN) from "never installed" (imag → omitted, never a
false negative). This is the #833 / `obs_process_count_from_listing` "never read a failed read as a
measured zero" class — any future "is process X alive on the box" facet needs the same tri-state.

## `wmic` is GONE on the stream box — CIM `Get-CimInstance Win32_Process` is the only start-time source

Verified live 2026-09-02: `wmic` is REMOVED (Win11 24H2+), so the ONLY way to read a process
CreationDate is `Get-CimInstance Win32_Process -Filter "ProcessId=$pid"` (readable NON-elevated, the
#1067 ExecutablePath precedent). Format it in-shell with **`.ToString("s")`** (the .NET sortable /
invariant-culture ISO-8601 form = `yyyy-MM-ddTHH:mm:ss`) — a custom `HH:mm:ss` pattern is NOT
locale-stable (`:` is the culture time separator). It carries the PowerShell interpreter cold-start
tax, so it reuses the **#1222 PID-keyed cache** (`_vb_matrix_start_cache`: cheap pid key from the
tasklist parse every request, expensive CIM resolve ONLY on a pid change; never-cache-empty,
clear-on-failure). Call it UNCONDITIONALLY with the pid — a falsy/non-numeric pid clears the cache
and returns "" with NO subprocess, so imag / a DOWN box never pays a CIM query AND a same-pid
DOWN→UP restart never serves a stale start (a review 🔵). ONE native `tasklist_csv()` feeds BOTH the
obs process-count facet and this — never two spawns (latency, `bundle-state-gather-latency.md`).

## The dev1 watchdog is DETECTION-ONLY — the cure is an owner step

`scripts/vb-matrix-alert-watchdog.sh` is the 5th obs-watchdog family sibling (reuses
`obs-watchdog-decision.sh` confirm/throttle VERBATIM). Verdicts: SKIP (fetch fail → #732/#1001) /
UNKNOWN (facet absent = imag/old server) / RUNNING / DOWN (page after 2-pass confirm). Stable
`--dedup-key vb-matrix-$box`, recovery log-only (#1206), `require_tools` fail-loud, ships DISABLED.
The cure is **`schtasks /run /tn StartVBMatrix`** on the box (owner/supervisor step — a dev1 timer
has no session-aware win-* MCP for a GUI app). The DURABLE fix is a supervisor step: add an
**AtLogon trigger** to `StartVBMatrix` (check its `ExecutionTimeLimit` is not a default 3-day cap
that would kill VB-Matrix after 72 h) + deploy the new bundle-state-server to the boxes (the server
scripts deploy SEPARATELY from the OBS genlock bundle — `version-integrity-gate.md` #1100).

## Tier-0 from a worktree worker: the fetch-seam dry-run works where a PATH-shim does not

A worktree worker CANNOT PATH-shim `curl` or source the lib under `bash -c` (#1265). But the
watchdog exposes a `VB_MATRIX_FETCH_CMD` env seam (asio-starve `PROBE_CMD` style): a fixture script
run with `<ip>` whose stdout is the bundle-state body. So the FULL watchdog `--dry-run` runs
directly (a plain `VB_MATRIX_FETCH_CMD=… bash scripts/vb-matrix-alert-watchdog.sh --dry-run` — not a
`bash -c`, not a PATH edit → not blocked), proving every verdict path + the 2-pass→alert + recovery
latch end-to-end. Pair with pytest for `vb_matrix_decision.py` + the gather parsers; the Rust
harness (`tests/harness_vb_matrix_alert_watchdog_1227.rs`) type-checks at CI. Add a committed
fixture as `.txt` (not `.log`, gitignored) — here all fixtures are inline, so it did not bite.
