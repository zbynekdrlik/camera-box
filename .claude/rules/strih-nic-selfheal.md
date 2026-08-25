---
paths:
  - "scripts/strih-nic-selfheal.ps1"
  - "scripts/install-strih-nic-selfheal.ps1"
  - "scripts/strih_nic_selfheal_decision.py"
  - "tests/python/test_strih_nic_selfheal_1199.py"
---

# strih on-box NIC-fail self-heal watcher (#1199)

A SYSTEM Scheduled Task on strih that, until the flaky NIC is physically replaced, self-recovers a
total LAN outage: Restart-NetAdapter → (still dead) graceful reboot. Complements the dev1-side
reach watchdog (#1001), which only ALERTS. WoL from S5 is unverified on strih (#1053), so the ladder
ends in a REBOOT (`shutdown /r`), never a power-off — the box returns via auto-logon + the AHK chain.

## The load-bearing design decision: trigger on REACHABILITY, not adapter status

On 2026-08-24 the NIC dropped every packet while the box stayed alive — `Get-NetAdapter` almost
certainly still read `Up`. So a `status=="Down"` trigger MISSES the exact incident. The pass is
classified purely on whether MULTIPLE LAN targets answer (`Test-Connection`); adapter status is
advisory/log only + used to pick which adapter to restart. Never "fix" this into a status check.

## Fail-safe = fail toward inaction

A pass is `dead` ONLY when every probed target returns a CLEAN negative. Any probe error, or nothing
probed, is `unknown` — it never advances the ladder and never resets it. Any single reachable target
is `alive` and resets every counter. (A dead switch also reads all-targets-dead — the ticket accepts
this: Restart-NetAdapter is harmless on an upstream outage and the reboot is bounded by MaxReboots.)

## One pass per schtasks fire + JSON state — NOT a long-running loop

Each task fire runs ONE pass and persists counters to
`C:\ProgramData\camera-box\nic-selfheal-state.json` (`phase`/`consecutive_dead`/`reboots`). Crash-safe
(the next tick just runs again) — deliberately unlike `avsync-watchdog.ps1`'s `while($true)` loop,
whose wedge is invisible (#812). State is written BEFORE acting so a reboot can't re-fire in its own
30 s grace window.

## Pure decision mirror is the source of truth; the ps1 is validated STATICALLY

There is no pwsh runtime on dev1 CI. So `scripts/strih_nic_selfheal_decision.py` carries the ladder
state machine (`classify_pass` + `decide`) with the RED→GREEN behavioural tests, and the ps1 MIRRORS
it in PowerShell. The three ladder constants (`DeadPassesBeforeRestart=5`, `DeadPassesBeforeReboot=5`,
`MaxReboots=2`) MUST stay byte-in-lock-step — `test_strih_nic_selfheal_1199.py` asserts the ps1 lines
equal the python constants. Same "pure core + static-anchor mirror" pattern as
`avsync_lineup.py` ↔ `avsync-watchdog.ps1`. Tier-0 local verify = `python3 -m pytest` on the test;
the ps1 has no local runtime check beyond the static anchors, so the LIVE exercise is a rig step.

## Best-effort OBS-WS graceful stop — never a hard dependency

Before a reboot, `Invoke-ObsGracefulStop` does an obs-websocket v5 handshake (SHA256 salt+challenge
auth) in pure .NET `ClientWebSocket` to StopStream/StopRecord on `127.0.0.1:4455`. strih's OBS-WS HAS
a password: resolved from `-WsPassword` → `$env:STRIH_OBS_WS_PASSWORD` → the out-of-band
`C:\ProgramData\camera-box\obs-ws-password.txt` (the same file `run-bundle-state-server.ps1` uses).
The WHOLE thing is wrapped in try/catch — absent/wrong password, WS down, protocol mismatch, timeout
are all caught and the reboot proceeds REGARDLESS.

## Install / uninstall / retire

`install-strih-nic-selfheal.ps1` (run over the win-strih MCP) copies the watcher into
`C:\ProgramData\camera-box\` and registers the SYSTEM task via `Register-ScheduledTask -Force`
(idempotent), SYSTEM + RunLevel Highest so Restart-NetAdapter/`shutdown /r` are permitted, using
`powershell.exe` (5.1) to match the watcher's `Test-Connection -ComputerName -Quiet` semantics.
`-Uninstall` unregisters + removes the deployed script (`-Purge` also deletes state/log). When the
card is physically replaced, RETIRE the watcher with `-Uninstall`. Live-verify: `-DryRun` (classify +
decide + log, no action) and `Start-ScheduledTask` then tail `nic-selfheal.log`.
