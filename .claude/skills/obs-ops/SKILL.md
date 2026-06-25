---
name: obs-ops
description: >
  OBS launch, recovery, and management on the dev-rig Windows boxes (strih 10.77.9.202,
  stream 10.77.9.204). Load when launching, restarting, or recovering OBS on either box,
  or when interacting with OBS WebSocket on the rig.
---

# OBS Ops (strih + stream)

Canonical extended reference: `/home/newlevel/devel/restreamer/.claude/skills/stream-lan-operations.md`

## Launch Requirements

**cwd MUST be `C:\Program Files\obs-studio\bin\64bit`** — wrong cwd produces
"Failed to find locale/en-US.ini" and a broken OBS (~37MB, no WebSocket, no NDI, no log).

Use the win-* MCP `Shell` `cwd` param, or `Start-Process -WorkingDirectory`.

**NB:** The MCP Shell blocks/times-out on a GUI `Start-Process` but the launch still issues.
Verify health in a SEPARATE call (don't put a long `Start-Sleep` in the launch command).

Clear `%APPDATA%\obs-studio\.sentinel\*` before relaunch — stale sentinel files trigger
"OBS Studio Crash Detected" modal, which hangs a headless launch. Both boxes have
`DisableSafeModePrompt=true` but clearing sentinels is the reliable fix. Avoids safe-mode
which disables DistroAV + genlock.

## Healthy OBS Proof

- Exactly ONE obs64 process
- WorkingSet >100 MB (~900 MB–1.4 GB when healthy)
- Port 4455 listening
- Log shows:
  - `[Safe Mode] Normal launch ... third-party plugins enabled`
  - `[distroav] plugin loaded`
  - `genlock: ... render tick ENABLED`
  - `[distroav] NDI Main Output started`
  - NO `Failed to find locale`
  - NO `Failed to initialize video`

**Genlock reserve/audit lines need a LIVE consuming source — don't read their absence as broken.**
The `genlock: timestamp-aligned release ENABLED (OBS_GENLOCK_TS_ALIGN)` + `sub-frame jitter reserve = N ms (#184)`
lines, and the every-5s `genlock-fifo audit '<src>': ... reserve_ms=N` lines, arm ONLY when the program
scene is rendering an NDI source that is ACTUALLY DELIVERING frames. If program is parked on a dead/black
camera (source not feeding — common off-air or after a probe leaves it on a stale scene), these lines NEVER
appear even though OBS is healthy (30fps render, NDI Main Output active, 0 skips). To get the reserve proof:
find a camera that's actually feeding (`Get-NetTCPConnection -OwningProcess <obs pid>` to a `10.77.9.6x:5961`),
switch program to its scene over WS (`SetCurrentProgramScene`), then the ts-align line + `reserve_ms=N`
audits appear within ~10s. **Verify the relaunched OBS's actual env, not just the log:** read the live child
process env (PEB `OBS_GENLOCK_*`) to PROVE `RESERVE_MS`/`TS_ALIGN` were inherited — the win-* MCP Shell env
snapshot is STALE, so always set the genlock vars EXPLICIT in the launch shell AND confirm on the child.
A genlock config change (e.g. `OBS_GENLOCK_RESERVE_MS`) is NOT hot-applied — it takes effect only at OBS
launch, so a reserve change = full relaunch (force-kill → clear `.sentinel\*` → relaunch cwd=bin\64bit with
the vars set). Machine-level env is the persisted default; a sweep that overrode reserve in its launch shell
leaves the running OBS on the override while Machine env still reads the correct value.

## Reliable genlock (re)launch — USE THE WRAPPER (#128)

**Every OBS (re)launch — deploy, crash-recovery, reboot, a reserve/config change — MUST go through
`scripts/launch-obs-genlock.sh`. Do NOT hand-roll a `Start-Process` for the broadcast OBS.** The
recurring "stale-env trap" (#128): when obs64 is spawned from a LONG-LIVED win-* MCP shell whose env
snapshot predates the Machine-scope env write, the child inherits the STALE snapshot → a genlock var
is UNSET → the render tick is silently OFF and the whole genlock guarantee is gone, invisibly (the
#126 deploy near-miss). A `$env:` read in that stale shell agrees with the wrong value, so it cannot
be trusted — only the launched process's own PEB env proves what obs64 actually inherited.

The wrapper is the PURE planner (scp/ssh to Windows is denied — the agent drives the win-* MCP). It
PRINTS the exact PowerShell program to paste into the box's `win-strih` / `win-stream-snv` MCP
`Shell`. The emitted program, in ONE self-contained run: reads the four `OBS_GENLOCK_*` vars FRESH
from **Machine** scope (the persistent HKLM source of truth, survives reboot — drift-guard #45), sets
them EXPLICIT in the spawning shell (defeats the stale snapshot), clears `.sentinel\*`, launches
obs64 cwd=`bin\64bit`, then VERIFIES the launched child's PEB env carries all four vars matching
Machine AND the OBS log shows `render tick ENABLED`, exiting **non-zero (fail loud)** otherwise — so
a relaunch can never leave a silent half-genlocked box.

```bash
# Print the launch+verify program for the box, then paste the program (between the dashed lines)
# into that box's MCP Shell. Use --force to force-kill a wedged obs64 first (DEV-rig recovery).
scripts/launch-obs-genlock.sh --box strih  --force   # win-strih  (10.77.9.202)
scripts/launch-obs-genlock.sh --box stream --force   # win-stream-snv (10.77.9.204)
```

The program EXITS 0 only when the child PEB carries all four genlock vars matching Machine AND the
log shows `render tick ENABLED`. A non-zero exit means the genlock env was NOT carried — do NOT trust
the box; re-run the wrapper. The `sub-frame jitter reserve = N ms` / `reserve_ms=N` audit lines need
a live consuming source (see the section above), so the wrapper treats their absence as a non-fatal
WARNING (the PEB read already proves `OBS_GENLOCK_RESERVE_MS=3`) — it does not fail the launch on
them. The PEB reader (NtQueryInformationProcess + ReadProcessMemory via `Add-Type`, confirmed
available on both boxes) is the canonical "read the child env to PROVE inheritance" step from the
section above — the wrapper does it automatically. Behavioral guard: `tests/launch_obs_genlock.rs`.

Proven on both boxes 2026-06-25: strih obs64 PID 6244 and stream obs64 PID 10844 each relaunched via
the wrapper with all four vars PEB-verified (`WALL_CLOCK=1 RESERVE_MS=3 TS_ALIGN=1 PRELOAD_FRAMES=1`)
+ `render tick ENABLED`; stream further showed `timestamp-aligned release ENABLED` + `sub-frame
jitter reserve = 3 ms` + live `reserve_ms=3` audits (it had a live 2ME PGM source).

### Read a running process's CHILD PEB env (ad-hoc, without relaunching)

To verify what env a RUNNING obs64 actually inherited WITHOUT relaunching it (the wrapper does this
automatically at launch; this is the standalone check) — the win-* MCP `$env:` read is a STALE
snapshot, so the only truth is the process's own PEB. Paste into the box's MCP `Shell`:

```powershell
$pid_obs = (Get-Process obs64 | Select-Object -First 1).Id
$cs = @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class PebEnv {
  [DllImport("ntdll.dll")] static extern int NtQueryInformationProcess(IntPtr h,int c,ref PBI p,int l,out int r);
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(int a,bool i,int pid);
  [DllImport("kernel32.dll")] static extern bool ReadProcessMemory(IntPtr h,IntPtr a,byte[] b,int s,out int r);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] struct PBI { public IntPtr R1; public IntPtr Peb; public IntPtr A; public IntPtr B; public IntPtr Pid; public IntPtr R3; }
  static IntPtr RP(IntPtr h,IntPtr a){ byte[] b=new byte[8]; int r; ReadProcessMemory(h,a,b,8,out r); return (IntPtr)BitConverter.ToInt64(b,0); }
  static int RI(IntPtr h,IntPtr a){ byte[] b=new byte[4]; int r; ReadProcessMemory(h,a,b,4,out r); return BitConverter.ToInt32(b,0); }
  public static string Get(int pid){
    IntPtr h=OpenProcess(0x0410,false,pid); if(h==IntPtr.Zero) return "ERR:open";
    var p=new PBI(); int rl; if(NtQueryInformationProcess(h,0,ref p,Marshal.SizeOf(p),out rl)!=0){CloseHandle(h);return "ERR:ntq";}
    IntPtr pp=RP(h,(IntPtr)((long)p.Peb+0x20)); IntPtr ea=RP(h,(IntPtr)((long)pp+0x80)); int es=RI(h,(IntPtr)((long)pp+0x3F0));
    if(es<=0||es>1048576) es=65536; byte[] buf=new byte[es]; int rd; ReadProcessMemory(h,ea,buf,es,out rd); CloseHandle(h);
    return Encoding.Unicode.GetString(buf,0,rd).Replace("\0","\n"); }
}
'@
Add-Type -TypeDefinition $cs
([PebEnv]::Get($pid_obs) -split "`n") | Where-Object { $_ -match '^OBS_GENLOCK_' } | Sort-Object
```

PEB offsets are the x64 layout: PEB+0x20 = ProcessParameters, RTL_USER_PROCESS_PARAMETERS+0x80 =
Environment, +0x3F0 = EnvironmentSize. `OpenProcess(0x0410,...)` = QUERY_INFORMATION | VM_READ.

## Recovery — Do It Autonomously (Never Ask)

The user has repeated this 2-3× and gets angry when re-asked which recovery method to use.

1. **Normal case:** graceful shutdown via OBS WebSocket `ExitOBS`
   (ws://<box>:4455; strih pwd: <OBS-strih-WS-pwd — NOT committed; in the box's OBS config / password store> if required; stream: no auth).
   **NB (#225):** the **genlock OBS build 32.1.2 / obs-websocket 5.7.3 does NOT expose `ExitOBS`**
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

3. **GPU wedge on stream box** (`DXGI_ERROR_DEVICE_REMOVED` / TDR on RTX 4060, open: #89):
   OBS restart alone often does NOT clear a wedged GPU. **Reboot the PC.**
   strih: render-black + crash + ~205% CPU hang (no D3D11 TDR signature, open: #93, dual RTX 2070 SUPER).
   User directive: fix GPU stability first (suggested nvidia driver upgrade on stream.lan).

Do NOT use AskUserQuestion for OBS recovery — just recover it.

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

**Apply path** (the change only takes effect at OBS launch — `[AdvOut]/*` is NOT hot-applied; the
output is created when OBS starts, same as `OBS_BURN_QR`, #195):
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

`D:\_APPS\NL_STARTUP.ahk` auto-respawns obs64 from `C:\Program Files\obs-studio`.
To restart strih's OBS WITH genlock mid-session: kill obs64 + `Start-Process` from a shell
that already has `$env:OBS_GENLOCK_WALL_CLOCK='1'` set (AHK then sees OBS running and won't
re-add a non-genlock one).

strih has OTHER OBS installs in `D:\_APPS` (1ME/2ME/vestibul/input/light) — do NOT touch;
broadcast = the Program Files 2ME one only.
