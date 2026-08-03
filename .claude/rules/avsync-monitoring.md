---
paths:
  - "scripts/avsync-watchdog.ps1"
  - "scripts/avsync-vlc-monitor.ps1"
  - "scripts/avsync-keepalive.ps1"
  - "scripts/avsync-watchdog-install.sh"
  - "scripts/avsync-heartbeat-alert-watchdog.sh"
  - "scripts/lib/avsync-heartbeat.sh"
  - "systemd/avsync-heartbeat-alert-watchdog*"
---

# Stream-box avsync watchdog + VLC monitor + dev1 heartbeat alert (#812/#807)

Windows-side long-running loops on the stream box (10.77.9.204) that measure A/V sync and let an
operator listen to the program audio, plus the dev1-side alert that watches both for silence.
Distinct from `.claude/rules/imag-obs-supervision.md` (a DIFFERENT box, OBS process supervision,
not a measurement/monitor pair) — the topology below generalizes that file's dev1-side-alerting
principle to an arbitrary heartbeat FILE rather than a process/WebSocket probe.

## Task Scheduler has no `Restart=on-failure` — the idiom is a periodic idempotent keep-alive check

Unlike systemd, Windows Task Scheduler cannot automatically restart a crashed/hung process tied to
one of its own triggers. `scripts/obs-self-heal-install.sh` (#411) established the repo's answer
for OBS itself: a Repetition-triggered task running a check-and-relaunch script. `scripts/
avsync-keepalive.ps1` (#812/#807) reuses the SAME idiom but far simpler — since restarting IS the
only recovery action needed here (no GPU-cause branching, no AHK-race ordering, no destructive
reboot path), a bespoke Rust `decide()` state machine like OBS self-heal's would be
disproportionate. Match on the process's **CommandLine** via `Get-CimInstance Win32_Process
-Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like "*<script-name>*" }`, never
just the process name — several unrelated `powershell.exe` instances can be running at once
(including the keep-alive check's own invocation), and only the CommandLine substring
disambiguates which script a given process is actually running.

## Generalizing the dev1-side alert topology from a PROCESS/WS probe to an arbitrary heartbeat FILE

`scripts/imag-obs-alert-watchdog.sh` (#882) and `scripts/obs-liveness-watchdog.sh` (#391) already
established: a remote appliance/Windows box has no `~/devel/airuleset` checkout and no Discord
credentials, so the alert MUST fire from dev1. Both of those probe a live process/WebSocket state.
`scripts/avsync-heartbeat-alert-watchdog.sh` generalizes the SAME topology to polling an on-box
**heartbeat FILE** instead — any future "does this Windows/Linux process still write its own
liveness file" need can reuse this shape directly: a one-line `<epoch_seconds>\t<status>` file
written by the monitored process on every pass, read remotely, staleness-checked purely
(`scripts/lib/avsync-heartbeat.sh`'s `avsync_heartbeat_is_stale` — missing/corrupt data is ALWAYS
treated as stale, never defaults to "fresh"), and alerted via the EXISTING `scripts/lib/
obs-watchdog-decision.sh` confirm/throttle functions (never invent a second confirm/throttle
mechanism just because the probe shape changed).

**Reading multiple remote files in ONE ssh round-trip on a Windows box (cmd.exe default shell,
confirmed live via `ssh ... "echo %COMSPEC%"` → `cmd.exe`, NOT bash/powershell):**

```
type "<path1>" 2>nul & echo <UNIQUE_SEPARATOR> & type "<path2>" 2>nul
```

`2>nul` swallows a missing file's error so a not-yet-written heartbeat produces an EMPTY segment
(never a false "ssh failed" reading); `&` (not `&&`) unconditionally runs the next segment
regardless of the previous command's exit code, so one missing file never hides the other's
content. Split the combined output on the separator with `sed -n '1,/^SEP$/p' | sed '$d'` (before)
/ `sed -n '/^SEP$/,$p' | sed '1d'` (after). Reusable for any future "read N status files from one
Windows box in one ssh call" need instead of N separate ssh round-trips.

## Bounding an external call so it can never wedge the loop — pick the mechanism by what needs killing

Two different bounding mechanisms were used deliberately, not interchangeably:

- **`Start-Process -PassThru` + `$proc.WaitForExit(ms)` + `Stop-Process -Id $proc.Id -Force`** —
  used in `avsync-watchdog.ps1`'s `Invoke-Measurement` for the python `av_sync_measure.py` call.
  Gives a DIRECT process handle to the real child process, so a force-kill on timeout actually
  terminates the right PID. The root incident's own evidence ("two orphan python PIDs...consistent
  with a hung av_sync_measure.py") is exactly what a job-based wrapper risks reproducing: `Start-
  Job`'s background job runs in a SEPARATE child `powershell.exe` process, and `Stop-Job` stops
  that wrapper without a guarantee the wrapper's own child (the real python.exe) dies with it.
- **`Start-Job` + `Wait-Job -Timeout` + `Stop-Job`/`Remove-Job`** — used in `avsync-vlc-monitor.ps1`'s
  `Test-RtmpPublishing` for a short `ffprobe` check. Fine here because ffprobe is a single
  short-lived leaf process with no meaningful grandchild-orphan risk, and the job form is simpler
  to write for a "return a value or false on timeout" one-shot check.

Pick `Start-Process`+`WaitForExit`+`Stop-Process -Id` whenever the bounded call is (a) a python/
long-running interpreter that could itself spawn or block indefinitely, or (b) anything where an
orphaned grandchild process would be a real operational problem (stale processes accumulating
across many restarts, as the live incident showed). Reach for the simpler `Start-Job`/`Wait-Job`
shape only for short, leaf, no-grandchild external calls.

## A commit message merely MENTIONING another issue number can block the CURRENT commit

Extends the top-level CLAUDE.md's existing GOTCHA on this (originally documented for #855/#836):
hit AGAIN twice in the #812/#807 session, writing `(see #814's grab-freshness gate)` and `(reusing
the #391 pure confirm/throttle lib)` in commit-message PROSE — both blocked by
`block-commit-without-design.sh` demanding a design comment on #814/#391 (unrelated, already-closed
tickets, cited only for context). Same fix as documented at the top level: write "issue 814"/"issue
391" (no `#`) instead, or reference the RULE/PATTERN by name instead of the ticket number. Two
separate hits in one session confirms this is worth checking proactively — before writing ANY
commit message that cites a past ticket for context, scan it for a bare `#<digits>` first.
