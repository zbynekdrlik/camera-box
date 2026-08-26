---
paths:
  - "scripts/strih-nic-selfheal.ps1"
  - "scripts/install-strih-nic-selfheal.ps1"
  - "scripts/strih_nic_selfheal_decision.py"
  - "tests/python/test_strih_nic_selfheal_1199.py"
---

# strih on-box NIC-fail self-heal watcher (#1199)

A SYSTEM Scheduled Task on strih that, until the flaky NIC is physically replaced, self-recovers a
total LAN outage with a SINGLE self-heal action: a graceful reboot. Complements the dev1-side reach
watchdog (#1001), which only ALERTS. WoL from S5 is unverified on strih (#1053), so the action is a
REBOOT (`shutdown /r`), never a power-off — the box returns via auto-logon + the AHK chain.

## OWNER RULING (2026-08-25) — NO adapter disable/enable/restart, ever

Verbatim: *"uz si nejaky restart eth karty riesil a neuspesne a ja ked vo windows dam ze sa ta karta
ma disablovat a enablovat tak sa to sekne a aj tak musim robit shutdown ... hlavne sa nemotaj vo
veciach ktore si uz skusal!!!!"* On strih a NIC disable/enable HANGS (the owner tried it by hand; a
past session's adapter-restart also failed). So the ladder has NO adapter-restart rung — do NOT
re-add one on any future lane. The only self-heal action is the graceful reboot. `Get-NetAdapter` is
READ for the log only (read-only, never touched).

## The load-bearing design decision: trigger on REACHABILITY, not adapter status

On 2026-08-24 the NIC dropped every packet while the box stayed alive — `Get-NetAdapter` almost
certainly still read `Up`. So a `status=="Down"` trigger MISSES the exact incident. The pass is
classified purely on whether MULTIPLE LAN targets answer (`Test-Connection`). Never "fix" this into
a status check.

## Fail-safe = fail toward inaction

A pass is `dead` ONLY when EVERY probed target returns a CLEAN negative (no probe threw). Any
reachable target is `alive` (resets everything); a partial throw with nothing reachable, or nothing
probed, is `unknown` — and `unknown` never advances the ladder and never resets it. (A dead switch
also reads all-targets-dead — the ticket accepts this: a reboot during an upstream outage is bounded
by MaxReboots and harmless.)

## One pass per schtasks fire + JSON state — NOT a long-running loop

Each task fire runs ONE pass and persists counters to
`C:\ProgramData\camera-box\nic-selfheal-state.json` (`phase`∈{armed,exhausted} / `consecutive_dead`
/ `reboots`). Crash-safe (the next tick just runs again) — deliberately unlike
`avsync-watchdog.ps1`'s `while($true)` loop, whose wedge is invisible (#812). State is written
BEFORE acting so a reboot can't re-fire in its own 30 s grace window — AND the reboot is GATED on a
successful state write (`Write-State` returns a bool; #1199 review W2): if the incremented-`reboots`
state cannot be persisted, the reboot is SUPPRESSED (fail toward NOT rebooting) so a
read-stale-state-after-reboot loop can never exceed `MaxReboots`.

## The ladder (single step, mirror-locked)

`armed --DeadPassesBeforeReboot (=5, ~10 min) dead--> graceful reboot (reboots+1, stays armed)`;
once `reboots==MaxReboots (=2)` and the reboot point is reached again → `give_up`/`exhausted` (never
reboots again, keeps logging loudly). `alive` → reset to `armed/0/0`.

## Pure decision mirror is the source of truth; the ps1 is validated STATICALLY

No pwsh runtime on dev1 CI. `scripts/strih_nic_selfheal_decision.py` carries the ladder state machine
(`classify_pass(reachable, clean, threw)` + `decide`) with the RED→GREEN behavioural tests; the ps1
MIRRORS it in PowerShell. The two ladder constants (`DeadPassesBeforeReboot=5`, `MaxReboots=2`) MUST
stay byte-in-lock-step — `test_strih_nic_selfheal_1199.py` asserts the ps1 lines equal the python
constants AND pins the safety-critical ps1 transitions (the `$rb -lt $MaxReboots` cap guard, the
`Reboots = ($rb + 1)` increment, the `give_up`/`exhausted` branch, the reboot-suppressed-on-persist
guard, `shutdown /r` present + `/s` absent, and that NO adapter cmdlet appears) since none of that is
behaviourally testable without pwsh (#1199 review W3). Tier-0 local verify = `python3 -m pytest`.

## Best-effort OBS-WS graceful stop — never a hard dependency

Before a reboot, `Invoke-ObsGracefulStop` does an obs-websocket v5 handshake (SHA256 salt+challenge
auth) in pure .NET `ClientWebSocket` to StopStream/StopRecord on `127.0.0.1:4455`. strih's OBS-WS HAS
a password: resolved from `-WsPassword` → `$env:STRIH_OBS_WS_PASSWORD` → the out-of-band
`C:\ProgramData\camera-box\obs-ws-password.txt` (the same file `run-bundle-state-server.ps1` uses).
The WHOLE thing is a ONE 5s CancellationToken budget wrapped in try/catch — absent/wrong password,
WS down, protocol mismatch, timeout are all caught and the reboot proceeds REGARDLESS.

## Install / uninstall / retire

`install-strih-nic-selfheal.ps1` (run over the win-strih MCP) copies the watcher into
`C:\ProgramData\camera-box\` and registers the SYSTEM task via `Register-ScheduledTask -Force`
(idempotent), SYSTEM + RunLevel Highest so `shutdown /r` is permitted, using `powershell.exe` (5.1)
to match the watcher's `Test-Connection -ComputerName -Quiet` semantics. `-Uninstall` unregisters +
removes the deployed script (`-Purge` also deletes state/log). When the card is physically replaced,
RETIRE the watcher with `-Uninstall`. Live-verify: `-DryRun` (classify + decide + log, no reboot) and
`Start-ScheduledTask` then tail `nic-selfheal.log`.
