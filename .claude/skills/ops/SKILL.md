---
name: ops
description: >
  Camera-box rig operations — DanteSync cluster clock, device deployment, and rig recovery.
  Load when: deploying camera-box to CAM1-4, diagnosing time-sync issues, checking DanteSync
  status, recovering wedged cameras, or any infra work on the cam/strih/stream cluster.
---

# Camera-box Ops

## Diagnostika: "multiview nejde plynulo" pri ČISTOM renderi = počítaj dantesync Stepped (2026-07-18)

Operator feel "MV nie je 30fps" while strih GetStats shows a PERFECT render (30.0fps, 0% skips)
and inputs show 60fps arrivals with zero holds in a short window: the hitches are sporadic
per-cell TS-ALIGN holds caused by sender CLOCK STEPS — count them per cam box:
`journalctl -u dantesync --since "-30 min" | grep -c Stepped`. Live event 2026-07-18: 1-33
steps/30min per cam (~100 fleet-wide = a visible hitch somewhere in the grid every ~18s) from
NTP jitter on the loaded venue LAN; at home offsets are sub-100µs with zero steps. Each ~1-2ms
step jumps that camera's genlock timecodes -> its cell holds a frame -> a single-cell hic. An
OBS restart does NOT help (nothing accumulates receiver-side); robustness/alarming is
dantesync#50. Second (structural) component of the feel: MV cells are sharp 60->30 decimation
judder — the #792 "CAMn (30p)" blend streams are the designed cure once strih consumes them.

## Venue/event network — MikroTik topology + the microburst egress-drop gotcha (#797, 2026-07-18)

The traveling rig LAN is all-MikroTik, ssh user `admin` (password = the fleet deploy pw — memory
`cam-fleet-deploy-credential`, NOT committed): router_snv RB4011 = 10.77.8.1; CRS310-8G+2S+
switches: foh1_audio 10.77.9.2, stage_av 10.77.9.3, foh1_video 10.77.9.4, foh2_video 10.77.9.5.
10G SFP+ trunks between them; port naming `etherN::dante|::basic|::trunk`. Map a device to its
physical port: `/interface bridge host print where mac-address=XX:...` per switch (a MAC learned
on a `::trunk` port means "behind that trunk, next switch").

**GOTCHA — a 2.5G edge port fed from a 10G trunk TAIL-DROPS microbursts with the default buffer
split** (CRS310 default `shared-buffers=40%`): live incident = imag on foh2 ether3 dropped ~8
pkt/s (`/interface ethernet print stats` → `tx-drop-queue1-packet`), strih on the 10G SFP+ port
clean. Fix (applied 2026-07-18, KEEP): `/interface ethernet switch qos settings set
shared-buffers=80%` → drops 0. Check drops first on any "receiver gets less than sender sends"
report: `:local a [/interface ethernet get [find name~"etherX"] tx-drop-packet]; :delay 6s; ...`.
NOTE: eliminating these drops did NOT lift the imag 50fps cap (#797 — NDI sender-side pacing vs
the Linux receiver; post-event lab plan on the ticket). Also: cam boxes sit on `::dante`-named
ports in places — verify no future Dante QoS policy throttles video there.

## DanteSync Cluster Clock

DanteSync (`~/devel/dantetimesync`) is the cluster wall-clock basis for genlock (#8/#42).

**Topology:** strih.lan (10.77.9.202) = NTP master (`ntp_server_mode`, stratum 3).
All nodes sync to strih: stream, cam2 (`--ntp-server strih.lan`), dev1.

**PTP (primary):** grandmaster 10.77.9.184 — all nodes LOCK to µs-grade parity.
**NTP fallback** (when GM absent): ±0.3-1 ms stepping sawtooth — genlock must degrade gracefully.

**Verified node status (2026-06-15 — do NOT re-doubt; VERSIONS in this table are historical):**

| Node | IP | Version | Mode | Measured offset to strih |
|---|---|---|---|---|
| cam1 | 10.77.9.61 | v1.8.11 | LOCK | 0.50 ms |
| cam2 | 10.77.9.62 | v1.8.11 | NANO | 0.23 ms |
| cam3 | 10.77.9.63 | v1.8.16 | LOCK | 0.07 ms |
| cam4 | 10.77.9.64 | v1.8.11 | — | — |
| dev1 | — | v1.8.16 | LOCK | 0.16 ms |

`systemctl is-active dantesync` = active+enabled on all.

**Fleet version = whatever `scripts/dantesync-version-gate.sh`'s `DANTESYNC_VERSION_PIN` says
(1.8.30 since 2026-08-11 — NTP server-mode self-discipline fix), and a fleet ROLLOUT MUST bump
that pin in the SAME cycle.** The pin's own comment mandates it, but the rollout happens in the
dantesync repo / on the boxes where nothing re-reads that comment — 2026-08-11 the 1.8.30 rollout
skipped the bump and EVERY E2E run refused with all 6 nodes "DRIFT" against the stale 1.8.25 pin
until it was bumped. Rollout checklist: canary → fleet → verify 8/8 → bump `DANTESYNC_VERSION_PIN`
→ push. **The fleet roll on the Windows boxes (strih+stream) covers TWO binaries: the
`dantesync.exe` SERVICE and the `dantesync-tray.exe` TRAY** — every upgrade script until
2026-08-12 swapped only the service, so the tray (the version the USER actually sees in the
system-tray UI) silently sat months stale (strih's was ~22 releases behind; user-caught). Tray
swap = stop `dantesync-tray` process, copy `dantesync-tray-windows-amd64.exe` over
`C:\Program Files\DanteSync\dantesync-tray.exe` (backup `.pre-<ver>`), relaunch via the win-*
MCP Shell (session 1 — an ssh-launched tray is invisible to the operator). Linux nodes have no
tray.

### resolume-snv (CG box) — DanteSync maintenance target (#811)

`resolume-snv` (RESOLUME-SNV, the CG / graphics PC: Resolume Arena → strih via Spout/NDI, plus a
`cg-obs`) is a **DanteSync maintenance target** — a **Windows** box (shares the OBS-WS password
with strih/stream; `.claude/rules/rig-state-inspection.md` §2), so the SAME two-binary
service+tray Windows path as strih/stream. #800 found its NDI feeds drifting **~+65 ms/h all day**
because it carried no dantesync — this brings it under the fleet clock-discipline umbrella.

- **IP is NOT pinned.** `resolume.lan` currently resolves to `10.77.9.201` — the SAME IP `bridge`
  lists in `targets.md` (event-LAN DHCP drift/collision). Always `getent hosts resolume.lan` and
  confirm the box identity (DHCP lease / OBS profile name, never "the shared OBS-WS password
  worked") before deploying. It is a **traveling box**, often powered off/away between events.
- **Deploy dantesync — canary-first, pin-not-latest** (`.claude/rules/dantesync-fleet-upgrade.md`).
  Roll it with the standard mechanism, adding it to the Windows node list:
  `scripts/dantesync-fleet-upgrade.sh --win "strih=newlevel@<strih> resolume=newlevel@<resolume>"`
  (or solo). strih is the NTP master and its OWN canary, so a fleet roll converges it first; a
  solo resolume roll verifies with `--ntp-master ""` (it is a slave). Bump `DANTESYNC_VERSION_PIN`
  only as part of a fleet-wide version bump, never for a single-box add.
- **Version-parity — STANDALONE, NOT the E2E `[0/8]` gate.** resolume is not a measured source in
  the cam→strih→stream recording path, so its dantesync version cannot corrupt an E2E verdict, and
  wiring it into `recording-e2e.sh`'s `[0/8]` roster would fail-CLOSE every E2E run whenever the
  traveling box is away (the imag-nb #1013 pain). Check it on its own during maintenance:
  `scripts/dantesync-version-gate.sh --win "resolume=newlevel@<resolume>"`. When away, exclude it
  from any fleet-wide roll/check via the `rig-fleet.txt` ack mechanism under the box name
  `resolume` (`cambox_offline_ack` is generic over any node name).
- **Frame-loss-free playback verify (acceptance #4).** After the roll, confirm its feed plays
  frame-loss-free by reading its genlock-FIFO audit on strih (and stream). Pull the box's OBS log,
  then run the verdict mode (INPUT names are whatever the OBS config uses for the resolume feed —
  e.g. `cg`, `NDI obs hudba`, `RESOLUME-SNV (cg-obs)`):
  `genlock-jitter-report --file <strih-obs.log> --verdict-source 'cg' --verdict-source 'NDI obs hudba'`
  → one `RESOLUME-VERDICT <name>: PASS/FAIL/ABSENT` line each (exits 3 on any FAIL/ABSENT, or 0
  with `--verdict-report-only`). PASS = skew flat within `--skew-bound-ms` (default 20) **and**
  zero drop/underrun/relock/late-hold/backward-regime deltas over the window. It is
  cadence-agnostic (resolume is a **non-60** CG source, #787 — holds/overruns from cadence
  adaptation are NOT gated; the frame-jump signal `backward_regime_ticks` IS). For the 24h
  acceptance (skew flat ±20 ms), sample a long OBS-log window and raise `--min-samples`. The
  supervisor owns the live deploy + the 24h run; this is a verify tool, not a hard E2E gate.
- **Reachability watchdog — REPORT-ONLY (#811).** resolume is in the dev1-side network-reach
  watchdog's `BOXES` roster + `NETWORK_REACH_REPORT_ONLY_BOXES` (default `resolume`), so it is
  probed (ping OR :4455) + logged + per-box state-tracked but **NEVER pages** — its absence is the
  normal state for a traveling box, so a page would be noise (`.claude/rules/network-reach-watchdog.md`).
  Once it becomes a permanent fixture, flip it to a paging node by removing `resolume` from
  `NETWORK_REACH_REPORT_ONLY_BOXES` (it stays in `BOXES`). The watchdog ships DISABLED — enable on
  dev1 with `systemctl --user enable --now network-reach-alert-watchdog.timer`, unchanged by this.
- **Remote wake (#811).** resolume's WoL MACs are in `scripts/wol-targets.txt` as TWO rows —
  `resolume` (primary NIC) + `resolume-alt` (2nd NIC, same `.201`, different MAC). `scripts/wake-box.sh
  resolume` fires ONLY the primary row's MAC, so when the active NIC is unknown wake BOTH before a
  maintenance roll: `scripts/wake-box.sh resolume; scripts/wake-box.sh resolume-alt`.

**HARD RULE:** DanteSync OWNS the clock on cam boxes AND dev1. NEVER install/enable
chrony / ptp4l / systemd-timesyncd / any other NTP/PTP tool, and NEVER run
`timedatectl set-ntp true` (DanteSync's `install.sh` disables them).

**#591 — timesyncd is a HARD-FAIL, and the appliance must not even HAVE it installed.**
cam5/cam6 (N150) shipped with `systemd-timesyncd` **active+enabled ALONGSIDE dantesync** → a real
**5.28-second** clock desync (`[NTP] offset:-5280959us`), invisible to weeks of "passing" verify
runs. Masking is a band-aid — a minimalist cambox/imag appliance runs ONLY dantesync, so the
package must be **PURGED**. Enforcement (all landed #591):
- **Provisioning purges it:** `setup-device.sh` STEP 17 + `create-usb-linux.sh` chroot both
  `apt-get purge -y systemd-timesyncd chrony ntp ntpsec openntpd` (loop, then `systemctl mask` as
  backstop). `apt-get -s purge systemd-timesyncd` confirms NO cascade (standalone removal).
- **`verify-device.sh` (r) hard-fails** any competing timesync daemon that is INSTALLED (even
  masked) / ACTIVE / enabled (`timesync_authority_verdict`). A clean box = every competing daemon
  not-installed, dantesync only.
- **`verify-device.sh` (d) rewritten (#550):** grades the FRESHEST `[NTP] offset:` line (reads
  `journalctl -o short-iso`, rejects a stale boot-step line via `DANTESYNC_OFFSET_FRESHNESS_S`
  default 300s), FAILs on a fresh offset outside ±2000µs (`CLOCK_GUARD_BOUND_US`). The pre-#591
  `tail -1` grabbed the stale `-5280959us` boot-step line (the #550 bug).
- **#595 — the SAME #550-class staleness bug also lived in the OTHER two DanteSync-offset gates**
  (found via #591's own review, ~3 weeks after #591 shipped): `scripts/dantesync-gate.sh` (#7
  recording-E2E precondition) and `scripts/clock-offset-painter-gate.sh` (#326 all-cambox sweep
  comparator) both still called the age-blind `offset_us_from_journal`/`offset_check` — #591 had
  only touched `verify-device.sh`. Fixed by MOVING `dantesync_offset_verdict` +
  `_short_iso_epoch` down into the SHARED `scripts/clock-offset-guard.sh` (so all THREE gates, plus
  any future one, share one implementation instead of copies drifting apart) and adding a
  value-returning sibling `freshest_offset_us(JOURNAL, FRESHNESS_S)` for the painter gate's
  RELATIVE dev1<->painter comparator (which needs the raw fresh value from each box, not a
  bound-graded verdict — check freshness on EACH box independently BEFORE ever comparing them).
  **Lesson for the next DanteSync-offset caller:** grep `offset_us_from_journal\b` (the raw,
  age-blind parser) across `scripts/*.sh` before adding a new gate — if a new caller still reaches
  for it directly instead of `dantesync_offset_verdict`/`freshest_offset_us`, it's exposed to the
  #550 bug. **Known remaining gap (filed #607, deliberately NOT fixed by #595):**
  `scripts/clock-offset-guard.sh`'s OWN standalone CLI (`main()`, the original #8 regression guard
  documented in SETUP.md) still uses the old age-blind primitives internally — it was not named in
  #595's scope. **Bash gotcha (the one non-obvious thing #595's review caught):** a freshness-window
  parameter fed unvalidated into `[ N -gt "$fresh" ]` — if `$fresh` is non-numeric, bash prints
  "integer expression expected" and the test returns non-zero, which an `if A || B || C` OR-chain
  reads as "condition false" (never surfacing the error) — silently disabling the staleness check
  and making every reading look fresh. Any bash gate with an `||`-chained numeric guard needs its
  own numeric inputs validated (`grep -qE '^[0-9]+$'`) the same way `--bound-us`/`--guard-us` CLI
  flags already are in these scripts — an env-var-only knob is easy to forget.
- **#596 — `drift-guard.sh --check-imag` (imag-nb) inherited the SAME `(r)` sole-timesync-authority
  gate**, closing #591's own "verify-fleet/drift-guard should inherit it too" item for imag-nb (a
  plain Linux box that isn't part of `verify-device.sh`/`verify-fleet.sh`'s cam1-6 fleet loop, so it
  never got check (r) automatically). `dpkg_status_installed`/`timesync_enabled_state_neutral`/
  `timesync_daemon_verdict`/`timesync_authority_verdict` moved into a new shared
  `scripts/lib/timesync-authority.sh` so `drift-guard.sh` can reuse the IDENTICAL verdict instead of
  a second copy. **Lesson for the next shared-check extraction:** sharing the VERDICT function is
  NOT enough — a code-review pass on #596 caught that the per-daemon SSH-*gathering* for-loop
  (the actual `dpkg -s`/`systemctl is-active`/`is-enabled` remote command) was STILL duplicated
  verbatim between the two callers even after the verdict was shared; a future daemon added to the
  competing set could silently diverge between them. Extracted THAT into
  `timesync_gather_remote_snippet()` too. **Testing lesson:** a shared "remote command string"
  function is easy to leave untested (the callers' tests only exercise hand-written fixtures
  through the parser, never the snippet's OWN output shape) — the fix was a test that runs the
  snippet locally via `bash -c "$snippet"` (safe read-only `dpkg`/`systemctl` calls, works on any
  Ubuntu CI runner) and feeds its real output through the verdict parser end-to-end.
- **#597 — linuxptp (`ptp4l`/`phc2sys`) closed the gap for a DIFFERENT class of competing daemon:
  one dpkg PACKAGE backing TWO systemd UNITS with different names** (unlike the 5 NTP daemons,
  where package name == unit name, so `dpkg -s "$_ts"` and `systemctl is-active "$_ts"` share one
  loop variable). `dpkg -s ptp4l`/`dpkg -s phc2sys` always read empty even when linuxptp IS
  installed — the fix gathers `dpkg -s linuxptp` ONCE and pairs that ONE status onto BOTH units'
  rows (each unit still gets its own `is-active`/`is-enabled`). **Lesson for the next daemon added
  to this competing set:** check whether its dpkg package name equals its systemd unit name before
  reusing the `for _ts in ...` loop verbatim — if they differ (as with any daemon shipping multiple
  units, e.g. `linuxptp`), it needs the hand-built two-step gather `timesync_gather_remote_snippet()`
  now demonstrates, not a blind loop-variable reuse. The purge side has the same split: purge the
  PACKAGE once, disable+mask each UNIT — see `scripts/setup-device.sh` STEP 17 and
  `scripts/create-usb-linux.sh` for the pattern (order matches the #591 loop: disable → purge →
  mask). **Verification technique for any future gather-snippet extension:** run it over REAL SSH
  against a live cam box, not just the local `bash -c "$snippet"` test harness — #597's post-merge
  check piped the sourced snippet through `sshpass -p "$DEVICE_ROOT_PW" ssh root@10.77.9.61` (see
  Device Deployment below for `DEVICE_ROOT_PW`) to prove it transports correctly as an SSH
  remote-command string (a different code path than a local `bash -c`) and that the verdict on the
  REAL fleet is `ok` (linuxptp genuinely absent).
- **#608 — a gate whose SSH-gather is inlined can be given the SAME offline test-injection seam as
  `clock-offset-painter-gate.sh`'s `DEV1_DANTE_JOURNAL`/`PAINTER_DANTE_JOURNAL`, generalized to
  N named nodes:** extract the inline `sshpass ssh ... journalctl ...` into its own function, and
  at its top check an env var built from the node's own name
  (`DANTESYNC_GATE_LINUX_JOURNAL_<NAME>`, NAME uppercased + `-`→`_` via
  `tr '[:lower:]-' '[:upper:]_'` so a hyphenated name like `imag-nb` still yields a valid shell
  variable name) via bash indirect expansion `${!var:-}` (safe under `set -u` — the `:-` default
  protects the *target* variable being unset, not just `var` itself). This is the pattern for any
  future gate that measures MULTIPLE named Linux nodes and needs offline fixture coverage
  (`scripts/dantesync-gate.sh`'s `read_linux_node_journal()`).

**Windows (strih + stream) — W32Time invariant, GATED (#598).** DanteSync is the clock authority
on the Windows OBS boxes too; the built-in Windows Time service (`W32Time`) must be **Stopped +
Disabled** (already fixed live fleet-wide 2026-07-07). `scripts/w32time-gate.sh` +
`scripts/lib/w32time-authority.sh` now assert this automatically: FAIL if W32Time is RUNNING as an
active NTP/NT5DS/AllSync client syncing to a real external Source, OR (mirroring #591's "masking
is not enough" philosophy) if it is merely stopped right now but START_TYPE is AUTO_START with an
NTP/NT5DS/AllSync Type (a latent 2nd authority that would resurrect on the next reboot). OK if
disabled/stopped (regardless of any leftover Type value) or Type=NoSync. UNKNOWN — never a silent
"ok" — on anything unreadable/unrecognized: an unknown STATE/START_TYPE, or a Type value that is
neither one of the four real registry values (NTP/NT5DS/NoSync/AllSync) nor readable at all.
`w32time_gather_remote_snippet()` is the exact READ-ONLY command block to run per box via the
`win-*` MCP `Shell`: `cmd /c "sc query w32time" | Out-String -Width 300` +
`cmd /c "sc qc w32time" | Out-String -Width 300` (both routed through `cmd /c` because the bare
PowerShell-native `sc.exe` invocation returned EMPTY output over the win-* MCP Shell in a live
2026-07-08 probe of both boxes) + a PLAIN (no `cmd /c` wrapper needed)
`reg query HKLM\SYSTEM\CurrentControlSet\Services\W32Time\Parameters /v Type | Out-String -Width 300`
+ `w32tm /query /status`. Write each box's combined output to a file and pass `--win-status
NAME=FILE` (the shared `scripts/lib/win-status-args.sh` NAME=FILE parser — `scripts/
dantesync-gate.sh` used the identical convention for its own Windows nodes until #835 removed it
there in favor of a live `--win-http` fetch; this gate's inputs are `sc`/`reg`/`w32tm`
service-control TEXT the win-* MCP is the natural fit for anyway, with no HTTP equivalent to
migrate to, so it stays offline-fixture-file-only with no live-SSH branch). A box with no
status file is UNKNOWN, never a silent pass. Live 2026-07-08 readings (both already fixed, both
PASS): strih `STATE=STOPPED START_TYPE=DISABLED Type=NoSync`, stream `STATE=STOPPED
START_TYPE=DISABLED Type=NTP` (a harmless leftover config value — DISABLED means it can never run
it).

**win-* MCP console-tool output can carry a trailing `\r` (#598).** Any Windows console tool
(`sc.exe`, `reg.exe`, `w32tm.exe`, …) captured over the win-* MCP `Shell` may embed a CRLF line
ending that survives into the captured text even after `| Out-String`. If that value is later
`printf`'d into a diagnostic/log line, the bare `\r` snaps the terminal cursor back to column 0 and
silently overwrites the rest of that line for a human reading it. Any parser that extracts a VALUE
(not just a whole-line match) from win-* MCP console output should `tr -d '\r'` it before using the
value anywhere it might get printed — see `w32time_source_from_text()` in
`scripts/lib/w32time-authority.sh` for the pattern.

**Multi-line Rust string fixtures built with `\`-newline continuation silently strip the next
line's leading whitespace (#598).** `"foo\<newline>    bar"` in Rust yields `"foobar"`, NOT
`"foo    bar"` — the compiler drops the newline AND every leading whitespace character on the
continuation line, regardless of the SOURCE file's own indentation. This bit `.replace()` calls in
`tests/w32time_gate.rs` written against what the *source* looked like indented (`"    Type
REG_SZ    NTP\n"`), when the *actual* string value has no leading spaces at all. When editing a
`\`-continued multi-line fixture in this repo's test files (the same pattern `dantesync_gate.rs`
and `clock_offset_painter_gate.rs` also use), verify the ACTUAL bytes with a quick Python/grep on
the raw source rather than trusting the visual indentation.

**`timedatectl` LIES here** — reports "System clock synchronized: no / NTP inactive"
because DanteSync disciplines the clock DIRECTLY (not via the kernel NTP path timedatectl
watches). This trap has recurred TWICE (2026-06-15, 2026-06-17). Trust DanteSync's own
log/IPC, never `timedatectl` or ad-hoc NTP probes. SSH-RTT offset probes to cam boxes
are also worthless (cam2 SSH RTT 0.4-0.8s while painting → ±0.4s error swamps the real offset).

**Status query:**
- Windows: named pipe `\\.\pipe\dantesync` (length-prefixed JSON; PowerShell NamedPipeClientStream)
- Linux: `journalctl -u dantesync` + `/var/log/dantesync/dantesync.log`
  - Lines: `[PTP] LOCK Drift … µs/s`, `[NTP] offset:…us`

**Gotchas (will bite again):**
- **BOM kills the config:** PowerShell 5 `Set-Content -Encoding UTF8` writes a BOM;
  serde_json then fails → DanteSync silently falls back to defaults (`10.77.8.2` +
  ntp_server_mode DISABLED — kills the cluster's time source). Always write with:
  `[System.IO.File]::WriteAllText($p, $s, (New-Object System.Text.UTF8Encoding $false))`
- **Hostname → IPv6:** `time.cloudflare.com` resolves AAAA-first; DanteSync's resolver
  picks IPv6 (no route) → timeout while `w32tm` (IPv4) worked. Use IPv4 literals in
  `ntp_server` (e.g. `162.159.200.1` for Cloudflare).

Fixed 2026-06-12: strih's upstream was dead `10.77.8.2` (old network); set to `162.159.200.1`.

**A single NTP/PTP gate failure on ONE node during preflight is often a genuinely TRANSIENT blip —
recheck before assuming the rig is broken (2026-07-10).** `dantesync-gate.sh` failed once with
`cam2 NTP DRIFT (fresh offset exceeds 2000us bound)` mid-preflight; the box's own journal showed an
active `[NTP] Stepped ...us` correction in progress at that exact moment (a normal sawtooth
re-lock, not a real problem) — PTP was already back to LOCK. A ~20s wait + re-check showed a clean
GATE PASS. Don't loosen the bound or bypass the gate on a single failure; do re-run it once after a
short wait and read the box's OWN freshest journal line (`journalctl -u dantesync -n 10`) to confirm
it's mid-correction rather than genuinely stuck DEGRADED.

## The strih/stream `:8899` `BundleStateServer` scheduled task has NO restart-on-failure by default (found 2026-07-10)

`version-integrity-gate` (drift-guard's live stack-state check) reads each Windows box's
`http://<box>:8899/bundle-state.json`, served by the `BundleStateServer` Scheduled Task (`ONSTART`
trigger, `run-bundle-state-server.ps1` — see `scripts/bundle-state-server.py`/
`bundle_state_gather.py`, #650). This task's default `Settings` ship with **`RestartCount=0`** — if
the underlying `python.exe` process ever crashes/is killed, the task just sits `Ready` (not
`Running`) FOREVER until the next reboot, and every subsequent `recording-e2e.sh` preflight reports
that box `UNKNOWN` (`version-integrity-gate` refuses to proceed on an unread box, by design — never
silently treats a missing state file as "fine").

**Diagnose:** `Get-ScheduledTaskInfo -TaskName "BundleStateServer"` — a `LastTaskResult` of
`3221225786` (`0xC000013A`, STATUS_CONTROL_C_EXIT) with the task `State=Ready` (not `Running`) means
it died and never restarted. Confirm with `Get-Process python | ... ; (unreachable :8899)`.

**Fix (live, one-time per box, then hardened so it self-heals going forward):**
```powershell
Start-ScheduledTask -TaskName "BundleStateServer"   # bring it back up NOW
$newSettings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
  -ExecutionTimeLimit (New-TimeSpan -Hours 0) -StartWhenAvailable -DontStopOnIdleEnd
Set-ScheduledTask -TaskName "BundleStateServer" -Settings $newSettings
```
Applied to BOTH strih and stream 2026-07-10 — the task now retries up to 999 times at 1-minute
intervals with no execution time limit, so a future crash self-heals instead of silently stranding
the gate until someone notices. There is no repo script that CREATES this task (it predates any
committed setup script, per #650's own history) — if a future re-provision recreates it from
scratch, re-apply this `Set-ScheduledTask` hardening (or fold it into whatever script eventually
DOES create the task).

**RECURRED 2026-07-12/13 (#732) — `RestartCount=999` does NOT cover every death mode.** Found
strih's `BundleStateServer` dead AGAIN, 3 days after the above hardening, with a DIFFERENT
`LastTaskResult`: `1073807364` = `0x40010004` (`SCHED_S_TASK_TERMINATED`) — an
`SCHED_S_*`-class (informational/SUCCESS) code, NOT a `SCHED_E_*`/`0xC0*` FAILURE code. Windows
Task Scheduler's restart-on-failure logic only re-launches after a genuine error exit; a
"terminated" result (an explicit stop, a session/parent teardown, etc.) is NOT treated as a
failure, so `RestartCount` never engages for THIS class of death — structurally different from the
07-10 crash (`0xC000013A`) the hardening targeted. `Get-ScheduledTask` still showed the settings
intact (`RestartCount=999` etc.) — the hardening didn't regress, it's just insufficient by design
for a non-failure termination. Same fix (`Start-ScheduledTask -TaskName "BundleStateServer"`,
confirm via `bundle-state-server.log`'s tail — NOT via `Invoke-WebRequest` over the win-strih MCP
Shell, which hung/timed out both times despite the server itself logging a prompt `200` response
to the SAME request within the same second; trust `curl http://<box>:8899/bundle-state.json` run
FROM dev1 instead, or the server's own log, over an MCP-Shell-side `Invoke-WebRequest`).
**#732 proposes the real fix**: an ACTIVE health-check watchdog (mirroring `#391`
`obs-liveness-watchdog`) that periodically probes `:8899` and restarts the task regardless of its
last exit code — a passive Task-Scheduler restart-on-failure setting can never catch a
"terminated but scheduler considers that fine" death, no matter how high `RestartCount` is set.

**#732 IMPLEMENTED (dev1-side watchdog; pending supervisor install)**: `scripts/bundle-state-alert-watchdog.sh`
+ `scripts/lib/bundle-state-health.sh` + `systemd/bundle-state-alert-watchdog.{service,timer}` — a
dev1 systemd `--user` timer (5-min cadence) that `curl`s each box's `:8899/bundle-state.json` FROM
dev1 (200 + JSON body), and on a box that is UP (ping OR OBS-WS `:4455`) but whose `:8899` does NOT
serve, after a 2-pass confirm: auto-restarts the task via `schtasks /run /tn BundleStateServer` over
ssh (session-agnostic — starts the HIDDEN headless task, never `/it`) AND fires a throttled Discord
alert. A fully-unreachable box is deferred to the `network-reach-alert-watchdog` (#1001) — no
double-page. It ships DISABLED; the SUPERVISOR installs + live-verifies + enables it per
`systemd/bundle-state-alert-watchdog.README.md`. This replaces the by-hand `Start-ScheduledTask`
recovery above for the "task terminated / never cold-started" class; the by-hand command still works
as the manual fallback. Not covered by the watchdog: recreating the task on re-provision, and the
stale-deployed-code drift class below — both remain separate follow-ups.

**A THIRD failure mode (found 2026-07-14, #756): the task can be UP and healthy, but running a
STALE deployed copy of `bundle-state-server.py`/`bundle_state_gather.py` that predates a recent
schema change to the payload — the running code is simply NOT what's currently committed in the
repo.** `curl http://<box>:8899/bundle-state.json` on both strih and stream came back with the
whole `genlock_build_sha` key MISSING even though `bundle_state_gather.py`'s committed source
(unmodified in the working tree) already had `genlock_build_sha_from_file(...)` wired in — the
`.py` files under `C:\ProgramData\camera-box\` were dated days before that change landed. This is
NOT a scheduled-task-health problem (the task was `Running`, `LastTaskResult=0`) — it's a plain
**deploy drift**: nothing automatically pushes a new commit's `scripts/bundle-state-server.py` /
`scripts/bundle_state_gather.py` to the boxes; a code change to these files needs an EXPLICIT
redeploy (`FileWrite` the current committed content to `C:\ProgramData\camera-box\`) + a restart
of the running `python.exe` (killing the stale PID is enough — the `run-bundle-state-server.ps1`
supervisor loop relaunches it within ~5s, picking up the new file) before its payload reflects the
new schema. **Whenever you add/change a field these two files gather, always confirm live** (`curl
.../bundle-state.json` on BOTH strih and stream, not just one) that the NEW field is actually
present in the served payload — don't assume "the repo has it" means "the boxes serve it".

## Device Deployment

Camera devices (CAM1-4) run x86_64 Ubuntu. Build in CI (never locally); download artifact, deploy over SSH.

```bash
# 1. Download the CI-built binary for the commit to deploy
gh run download --repo zbynekdrlik/camera-box -n camera-box-linux-amd64 --dir ./dist
chmod +x ./dist/camera-box

# DEVICE_ROOT_PW: the box root password (NOT committed — export it from your password store before deploying)
# 2. Deploy to device (device root pw — NOT committed)
sshpass -p "$DEVICE_ROOT_PW" ssh root@10.77.9.6X "mount -o remount,rw / && systemctl stop camera-box"
sshpass -p "$DEVICE_ROOT_PW" scp ./dist/camera-box root@10.77.9.6X:/usr/local/bin/
sshpass -p "$DEVICE_ROOT_PW" ssh root@10.77.9.6X "systemctl start camera-box && mount -o remount,ro / 2>/dev/null; true"
```

Use IP addresses — `.lan` DNS may not resolve.

**Main CI has NO fleet-deploy job (verified on the live job list, 2026-08-05)** — a merged
dev→main PR builds + uploads the artifacts and stops there. Nothing pushes the new binary to the
cam boxes. So "merged + main green" ≠ deployed: after every merge whose diff changes the
`camera-box` binary's runtime behavior, the SUPERVISOR runs the recipe above against every
`CAMERA_ACTIVE_SET` member (read the set from `scripts/camera-set.sh`, never a hardcoded list) and
proves the new version live (`/usr/local/bin/camera-box --version` + `systemctl is-active` on each
box, and the box's journal showing the new behavior). A box still logging the OLD message text
after a merge is the tell that this step was skipped.

**The dev1 checkout is PERMANENTLY dirty (`M targets.md` by design — local-only IP file), so the
`pre-deploy-clean-tree.sh` hook blocks every scp/rsync from this repo.** For a deploy of the
CI-BUILT ARTIFACT (bytes from the merged commit, not the working tree) the sanctioned bypass is
annotating the command with `# airuleset:deploy-dirty-ok <why: CI artifact from <merge-sha>>`.
Never use the bypass to ship a working-tree file — only artifact/committed-ref bytes.

**#362 — a FRESH USB clone needs the NDI/audio RUNTIME deps baked in (re-image / #301 checklist).**
A fresh CAM3 clone booted but camera-box crash-looped because libndi could not `dlopen`. If a fresh
box crash-loops camera-box, check these four (the builders now bake all of them — verify if a clone
predates the fix):
1. `libasound2t64` (ALSA) — camera-box exits on missing `libasound.so.2` (intercom).
2. `/etc/ld.so.conf.d/ndi.conf` = `/usr/lib/ndi` (+ `ldconfig`) — without it `dlopen("libndi.so")`
   fails (`cannot open shared object file`) even though the lib is present at `/usr/lib/ndi/`.
3. `libavahi-client3` + `libavahi-common3` — libndi links them; NDI load fails without them.
4. `avahi-daemon` (installed + `systemctl enable`d + running) — libndi browses mDNS via it for NDI
   source discovery; no daemon → `find()` returns nothing → black `--display` receiver even though
   the source is reachable at TCP level. `avahi-utils` (`avahi-browse _ndi._tcp`) diagnoses it.
   avahi-daemon is mDNS only — NO conflict with DanteSync's clock ownership (cam4 runs both; do NOT
   touch chrony/timesyncd/timedatectl).
Baked into `scripts/create-usb-linux.sh` (base image) + `setup.sh` + `setup-device.sh`; pinned by
content-assertion tests in `tests/appliance_boot_hardening.rs`.

## #541 (fixed, was #531) — dev1 has a real SSH key on imag-nb; `--check-imag` runs headless

`scripts/drift-guard.sh --check-imag` SSHes from **dev1** straight to imag-nb
(`newlevel@10.77.9.182`) with `-o BatchMode=yes` (deliberately refuses to prompt for a password —
fail-fast, matching the doc comment on `gather_and_check_imag`'s `ssh_cmd`). Until #541, dev1's keys
were NOT in imag-nb's `authorized_keys`, so `--check-imag` run from dev1 returned all-UNKNOWN — not
because the check logic was wrong, but because the SSH never authenticated (documented workaround
was reading imag-nb's live state via the `mcp__linux-imag-nb__Shell` MCP tool instead, a different
transport).

**Fixed 2026-07-06 (#541):** dev1's `~/.ssh/id_ed25519.pub` (comment relabeled
`dev1-driftguard-control-541` on the authorized_keys line — same cryptographic key, cosmetic label
only) is now in imag-nb's `~/.ssh/authorized_keys` for `newlevel`. `scripts/setup-imag.sh` step 19
installs it idempotently on every (re-)provision, so a rebuilt box never regresses this. Verified
live:

```
$ ssh -o BatchMode=yes -o ConnectTimeout=5 newlevel@10.77.9.182 hostname
imag-pc
$ scripts/drift-guard.sh --check-imag
== drift-guard --check-imag  host=10.77.9.182  (SSH-gathered + git-compared; FAILS loudly on drift) ==
  genlock_build          DRIFT    (imag-nb genlock STALE: box=80dac432... is N genlock-commit(s) behind origin/main ...)
  distroav_so_path       OK       (/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so)
  dantesync_locked       OK       (locked)
```

`--check-imag` now runs headless from dev1 (no MCP workaround needed) and correctly reports genlock
build drift as an advisory — that DRIFT is expected until someone runs the #460 hot-swap
(`setup-imag.sh` step 12) again at a safe off-event time; it is NOT a #541 regression. The
`mcp__linux-imag-nb__Shell` fallback above still works and remains useful for anything outside
drift-guard's own facets, but is no longer required for `--check-imag` itself.

**Reusable pattern for installing a NEW SSH key on a box whose sudo needs an interactive password**
(applies to imag-nb, and any future non-cam-fleet Linux box with the same setup): writing to
`~/.ssh/authorized_keys` needs only the UNPRIVILEGED user's own home directory, not root, so the
`linux-imag-nb__Shell` MCP tool (which already runs as that user, no sudo) can create `~/.ssh`
(700) + `authorized_keys` (600) and append the key directly — no interactive sudo password needed
at all for THIS specific write. Only privileged changes elsewhere on the box (package installs,
`/etc/*` edits) need the actual sudo password. Don't assume "sudo needs a password" blocks every
kind of MCP-driven change — check whether the specific write is actually privileged first.

## #132/#445/#452 — `scripts/upgrade-fleet-ndi.sh` canary set + version-scoped backup

Safe, canary-first NDI Linux runtime (`libndi.so.6`) upgrade across the fleet. The fleet is NOT
uniform in HOW it hosts that runtime, discovered incrementally:

- **#445 — cam3 is a distinct "class"**: real-file `libndi.so.6`/`libndi.so` (not symlinks), no
  `strings` binary, and an older "Streaming: X.Y fps" log shape instead of the genlock-report
  line. `ndi_link_kind_remote` detects symlink-vs-regular live; `ndi_read_banner_local` /
  `ndi_active_version_remote` fall back to `grep -a` when `strings` is missing.
- **#452 — a green canary on ONE box proves nothing about a DIFFERENT class.** cam1 (symlink
  layout) passing tells you nothing about cam3 (real-file, no `strings`) — exactly the #132
  history where cam3 needed a manual upgrade after the tool "succeeded". Fix: `ndi_camera_class`
  is a STATIC, hardcoded table (cam3 -> `"cam3class"`, everything else -> `"standard"`) — add any
  future box with cam3's quirks there. `resolve_canary_set(SET, OVERRIDE)` defaults the canary to
  ONE representative per DISTINCT class present in `SET` (first member of each newly-seen class,
  in SET order) — a single-class SET still gets exactly one canary (unchanged #132 behavior); a
  mixed SET like `cam1 cam2 cam3 cam4` defaults to canary set `"cam1 cam3"`. `--canary` now takes
  a space-separated LIST (same shape as `--set`); an override with any non-member is rejected
  whole (never silently drops just the bad one). The main flow loops `for cam in $CANARY_SET`
  BEFORE `for cam in $REST` — every canary is tried (surfacing every class's result in one run)
  and the whole fleet is skipped if ANY canary failed.
- **#452 — version-scoped real-file backup.** The regular/real-file swap branch used to `cp -a
  libndi.so.6 libndi.so.6.bak` with a FIXED name — every upgrade overwrote the same `.bak`, one
  generation of rollback only. Now `ndi_swap_remote` takes a 4th arg `OLD_VERSION` (the caller's
  already-read `cur_ver`, from the SAME `ndi_active_version_remote` call `upgrade_one_camera`
  makes before swapping — never re-derived) and names the backup
  `libndi.so.6.<OLD_VERSION>.bak`, giving the real-file layout the same multi-generation depth the
  symlink layout already has for free (each symlink backup keeps its original versioned
  filename). `ndi_rollback_remote` needed NO code change — it already restores from whatever
  `OLD_BASE` string the caller passes.
- All of the above is pure-function unit-tested (`tests/upgrade_fleet_ndi.rs`, sourced-script
  harness, no real ssh) — never verified by running the script against real cameras (it swaps a
  live NDI runtime under a running service).

## Realtime CPU + IRQ isolation of the grab (#289)

The cam-box grab/emit must run ALONE on the `isolcpus`-reserved core or it wobbles under box load
(USB kworkers, ssh, the QR painter on .62) → fps dips + underruns + head_skew. The logic lives in
`src/affinity.rs` (pure, unit-tested) + its call sites:
- **Capture+emit** thread → the isolated core; **painter / `--display` / intercom** → OFF it (cores 0-2).
- The isolated core is DERIVED from `/sys/devices/system/cpu/isolated` (highest online isolated;
  fallback last online; `CAMERA_BOX_CAPTURE_CORE` override) — **never hardcode a core number**.
- USB capture IRQ routed onto the isolated core via `/proc/irq/<n>/smp_affinity`, discovered from
  `/proc/interrupts` — run by `camera-box --setup-irq-affinity` from the unit's `ExecStartPre`.
- `systemd/camera-box.service` carries `CPUAffinity=3` (soft belt-and-braces; the binary refines).
- **Provisioned by `scripts/setup-device.sh`** (#450): STEP 10 adds `isolcpus=3` to `GRUB_CMDLINE_LINUX`
  (idempotent, INSIDE the #295 initrd-guaranteed grub block — that block is the ONE safe place to touch
  grub, so this does NOT contradict the "never ad-hoc edit grub" rule below); STEP 7 writes the
  `cpu-affinity.conf` (`CPUAffinity=3`) + `genlock.conf` (`CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}`,
  per-cam table, #451) drop-ins. The capture core is AUTO-DERIVED from `isolcpus` — the live fleet sets
  NO `CAMERA_BOX_CAPTURE_CORE` (only `CAMERA_BOX_GENLOCK_FPS=60`, confirmed on cam1). Guard:
  `tests/provisioning_realtime_isolation.rs`.
- **`setup-device.sh` invocation is now NAME-RESOLVED, single-arg (#450 rework, 2026-07-05):**
  `sudo ./setup-device.sh CAM5` (case-insensitive) alone brings a booted box to full fleet parity —
  the OLD 3-positional-arg form (`setup-device.sh CAM5 10.77.9.65 cam5`) is GONE. It sources
  `scripts/camera-set.sh` and resolves `DEVICE_NAME -> DEVICE_IP / VBAN_STREAM / CAMERA_GENLOCK_FPS`
  via a pure `resolve_device_name()` function (unit-tested against the real fleet map in
  `tests/setup_device_pure_functions.rs`, sourced the same way as `setup-imag.sh`'s pure functions —
  its `BASH_SOURCE` guard skips the destructive provisioning flow when sourced). The script is now
  `set -euo pipefail` + fails loud (via a `fail()` helper) on binary/NDI/ALSA/dantesync install
  failure instead of warn-and-continue, and STEP 19 hard-fails (never prints "Setup Complete!") if
  the binary or `/usr/lib/ndi/libndi.so.6` is still missing. Guard:
  `tests/setup_device_provisioner_hardening.rs`.
- Verify on a box: `taskset -acp $(pidof camera-box)` (capture threads on the isolated core),
  `journalctl -u camera-box | grep '#289'` (pin + IRQ log lines), `grep . /proc/irq/*/smp_affinity`.
- **Kernel-cmdline quiet-core tuning SHIPPED in code (#303, PR closing #303):** `setup-device.sh`
  STEP 10 now also appends `nohz_full=3 rcu_nocbs=3 irqaffinity=0-2` (alongside #289's `isolcpus=3`)
  through the SAME idempotent per-flag append, INSIDE the #295 safe-grub path (initrd-guaranteed
  before `update-grub`, abort-if-no-kernel guard) — never a standalone grub edit. Guard:
  `tests/provisioning_realtime_isolation.rs`. **Applying it to a LIVE box is still a user-timed step**
  (a fresh `setup-device.sh` run or a deliberate reprovision + reboot) — no fleet box has been
  rebooted onto the new cmdline yet; verify with `cat /proc/cmdline` post-reboot and re-run
  cyclictest (like #484's jitter-before/after) to confirm the isolated core actually quiets down.
  **NEVER edit `/etc/default/grub` + `update-grub` by hand** (bricked 2 boxes, #295/#301) — always go
  through `setup-device.sh`'s safe-grub path.

## imag-nb kernel/CPU hardening (#482/#483/#487) — preempt=full + P-core isolation

imag-nb (10.77.9.182) runs 6× NDI decode + render + genlock + audio in OBS (~106 threads) on a
13th-gen-class notebook (10 P-core HT threads cpu0-11 + 4 E-cores cpu12-15). Fully codified in
`scripts/setup-imag.sh` (steps 6-8); a from-scratch provision reproduces this exactly.

**Gotcha (will recur on any modern-kernel low-latency tuning job): there is often NO lowlatency
kernel IMAGE at the current kernel line.** On imag's 6.17.0-35, the newest `linux-image-*-lowlatency`
builds are 6.8/6.11 — installing one would be a DOWNGRADE (loses 13th-gen CPU/iGPU/USB-NIC
support). Check first: **the generic kernel may already be `PREEMPT_DYNAMIC`** — if so,
`apt-get install linux-lowlatency-hwe-24.04` pulls ONLY the `lowlatency-kernel` CONFIG package
(no image change), which drops `/etc/default/grub.d/99-lowlatency.cfg` =
`GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full rcu_nocbs=all"` — full preemption on the kernel
already running, zero downgrade.

**Grub convention for imag differs from the cam-fleet #295 mechanism**: instead of editing
`/etc/default/grub`'s `GRUB_CMDLINE_LINUX(_DEFAULT)` in place, imag writes standalone
`/etc/default/grub.d/*.cfg` drop-ins (`98-imag-isolation.cfg`, `99-lowlatency.cfg` — sourced in
lexical order by `grub-mkconfig`, each appending to `$GRUB_CMDLINE_LINUX_DEFAULT`). A whole-file
`cat > file <<'EOF'` write is trivially idempotent (no grep-before-append needed) — but it is
STILL routed through the shared `safe_grub_regen()` bash function (defined in setup-imag.sh step
6, #487) before trusting it: initrd-guarantee loop → `update-grub` → validate the generated
default menuentry has both a kernel image and an initrd, `fail()` otherwise. Call it ONCE after
ALL grub.d drops for a batch of changes are written — not once per file.

**CPU layout** (HT pairs confirmed via `thread_siblings_list`, NOT `lscpu`'s flat count):
cpu0-1 = P-core0 (kept for GNOME/Xorg/sshd/MCP), cpu2-11 = the OTHER 5 P-cores (isolated, reserved
for OBS's whole thread pool via `isolcpus=2-11`), cpu12-15 = E-cores (no HT, also housekeeping).
`nohz_full`/`rcu_nocbs` are scoped to ONLY cpu10,11 — the single core pair reserved for the future
#484 SCHED_FIFO genlock render-tick thread — never the whole isolated block (spreading it wider
removes load-balancing signal for everything else on the block, per #303's cam-fleet caveat).

**OBS must be `taskset -c 2-11`-pinned on EVERY launch path** or `isolcpus` starves it onto the
tiny cpu0,1,12-15 remainder. Two launch paths exist in setup-imag.sh: the autostart
`~/.config/autostart/obs.desktop` (`sed`-patch `Exec=obs` → `Exec=taskset -c 2-11 obs` — the file
is copied FRESH from `/usr/share/applications/...` every run so the sed always finds its target,
which makes this idempotent for free) and the script's own provisioning-time launcher. The
Desktop *icon* (double-click) is deliberately left unpinned on the live box — don't "fix" that.
`nice -n -5` was tried and dropped: the desktop user lacks `CAP_SYS_NICE`.

**Live status (2026-07-04): only the grub/isolcpus/preempt changes were hand-applied on the box —
the #487 boot-safety net (apt-mark hold, the postinst initrd hook, the Unattended-Upgrade
kernel-blacklist) was NEVER live-applied, only codified.** `apt-mark showhold` on imag currently
shows only `obs-studio`. Don't assume "codified in setup-imag.sh" means "already protecting the
live box" — check `apt-mark showhold` / `systemctl is-enabled unattended-upgrades` if it matters.

**The same gap runs the OTHER direction on a REVERT (#536): fixing setup-imag.sh does NOT undo an
effect the old script already baked into a live box's generated file.** `setup-imag.sh` writes
`~/.config/openbox/autostart` and seeds `user.ini` from a template at provision/boot time — once
written, those files are just static files; removing the writer from the script does nothing to a
copy that already has the old line in it. #525 force-set `DocksLocked=false` at provision AND every
boot; #536 reverted the *script*, but imag-nb's `~/.config/openbox/autostart` still had the every-
boot `sed -i "s/^DocksLocked=.*/DocksLocked=false/"` line baked in from a prior boot and needed its
own manual removal (`grep -n DocksLocked ~/.config/openbox/autostart` → delete that exact block).
This is NOT the "hand-patch = drift" anti-pattern above — it's the mandatory second half of any
revert that touches a generated/materialized file: fix the generator AND sync the artifact it
already produced on every already-provisioned box. Leave anything the operator controls (e.g. the
`user.ini` `DocksLocked` value itself) exactly as they set it — only remove the FORCE, never repin
the value.

## Rig Recovery Policy

The camera-box rig (cam1-4, strih.lan, stream.lan) is a **dev rig, not production**.
Fix infra breakage autonomously — including host reboot — without asking.

When the stream box GPU wedges (`DXGI_ERROR_DEVICE_REMOVED` / TDR on the RTX 4060),
an OBS restart alone often does NOT clear it; a full reboot does.
User directive: "Nie sme v produkcii — mas to opravit ci uz tak alebo onak kludne aj
restartom pc a pokracovat vo vyvoji!!!"

Do NOT gate, classify, or ask before recovering dev rig infra.
The user interrupts Claude when the rig is needed live.

## #265 — NDI-receive STUCK state (detect by NDI fps, NOT by dantesync CPU)

THE STUCK STATE (intermittent, after long uptime): the cam→OBS NDI receive on a broadcast box
(strih/stream) collapses to ~10 fps (genlock STARVED, underruns CLIMBING). When this genuinely
happens, a full PC reboot clears it (2026-06-26: cam1→strih ~10→30.2 fps, underruns 290k→0 after
reboot). Restarting only the dantesync service does NOT fix the NDI collapse.

**DO NOT use `dantesync.exe` CPU as the stuck signal on strih — it is a RED HERRING (corrected
2026-06-29).** strih is the cluster NTP MASTER (`ntp_server_mode.enabled=true` in
`C:\ProgramData\DanteSync\config.json`); its NTP-serving thread busy-loops ~99% of ONE core
CONTINUOUSLY since process start — present at every boot, healthy or not. The stream box (an NTP
client) does NOT do this. So a 99%-core dantesync on strih is NORMAL and a reboot does NOT fix it
(the busy-loop resumes identically next boot). The 2026-06-29 incident: a black #312 program +
dantesync-core-peg were mis-read as "#265 stuck"; forensics showed NDI receive was actually HEALTHY
(all sources 60 fps, underruns CONSTANT not climbing) — the black program was cam1's NDI
mid-renegotiation after its restart (see #312 self-check fix), not a stuck box. The dantesync
busy-loop itself is a fixable bug in `~/devel/dantetimesync` (NTP-server socket set non-blocking AND
read-timeout → `recv_from` spins) — tracked + fixed separately; once deployed strih's dantesync
idles ~0%.

**Stuck-state detection keys SOLELY on the NDI receive trajectory, never on CPU:** read the OBS
`genlock-fifo audit` lines for the cam sources — STUCK = received-fps dropped to ~10 AND underruns
CLIMBING between samples. Healthy = 60 fps received==consumed, underruns CONSTANT. (On the stream
box, an NTP client, dantesync CPU is still a valid secondary signal; on strih it is not.)

A detect+alert watchdog is tracked in **#266** (split out of PR #268; first cut over-/under-
discriminated STUCK vs benign IDLE). It MUST key on the NDI received-fps + underrun trajectory
(2-live-sample on its own monotonic clock), NOT on dantesync CPU. No watchdog script lives in the
tree yet; until #266 lands, a genuine stuck state is caught by hand (genlock-fifo audit fps +
climbing underruns) and recovered by reboot.

## #281 Fix#3 — rig stranded-in-TEST auto-restore watchdog (heartbeat-gated)

Dead workers leave the rig in a TEST state (prod `camera-box` down + a manual `/tmp` probe holding
the device; OBS program on `PHASE2-PROBE`; burns on). Fix#3 added a conservative safety net.

**Heartbeat = "a legit E2E is running, do NOT auto-restore."** `scripts/lib/rig-heartbeat.sh` owns a
well-known file `${XDG_RUNTIME_DIR}/camera-box-rig-active` (fallback `/tmp/camera-box-rig-active`):
- `recording-e2e.sh` `rig_heartbeat_start` (after the cleanup trap is armed) + `rig_heartbeat_stop`
  (FIRST line of `cleanup()`) → fresh for the whole run, cleared on clean exit OR death.
- `rig-mode.sh` TEST `rig_heartbeat_write`, EVENT `rig_heartbeat_clear`. **One-shot** (rig-mode
  exits) → goes STALE after `RIG_HEARTBEAT_STALE_SEC` (default 600s); an idle TEST rig then becomes
  watchdog-actionable (intended — run an active E2E to keep it fresh).

**Pure decision** `scripts/lib/rig-restore-decision.sh::rig_restore_decide` (no I/O, unit-tested in
`tests/harness_rig_restore_watchdog.rs`): fresh heartbeat → never act; else a CLEAR stranded signal
(cam down / stale probe / the **#353 E2E marker** / — fallback — OBS on a known TEST scene
`RIG_KNOWN_TEST_SCENES`) → act, but only after **2 consecutive confirmations**
(`RIG_CONFIRM_THRESHOLD`, the #266 lesson); acting ALWAYS alerts.

**#353 — the E2E MARKER is now the PRIMARY stranded signal (replaced scene-name scraping).**
`scripts/lib/rig-heartbeat.sh` gained `rig_e2e_marker_{path,set,clear,present}`. The marker
(`${XDG_RUNTIME_DIR:-/run}/rig-in-e2e`) is written ONCE on harness entry and removed ONLY on clean
exit — so an UNCLEAN death (SIGKILL / interrupted trap) leaves it behind. This is **distinct from the
heartbeat**, which the background refresher self-removes the instant the harness dies. So:
- **marker present AND heartbeat absent/stale** = a harness entered a test state and never cleaned up
  = rig stranded, REGARDLESS of which scene OBS is on. When set, the decision restores EVERY observed
  OBS box (`RIG_E2E_MARKER=1` env). The watchdog reads it (`rig_e2e_marker_present`) → exports it →
  **clears it after restoring** so a stale marker can't re-trigger/re-alert each pass.
- Wired: `recording-e2e.sh` sets the marker next to `rig_heartbeat_start`, clears it in `cleanup()`.
- Why: scene scraping was fragile (env-overridden / custom scene names slip through; two files to keep
  in sync). The marker needs no scene-list and is robust to any scene name.

**`RIG_KNOWN_TEST_SCENES` is now a FALLBACK only** (marker-less runs: older harness / manual testing).
Default (both `rig-restore-decision.sh` ~line 60 and `rig-restore-watchdog.sh` ~line 80):

```
RIG_KNOWN_TEST_SCENES="PHASE2-PROBE"
```

- `PHASE2-PROBE` — the `obs_phase2.py` phase2 probe scene on strih (still a live test scene).
- **`REC-STRIH-TMP` was DROPPED** (#343/#353): `recording-e2e.sh` now records the stream box's
  already-active prod scene `PRO` (`STREAM_PROG_SCENE` default), so the stream box never lands on an
  ephemeral scene — the marker, not a scene name, detects a stranded stream box now.

**INVARIANT (post-#353)**: prefer the MARKER for new rig-touching harnesses — wrap them so they
`rig_e2e_marker_set` on entry + `rig_e2e_marker_clear` on clean exit (and the heartbeat too). The
scene-list fallback only matters for marker-less callers; if you DO add a scene to it, add it to BOTH
default strings (decision + watchdog — the two-file sync the marker eliminates) and a test.

**GOTCHA — fallback scene names must not contain spaces**: the match loop uses IFS word-split
(`for ks in $known; do`) — a name like `"NDI 2ME PGM"` would split into three tokens and silently
fail to match. All current names are hyphenated. Locked by #352 (code comment + lock test).

**Watchdog** `scripts/rig-restore-watchdog.sh` (runs on dev1 from a `systemd --user` timer,
session-independent): probes cam1/2/4 (`systemctl is-active camera-box` + stale-probe `pgrep`) + the
E2E marker + OBS program scene (`obs_phase2.py program-scene` reader, reuses `_conn`/`_rpc`), persists
the confirm counter across runs in a state file, restores prod (`systemctl restart camera-box` /
`obs_phase2.py teardown`), clears the marker **only after a POSITIVE full OBS restore** (pure
`rig_marker_should_clear`: marker present + acted + no OBS box unreadable + every teardown succeeded —
else KEEP so a later pass retries; clearing while an OBS box was unreadable/failed would mask a
still-stranded box), and ALWAYS `airuleset.py notify`. `--dry-run` = observe+decide+log only (never
clears the marker).

**#370 — distinct alert body + rate-limit for the partial/KEPT case.**
Two new pure functions in `rig-restore-decision.sh` (unit-tested in `harness_rig_restore_watchdog.rs`):
- `rig_classify_restore(act, obs_unreadable, obs_failed)` → `kind=positive|partial`. Positive = full
  restore (0 unreadable, 0 failed → "AUTO-RECOVERED"). Partial = marker KEPT, lower-urgency body.
- `rig_alert_throttle(kind, current_sig, prior_sig, prior_passes, [N])` → `alert_now=0|1 + new_sig +
  new_passes`. Positive: always alert. Partial: alert on first occurrence or after `N` passes
  (default `RIG_ALERT_THROTTLE_PASSES=5`). Sig = `kind:unreadable_names:failed_count`.
  STATE_FILE extended to `confirm=N / alert_sig=... / alert_passes=N` (key=value; backward-compat:
  bare-number legacy files are transparently upgraded on next write). Two-write pattern:
  early write (crash-safe confirm), late write (alert state, only runs when act=1).

**ENABLED + live-verified on dev1 (#350, 2026-07-01).** The timer is active (~2 min cadence). The
conservative re-do of the #266 watchdog (removed for false positives) — heartbeat gate + 2-confirm +
clear-signal-only. Full procedure: `systemd/rig-restore-watchdog.README.md`. Enable recipe used:

```bash
cp systemd/rig-restore-watchdog.{service,timer} ~/.config/systemd/user/
# strih OBS-WS secret is NOT committed — supply it via a LOCAL drop-in (mode 0600, not git):
mkdir -p ~/.config/systemd/user/rig-restore-watchdog.service.d
printf '[Service]\nEnvironment=OBS_WS_PASSWORD=<strih WS pw — local memory, NOT committed>\nEnvironment=CAM_PW=<fleet deploy pw — memory cam-fleet-deploy-credential, NOT committed>\n' \
  > ~/.config/systemd/user/rig-restore-watchdog.service.d/override.conf
systemctl --user daemon-reload
systemctl --user enable --now rig-restore-watchdog.timer
systemctl --user list-timers | grep rig-restore-watchdog   # confirm NEXT is ~2 min out
```

Live-verify (all passed): dry-run on the healthy rig → `no-stranded-signal`; simulate a strand by
`rig_e2e_marker_set` (source `scripts/lib/rig-heartbeat.sh`) with NO heartbeat → pass1 `confirm=1
act=0`, pass2 `confirm=2 act=1` restore both OBS boxes + Discord alert + marker auto-cleared; then
`rig_heartbeat_write` a fresh heartbeat WITH the marker still set → `heartbeat-fresh-legit-e2e act=0`.
Confirm one UNATTENDED timer-driven pass fired (`journalctl --user -u rig-restore-watchdog`). Clear the
test marker/heartbeat + `rm` the state file after.

**GOTCHA — `obs_phase2.py teardown` consults `/tmp/obs_phase2_state.json`; a STALE leftover switches a
healthy box's scene.** teardown restores the `prev_scene`/`prev_preview` that a prior `setup`/`prod-scene`
saved there, and only switches when current≠saved. A leftover state from an OLD session (e.g. the
simulation above ran teardown while a Jun-30 file said stream `prev_scene:"PRO"`) makes teardown move a
healthy box's program to that stale scene. In a REAL strand this is correct (the dead harness saved the
true pre-test scene); when SIMULATING against a healthy rig, `rm -f /tmp/obs_phase2_state.json` first (or
expect the switch and restore after). Prod scenes: strih `Cam 5`; stream `PRO` = live program,
`PRE` = standby (both real scenes — `PRO` is what `recording-e2e.sh` records).

**GOTCHA (observability, #394) — under the systemd unit the per-box observation LOG lines are
intermittently dropped from the journal** (journald racing `log()` in the fast-exiting `$()` subshells
of `obs+="$(probe_obs …)"`). The detect→restore path is UNAFFECTED — the `obs … scene=…` RECORD is
captured in-process, not via journald (proven: dry-run+marker → `restore_obs:strih restore_obs:stream`
even on a pass whose log line is missing). To SEE the reads reliably, run the script directly or with
`bash -x`. Fix tracked in #394 (collect observations without a per-call `$()` subshell).

## #297 — NDI sender re-announce (OBS discovery reliability across reboots)

OBS/DistroAV mDNS NDI discovery is flaky on this LAN: the source dropdown does NOT reliably list
all live senders (observed live: finder returned only {CAM2} while CAM1/3/4 were up + emitting; a
rebooted box appears "gone"). Setting `ndi_source_name` by hand still connects → the source IS
reachable, only DISCOVERY fails.

**Sender-side fix (shipped, code):** the camera-box NDI sender (`NDIlib_send_create`, `src/ndi.rs`)
used to be created ONCE at startup and never re-register. It now **re-announces** (re-creates the
sender → re-runs the mDNS announce on the CURRENT network) when the host's usable **LAN** address
set CHANGES or RECOVERS from an outage. Pure trigger in `src/reannounce.rs`
(`should_reannounce(announced, current, saw_down_since_announce)`, `is_discoverable_interface()`
LAN-NIC denylist, `REANNOUNCE_POLL_INTERVAL=2s`); Linux IO (`getifaddrs`) +
`NdiSender::maybe_reannounce()` in `src/ndi.rs`; called each report tick by the capture loop.
Steady state never re-creates (would drop connected OBS receivers) — only a real change does.

**GOTCHA — re-announce MUST destroy before create (#297 dev.139 infinite loop):** `NDIlib_send_create`
refuses to register a SECOND sender whose name is already live in this process → returns null. So a
same-name re-create while the old handle still exists ALWAYS fails. `reannounce_now` must
**`send_destroy(old)` FIRST, then `send_create`** (guard the null handle on destroy — on a retry the
field is already null, same as `Drop`). The shipped dev.139 created-first → null → `bail!` every 2s
without ever advancing the announced signature → infinite WARN loop, box never rediscovered. Two
invariants: (1) advance the trigger (`announced`) ONLY on a SUCCESSFUL create — `ReannounceState::record_reannounce_attempt(current, created_ok)`; a failed create leaves state unchanged so the next
poll retries. (2) the emit path (`send_frame_data_with_timecode`) must GUARD a null `self.sender` (drop
the frame, don't call `send_send_video_v2(NULL)` = UB) for the brief destroy→create window. Convergence
+ retry are unit-tested purely in `src/reannounce.rs`.

**FFI ORDERING is now LOCKED by a test seam (#317):** the pure `src/reannounce.rs` tests never call the
FFI, so a revert to create-first kept them green while reintroducing the loop. `src/ndi.rs` has a private
`trait NdiSendOps { send_create; send_destroy }` separating the two FFI calls from `NdiLib`'s
`libloading::Library`; `NdiLib` impls it as a thin `#[inline]` pass-through (live path byte-identical),
and `fn reannounce_dance<O: NdiSendOps>(ops, &mut sender, settings, &mut trigger, current)` holds the
destroy-first logic that `reannounce_now` delegates to. `mod ffi_seam_tests` drives the dance with a
`FakeNdi` that records `[Op::Destroy(ptr)|Op::Create]` order AND models the SDK one-live-sender-per-name
rule (create while a same-name handle is live → null). To unit-test ANY other NDI FFI ordering, add the
method to `NdiSendOps` + `FakeNdi`, never construct a real `NdiLib`. Run cheap: `cargo test --lib
ffi_seam_tests` (append `# airuleset:build-ok` — the Tier-0 hook blocks `cargo test` otherwise).

**SCOPE LIMIT (do NOT overstate):** re-announce fixes the **boot-race / late-DHCP / link-flap**
cases. It does **NOT** fix a STABLE box whose mDNS registration was simply lost/missed by OBS
(stable network, no change → no trigger) — and the cam boxes have STATIC IPs, so a clean reboot may
present a stable sig from process start. That persistent-flakiness case is the **central NDI
Discovery Server** (`NDI_DISCOVERY_SERVER` on every cam box + both OBS) or a LAN multicast fix
(avahi reflector / IGMP snooping) — a **fleet-infra decision raised to the user** (pending; file a
follow-up if chosen). When diagnosing "camera missing from OBS dropdown", check WHICH case it is
(boot-race/flap → re-announce should cure within ~2s; stable-lost-announce → needs the discovery
server).

**Rig verify (supervisor):** reboot a box, confirm it reappears in the OBS NDI dropdown within
seconds (and on a link flap). The unit tests prove the trigger logic only — discovery is a rig check.

## #295 — cam-box boot hardening (what bricked CAM3/CAM4 + the durable fix)

**How the boxes ACTUALLY boot:** the live cam boxes run a plain **RW root on `/dev/sda2`**
(ext4 rw,relatime), provisioned MANUALLY — NOT the repo's ro-root+overlay image. `build-image.sh`
designs a read-only root + 512M overlay, but the deployed boxes don't use it (long-term target;
operational re-image tracked in **#301**).

**CORRECTION (#679, live-confirmed 2026-07-11) — the fleet is NOT uniformly rw-root any more.**
Writing directly to `/etc/` on **cam1, cam3, cam5, cam6** now fails `Read-only file system` — some
prior provisioning pass already converted them to ro-root (setup-device.sh STEP 18's fstab
rewrite, applied at SOME point after the paragraph above was written). **cam2 and cam4 are still
genuinely rw-root** (a direct `/etc/...` write succeeds with no remount). The
dantesync-deployment skill's "cam3 is the ONE exception" framing is now WRONG — treat EVERY box
individually: attempt the plain write first, and on `Read-only file system` wrap it in
`mount -o remount,rw /` ... `mount -o remount,ro / 2>/dev/null || true` (the same pattern
`setup-device.sh`'s own `ensure_root_writable`/`restore_root_mode` already uses). Never assume a
box's mode from this doc or from another box's result — check live.

**The brick (NOT fs corruption):** `unattended-upgrades` was active → auto-installed a `6.8.0-124`
kernel; a FULL 100M `/var/cache` tmpfs broke apt with ENOSPC so its **initrd never generated**; a
later `update-grub` (the #289 isolcpus edit) made that initrd-less kernel the default → can't mount
root → unbootable. Recovery was a dev1 chroot: `update-initramfs -c -k <ver>` + `update-grub`.

**Durable fix (PR #306, in provisioning `setup.sh` + `setup-device.sh`):**
- `apt-mark hold linux-image-generic linux-headers-generic linux-generic` — **BEFORE** `apt-get upgrade`.
- unattended upgrades OFF (`/etc/apt/apt.conf.d/20auto-upgrades` periodic=0 + kernel blacklist + masked).
- every kernel gets an initrd BEFORE `update-grub` (loop + `/etc/kernel/postinst.d/zz-camera-box-initrd-guarantee`).
- `GRUB_DEFAULT=saved` + validate the default grub.cfg menuentry has BOTH a kernel image AND an initrd
  (abort loudly otherwise) + `grub-set-default 0`.
- `/var/cache` tmpfs sized **512M** uniformly (was the 100M that ENOSPC'd).

**Live-box rules (unchanged):** NEVER edit `/etc/default/grub` + `update-grub` on a live box directly
— that is what bricked them. `setup-device.sh`'s STEP 10 (the #295 safe-grub path) is the only
sanctioned way to change the kernel cmdline; #303's `nohz_full`/`rcu_nocbs`/`irqaffinity=` quiet-core
flags now ship through it, but applying them to a fleet box still means running `setup-device.sh`
(or reprovisioning) and rebooting — a user-timed step, not yet done on any live box. Safe live
mitigation already on survivors (.61/.62/.64): kernels dpkg-held, `/var/cache` freed.

**#307 SHIPPED (PR #322):** the hardening now also covers the two builders the setup scripts didn't —
`create-usb-linux.sh` (master base-image builder: `apt-mark hold` the kernel + unattended-upgrades off
via `20auto-upgrades` periodic=0 + `GRUB_DEFAULT=saved`/`grub-set-default 0`, was the hardcoded
boot-newest default) and `build-image.sh install_bootloader` (fail-closed guard: validate the generated
grub.cfg default menuentry has BOTH a kernel image AND an initrd before pinning). All four scripts
(`setup.sh`, `setup-device.sh`, `create-usb-linux.sh`, `build-image.sh`) are now content-asserted by
`tests/appliance_boot_hardening.rs` (10 tests). These are BUILD-time scripts — verified by
content-assertion tests in CI, NOT a rig deploy (runtime boot-verify happens at the #301 re-image).

**#344 SHIPPED (PR #345) — USB/image actually BOOTS (the historical "USB never boots" pain):**
`grub-install --removable` on Ubuntu 24.04 embeds a signed-grub memdisk core whose live-media probe
(`search /.disk/info`) FAILS on an installed root → never chains to the real grub.cfg → drops to the
`grub>` rescue prompt → USB never boots. **Fix:** a shared `scripts/lib/install-grub-efi.sh`
`build_grub_efi_core` rebuilds `EFI/BOOT/BOOTX64.EFI` with `grub-mkimage` + a minimal embedded config
(`search.fs_uuid <root> → set prefix=($root)/boot/grub → configfile $prefix/grub.cfg`) — topology-
independent, no live-media probe (819 KB clean core vs 2.3 MB broken Ubuntu core). Wired into BOTH
`create-usb-linux.sh` AND `build-image.sh` (after `grub-set-default 0`) + `GRUB_DISABLE_OS_PROBER=true`.
**Secure Boot must be OFF** on the target (the grub-mkimage core is UNSIGNED — not a regression, the
old --removable core wasn't shim-chained either; cam boxes run SB off). Boot regression test
`scripts/test-usb-grub-boot.sh` (on-demand, root+qemu+OVMF): builds a loopback GPT image, installs the
core, boots it headless over a USB-storage controller, asserts a marker reaches the serial. The cam3
USB stick (clone of cam4 + cam3 identity, static 10.77.9.63) was built + boot-proven this way before
hand-off.

## #369 — auto-grow root partition + fleet disk-space guard (PR #384)

**Root cause:** cam4 shipped 3.5G/92%-full on a 57G disk — the partition was written by the image builder
but NEVER expanded to fill the physical disk. cam1's `/var/cache` tmpfs was 100M/100%-full (pre-#306 era).

**Fix: `systemd/camera-box-grow-root.service` + `scripts/lib/camera-box-grow-root.sh`**

Run-once first-boot service installed + enabled by BOTH builders. Key design decisions:

- **Run-once gate:** `ConditionPathExists=!/var/lib/camera-box/grow-root.done`. Marker written
  unconditionally at end of script regardless of growpart/resize2fs success. The marker ALWAYS lands
  so subsequent boots skip the service entirely and boot is never blocked.
- **Fault-tolerant growpart:** `build-image.sh` creates a 3-partition layout (EFI + root + overlay).
  Root is NOT the last partition, so `growpart` WILL refuse to expand it (non-zero exit). Script handles
  this with `growpart … 2>/dev/null && resize2fs … 2>/dev/null || echo "non-fatal"`. The marker is
  still written. `create-usb-linux.sh` images have root as the last partition — growpart succeeds there.
- **Unit in `[Unit]` section:** `After=local-fs.target` + `Before=multi-user.target` (ordering explicit
  so grow happens before any service that needs space).

**`scripts/lib/` is the canonical shared-helper location for both builders.** Extract any bash helper
that both `create-usb-linux.sh` AND `build-image.sh` need into `scripts/lib/<name>.sh`; then each
builder does `install -m 0755 "$SCRIPT_DIR/lib/<name>.sh" "$MOUNT_ROOT/usr/local/sbin/<name>.sh"`.
Do NOT embed the same script verbatim via heredoc in both builders (the reviewer flagged that as
important and it was refactored out in commit `6ea08a216`). For **service unit files**, always use
`install -m 0644 … "$MOUNT_ROOT/etc/systemd/system/<name>.service"` — NOT bare `cp`; `cp` is
mode-preserving from the source and silently propagates any unusual source mode. Current `scripts/lib/` contents:
`install-grub-efi.sh`, `rig-heartbeat.sh`, `rig-restore-decision.sh`, `camera-box-grow-root.sh`,
`disk-guard-thresholds.sh`.

## #458 — `set -euo pipefail` footguns in one-shot provisioning scripts (setup-*.sh)

Two review passes on `scripts/setup-imag.sh`'s genlock hot-swap step (#460 integrity check)
found the SAME class of bug three separate times, in three different shapes. All three are now
fixed there (and empirically spot-check-verified — revert the fix, confirm the test fails,
restore) but WILL recur in any future `setup-*.sh`/provisioning script unless watched for:

1. **`ls`/`grep`/`curl|grep|grep` piped into `| head -1`, assigned bare — aborts SILENTLY on an
   empty match.** `VAR="$(ls -t DIR/*.txt 2>/dev/null | head -1)"` — if the glob matches nothing,
   `ls` exits non-zero even though `head` succeeds (empty output is not an error for `head`).
   Under `pipefail`, the pipeline's exit status is the RIGHTMOST non-zero command, so the bare
   assignment inherits `ls`'s failure and `set -e` aborts the WHOLE SCRIPT right there — before
   whatever `[ -n "$VAR" ] || fail "..."` check you wrote next ever runs, usually with **zero
   diagnostic** (stderr is typically `2>/dev/null`'d). **Fix: append `|| true` to the whole
   pipeline** (`... | head -1 || true`), then let the following `[ -n "$VAR" ] || fail ...` do its
   job normally. Same fix applies to any `curl | grep | grep | head -1` URL-scraping pipeline.
2. **A `fail()`-calling function invoked via `cmd "$(func)" other-arg` — its abort is silently
   swallowed.** `$(...)` always forks a subshell. A BARE `VAR="$(func)"` assignment correctly
   propagates that subshell's exit status to `set -e` (empirically verified — this DOES abort the
   script). But `cmd "$(func_that_calls_fail)" other-arg` does NOT: the subshell's `exit` only
   kills the command-substitution subshell, and `cmd` still runs with that argument silently
   EMPTY. **Rule: any function that can call `fail()`/`exit` must ALWAYS be captured via a bare
   `VAR="$(func ...)"` assignment on its own line — never embedded as one of several arguments to
   another command.** This is the subtler sibling of the already-documented `| while read; do
   fail; done` pipe-subshell trap (same root cause — a subshell's `exit` doesn't propagate through
   an enclosing command the way `set -e` needs — different shape, easy to reintroduce while fixing
   the first one).
3. **`gh ... -q '.[0].someField'` on an EMPTY JSON array/result returns the literal 4-char text
   `"null"`, not an empty string** (jq semantics: indexing a nonexistent element yields `null`,
   and `-r`/`gh -q` renders it as text). A bare `[ -n "$VAR" ]` guard wrongly treats `"null"` as a
   valid value. **Fix: `-q '.[0].someField // empty'`.**
4. **(#463, `drift-guard.sh gather_and_check_imag`) You need the EXIT CODE of a `$(cmd)` that can
   itself legitimately fail (an `ssh`/`timeout` call whose 255/124 means "unreachable", not just
   pipe-exit-status like point 1) — capturing it on the NEXT line crashes instead.**
   `var="$(ssh ... cmd)"` then `local rc=$?` on the FOLLOWING line looks reasonable, but under
   `set -e` the FAILING ASSIGNMENT ITSELF aborts the whole script the instant `cmd` returns
   nonzero — `rc=$?` never even runs (empirically verified: `bash -c 'set -e; f(){ return 255;
   }; x="$(f)"; echo after'` prints nothing and exits 255). **Fix: put the assignment in an
   OR-list** — `rc=0; var="$(cmd)" || rc=$?` — the one shape `set -e` exempts (only the LAST
   command of an AND/OR list is errexit-checked), so it survives AND captures the real code.
   Different failure shape from point 2 above (that one is about `fail()`/`exit` INSIDE a
   function losing propagation through `$(...)`; this one is about a command that returns a
   plain nonzero you need to READ, not propagate).
5. **(#489, `drift-guard.sh check_imag_report`) A bare `${12}`/`${13}`-style reference to an
   OPTIONAL trailing positional param crashes the whole script under `set -u` when an OLDER
   caller doesn't pass that many args — it does NOT quietly become an empty string.** Adding a
   new arg to a widely-called pure function (`check_imag_report` gained a 12th/13th param for the
   dantesync-lock check) means every EXISTING call site that still passes only 11 args now
   references an unset positional parameter — under `set -uo pipefail` (this file's own top-of-
   file `set`, in effect for the whole test-harness `run_sourced` session too, #463's own
   footgun-comment above) that is a hard `unbound variable` abort, not a graceful empty string
   (verified: `bash -c 'set -u; f(){ local a="$1" c="$3"; }; f one'` → `3: unbound variable`,
   exit 127). **Fix: default-empty expansion on the OPTIONAL trailing params —
   `local exp="${12:-}" obs="${13:-}"`** — mirrors `compare_observed`'s existing
   `o_av_sync_calibrated_ms="${25:-}"` pattern for its own optional trailing args. Whenever you
   extend a pure function's positional-arg contract, grep every existing call site first; if any
   don't pass the new args, the new params MUST use `${N:-}`, never a bare `${N}`.
6. **(#489 review, `tests/drift_guard.rs`) `big="$(yes "$line" | head -n N)"` to manufacture a
   large test blob has its OWN unrelated SIGPIPE hazard — it can crash the WHOLE test harness
   before the function under test is ever called, and is easy to misread as a bug in that
   function.** `head -n N` closes its read end after N lines; `yes` (which never stops producing)
   gets SIGPIPE on its next write; under `set -o pipefail` (this file's own top-of-file `set`,
   inherited by `run_sourced`'s sourced-script session) that non-zero exit aborts the `$(...)`
   assignment itself — verified: `bash -c 'set -euo pipefail; big="$(yes x | head -n 3000)"; echo
   after'` prints nothing, exits 141, `after` never runs. A `_from_log` parser call placed AFTER
   such a line looks like it crashed when in fact the crash happened one line earlier, unrelated
   to anything the parser does. **Fix: build the large blob as a plain string (a Rust `for` loop
   appending to a `String`, or a bash `for`/`printf` loop — never `yes | head`), then either pass
   it directly or (to dodge `ARG_MAX` on a huge value) write it to a temp FILE and `cat` it inside
   the bash body** — this is the file's OWN established, already-proven-correct pattern for
   exactly this scenario: `tests/drift_guard.rs`'s `genlock_parser_reads_running_state_from_log`
   (~line 200, builds via a `for i in 0..5000 { push_str(...) }` loop into a temp file, sourced
   via `cat "$LOGFILE"`) and its #489 sibling `dantesync_locked_from_log_survives_a_large_journal_
   without_sigpipe_489`. Before believing a "confirmed" large-input crash in a `_from_log` parser,
   isolate the blob-construction line ALONE (no function call after it) and confirm it does NOT
   also crash on its own — an incident here (#514, opened then corrected+closed) mistook the
   `yes | head` artifact for a real crash in the already-shipped `genlock_from_log`, when the
   function itself is safe (verified up to 200,000 lines / ~12.5MB via a properly materialized,
   `cat`-read input) and the real underlying bug was a silent WRONG ANSWER, not a crash.

**Test-quality corollary (found 3× in `tests/setup_imag_guards.rs` across both review passes):** a
purely textual `body.contains("some string")` guard can pass even when the real check is gutted,
if that exact substring ALSO appears in unrelated prose (a WARNING fallback echo, a success
banner). Anchor textual guards on the literal FUNCTIONAL invocation (`grep -iE 'the actual
pattern'`), not just "the words appear somewhere" — and for anything with real comparison logic
(not just presence/absence of a string), add a REAL execution test: give the script a
`if [ "${BASH_SOURCE[0]}" != "${0}" ]; then return 0; fi` guard (mirrors
`scripts/launch-obs-genlock.sh`/`scripts/genlock-manifest.sh`/`scripts/drift-guard.sh`), define
pure helper functions BEFORE it, and source it from a Rust test (`tests/genlock_manifest.rs::run_sourced`
is the reference pattern; `tests/setup_imag_pure_functions.rs` is the new sibling for
`setup-imag.sh`). A textual pin proves the code EXISTS; a sourced execution test proves it WORKS —
neither substitutes for the other.

**Fleet disk-space guard (SHIPS DISABLED):** `scripts/cam-disk-guard.sh` + `scripts/lib/disk-guard-thresholds.sh`
+ `systemd/cam-disk-guard.{service,timer}` are committed to the repo but NOT installed/enabled — same
pattern as `systemd/rig-restore-watchdog.{service,timer}` (#281 Fix#3). Enable manually on dev1 when
ready: `systemctl --user enable --now cam-disk-guard.timer`. Checks CAM1/CAM2/CAM4 only (cam3 excluded,
#301). Alert threshold: `CAM_DISK_ALERT_THRESHOLD=80` (env-overridable; pure function in
`scripts/lib/disk-guard-thresholds.sh` makes it Tier-0 unit-testable without SSH).

## #449 — one canonical live-install builder (`scripts/create-usb-linux.sh`)

`scripts/create-ubuntu-vm.sh` (old QEMU-manual install-to-image flow), `scripts/write-image.sh`
(dd a pre-built master image to USB), and `scripts/create-image.sh` (clone a running device's
disk into an image) were deleted (#449, mechanical part) — all three pre-dated the #448 one-shot
`create-usb-linux.sh` installer and were unreachable dead code (confirmed via `git grep`: not
sourced by any script or `scripts/lib/*`, not any script's sole consumer). `SETUP.md` and
`.github/workflows/release.yml` were updated to stop referencing them. Pinned by
`tests/appliance_boot_hardening.rs::dead_image_builders_are_removed` +
`::create_usb_linux_is_the_sole_live_install_builder` — a revert or stray re-add fails CI.

**`scripts/build-image.sh` is DELIBERATELY NOT part of this cleanup.** It is the live, tested
ro-root+overlay builder (`RO_IMAGE_BUILDER`/`IMAGE_BUILDERS[]` in
`tests/appliance_boot_hardening.rs`, `SETUP.md` ro-root section, the "Unified cam-box
provisioning" section above). #449 stays OPEN for a queued user decision on its fate — fold it
into `create-usb-linux.sh` as the sole builder, or keep it as a sanctioned second (RO-ROOT)
builder alongside the live-install one. Do not delete/demote it without that decision.

`.github/workflows/build-image.yml` (a 5th, inline, script-less image-build path) was NOT
retired by #449 — noted as a follow-up.

## #626 — `pkill -f PATTERN` sent over ssh can self-match the invoking shell

`ssh host "pkill -9 -f 'camera-box-burn-' ...; systemctl restart camera-box ..."` self-matches:
`pkill -f` greps every process's full `/proc/*/cmdline`, and the remote `sh -c "..."` process
running that very command literally CONTAINS the pattern text as its own argument. It SIGKILLs
itself mid-script — everything after the pkill in the chain (the `systemctl restart`) never runs,
and the SSH connection just drops (rc=255) with ZERO stdout/stderr — no error to notice. This
caused a real, silent ~3h40m production camera outage (`recording-e2e.sh` cleanup()).

**Fix pattern:** anchor the pkill pattern so it matches ONLY the real target's argv0, never the
invoking command's own text — e.g. a digit immediately after the hyphen
(`camera-box-burn-[0-9]`) when the real target is always named `/tmp/camera-box-burn-<run_id>`.
Before writing any `pkill -f PATTERN` sent as a single ssh command string, check whether PATTERN's
literal text could appear verbatim in the ssh command argument itself (it always can, when the
pattern isn't built from a variable substitution) — if so, anchor it.

**#640 correction (2026-07-09) — the digit-only anchor above was ITSELF incomplete.** cam2/cam3/
cam4/cam5/cam6's ALL_CAMBOX deploy names their burn binary `/tmp/camera-box-burn-<camname>-<run_id>`
(e.g. `camera-box-burn-cam3-1783530925` — a LETTER right after the hyphen, not a digit), which
`camera-box-burn-[0-9]` never matches. Confirmed live: after an interrupted ALL_CAMBOX run, all
five boxes' orphaned burn processes survived `cleanup()` and crash-looped `camera-box` ("Device or
resource busy") for hours. The pattern is now `camera-box-burn-[a-z0-9]` everywhere in
`scripts/recording-e2e.sh` — still doesn't self-match (the invoking command's own literal text has
`[` right after the hyphen, never a letter/digit). **If you ever see this bug again (a cam box
stuck in a systemd restart-loop with "Device or resource busy" on `/dev/video0`), the manual
recovery is:**
```bash
# On the affected box (root over ssh, or the linux-camN MCP Shell):
fuser -v /dev/video0                       # find the PID holding it (a stray camera-box-burn-*)
kill -9 <pid>; sleep 1
rm -f /tmp/camera-box-burn-*
systemctl restart camera-box
systemctl is-active camera-box             # confirm active, then fuser /dev/fb0 shows the NEW pid
```
Check ALL of cam2/cam3/cam4/cam5/cam6 (not just the one you noticed) — an interrupted ALL_CAMBOX
run orphans all of them at once, not just one.

## #722 — the SAME self-match footgun also hits `pgrep -c -f` (read-only, not just `pkill -f`), and two commands double-print their own value on "failure"

The #626/#640 self-match above is about `pkill -f` SELF-KILLING mid-script (silent, catastrophic).
`pgrep -c -f PATTERN` has the identical root cause but a subtler symptom: it doesn't kill
anything, it just returns a WRONG COUNT. Live-caught (2026-07-13, the #722 EVENT-mode CONTRACT's
fleet paint-process sweep): `ssh cam1 "PAINT_COUNT=\$(pgrep -c -f -- --paint-only ...); ..."`
reported `PAINT_COUNT=2` on every cam box, even with zero real painters running — the ENCLOSING
`bash -c "$WHOLE_SCRIPT"` process's own `/proc/PID/cmdline` contains the literal search pattern
(it's part of the script text ssh sends), so `pgrep -f` counts that shell itself as a match.

**When the anchor trick (#626/#640) doesn't apply** — you're searching for a literal FLAG
(`--paint-only`) that could legitimately appear anywhere in a real target's argv, so there's no
digit/letter boundary to anchor against — **obfuscate the pattern instead**: base64-encode it in
the script's SOURCE TEXT, decode into a runtime variable, search on the DECODED value. The literal
substring then never appears in any process's cmdline except a genuine match's own:
```bash
pattern_b64="$(printf '%s' --paint-only | base64 | tr -d '\n')"   # built LOCALLY, dev1-side
# embedded into the remote script as literal source text — the ONLY occurrence of the base64
# blob, never the raw pattern:
cat <<REMOTE
PAINT_PATTERN=\$(printf '%s' '$pattern_b64' | base64 -d)
PAINT_COUNT=\$(pgrep -c -f -- "\$PAINT_PATTERN" 2>/dev/null)
REMOTE
```
Before writing ANY `pgrep -f`/`pkill -f`/`grep -f`-style check whose pattern text will appear
verbatim in the ssh command string itself, ask: can the anchor trick work here (a target-only
argv boundary), or does the pattern need obfuscating (a literal flag/string with no safe anchor)?

**Second, unrelated bug in the SAME check, same incident:** `pgrep -c` prints its own count
(even `"0"` on zero matches) but ALSO exits NON-ZERO when the count is 0 — and `systemctl
is-active` prints its own state word (`inactive`/`failed`/...) while ALSO exiting non-zero for
anything but `active`. A naive `VAR=$(cmd || echo fallback)` then DOUBLE-PRINTS: `cmd`'s own
stdout PLUS the fallback, e.g. `PAINT_COUNT="0\n0"` or `SERVICE_ACTIVE="inactive\nunknown"`. Fix:
trust the command's own printed value; only substitute a fallback when the variable is genuinely
EMPTY: `VAR=$(cmd 2>/dev/null); VAR="${VAR:-fallback}"` — never `$(cmd || echo fallback)` for any
command that prints a value on its "failure" exit path (`pgrep -c`, `systemctl is-active`, and
likely others — check what a command prints on ITS OWN non-zero path before reaching for `||`).

## #625 — a RECORDED-order tick/id walk misreads a benign stream-recording reorder as a fault

The stream recording is documented (`#133`/`#196`/`#216`) to occasionally deliver a frame
"softened"/out of order (an extra NDI hop + HEVC re-encode causes a one-frame-late 60→30
straddle). Any continuity check that walks a monotonically-increasing counter (a node burn ID, or
here the cam2 painted-tick) in RECORDED order and treats a backward step as an unconditional fault
will manufacture THREE phantom gaps from ONE benign swap (`a,a+4,a+2,a+6` instead of
`a,a+2,a+4,a+6`) — for zero actual loss. `probe::burn_contiguity` was already hardened against
this for node bursts; the all-cambox painted-tick window check (`recording_segments.rs::
window_segment`) was not, until #625 (`src/painted_tick_gaps.rs`: sort the DISTINCT present
values before walking — a monotone-at-the-source counter's sorted order recovers the true
delivery-order-independent sequence; sorting can never make a genuinely-missing value appear, so a
real drop is still caught). **Lesson for the next monotone-counter continuity check:** if the
values can arrive out of RECORDED order (any hop through NDI/HEVC re-encode is a candidate), walk
the sorted DISTINCT value set, never the raw delivery-order sequence.

## #555 — the `remoteos-mcp` control-channel agent is a SEPARATE project, not managed by camera-box

The `win-*`/`linux-*` MCP tools this session (and every session working this rig) uses are served
by `remoteos-mcp` — its own repo (`~/devel/remoteos-mcp`, GitHub `zbynekdrlik/remoteos-mcp`), with
its own CLAUDE.md, its own `.claude/skills/install` playbook, and its own versioning
(`0.7.0.devN`). camera-box does NOT re-implement, re-pin, or UPGRADE this agent — upgrades always
use the agent's own installer (below). Since #858 the ONE exception is initial PROVISIONING of a
fresh imag box: `scripts/setup-imag.sh` step 23 INVOKES the canonical `install-linux.sh` (it does
not duplicate or re-pin it) so a freshly hardware'd imag notebook comes up with a working
`linux-imag-nb` MCP surface instead of needing a hand-install.

**How it runs on each box type** (all four rig Windows/Linux boxes, `--enable-all --port 8092`):
- **Windows (strih/stream):** scheduled task `RemoteOSMCP` (`wscript.exe .remoteos-mcp\
  start-remoteos.vbs`, hidden, at-logon + every-5-min repeat) launches a plain pip-installed
  `python -m remoteos`. Upgrade = the ONE canonical one-liner (never ad-hoc pip):
  `irm https://raw.githubusercontent.com/zbynekdrlik/remoteos-mcp/master/install.ps1 | iex`
  (`master` is GitHub's auto-redirect alias for the renamed default branch `main` — both resolve).
  It preserves the existing `auth_key`/config and scheduled task, then — as its LAST step — kills
  the OLD server process and starts the new one. **Running this via the box's OWN win-* MCP Shell
  tool call WILL drop that call** ("transport dropped mid-call") because the script kills the very
  process serving the request — this is expected, not a failure; the upgrade completes
  server-side before the kill. Verify by simply calling `GetSystemInfo`/`Ping` again right after
  (fresh HTTP request, same host:port:auth_key) and `pip show remoteos-mcp`.
- **Linux (imag-nb/cam1-4):** `systemd` unit `remoteos-mcp.service`, package installed globally
  via `pip install --break-system-packages --ignore-installed git+https://github.com/zbynekdrlik/
  remoteos-mcp.git` (its own `install-linux.sh`). Same upgrade discipline: use the installer, never
  a bare pip command. On imag, that installer is now invoked automatically by `setup-imag.sh` step
  23 at provisioning time (#858; `REMOTEOS_MCP_AUTH_KEY` env pre-seeds the key so dev1's `.mcp.json`
  keeps matching a fresh box) — cam1-4 remain hand-installed until their provisioner gains the same.

**Rollback point for a specific old version:** find the exact commit via
`git log --oneline -- pyproject.toml` in the remoteos-mcp checkout + `git show <sha>:pyproject.toml`
— e.g. 0.7.0.dev1 (what strih/stream ran before the #555 convergence to dev8) is commit `66e33c2`.
Reinstall a pinned commit via `git+https://github.com/zbynekdrlik/remoteos-mcp.git@<sha>` (Linux) or
`.../archive/<sha>.zip` (Windows, same as the installer's `master.zip` but a specific ref).

## #663 — capture-delivery-rate SELF-HEAL (automatic USB reset) + physical-fix escalation

#656 shipped DETECT only (`src/capture_rate_health.rs` WARNs once captured fps sustains a >1%
deviation from the negotiated rate for 6 consecutive 5s windows, ~30s) — recovery still needed a
human/agent to notice the WARN, SSH in, and manually run the USB reset. #663's live finding: that
manual fix is only TEMPORARY — cam1's ShadowCast 2 recurred the SAME defect within hours, three
times in one day (2026-07-10). `src/capture_rate_selfheal.rs` closes the loop: the capture loop now
acts automatically the instant #656's WARN fires.

**Mechanism (fully in-process, no external script):** `systemd/camera-box.service` has no `User=`
(runs as root) and already grants `ReadWritePaths=/dev /sys /run /proc/irq` (added for #289's IRQ
affinity `ExecStartPre`) on top of `ProtectSystem=strict` — so the binary can already write
`/sys/bus/usb/drivers/uvcvideo/unbind` + `/sys/bus/usb/devices/*/authorized` directly. On a
confirmed sustained deviation the capture loop: (1) best-effort unbinds the uvcvideo driver from the
capture device's USB interface, (2) writes `authorized` 0 then 1 (forces full re-enumeration —
mirrors the manually-verified #656 fix sequence exactly), (3) **exits the process AFTER the
capture loop's normal shutdown cleanup has run** (burn-ring drain, grab-recorder flush, burn-thread
join, capture-stats sidecar write — the loop stops via the same `running_capture` flag a Ctrl+C
would use, never a raw mid-loop `process::exit`, so an in-flight `--record-grab` E2E recording
isn't truncated) so the unit's `Restart=always`/`RestartSec=3` brings it back up fresh against
whatever device node the kernel just re-enumerated (`Config::device_path()`'s `"auto"` re-detects
it). Exit code **77** = the USB reset itself succeeded; **78** = the reset attempt FAILED (still
exits + still logs CRITICAL, so `systemctl status`/journal visibly reflect a real failure instead
of camera-box quietly limping along with the device possibly left disconnected). No `systemctl
stop`/`start` needed — the process exit + `Restart=always` IS the stop+start.

**Resolving the sysfs `authorized` path for the open device:** `/sys/class/video4linux/<videoN>/
device` symlinks to the USB INTERFACE directory (e.g. live-confirmed on cam1:
`/sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/2-3:1.0`). Sysfs always nests an interface directory
DIRECTLY under its USB device directory regardless of hub depth, so the interface path's immediate
PARENT is always the device directory carrying `authorized` (here: `.../usb2/2-3/authorized`) — no
special-casing needed for a device behind nested hubs. `capture_rate_selfheal::
authorized_path_from_interface_syspath` / `interface_busid_from_syspath` are the pure derivations
(unit-tested against this exact live path).

**Rate-limit + escalation (state MUST survive the restart the fix triggers):** since the fix path
deliberately restarts the whole process, in-memory bookkeeping can't carry "when did we last heal" —
it's persisted to `/run/camera-box/capture-rate-selfheal.state` (tmpfs, already `ReadWritePaths`-
granted; cleared on reboot, which is correct — a fresh boot deserves a fresh attempt count). Simple
`key=value` text, mirrors the bash state-file convention (`rig-restore-decision.sh`) ported to Rust.
- **Max one reset attempt per 10 min** (`DEFAULT_MIN_HEAL_INTERVAL_S=600`) — a genuinely dying
  grabber can't reset-loop forever (the WARN keeps firing every window while the defect persists).
- **3 heals within a 1h recurrence window** (`DEFAULT_CRITICAL_ESCALATION_HEALS=3` /
  `DEFAULT_RECURRENCE_WINDOW_S=3600` — matches #663's own live incident) → a `CRITICAL #663/#685:
  ... persistent BEYOND the <model>'s normal characteristic envelope; investigate.` log line. The
  heal STILL runs even when escalating — this is the honest signal that the software self-heal
  hasn't been able to hold, not a stop condition.
- A gap longer than the recurrence window resets the heal count to 1 (a fresh occurrence, not a
  continuation of a failing streak) — so a box that heals once, stays healthy for days, then
  re-drifts months later is NOT treated as "still failing" from the earlier incident.

**#685 — per-model tolerance recalibration (this escalation no longer means "replace hardware").**
Fleet-wide forensics (2026-07-11) found ALL 3 deployed ShadowCast 2 units (CAM1-3) show the SAME
characteristic capture-rate wobble that used to trigger this escalation (USB output clock
free-running against its own HDMI input — internal resampling — live-observed ~55fps-64.02fps
against a negotiated 60.000fps, up to ~8.3% deviation), while 0/3 other-model grabbers (CAM4 NZXT
Signal HD60, CAM5-6 Elgato 4K S) do — a MODEL trait, not a per-unit defect, and there are no spare
units to swap in. Consequences:
- `capture_rate_health::tolerance_pct_for_model` now gives `GrabberModel::ShadowCast2` a wider
  10% deviation floor (`SHADOWCAST2_CAPTURE_RATE_TOLERANCE_PCT`) instead of the shared 1%
  default, so ShadowCast 2's own characteristic wobble never even reaches the #656 WARN, and
  therefore never reaches this self-heal module at all. CAM4-6 keep the original strict 1% floor
  unchanged.
- `critical_escalation_message` (this section's CRITICAL line) is reworded — it no longer says
  "replace the hardware" / names a cable-port-dongle swap / mentions warranty. **Because a genuine
  trigger has ALREADY cleared the model's own (possibly widened) tolerance, it is by construction
  beyond that model's normal envelope — but this module still cannot itself confirm the root
  cause is hardware**, so the honest instruction is "investigate" (see the technician-session
  ticket for physical discrimination), not a reflexive part swap.
- The `#656 capture-delivery-rate DEFECTIVE` WARN line itself now also names the grabber model and
  the actual tolerance applied (e.g. `..., ShadowCast 2 tolerance) — USB-reset the capture device
  (see #656, #685)`), so the model context is visible even before any escalation.

**GOTCHA — the deployed fleet's `config.toml` does NOT set the optional `hostname` field; resolving
the grabber model from `config.hostname` alone silently degrades to `Unknown` forever.** Confirmed
live on cam1 (2026-07-11): `/etc/camera-box/config.toml` has no `hostname = "..."` line at all, so
`Config`'s `hostname` field falls back to its coded default (`"camera-box"`, a generic string) —
NOT the box's real identity. `capture_rate_health::grabber_model_for_hostname("camera-box")` can
never match `CAM1`-`CAM6`, so it silently resolves `GrabberModel::Unknown` (the strict 1% floor) on
EVERY box in the fleet as deployed, defeating the whole #685 recalibration even after the fixed
binary is running. The FIX (`os_hostname()` in `src/main.rs`, a small `gethostname(2)` wrapper):
prefer the box's real OS-level hostname (correctly set per-box by `scripts/setup-device.sh` —
confirmed live: cam1 → `"CAM1"`) over the config-derived value, falling back to config only if the
syscall itself fails. **Any future feature that needs "which physical box is this" must use
`os_hostname()` (or read `/etc/hostname` directly) — never trust `config.hostname` to reflect the
box's real identity on the live fleet**, unless a future provisioning change starts actually
populating it (grep `config.toml` on a live box first to check, don't assume).

**Verifying it worked on a box:** `journalctl -u camera-box | grep -E "#656|#663"` — look for the
`#656 capture-delivery-rate DEFECTIVE` WARN immediately followed by `#663 self-heal: USB-resetting
capture device ...` / `USB reset complete` / `shutdown cleanup complete — exiting now (code 77)`,
then a fresh `camera-box.service` start (PID changes) with `Streaming: ... fps` settling back near
the negotiated rate (~60.0) within a few seconds. `systemctl status camera-box` shows the recent
restart with exit code 77 if it just self-healed, or **78** if the reset attempt itself failed
(check the CRITICAL journal line right above it — the device may need manual
`echo 1 > <authorized path>` recovery).

**GOTCHA — a plain `journalctl -u camera-box -n N` tail can silently MISS the actual heal-action
lines during live verification.** The capture loop's own intercom (`recv N pkt/s...`) and #299
chroma lines fire every ~5s alongside the fps report, at a high enough rate that a small `-n`
window (e.g. `-n 30`) can scroll straight past the one-shot `USB-resetting`/`USB reset complete`/
`CRITICAL` lines if you fetch it even a few seconds late. Confirmed live (2026-07-10, #663
verification): a `-n 30` tail showed only `#663 self-heal rate-limited: ...s remaining` lines
(the routine per-window log) with the actual reset action nowhere visible, even though it had
JUST happened — the reset lines were already pushed out by ~15+ intervening intercom/chroma
lines. **Fix:** bracket the known event time with `journalctl -u camera-box --since "HH:MM:SS"
--until "HH:MM:SS"` (from the "Xs remaining" countdown you were already watching, which pins the
attempt's timestamp precisely) rather than trusting a small `-n` tail — or grep the FULL
unbounded journal (`journalctl -u camera-box | grep -E "..."`) when hunting a specific one-shot
event rather than the routine per-window noise.

**For a SCRIPTED windowed read (a gate, not a human doing live verification), prefer
`--since=@EPOCH --until=@EPOCH`** (systemd's native absolute-time epoch-seconds form) over
`HH:MM:SS` strings — no timezone ambiguity, no `date +%H:%M:%S` formatting step, and it composes
directly with a `$(date +%s)` snapshot taken at the moment the window opens/closes. #705's
mid-recording capture-rate recheck (`capture_rate_window_journalctl_cmd`,
`scripts/lib/capture-rate-guard.sh`) uses exactly this: snapshot `START_EPOCH`/`END_EPOCH` around
the operation being bracketed, then `journalctl _SYSTEMD_INVOCATION_ID=<id> --since=@$START_EPOCH
--until=@$END_EPOCH` — simpler than this repo's other pattern (`freshest_offset_us` in
`scripts/clock-offset-guard.sh`), which has to parse `-o short-iso` timestamps back OUT of an
already-fetched window because it grades a value gathered for a different purpose; a NEW gate that
controls its own fetch should push the window bound into journalctl itself instead of re-deriving
that parsing.

**Generic across the fleet:** this lives in the shared capture loop every cam box runs — no per-box
config. Any cam box (cam1, cam3, or any future box) running a ShadowCast-class or similar USB
capture dongle that drifts off its negotiated rate self-heals identically once deployed.

**GOTCHA — `recording-e2e.sh`'s cleanup can silently leave the SOURCE camera's (and/or cam2's)
production `camera-box.service` INACTIVE after the run — the harness's own log shows no failure
signal (#675, 2026-07-11).** Confirmed 4x in one session (cam2 twice, cam4 once, cam1 once, always
on a box that dispatch actually used, including after a FULLY GREEN `Full-path E2E` CI run) — the
cleanup's remote restore one-liner (`systemctl restart camera-box`) is wrapped in `2>/dev/null ||
true`, so any failure (ssh timeout/refused, a hiccup earlier in the same remote one-liner) is
swallowed with zero visible signal. Manually re-running the SAME one-liner standalone succeeds
cleanly in ~3s — so it's likely session/connection-contention-specific (suspect sshd
`MaxStartups`/`MaxSessions` hit during a busy dispatch with many concurrent MCP/ssh sessions to
the same box), not a broken command. **Practical rule: after ANY `recording-e2e.sh` dispatch,
before trusting the rig is restored, run `systemctl is-active camera-box` on EVERY box the run
touched (the SOURCE camera + cam2, and cam1/cam3-6 too if `ALL_CAMBOX=1`) — do not trust the
harness's own cleanup log.** This bit a live PR's REQUIRED CI gate (#676) purely from leftover
state an earlier, unrelated dispatch left behind. Root cause not yet fixed — tracked on #675
(suggested fix: an explicit post-restart `systemctl is-active` verification that fails LOUD).

## GOTCHA — every cam box's `/var/log` is a fixed 50MB tmpfs; it fills in ~4-5 days and CAN crash `camera-box.service` (#679, 2026-07-11)

Live incident: cam2's `camera-box.service` was found `inactive (dead)` (SIGTERM'd, no auto-restart)
because `/var/log` (a RAM-backed `tmpfs`, 50MB, on every cam box by image design) was 100% full —
`rsyslogd` was spamming `No space left on device`. Checking the WHOLE active fleet found
**cam1/cam2/cam3/cam4/cam5/cam6 all at 48-50M/50M** simultaneously; cam2 was just the first to
actually crash. Dominant volume driver (confirmed via a line-count breakdown of a live syslog
tail): **`dantesync`'s `[PTP] NANO Drift: ...` line, logged ~once per SECOND, forever** (~65% of
total log volume — roughly 5x camera-box's own routine `Streaming: ...` line at 1/5s). At that
combined rate the 50MB tmpfs fills in ~4-5 days of uptime with zero external trigger. The
`ndi_display` "Waiting for display... /dev/fb0" WARN is NOT a contributor — it's already correctly
throttled to 1 line/30s in `src/ndi_display.rs`.

**Health check (run fleet-wide when investigating ANY cam box weirdness — a full log disk can
cause silent, hard-to-diagnose service instability):**
```bash
# per box (root@10.77.9.6X or the linux-camN MCP Shell):
df -h /var/log   # 100%/50M used = the box is at risk NOW
```

**Stopgap mitigation (safe, reversible, does not touch camera-box.service or its config):**
```bash
tail -c 500000 /var/log/syslog > /tmp/syslog_tail_backup.txt 2>/dev/null   # keep recent context
: > /var/log/syslog
systemctl restart rsyslog
systemctl is-active camera-box   # confirm it wasn't already down; restart if it was
```
This is a STOPGAP ONLY — the tmpfs refills on the same ~4-5 day cadence until dantesync's own
per-second PTP debug logging is throttled/leveled down (dantesync is a separate repo, tracked on
`#679` — do not attempt that fix from this repo).

## GOTCHA — live `journalctl -f` tailing on a cam box for diagnostics is DANGEROUS; retention is already only 2-20 minutes (#707, 2026-07-14)

**Never leave an unfiltered `journalctl -f` (or `dmesg -w`) running on a cam box while investigating
something** — confirmed live while root-causing #707's CAM1 residual: an unfiltered tail (piped to a
file for later inspection) generated enough volume that, combined with the box already sitting close
to its `/var/log` 50MB tmfps ceiling (the #679 gotcha above), it tipped `rsyslogd` into a
`No space left on device` write-failure retry SPIRAL — hundreds of thousands of duplicate lines in
under 10 minutes, which then ALSO filled `/tmp` (a separate 100MB tmpfs) via the runaway capture file
itself. This didn't just risk disk — it produced ONE fully CONTAMINATED gate run (`all_cambox_continuity`
copies=844/846, delivery-latency p50=27.8s/max=215s on CAM1) that looked like a catastrophic new
finding but was entirely an artifact of the monitoring itself, not a real recurrence. **Two separate
lessons:**

1. **Even under NORMAL conditions, the live journal retains only ~2-20 minutes** on these boxes
   (tmpfs-backed, aggressively vacuumed under any real log volume) — a post-hoc `journalctl --since/
   --until` query for a burst that happened even 10-15 minutes ago will very likely come back empty.
   There is NO reliable way to retroactively inspect a cam box's journal for an event more than a few
   minutes old. If you need evidence from a specific mechanism, you MUST be tailing (safely, see below)
   BEFORE the event happens, not querying after.
2. **If you must live-tail for a specific diagnostic, filter to an EXACT, narrow, low-frequency
   pattern with `grep -F`/`grep -E --line-buffered` PIPED on the remote host** (so only matching lines
   ever hit disk) — e.g. `journalctl -f -o short-iso --no-pager | grep --line-buffered -F 'blocking
   send STALL' > /tmp/x.log &`. NEVER use a broad pattern like `error` or `stall` alone — `rsyslogd`'s
   OWN cascading disk-full messages contain generic words like "error" and will match, turning your
   "safe" filtered tail into the same runaway spiral. Check `df -h /tmp` periodically while it runs,
   and ALWAYS kill the tail process (by PID, never `pkill -f '<pattern>'` — the pattern can self-match
   your own `ssh ... "pkill -f '<pattern>' ..."` command line and kill the SSH session instead, see
   `capture` skill's V4L2/rig-mode `pgrep -x` footgun for the same self-match class) and delete the
   capture file the moment you're done, before triggering another rig run.

**Separate, unrelated tmpfs accumulation bug found the same session: #749** — `recording-e2e.sh`
never cleans up its per-run `camera-box-burn-<RUN_ID>` binaries in `/tmp` (a DIFFERENT, also-100MB
tmpfs from `/var/log`), so repeated gate runs alone (no live-tailing needed) eventually fill it and
break the next run's `scp` deploy outright. `df -h /tmp` (not just `/var/log`) belongs in the health
check above when investigating ANY cam box weirdness, especially right before/after a `full-path-e2e`
run.

## #731 — Companion Satellite on imag-nb (headless Stream Deck client) — the systemd hwdb/uaccess ACL trap

imag-nb (10.77.9.182) runs Bitfocus Companion Satellite as a headless systemd service so the
physically-connected Elgato Stream Deck can drive Bitfocus Companion running on the SEPARATE
`companion.lan` (10.77.9.205) box. Installed via `scripts/setup-imag.sh` step 20 (idempotent,
safe to re-run) — installs via the OFFICIAL `bitfocus/companion-satellite` installer script
(`pi-image/install.sh`, creates a dedicated `satellite` system user + a `satellite.service`
systemd unit), never a hand-rolled reimplementation.

**Config mechanism — `/boot/satellite-config` is a PER-START import, not one-shot.**
`fixup-pi-config.js` (the service's `ExecStartPre`) re-reads `/boot/satellite-config` on EVERY
service start, imports `COMPANION_IP`/`REST_PORT` into the persisted
`/home/satellite/satellite-config.json`, THEN resets the boot file back to a blank commented
template. So (re-)pointing the satellite at a different Companion host is: write
`/boot/satellite-config` fresh with the real values, then `systemctl restart satellite` — NOT a
one-time file that only matters on first install.

**GOTCHA — the officially-installed `50-satellite.rules` udev rule (`GROUP=satellite,
MODE=660`) can be silently OVERRIDDEN by systemd's own hwdb/uaccess mechanism, leaving the
headless service user unable to open the device (live-confirmed 2026-07-13).** Any HID device
systemd's hwdb classifies as an "AV production controller" (`ID_AV_PRODUCTION_CONTROLLER=1` env
var, set via `SUBSYSTEM=="hidraw", IMPORT{builtin}="hwdb"` — the Elgato Stream Deck matches this)
gets tagged `uaccess` by `/usr/lib/udev/rules.d/70-uaccess.rules:89`
(`SUBSYSTEM=="hidraw", ENV{ID_AV_PRODUCTION_CONTROLLER}=="1", TAG+="uaccess"`). systemd-logind
then grants a PER-SEAT ACL to whichever user owns the active graphical session (the desktop
autologin user, `newlevel` on imag-nb) — and that ACL grant **explicitly sets `group::---`**,
which REPLACES the plain group-permission bits entirely once any ACL exists on the file (POSIX
ACL semantics: an ACL present means the traditional `group` bits are ignored in favor of the
ACL's own `group::` entry). Symptom: `journalctl -u satellite` shows
`Open "streamdeck:..." failed: ... cannot open device with path /dev/hidrawN` even though
`ls -l /dev/hidrawN` correctly shows `crw-rw----+ 1 root satellite` (the `+` is the tell — an ACL
is present and is silently overriding what the ownership line implies).

**Fix — strip the uaccess tag for these devices, numbered AFTER `70-uaccess.rules` (tag removal
must run after the tag is added):**
```
# /etc/udev/rules.d/99-companion-satellite-no-uaccess.rules
SUBSYSTEM=="hidraw", ENV{ID_AV_PRODUCTION_CONTROLLER}=="1", TAG-="uaccess"
```
`udevadm control --reload-rules && udevadm trigger --subsystem-match=hidraw` applies it going
forward (a FUTURE hotplug/reboot never gains the ACL). A device that's ALREADY plugged in and
already carries the stale ACL from before the rule existed needs an explicit
`setfacl -b /dev/hidrawN` reset — the udev rule alone doesn't retroactively strip an
already-applied ACL. Both steps are baked into `setup-imag.sh` step 20 (loops every
`ID_AV_PRODUCTION_CONTROLLER=1` hidraw device and resets its ACL on every run — idempotent-safe).

**Verify:** `journalctl -u satellite -n 30` should show `Connected to Companion:
CompanionVersion="..."` with no `cannot open device` error, and `curl
http://127.0.0.1:9999/api/surfaces` (the satellite's own REST server, note the `/api/` prefix —
plain `/status`/`/surfaces` 404) should list the claimed surface, e.g.
`[{"pluginId":"elgato-stream-deck",...,"surfaceId":"streamdeck:...","productName":"Elgato Stream
Deck MK.2"}]`. This proves the TECHNICAL connection + surface claim — it does NOT prove the
operator's actual Companion button config is wired up correctly on the Companion server side
(adding/mapping the surface in Companion's own web UI is a separate, human step).

**Reusable lesson for ANY future headless-service-needs-a-HID-device work on a Linux desktop
box** (imag-nb or any future similar provision): the `70-uaccess.rules`/`logind` ACL mechanism
exists to let a LOGGED-IN desktop user access convenience devices (webcams, joysticks, "AV
production controllers", smartcards, etc.) without root — it actively fights a udev rule that
tries to grant GROUP-based access instead, because ONCE an ACL exists on a file, the ACL wins
over the group bits entirely. Any headless service (not tied to a login session) that needs to
open such a device needs its own `TAG-="uaccess"` rule numbered after `70-`, not just a
`GROUP=`/`MODE=` rule alone.
