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
