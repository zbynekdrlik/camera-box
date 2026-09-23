---
paths:
  - "scripts/lib/strih-log-read.sh"
  - "scripts/lib/strih-platform.sh"
  - "scripts/lib/qr-align.sh"
  - "scripts/lib/genlock-settle.sh"
  - "scripts/lib/mv-reverify-escalate.sh"
  - "scripts/lib/frozen-cam-received.sh"
  - "scripts/lib/ndi-cadence-heal.sh"
  - "scripts/lib/mv-fps-preflight.sh"
  - "scripts/mv-fps-alert-watchdog.sh"
  - "tests/harness_strih_log_read_1360.rs"
---

# Reading the strih OBS log — ONE platform-resolved reader (issue 1360)

Since the M4 cut-over the strih role is the Linux notebook **strih-lx** (10.77.9.202): its OBS log
is the newest `~/.config/obs-studio/logs/*.txt` (the name carries SPACES — `2026-09-23 09-23-36.txt`),
not `%APPDATA%\obs-studio\logs\*.txt`. Every helper that grew its own Windows-only PowerShell reader
went silently blind there (READ_FAIL, "ssh flake/timeout", settle never quiet, the align without its
measured arrival floor). **Never add a new inline strih-log read — call `scripts/lib/strih-log-read.sh`.**

## The API (every function returns 0; any failure = EMPTY output)

- `strih_log_tail <host> <user> <pw> <n> [timeout_s]` — last N lines of the newest log.
- `strih_log_line_count <host> <user> <pw> [timeout_s]` + `strih_log_since_line <host> <user> <pw> <start> [timeout_s]`
  — the time-scoped pair (mark the length, let lines accrue, fetch ONLY the lines after the mark; the
  `[4g/8]` Correction-2 discipline). A non-numeric mark returns EMPTY without an ssh — never a
  whole-log (regime-mixed) fetch.
- `strih_log_os <host>` → `linux|win` — for a consumer that owns its own two-platform reader keyed on
  an `os` token (the MV-fps pair); give that consumer the `strih` token and resolve it through this.
- `strih_log_remote_cmd <linux|windows> <count|since|tail> [arg]` — the PURE builder (unit-testable).

The platform comes from `strih-platform.sh` `strih_platform` (env `STRIH_PLATFORM` override, else the
strih-lx address → linux, else windows). The Windows strings are the pre-existing ones, verbatim; the
tail is the `gc (gci …).FullName -Tail N` form sent as `-EncodedCommand` (the issue-1258 cmd.exe rule).

## Transport rules baked in (keep them when extending)

- `sshpass -p PW timeout T ssh …` — `timeout` INSIDE sshpass, so a function/PATH-stubbed `sshpass`
  stays the outermost command (the ci-testing-gotchas stub-bypass rule) and the ssh itself is bounded.
- `-o UserKnownHostsFile=/dev/null`: 10.77.9.202 was the Windows box before M4; a stale known_hosts
  key with `StrictHostKeyChecking=no` makes OpenSSH DISABLE password auth → a silent empty read.
- The lib never sources `win-ssh-exec.sh` (its top-level `set -euo pipefail` would leak into the
  non-strict watchdog callers); it sources only `strih-platform.sh` + `ps-encoded.sh`, both set-e-free.

## Not yet migrated (still Windows-only strih-log reads — follow-ups, not this reader's bugs)

The dormant `PRERECORD_PHASE_CALIBRATE=1` `[4g/8]` block in `recording-e2e.sh`, and the dev1 watchdogs
`ndi-halving` / `cadence-alert` / `asio-starve` / `frozen-input` `probe_received`, plus
`rig-health-audit.py`. `frozen-input`'s ENUMERATION read already goes through `mv_reverify_probe_raw`
and so through this reader. `scripts/lib/genlock-audit-snapshot.sh` (issue 1354 scope 3) carries its
OWN inline `strih_platform` if-linux branch with a REMOTE grep and a logged SKIP on Windows — exactly
the per-helper-branch shape this reader replaces; migrating it needs a local grep over
`strih_log_tail` (or a grep-capable reader op, added only when a consumer needs it).

The MV-fps pair's OWN Linux ssh carries the same `UserKnownHostsFile=/dev/null -o LogLevel=ERROR`
options as this reader (a strih-lx read through them hits the same stale-key hazard).

## Tier-0 test recipe

`tests/harness_strih_log_read_1360.rs`: each case is a bash FILE run as `bash <case.sh> <tmpdir>`
under `set -euo pipefail`; a PATH-stubbed `sshpass` logs its argv and `exec`s the rest; a PATH-stubbed
`ssh` RUNS a Linux remote command for real against a fixture HOME (two logs, the newer one spaced,
mtimes set with `touch -d`) and answers a `powershell …` one with a canned reply — so the platform
branch actually taken, the newest-file choice and the spaced-name quoting are all observable. A
worktree worker can run the same case files locally with plain `bash <file> <dir>` (the isolation
guard refuses `bash -c`, not a file).
