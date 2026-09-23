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
  - "scripts/cadence-alert-watchdog.sh"
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/ndi-halving-watchdog.sh"
  - "scripts/asio-starve-alert-watchdog.sh"
  - "scripts/rig-health-audit.py"
  - "tests/harness_strih_log_read_1360.rs"
  - "tests/harness_strih_log_read_consumers_1360.rs"
  - "tests/python/test_rig_health_audit_strih_lx_1360.py"
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
- `strih_log_remote_cmd <linux|windows> <count|since|tail|headtail> [arg]` — the PURE builder
  (unit-testable). `headtail N` = first 600 + last N lines (the rig-health-audit read: the launch-time
  audio-buffering burst is in the head); no bash consumer calls it, it is the ONE pinned source of the
  audit's python twin (below).

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

## Consumers that watch several boxes (the dev1 watchdogs, issue 1360 part 2)

`cadence-alert` / `ndi-halving` / `asio-starve` `fetch_box_log` and `frozen-input` `probe_received`
watch a configurable box (`*_BOX` / `*_RECEIVER`, stream by default for three of them). The shape:
source this reader, and right after the tail count is clamped,
`if [ "$(strih_log_os "$ip")" = linux ]; then strih_log_tail "$ip" "$SSH_USER" "$SSH_PW" "$_tail" "$SSH_TIMEOUT"; return 0; fi`
— the Windows boxes keep their own `-EncodedCommand` read BYTE-IDENTICAL (the issue-1259
`harness_ps_encoded_fleet_1259` payload tests keep pinning it) and the `*_PROBE_CMD` seam stays the
FIRST branch, so every `--dry-run`/fixture test is unaffected. `frozen-input`'s enumeration read was
already on the reader via `mv_reverify_probe_raw`.

Live proof recipe (read-only, allowed while an E2E holds the rig lease — log reads never mutate):
run each watchdog `--dry-run` with `*_STATE_DIR=<scratch>` (NEVER the real dev1 state dir) and the
box pointed at strih-lx (`FROZEN_INPUT_RECEIVER='strih|10.77.9.202'`, `NDI_HALVING_RECEIVER=…`
+ `NDI_HALVING_RIGMODE_CMD=true`), twice — pass 1 is UNKNOWN by design (no prev sample), pass 2 must
read OK / ADVANCING / HEALTHY. strih-lx carries NO `asrc: source … starved_blocks` lines (the ASIO
sources live on stream), so asio-starve pointed at strih-lx is honestly UNKNOWN.

## The python twin (`rig-health-audit.py`, the status-page feeder)

Python cannot source bash, so the audit carries `strih_platform(host)` + `_linux_obs_log_tail_cmd`
(head 600 + tail N) + `_linux_obs_count_cmd` (`ps -C obs`, zombies excluded — the comm is `obs`, the
CEF children are `obs-browser-pag`, never matched), dispatched per box by `_obs_log_tail_cmd` /
`_obs_count_cmd`. `tests/python/test_rig_health_audit_strih_lx_1360.py` RUNS the bash
`strih_platform` + `strih_log_remote_cmd <platform> headtail N` and asserts byte equality on BOTH
platforms — change the command in `strih-log-read.sh` first, the pytest then forces the twin to follow.
The audit's `ssh()` carries `UserKnownHostsFile=/dev/null` too. The strih row keeps the `obs64=` key
(the status page's generic key=value renderer) even though the Linux process is `obs`.

## Not yet migrated

`scripts/lib/genlock-audit-snapshot.sh` (issue 1354 scope 3) carries its OWN inline `strih_platform`
if-linux branch with a REMOTE grep and a logged SKIP on Windows — the per-helper-branch shape this
reader replaces; migrating it needs a local grep over `strih_log_tail` (or a grep-capable reader op,
added only when a consumer needs it). The remaining `APPDATA\obs-studio` hits under `scripts/` are
NOT strih log readers (launch/deploy/self-heal programs that only ever target a Windows box, the
on-box `bundle-state-server.py` with its own `--obs-log-dir`, and comments).

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

Sourcing a WATCHDOG into such a case: `set --` first (each watchdog parses `$@` at source time and
rejects the case's own argv), and `set +e` right after the source — the watchdogs run under
`set -uo pipefail` (NO `-e`), so a probe whose parse pipeline ends in a `grep` no-match (frozen-input
`probe_received` on an empty read) returns non-zero there by design; under the case's `-e` that
would abort the case before the sentinel. The whole `tests/*.rs` file runs locally with plain
`rustc --test` + a stub `tempfile` rlib (ci-testing-gotchas.md) — the stub needs `TempDir::new()`
too for some sibling files (harness_mv_fps_asio_byte_safety_1262).
