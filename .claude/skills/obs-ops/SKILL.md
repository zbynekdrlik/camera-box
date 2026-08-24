---
name: obs-ops
description: >
  OBS launch, recovery, and management on the dev-rig Windows boxes (strih 10.77.9.202,
  stream 10.77.9.204). Load when launching, restarting, or recovering OBS on either box,
  or when interacting with OBS WebSocket on the rig.
---

# OBS Ops (strih + stream)

Canonical extended reference: `/home/newlevel/devel/restreamer/.claude/skills/stream-lan-operations.md`

## #257 — genlock is HARD-LOCKED + ENV-FREE (read this first; supersedes the env model below)

The genlock build no longer uses ANY `OBS_GENLOCK_*` / `OBS_BURN_*` env (removed in #257). So the
PEB-env-verify sections below are HISTORY — `launch-obs-genlock.sh` no longer carries or checks env.
Current ops:

- **Relaunch OBS:** `scripts/launch-obs-genlock.sh --box strih|stream [--force]` — env-free; it
  clears sentinels, launches cwd=bin\64bit, and log-verifies `genlock: … render tick ENABLED` (the
  build-default proof) + DistroAV. There is no `--mode`, no PEB env check.
- **Genlock config:** render tick + ts-align are build defaults (always on); latency is a build
  const (3 ms, floor 3) with the per-source override in the DistroAV source UI ("Latency (ms)").
- **Measurement burn:** a per-source `genlock_burn` bool toggled over OBS WebSocket with NO relaunch
  — `scripts/obs_burn_filter.py add|remove --host <ip> --input "<NDI input>"`, or
  `scripts/rig-mode.sh test|event` for both boxes. (No `OBS_BURN_QR` relaunch.) See the genlock skill.

## Launch Requirements

**cwd MUST be `C:\Program Files\obs-studio\bin\64bit`** — wrong cwd produces
"Failed to find locale/en-US.ini" and a broken OBS (~37MB, no WebSocket, no NDI, no log).

Use the win-* MCP `Shell` `cwd` param, or `Start-Process -WorkingDirectory`.

**NB:** The MCP Shell blocks/times-out on a GUI `Start-Process` but the launch still issues.
Verify health in a SEPARATE call (don't put a long `Start-Sleep` in the launch command).

Clear `%APPDATA%\obs-studio\.sentinel\*` before relaunch — stale sentinel files trigger the
"OBS Studio did not properly shut down / Run in Safe Mode?" modal, which hangs a headless launch.
The genlock rig build removes that modal at the source (#1195): `checkForUncleanShutdown()`
auto-selects a NORMAL launch and never shows the dialog (safe mode disables DistroAV + genlock, so
it stays OFF). There is NO `DisableSafeModePrompt` config key in this vendored 32.x tree — it does
not exist (an earlier note here claimed it did; that was wrong). Clearing sentinels is the
belt-&-braces cleanup the launch wrapper (`launch-obs-genlock.sh`) and the strih respawn AHK
(`scripts/strih/NL_STARTUP.ahk`) both do, so even a not-yet-redeployed binary never hits the modal.

### GOTCHA — an OBS launched over plain SSH DIES when the ssh session closes (#859, cost a live outage)

**Launching OBS from an ssh-spawned PowerShell "works" and then silently takes the box down
minutes later.** Windows OpenSSH runs each session's processes in a job object and tears the whole
tree down at disconnect, so `Start-Process obs64` over ssh gives a fully healthy OBS — it renders,
NDI delivers, the genlock FIFOs lock, the launch wrapper's own log verification PASSES — and then
obs64 is killed the moment the ssh command returns. **Live incident 2026-07-29:** the DLL hot-swap
for #859 was driven over ssh (the `#701 plain scp/ssh DOES work` note above is about FILE COPY, not
about launching a GUI app); both strih AND stream were left with **no OBS at all**, discovered only
when a follow-up check showed `obs64=0` on both. The wrapper's `#257 OK: obs64 PID … launched` line
is NOT evidence of persistence — it is evidence of a launch that is about to be reaped.

- **Preferred: the win-* MCP `Shell`.** It holds a persistent session, so the launch survives. This
  is what "run the program via the win-* MCP Shell" in the wrapper's own header means — it is not a
  stylistic preference.
- **When the box has NO MCP** (e.g. `win-strih` configured in `.mcp.json` but not connected this
  session): create the process so it BREAKS AWAY from the ssh job object —
  ```powershell
  Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
    CommandLine      = "C:\Program Files\obs-studio\bin\64bit\obs64.exe"
    CurrentDirectory = "C:\Program Files\obs-studio\bin\64bit"
  }
  ```
  Verified to survive the disconnect (`rc=0`, and the pid is still there from a FRESH ssh session).
  Note it takes `CurrentDirectory`, so the cwd requirement above is still satisfied.
  - **GOTCHA (2026-08-03, live user complaint): the CIM-created process lands in SESSION 0 —
    INVISIBLE at the physical console.** Windows OpenSSH runs in session 0, and
    `Win32_Process.Create` inherits the caller's session, so the launched OBS survives the
    disconnect but has NO window on the operator's desktop (`tasklist` shows
    `Session Name=Services, Session#=0`; `MainWindowTitle` empty). Everything HEADLESS keeps
    working — NDI output, WebSocket :4455, genlock log lines, recording — so every remote
    verification passes while the operator at the rig sees "OBS nebeží" (the #962-deploy relaunch
    did exactly this to BOTH boxes + strih's AutoHotkey64). So the CIM fallback is a DEGRADED
    last resort for keeping the broadcast chain alive when no MCP exists, NEVER an acceptable
    end state: as soon as the win-* MCP is back, force-kill the session-0 instance and relaunch
    via the MCP `Shell` (the MCP server runs in the interactive session — check with
    `(Get-Process -Id $PID).SessionId` → must be `1`). Acceptance check after ANY launch:
    `Get-Process obs64 | Format-List Id,SessionId,MainWindowTitle` must show `SessionId: 1` AND a
    non-empty window title. On strih, restart AutoHotkey64 the same way (MCP Shell, session 1) —
    an ssh-restarted AHK is equally invisible and can't drive session-1 windows.
  - **REPEATED 2026-08-04 (issue-940 fast-DLL rollout — the user's 3rd "OBS nebeží" complaint):
    the ssh+CIM relaunch was used even though BOTH win-* MCPs were connected, and the session-0
    instances sat invisible for ~3.5 h passing every headless check (WS, NDI, recording, burns).
    The rule is therefore absolute: when a `win-strih`/`win-stream-snv` MCP is CONNECTED, the
    ssh+CIM launch path is BANNED for GUI apps — launch via the MCP `Shell` only, and end every
    relaunch with the SessionId+MainWindowTitle acceptance check above ON BOTH obs64 AND (strih)
    AutoHotkey64. A relaunch that skips that check is not a completed relaunch.**
- **Dead ends, do not retry:** `schtasks /run` on an `/IT` task returns `ERROR: Element not found`,
  and the same task with a near-future time trigger never fires (`Last Result: 267011` = never ran)
  — `/IT` will not start from a non-interactive context even though `Get-Process explorer` shows a
  live desktop session. `Start-Process explorer.exe <lnk>` did not launch OBS at all.
- **ALWAYS verify from a SECOND, fresh ssh connection** after any launch — a check inside the same
  session that launched it cannot see the teardown. Health bar is the existing one: exactly ONE
  obs64, >100 MB (900 MB–1.4 GB when warm), port 4455 listening, a log whose mtime is advancing.
- **On strih, expect AHK to add a SECOND obs64** a few seconds later (it keys on the obs64 WINDOW —
  see "AHK on strih"). After a manual relaunch, count instances and kill the small (~70 MB) one that
  owns neither :4455 nor the newest log; keep the warm one.
- **After ANY force-kill relaunch, re-check `genlock_burn` before trusting the box for a gate run.**
  The measurement burn is a per-source runtime bool that lives in the scene collection, so a
  force-kill loses the E2E's burn-OFF cleanup and OBS restores the last SAVED state — which, if the
  kill landed after a gate run had turned burns on, is burns ON. Live 2026-07-29: the swap's
  force-kill left `genlock_burn=True` on all four strih cam inputs AND on stream's `NDI 2ME PGM`,
  and the next gate run aborted at `[0/8]` with `FAIL: strih NDI cam1: genlock_burn is still ON from
  a prior…` before computing any verdict. Same class as #844 (a cancelled run leaking the burn), but
  reached through a force-kill rather than a cancel. Check + clear, no restart needed:
  ```bash
  python3 scripts/obs_burn_filter.py check  --host 10.77.9.202 --input "NDI cam1"
  python3 scripts/obs_burn_filter.py remove --host 10.77.9.202 --input "NDI cam1"   # per input
  ```
  This composes with the existing "force-kill relaunch restores a STALE saved config" section below
  — the burn is one more piece of state that silently reverts.

## Healthy OBS Proof

- Exactly ONE obs64 process
- WorkingSet >100 MB (~900 MB–1.4 GB when healthy)
- Port 4455 listening
- Log shows:
  - NO `Safe Mode enabled.` (safe mode would disable DistroAV + genlock; the distroav/genlock lines below prove it is OFF)
  - after an unclean shutdown: `Crash or unclean shutdown detected -- auto-selecting NORMAL launch` (#1195 — a NORMAL launch, never a blocking Safe-Mode modal; a clean start logs no safe-mode line at all)
  - `[distroav] plugin loaded`
  - `genlock: ... render tick ENABLED`
  - `[distroav] NDI Main Output started`
  - NO `Failed to find locale`
  - NO `Failed to initialize video`

**Genlock latency/audit lines need a LIVE consuming source — don't read their absence as broken.**
The `genlock: timestamp-aligned release ENABLED` + the #235 single-knob `genlock: latency = N ms
(≈ M frames @ Ffps)` line, and the every-5s `genlock-fifo audit '<src>': ... latency_ms=N
(≈M frames) ... reserve_ms=N` lines, arm ONLY when the program scene is rendering an NDI source that
is ACTUALLY DELIVERING frames. If program is parked on a dead/black camera (source not feeding —
common off-air or after a probe leaves it on a stale scene), these lines NEVER appear even though OBS
is healthy (30fps render, NDI Main Output active, 0 skips). To get the latency proof: find a camera
that's actually feeding (`Get-NetTCPConnection -OwningProcess <obs pid>` to a `10.77.9.6x:5961`),
switch program to its scene over WS (`SetCurrentProgramScene`), then the ts-align line + `latency_ms=N`
audits appear within ~10s. **Genlock is a build default now (#257) — there is no env to verify.** The
per-source latency (ms, floor 3) and the measurement burn are RUNTIME settings: the latency is the
DistroAV source UI int, the burn is the `genlock_burn` WS toggle — **both hot-apply with NO relaunch**
(set them over OBS WebSocket / the source UI and they take effect live). (Pre-#257 a genlock change was
an env var that took effect only at OBS launch, requiring a full relaunch, and a win-* MCP `$env:` /
child-PEB read was needed to prove the var was inherited — all gone with the env.)

## Reliable genlock (re)launch — USE THE WRAPPER (#128)

**Every OBS (re)launch — deploy, crash-recovery, reboot — MUST go through
`scripts/launch-obs-genlock.sh`. Do NOT hand-roll a `Start-Process` for the broadcast OBS.**

The wrapper is the PURE planner — the agent drives the win-* MCP (a GUI relaunch + on-screen log
verification is exactly what the win-* MCP is for; #701 proved plain scp/ssh DOES work against
strih/stream, but that doesn't help drive/verify a GUI app). It
PRINTS the exact PowerShell program to paste into the box's `win-strih` / `win-stream-snv` MCP
`Shell`. The emitted program, in ONE self-contained run: clears `%APPDATA%\obs-studio\.sentinel\*`,
launches obs64 cwd=`bin\64bit`, then VERIFIES the OBS log shows `genlock: … render tick ENABLED` (the
build-default genlock proof) + DistroAV loaded, exiting **non-zero (fail loud)** otherwise — so a
relaunch can never leave a silent half-broken box.

```bash
# Print the launch+verify program for the box, then paste the program (between the dashed lines)
# into that box's MCP Shell. Use --force to force-kill a wedged obs64 first (DEV-rig recovery).
scripts/launch-obs-genlock.sh --box strih  --force   # win-strih  (10.77.9.202)
scripts/launch-obs-genlock.sh --box stream --force   # win-stream-snv (10.77.9.204)
```

**#257: the wrapper is ENV-FREE.** Render tick + ts-align are build defaults — there is no
`OBS_GENLOCK_*` env to read fresh from Machine, set explicit, or PEB-verify, and there is no `--mode`
(the measurement burn is a runtime `genlock_burn` WS toggle, not a launch flag). The old "stale-env
trap" (#128/#126 — a long-lived MCP shell inheriting a stale env snapshot → a genlock var silently
UNSET → render tick off, invisibly) is **gone with the env**. The `genlock: latency = N ms` /
`latency_ms=N` audit lines need a live consuming source (see the section above), so the wrapper treats
their absence as a non-fatal WARNING — it gates on `render tick ENABLED` + DistroAV. Behavioral guard:
`tests/launch_obs_genlock.rs`.

### (HISTORY, pre-#257) Reading a running OBS's child PEB env

⚠️ **Obsolete for genlock — there is no `OBS_GENLOCK_*` env any more (#257).** Pre-#257 the win-* MCP
`$env:` read was a stale snapshot, so the only truth about what env an OBS had inherited was its own
PEB, read via `NtQueryInformationProcess` + `ReadProcessMemory` (PEB+0x20 → ProcessParameters,
+0x80 → Environment, +0x3F0 → EnvironmentSize; `OpenProcess(0x0410,…)` = QUERY_INFORMATION | VM_READ).
With genlock now a build default this verification is no longer needed — genlock state is read from
the OBS log (`render tick ENABLED`).

## GOTCHA — NDI source dropdown is an EDITABLE combo: never type into it when the finder list is empty (#795)

Live incident (2026-07-17 event): the strih→stream feed collapsed (network), the operator opened
the DistroAV source properties to re-pick — the NDI list was EMPTY (discovery lost on the sick
network) and the dropdown is an editable combobox, so keystrokes MANGLED the stored source name
character by character ('NDI 2ME-PGM' → 'N2ME-PGM' → '2ME-PGM'), each update pointing at a
nonexistent source → black, unrecoverable from the UI. Recovery = restart OBS (the saved scene
JSON still holds the last-SAVED correct name). Operator guidance until #795 lands (list-only
selector): when a feed dies, do NOT open/edit the source picker — restart OBS via the launcher.
The mangled names are visible in the log as successive `[distroav] NDI Source Updated:` lines.

## Recovery — Do It Autonomously (Never Ask)

The user has repeated this 2-3× and gets angry when re-asked which recovery method to use.

1. **Normal case:** graceful shutdown via OBS WebSocket `ExitOBS`
   (ws://<box>:4455; strih pwd: <OBS-strih-WS-pwd — NOT committed; in the box's OBS config / password store> if required; stream: no auth).
   **NB (#225):** the **genlock OBS build 32.2.0 / obs-websocket 5.7.3 does NOT expose `ExitOBS`**
   in its `availableRequests` — the request returns code 204 `UnknownRequestType`. So a graceful
   WebSocket exit is unavailable on this build; to restart it for a config change, gracefully
   `StopStream`/`StopRecord` over WS first (clean disconnect from the restreamer), then fall through
   to the force-kill + relaunch path below. This is the documented dev-rig action when the graceful
   route is unavailable.

2. **Wedged OBS** (dead WebSocket, ignores ExitOBS, ~33 MB or pegged CPU, crash-recovery
   dialog, renders black) → **force-kill obs64 + relaunch**.
   This is a DEV rig; the restreamer skill's "never kill" is OVERRIDDEN here.
   User verbatim: "kludne ho killni" — just kill it.

   After kill: clear `.sentinel\*`, then relaunch cwd=bin\64bit.

   **#391 diagnostic-capture-before-kill (when the #391 obs-liveness-watchdog alerted a wedge and
   root cause is still unknown):** before force-killing, grab a forensic snapshot via the win-* MCP
   so a recurrence is diagnosable — no DXGI/TDR signature was found for the 2026-07 ~25h stream-OBS
   wedge (obs64 pegged ~168% CPU, `Responding=False`, 16.0% render-lag), and Windows Event Log's own
   hang detector (`Application`, ID 1002) did NOT fire/log for that window, so only this capture (or
   the watchdog's own `GetStats`/process-state history) preserves any evidence:
   ```powershell
   $ts = Get-Date -Format 'yyyyMMdd-HHmmss'
   Get-Process obs64 | Select-Object Id,CPU,WorkingSet,Responding,StartTime |
     Out-File "$env:TEMP\obs64-wedge-$ts.txt"
   Get-Content (Get-ChildItem "C:\Users\*\AppData\Roaming\obs-studio\logs\*.txt" |
     Sort-Object LastWriteTime -Descending | Select-Object -First 1) -Tail 200 |
     Out-File -Append "$env:TEMP\obs64-wedge-$ts.txt"
   ```
   Then proceed with the force-kill + relaunch as usual. Pull the file back via `FileDownload` if the
   wedge doesn't reproduce again soon — otherwise it's fine to leave it in `%TEMP%` for later.

3. **GPU wedge on stream box** (`DXGI_ERROR_DEVICE_REMOVED` / TDR on RTX 4060, open: #89):
   OBS restart alone often does NOT clear a wedged GPU. **Reboot the PC.**
   strih: render-black + crash + ~205% CPU hang (no D3D11 TDR signature, open: #93, dual RTX 2070 SUPER).
   User directive: fix GPU stability first (suggested nvidia driver upgrade on stream.lan).
   **Diagnosis refinements (proven 2026-07-02):**
   - **`nvidia-smi` healthy ≠ D3D healthy.** After the TDR, nvidia-smi answered normally (0 %,
     normal temp) while EVERY new D3D device stayed broken — a freshly relaunched OBS re-hit
     `Device Removed 887A0007` within seconds and NVENC failed `NV_ENC_ERR_INVALID_DEVICE`.
     Don't let a clean nvidia-smi talk you out of the reboot.
   - **Signature over WS:** `StartRecord` returns OK but `outputActive` stays False (0 bytes,
     no file) and/or `StopRecord` → 501; obs-websocket log spams `Sending message to client
     failed: invalid state`. That silent no-op = encoder/device dead ⇒ TDR path, not a config bug.
   - **Reading the OBS log on stream:** it is DROWNED by `ytfast.py`/`ytslow.py` script spam
     (~4 lines/s). Filter first: `Get-Content $log | Where-Object { $_ -notmatch 'Unknown Script|ytfast|ytslow' }`.

4. **Unkillable obs64 after an NVENC crash** (2026-07-09, stream box): a stale obs64 instance left
   over from an earlier session — "Recording error: NVENC EncodeAPI Internal Error
   (NV_ENC_ERR_INVALID_PARAM)" + a stale "OBS has crashed!" crash-reporter dialog both still open —
   survived BOTH `Stop-Process -Id <pid> -Force` (returns OK, process still there after) AND
   `taskkill /PID <pid> /F` (printed `ERROR: The process with PID <pid> could not be terminated.
   Reason: There is no running instance of the task.` — a misleading message; the process was
   provably still running via `Get-Process`). No classic TDR/DXGI signature (`nvidia-smi` showed
   healthy 0% util, normal temp — unlike point 3's GPU wedge) and `Responding: True` throughout (not
   the #391 pegged-CPU/Responding=False signature either) — this is a THIRD distinct unkillable
   pattern, not yet root-caused. **Fix: `Restart-Computer -Force`** (same dev-rig-recovery
   authority as point 3) — confirmed clean afterward (`(Get-Process obs64).Count == 0` post-boot,
   fresh launch via `launch-obs-genlock.sh --box stream --force` came up with a single instance,
   genlock render tick ENABLED, and a real recording produced real bytes). If you hit an obs64 that
   `Stop-Process -Force` claims to kill but `Get-Process` still shows running a few seconds later,
   don't loop retrying kill commands — go straight to the reboot.

Do NOT use AskUserQuestion for OBS recovery — just recover it.

## #391 — broadcast-OBS liveness watchdog (detect+alert; ships disabled)

A wedged obs64 (pegged CPU, `Responding=False`, high render-lag) is otherwise SILENT — it emits no
error and the encoder can keep reporting a green `outputFps` while the render loop is choking or
fully stuck. `scripts/obs-liveness-watchdog.sh` (+ `scripts/lib/obs-watchdog-decision.sh` +
`camera_box::obs_watchdog::classify`) polls `GetStats` on both boxes from a dev1 systemd timer and
fires a Discord alert once a wedge is confirmed over 2 consecutive passes — see
`systemd/obs-liveness-watchdog.README.md` for the install/live-verify procedure. **Detection is
fully automatic from dev1 (no ssh/MCP needed for `GetStats`); recovery from THIS dev1 timer is
agent-driven** — the alert embeds the exact `scripts/launch-obs-genlock.sh --box <box> --force`
command, because the win-* MCP is agent-only (a dev1 timer has no agent session to drive it) and
this recovery step was never migrated to a headless ssh path even though #701 proved plain
scp/ssh DOES reach strih/stream (a bare systemd timer still cannot itself force-kill or relaunch
obs64.exe today). When you (the agent) see that alert, just run the
embedded recovery command — do NOT ask before recovering (same "recover it, don't ask" rule as the
rest of this file).

## #411 — Windows-local unattended self-heal (ships disabled)

Recovery from the #391 alert above still needs an agent watching Discord — it fails the exact
overnight/unattended case the watchdog exists to cover. `scripts/obs-self-heal-install.sh` (+
`camera_box::obs_self_heal`) emits a per-box Windows Task Scheduler job (~2 min cadence) that runs
ENTIRELY on the box itself: no ssh, no MCP, no agent session. It gathers a LOCAL sample
(`Get-Process obs64`: count / `Responding` / CPU% — no OBS WebSocket round-trip), pipes it through
`obs-watchdog-gate.exe` for the WEDGE VERDICT (reusing `obs_watchdog::classify` unchanged), and
SEPARATELY pipes its persisted state through `obs-self-heal-gate.exe` for the RECOVERY decision
(reusing `obs_self_heal::decide` unchanged — confirm-threshold / throttle / single-recovery-lock /
stale-lock detection). **Both decisions get their own thin gate binary** — the pattern generalizes
beyond stateless classification (#391) to genuinely stateful branching logic (#411): whenever a
decision needs to be reused in a PowerShell script, bridge it through a small Rust CLI with a JSON
stdin/stdout contract and a `0`/`1`/`2` exit-code convention (`0`=no action, `1`=act,
`2`=tooling-error-in-our-own-payload — kept DISTINCT from an unexpected/unknown exit code, which
must fail loud rather than silently read as healthy), rather than re-deriving the logic in
PowerShell. On a CONFIRMED wedge it force-kills + relaunches obs64 via `launch-obs-genlock.sh`'s
own program (one launch path, reused verbatim). The AHK race documented above is solved
structurally: `Stop-Process AutoHotkey64` runs FIRST (before obs64 is ever touched), and AHK is
restarted only LAST, after the relaunch is verified — a double-launch is impossible by
construction, not just by convention. Ships DISABLED (`<Enabled>false</Enabled>` in the generated
Task Scheduler XML); run `scripts/obs-self-heal-install.sh --box strih|stream --help` for the full
install + mandatory live-verify (healthy-box dry run + simulated-wedge run) procedure before
enabling on either box.

### Gotchas hit building this (apply to any future Windows-planner-script / CI-gate work)

- **`${var:-default text}` in bash breaks if `default text` contains an apostrophe/single-quote —
  even inside an outer double-quoted string.** `local x="${y:-obs-self-heal-gate.exe's default}"`
  fails with `unexpected EOF while looking for matching ''` — bash re-parses quoting INSIDE a
  `${parameter:-word}` expansion independently of the enclosing quotes. Never put a literal `'`
  in a `${var:-...}` fallback string; rephrase without a contraction/possessive.
- **A GitHub Actions `pwsh` step FAILS on any lingering non-zero `$LASTEXITCODE` at script end —
  even when your OWN `if` check already treated that exact code as the expected pass case.**
  E.g. `'{}' | ./gate.exe; if ($LASTEXITCODE -ne 2) { throw ... }` still fails the step when the
  gate correctly exits 2, because GH Actions checks `$LASTEXITCODE` itself after the script
  finishes. Fix: explicitly `exit 0` at the end of the step once your own checks pass, to override
  the residual exit code from the last native command.

## WebSocket Credentials

| Box | Address | Port | Auth |
|---|---|---|---|
| strih | 10.77.9.202 | 4455 | pwd `<OBS-strih-WS-pwd — NOT committed; in the box's OBS config / password store>` (or no auth) |
| stream | 10.77.9.204 | 4455 | no auth |

## Recording Output = native 1080p, NOT 4K (#225)

The stream box program is a **1920×1080 canvas** (`[Video] BaseCX/CY=1920×1080`,
`OutputCX/CY=1920×1080`). For a long time its **recording** file came out **3840×2160 (4K)** —
NVENC GPU-upscaled the 1080p program. That softened the small burns (cam1/strih/stream QR ~300px),
caused decode mis-reads / over-counts (#226, #133/#202), and made every proof decode ~4× slower.

**Root cause:** in the active `Stream_Obs` profile (`[Output] Mode=Advanced`), the recording
**reused the streaming encoder** (`[AdvOut] RecEncoder=none` → reuse-stream) and the **streaming
encoder rescales to 4K** (`Rescale=true`, `RescaleRes=3840x2160`, `RescaleFilter=4` lanczos) — that
4K stream is the production broadcast to the local restreamer (`rtmp://127.0.0.1:1234/live`). So the
1080p program was upscaled to 4K and HEVC/h264-encoded into the recording. The recording's OWN rescale
(`RecRescale=false`, `RecRescaleRes=1920x1080`) was inactive while `RecEncoder=none`.

**Correct config — recording gets its OWN native-1080p encoder, stream output UNTOUCHED:**

```
[AdvOut]
RecEncoder=obs_nvenc_h264_tex   ; was "none" (reuse 4K stream encoder) → now a dedicated encoder
RecRescale=false                ; record the native 1920×1080 canvas, no upscale
RecRescaleRes=1920x1080         ; (only used if RecRescale=true)
; --- stream output (prod → restreamer) — DO NOT CHANGE ---
Encoder=obs_nvenc_h264_tex
Rescale=true                    ; prod stream stays 4K-rescaled (what the restreamer is fed)
RescaleRes=3840x2160
```

**Apply path** (the change only takes effect at OBS launch — `[AdvOut]/*` profile settings are NOT
hot-applied; the recording output is created when OBS starts):
1. Back up `…\basic\profiles\Stream_Obs\basic.ini`.
2. Set `RecEncoder=none` → `RecEncoder=obs_nvenc_h264_tex` (leave all `Rescale*`/`RescaleRes` alone).
3. Restart OBS (StopStream/StopRecord over WS → force-kill → clear `.sentinel\*` → relaunch cwd=bin\64bit).
4. **Verify with ffprobe** a fresh short recording is `1920x1080`, AND read the OBS log encoder blocks:
   `advanced_video_recording` → `width 1920 height 1080` (no "GPU scaling enabled");
   `advanced_video_stream` → `width 3840 height 2160` + "GPU scaling enabled" (prod unchanged).

**Before/after (proof 2026-06-24):** recording `3840x2160` → `1920x1080`; stream encoder still
`3840x2160`. Always ffprobe the recording before trusting a proof run — a silently-4K recording
softens the burns and inflates cam1's count.

## AHK on strih

`D:\_APPS\NL_STARTUP.ahk` auto-respawns obs64 from `C:\Program Files\obs-studio` (the genlock build).
To restart strih's OBS mid-session: kill obs64 + relaunch via `scripts/launch-obs-genlock.sh --box
strih --force` (genlock comes up automatically — it is a build default, #257, no env needed). AHK then
sees OBS running and won't re-add another one.

**AHK is STRIH-ONLY — `build_launch_program` takes a third arg `HAS_AHK` (#792 guard repair,
2026-07-16).** The #786 audio-gate redraw loop brackets AHK (stop-first/restart-last), but ONLY a
box that actually runs the watcher may get those commands: `build_launch_program OBS_DIR FORCE 1`
(default = strih behavior) emits the bracket; `... 0` (stream — the `--box stream` case and
obs-self-heal-install.sh both pass 0) emits documented no-ops instead. Pinned by
`tests/launch_obs_genlock.rs::program_without_ahk_watcher_carries_no_ahk_commands_786` and the
`#411` self-heal stream guard — a new caller of `build_launch_program` must pass the box-correct
third arg, never rely on the default for stream.

**GOTCHA — a libobs HOT-SWAP (deploying a new `obs.dll`) needs AHK STOPPED for the swap window, not
just kill+relaunch.** AHK's `SafeLoop` (`Run obs64` after a ~5 s delay when its window is gone, polled
every 1 s) is fine for a fast restart, but a DLL swap is multi-step (kill obs64 → `Copy-Item` the new
`bin\64bit\obs.dll` → relaunch) and AHK can respawn obs64 *mid-copy*, which **locks the DLL** and the
copy fails (or, worse, AHK launches a SECOND obs64 around the relaunch since it keys on the obs64
*window*, which appears a few seconds after `Start-Process`). So for a hot-swap: `Stop-Process
AutoHotkey64` FIRST → kill obs64 → backup + copy the new obs.dll (verify `Get-FileHash`) → clear
`.sentinel\*` → relaunch via the wrapper → confirm ONE obs64 + render tick ENABLED → then RESTART AHK
(`Start-Process AutoHotkey64.exe "D:\_APPS\NL_STARTUP.ahk"`) so reboot-recovery is restored. On
restart AHK sees the OBS window already up → takes the `else Startup()` branch (no "zapnúť všetko?"
MsgBox) and won't double-launch. (Proven on the 2026-06-27 #147 obs.dll deploy.)

**GOTCHA (#767 deploy, 2026-07-15) — EVERY Windows hot-swap MUST also update
`C:\Program Files\obs-studio\GENLOCK_BUILD_SHA.txt` to the deployed build's commit SHA** (same
role as imag's `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt`; the bundle-state-server serves it as
`genlock_build_sha` and the #756 CROSS-BOX parity facet in the fused E2E's `[0/8]`
version-integrity gate compares it across strih+stream+imag). Swapping the DLLs without the
marker leaves the box reporting the OLD SHA → `genlock_parity DRIFT` → the E2E gate REFUSES the
rig at minute 0 (live incident: the first post-#767-deploy gate run failed exactly this way —
imag=dede91825, strih=stream=2789f46c). Write it BOM-free:
`[IO.File]::WriteAllText('C:\Program Files\obs-studio\GENLOCK_BUILD_SHA.txt', "<sha>`n",
(New-Object System.Text.UTF8Encoding $false))`.

strih has OTHER OBS installs in `D:\_APPS` (1ME/2ME/vestibul/input/light) — do NOT touch;
broadcast = the Program Files 2ME one only.

**GOTCHA (#867, 2026-07-29) — `AutoHotkey64.exe` is NOT on PATH; a bare `-FilePath
'AutoHotkey64.exe'` (as the hot-swap recipe above literally shows) can never resolve.** AHK v2 is
installed on strih USER-SCOPED under
`C:\Users\newlevel\AppData\Local\Programs\AutoHotkey\v2\AutoHotkey64.exe` (also
`...\AutoHotkey\UX\AutoHotkeyUX.exe`) — confirmed live, `where AutoHotkey64.exe` returns nothing.
The `.ahk` file association DOES work (it lives in HKCU, not HKCR/HKLM, so a non-interactive
`assoc .ahk` misleadingly reports "not found" — that's expected, not evidence AHK is missing).
Root cause of a real strih outage: a prior obs.dll hot-swap run's `Stop-Process -Name
AutoHotkey64 -Force` (correctly, before touching obs64) followed by exactly the bare
`Start-Process -FilePath "D:\_APPS\NL_STARTUP.ahk"` this doc used to show — the restart step
was never verified, silently failed, and strih ran with no live respawn watcher for hours before
the user noticed. `scripts/obs-self-heal-install.sh` and `scripts/launch-obs-genlock.sh` had the
identical shape (`ahk_start_block` / `ahk_restart_ps`) and got the real fix: both now source
**`scripts/lib/ahk-watchdog.sh`'s `ahk_resolve_and_relaunch_ps`**, which probes, in order,
`%LOCALAPPDATA%\Programs\AutoHotkey\v2\`, `%ProgramFiles%\AutoHotkey\v2\`,
`%ProgramFiles%\AutoHotkey\`, then the NL_STARTUP Startup shortcut, then `Get-Command
AutoHotkey64.exe` (PATH) — and then POLLS `Get-Process AutoHotkey64` for up to ~10s
(`$ahkRelaunchVerified`/`$ahkRelaunchTarget`) instead of unconditionally logging success. **Any
future manual/ad-hoc AHK restart (including a one-off hot-swap paste like the recipe two
sections above) should reuse that shared function rather than a bare `-FilePath` relaunch, and
should always verify with `Get-Process AutoHotkey64` before trusting it came back.**

## A force-kill relaunch restores a STALE saved config — clean the baseline before measuring (#276 deploy)

OBS persists its scene-collection / global state on GRACEFUL exit. A **force-kill** (the deploy/recovery
path) cannot save, so on relaunch OBS restores the LAST-SAVED config — usually OLDER than the running
session's live state. Observed 2026-06-27 (#276 deploy): the live pre-deploy session was Studio-Mode
OFF / no burns / program on the prod scene (~4-5ms render), but after force-kill+relaunch both boxes
came up **Studio Mode ON + a stray `genlock_burn` ON + a stale program scene** (strih on Cam5-with-burn,
stream on `REC-STRIH-TMP` not `PRE`). Studio Mode renders preview+program (≈2×) and the burn adds cost,
so a naive post-relaunch "baseline" looked like 18.9ms / ~14% renderSkip — NOT a regression, just the
restored heavier state.

- **Before any render measurement:** establish a clean baseline — `SetStudioModeEnabled false` over WS +
  turn off any `genlock_burn` (`scripts/obs_burn_filter.py remove`/`check`), confirm program is on the
  live prod scene. Then A/B only the thing under test.
- **Reset to clean prod after a deploy** = restore the operator's pre-deploy LIVE state, not the stale
  restored one: burns OFF both boxes, program on the prod scene (strih `Cam 5`, stream `PRE`), Studio
  Mode OFF, no leftover test projector. (The saved config will still restore the stale state on the NEXT
  restart — a pre-existing quirk, not the deploy's fault.)

## Rig measurement helpers over OBS WebSocket (no Windows GUI needed)

- **Per-window render stats:** snapshot `GetStats` (activeFps, averageFrameRenderTime, render/output
  Skipped/Total) at T0, wait N s, snapshot again → DELTAS (renderSkipped delta, outputFps) are immune to
  startup transient and to the huge cumulative counters. `renderSkippedFrames` = GPU compositor missed
  the 60fps deadline; `outputSkippedFrames` = encoder dropped a broadcast frame (the one that matters).
  No dedicated CLI verb exists for a one-off ad-hoc read on OTHER boxes — call the RPC directly (#726):
  ```python
  import sys; sys.path.insert(0, "scripts")
  import obs_phase2 as op
  ws = op._conn("10.77.9.202", "<strih WS password — local memory, not committed>")  # stream: "10.77.9.204", "" (no auth)
  print(op._rpc(ws, "GetStats"))
  ws.close()
  ```
  On strih specifically, `scripts/strih_mv_scenes.py --host 10.77.9.202 --password <pw> --stats
  <seconds>` (#730) now DOES give a one-shot before/after-friendly delta report (activeFps,
  avgRenderMs, renderSkipped/renderTotal + %, outputSkipped/outputTotal) without hand-rolling the
  RPC — reuse it instead of the raw snippet above when measuring strih.
- **Open the built-in Multiview projector:** `OpenVideoMixProjector {videoMixType:
  OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW, monitorIndex:0}` (fullscreen). There is **no WS request to
  CLOSE a projector** — close it by `PostMessage WM_CLOSE (0x0010)` to the window titled
  `Projector - Multiview` (enumerate obs64 windows via win-* MCP). Confirm open/closed by window title.
- **Decode a measurement burn live (no recording):** turn burn ON (`obs_burn_filter.py add`), then
  `GetSourceScreenshot {sourceName:"<program scene>", imageFormat:"png", imageWidth:1920}` → base64 →
  decode the QR with opencv `QRCodeDetector` (full frame, fallback to a 2× upscaled bottom-left crop).
  A clean decode of `P{run_id}.{frame}.{gen_ts_ns}.{crc32}` (strih run_id 911002 bottom-left, stream
  911004 bottom-right) proves the burn renders + decodes; then burn OFF. (Reuses the `scripts/obs_phase2.py`
  `_conn`/`_rpc` WS helpers; the WS pw is in local memory, not committed.)

## Canonical OBS plugin-load path (#124 — no ProgramData-vs-Program Files shadow)

OBS scans MULTIPLE module locations, so the SAME `distroav.dll` in more than one of them
lets a stale copy silently shadow the intended genlock build (the mixed-version incident
#119). The genlock DistroAV plugin lives in EXACTLY ONE place:

**`C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll`** (canonical, both boxes).

- First-party OBS plugins ship under `C:\Program Files\obs-studio\obs-plugins\64bit`; DistroAV
  is the ONLY one deployed to ProgramData. A deploy MUST NOT also drop `distroav.dll` into
  `Program Files\obs-studio\obs-plugins\64bit` — that re-creates the shadow.
- The `C:\Program Files\obs-studio\data\obs-plugins\distroav` folder is resources/locale, NOT
  the binary — leave it.
- Verified live 2026-06-25: one `distroav.dll` per box (663040 B), loaded by the Program Files
  genlock `obs64.exe`, render tick ENABLED; none under `Program Files\obs-plugins\64bit`.
- `/drift-guard` step 1c reads every `distroav.dll` location across the scan paths
  (`distroav_dll_paths`) and FAILS if there is more than one, or the lone one is off canonical.
  Pinned in `vendor/README.md` as `canonical_plugin_path`. Do NOT remove the canonical copy —
  only ever clean a duplicate in a SHADOW path (and only after confirming which is canonical).

## #786 — stream OBS A/V sync mimo o ~0.9s po štarte: ASIO launch race → audio buffering 960ms (sticky)

libobs GLOBAL audio buffering is a one-way ratchet (grows on late audio, NEVER shrinks until OBS
restart). On SOME stream-OBS launches the 'ASIO Input Capture' source on **VB-Matrix VASIO-8**
floods stale audio in the ~1s window before **Dante Virtual Soundcard** finishes initializing →
43× "adding 21 ms" in 0.9s → `Max audio buffering reached!` → 960ms, stuck for the whole session.
Healthy launch = exactly ONE `adding 64 milliseconds` line, peak 64ms. Effect: audio +896ms vs
normal → the operator's ~900ms genlock latency reads as ~2000ms-worth wrong; OBS restart with a
clean launch restores it. **Launch-window only** — a 7h bad session showed ZERO further growth, so
it cannot flip spontaneously mid-event; only a relaunch re-rolls the dice.

**Diagnóza (10s):** v čerstvom logu `Select-String "Max audio buffering|total audio buffering"` —
`960` = zlý štart (reštartuj OBS kým nie je 64ms); `64` = čistý. **Núdzová kompenzácia naživo bez
reštartu:** genlock latency +~900ms (overené operátorom). Trvalý fix = launch-gate v
`launch-obs-genlock.sh` (#786).
