---
paths:
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/launch-obs-genlock.sh"
  - "scripts/lib/ahk-watchdog.sh"
  - "scripts/lib/genlock-fleet-boxes.sh"
  - "scripts/cg-chain-verify.sh"
  - "scripts/latency-pins-baseline.json"
---

# RESOLUME-SNV cg OBS on the genlock build (#1295) — the supervisor live-sitting runbook

`cg` OBS on **RESOLUME-SNV** (win-resolume MCP, traveling CG box, `resolume.lan`) is a first-class
genlock fleet member: the SongPlayer `sp-*` inputs ride the genlock FIFO and `RESOLUME-SNV (cg-obs)`
sends on the fleet wall-clock, locking the CG chain (SongPlayer → cg OBS → LED TVs + strih `cg` /
stream `NDI obs hudba`) end-to-end like the camera chain. The CODE half (#1295) wires the
deploy/launch arms + the verify-read-back; the LIVE deploy/relaunch/pin/verify is a SUPERVISOR rig
step (win-resolume MCP + read-only OBS-WS from dev1 — a worker never touches the box).

## The load-bearing fact that shapes the "pin" step: the build DEFAULTS it (no WS write)

`vendor/distroav/src/ndi-source.cpp` DEFAULTS every `ndi_source` input to **`genlock_fifo=true`**
(`obs_data_set_default_bool(..., PROP_GENLOCK_FIFO, true)`, line ~730) and
**`genlock_latency_ms_src=3`** (the floor, line ~733), and `force_genlock_certified_settings()`
(run from `ndi_source_update` whenever genlock_fifo is on) AUTO-FORCES the certified coercion
(`ndi_sync=2` NDI-timecode, `ndi_bw_mode=0` highest, etc.). So the moment cg OBS loads the genlock
build, the `sp-*_video` inputs — which on the stock plugin carry NO genlock keys — become
genlock-FIFO'd at latency 3 + certified-coerced **by construction**. The "pin every sp-* input"
step is therefore a **CONFIRM (verify-read-back), not a WS write** — which is why the tooling adds
no new WS client/writer. A hand re-pin (only if an operator had set a sp-* input off 3) uses the
SANCTIONED writer `apply_latency_pins.py --box resolume --host <ip> --pins '{...}' --execute`.

## NDI output name + the :5961 pin semantics for THIS box (#1185)

cg OBS's NDI main output is **`RESOLUME-SNV (cg-obs)`** — the name strih's `cg` input and the stock
LED TVs cache. DistroAV assigns sender ports by CREATION ORDER from `:5961`
(`.claude/rules/distroav-sender-output-lifecycle.md`): the 2ME/program output starts LAST at
`FINISHED_LOADING`, and the #1185 reserve-at-`obs_module_post_load` + adopt-in-`ndi_output_start`
pins the program sender to `:5961` so an `ndi_filter` republish can never steal the low port and
hand the TVs a frameless ghost. For cg OBS the same ordering discipline applies — after relaunch,
confirm `RESOLUME-SNV (cg-obs)` is the sender the downstream consumers resolve (the NDI port-map
watchdog baseline, `.claude/rules/ndi-portmap-watchdog.md`, is the durable guard once registered).

## AHK on this box — the OWNER's watcher, our tooling never starts it (issue 1372, was `has_ahk=1` in issue 1295)

RESOLUME-SNV has an AutoHotkey **v2** auto-respawn watcher installed:
`C:\Program Files\AutoHotkey\v2\AutoHotkey64.exe` runs `C:\Users\Resolume\Documents\_NLMEDIA
resolume\_APPS\NL_STARTUP.ahk` (Startup shortcut `NL_STARTUP.ahk - Shortcut.lnk`, `SafeLoop := 1`).
It respawns **both Resolume Arena AND OBS**.

**Owner ruling 25.9.2026 (issue 1372, "ale ahk nespustaj"): do NOT start AHK on this box.** Whether
the watcher runs is the owner's choice, never a deploy or launch postcondition. So both planners pass
the AHK mode `guard` for resolume (`fleet_box_ahk_mode` in `scripts/lib/genlock-fleet-boxes.sh`):

- **Stop it only if it is running**, so it cannot respawn a second obs64 during the byte copy or the
  relaunch. A deploy or relaunch that leaves a running watcher up races a **second obs64** (the likely
  origin of the dead 0-thread pid seen 2026-09-12). The stop sits before the byte copy, before every
  obs64 kill, and (plain launch) after the "already running" refusal.
- **Never start or restart it.** The guard programs carry no relaunch primitive, no `exit 9`, no
  best-effort restart.
- **The AutoHotkey64 count is report-only** (`#1372 AHK REPORT (report-only, never a gate)`), logged
  in deploy step (8) and after the launch's session gate. The old #978 "exactly 1 AutoHotkey64" gate
  failed the launch whenever the owner had AHK off.
- **OBS is launched directly**, in session 1, through the box's own `OBS Studio.lnk` (the #786
  guarded launcher when the shortcut is retargeted to it, exactly as on stream). The launch then
  verifies exactly one LIVE obs64 in the active session and the window title
  `build <9-char short sha of GENLOCK_BUILD_SHA.txt> - Profile: cg` (`fleet_box_obs_profile`).

Both guard fragments (`ahk_guard_stop_ps`, `ahk_guard_report_ps`) live in `scripts/lib/ahk-watchdog.sh`,
so the deploy and launch programs cannot drift. The obs-fleet `has_ahk` fact still says the watcher is
INSTALLED (`obs_fleet_has_ahk resolume` = 1). Two consumers of that fact still assume a managed
watcher; the issue-1372 slice that introduced the guard handed both to the supervisor in its
LANE-RETURN comment on the ticket (a worker cannot file tickets):
- the ENABLED obs-session watchdog still grades "AutoHotkey64 count == 1" on resolume, so with AHK
  off it reports a count of 0 as a fault;
- the obs-self-heal builder still emits the managed restart + backstop for an AHK box (latent: its
  CLI accepts `--box stream` only).
The managed stop→restart-verified mode `1` still exists in the builders for a box that sets it, but
no planner chooses it. The title match takes any profile LABEL word (en `Profile`, sk/cs `Profil`).
The `.ahk` also RunAs-launches an `Arena-Bridge` app under a second user and embeds a credential.
Tooling NEVER reads, echoes, or copies the `.ahk` body; it only stops the watcher by process name.

## SUPERVISOR RUNBOOK — the exact ordered commands for the live sitting

Run when the box is home + its IDENTITY is CONFIRMED (the plan prints this step; `resolume.lan`
currently resolves to `10.77.9.201`, which collides with `bridge` — confirm the cg OBS profile /
OBS-WS identity before touching it, `.claude/rules/rig-state-inspection.md §2`). `<ip>` below is the
live `getent hosts resolume.lan` address.

1. **DEPLOY** the genlock bundle (full — frontend changes ride along). From dev1, with an anchor
   genlock CI run id at the fleet's current SHA (see `.claude/rules/genlock-fleet-deploy.md` for
   same-SHA cross-workflow resolution):

   ```
   bash scripts/deploy-genlock-fleet.sh --run-id <anchor-run-id> --boxes resolume
   ```

   Follow the emitted plan: the STEP -1 identity-confirm, then upload the staged bytes to
   `C:\stage-genlock-<sha>` via the **win-resolume MCP** FileUpload, then paste the emitted deploy
   program into the **win-resolume MCP Shell** (timeout ≥ 240 s). It stops the AutoHotkey64 watcher
   ONLY if it is running (so it can't respawn obs64 mid-copy) + obs64, backs up, swaps the bytes,
   writes the markers, byte-verifies the deployed obs.dll/distroav.dll, and only REPORTS the
   AutoHotkey64 count. It never restarts AHK (issue 1372); OBS stays down until STEP 2.

2. **RELAUNCH** OBS in the interactive session (NEVER over ssh — `win-ssh-vs-mcp`):

   ```
   bash scripts/launch-obs-genlock.sh --box resolume --force
   ```

   Paste its emitted program into the **win-resolume MCP Shell**. It stops a running AutoHotkey64
   before the obs64 force-kill (so the watcher can't race a duplicate obs64), launches OBS directly
   via the box's `OBS Studio.lnk` (the #786 guarded launcher when the shortcut points at it), and
   NEVER starts AutoHotkey64 (issue 1372). It then verifies `render tick ENABLED` + DistroAV loaded,
   exactly one LIVE obs64 in the active session, and the title `build <short sha> - Profile: cg`,
   failing loud otherwise. The AutoHotkey64 count is only logged (`#1372 AHK REPORT`).

3. **CONFIRM the pins** (NOT a write — the build defaulted them). From dev1 (read-only OBS-WS,
   session-agnostic):

   ```
   OBS_PASSWORD=<rig-obs-ws pw> python3 scripts/latency_pins_verify.py --box resolume --host <ip>
   ```

   exit 0 = every live `sp-*_video` input is at `genlock_latency_ms_src=3` (REPORT-ONLY drift check;
   `.claude/rules/latency-pins-verify.md`). #1295: the stock-saved `cg_scenes` collection has that
   key ABSENT from every sp-* input, but the verifier reads the box's build identity over the same
   WS (`GetInputDefaultSettings` ndi_source) and reports an absent key on a genlock build as
   `got=default(3)` = OK — NOT a false `N/A`/DRIFT (the genlock build defaults the key to 3). A
   re-pin, only if an sp-* input was explicitly set OFF 3, is
   `apply_latency_pins.py --box resolume --host <ip> --pins '{"<live sp-* name>":3, ...}' --execute`.

4. **VERIFY the CG chain FIFO lock** end-to-end (#1300). cg OBS `genlock-fifo audit 'sp-*_video'`
   must reach `locked=1` once SongPlayer stamps real wall-clock timecodes (until the SongPlayer
   sender half — songplayer#151 — ships, the sp-* inputs sit in the count gate: record THAT as the
   explicit BEFORE baseline, it is expected, not a failure). Then strih's `cg` input must lock:

   ```
   # cg-obs hop: paste the win-resolume MCP FileRead of the RESOLUME-SNV OBS log to a file, then:
   CG_CHAIN_CG_OBS_LOG=<that file> \
   CG_CHAIN_STRIH_CMD='<byte-safe ssh strih "gc <obslog> | select -last 4000">' \
   CG_CHAIN_STREAM_CMD='<...stream...>' \
     bash scripts/cg-chain-verify.sh --hops "cg-obs strih stream"
   ```

   exit 0 = every hop PASS (`locked=1`, `ts_head_skew_ms` steady, `dropped_due`/`underruns`/`relocks`
   flat over ≥ 1 h). Exit 3 = a hop FAIL or an unreadable log.

## Out of scope (own tickets, NOT this one)

Watchdog/bundle-state/fleet registration → issue 1296 (merged — `scripts/lib/obs-fleet.sh`);
dantesync NTP/phase-slew on this box → issue 1297; the in-OBS lock indicator → issue 1298;
the per-frame pixel contiguity of the CG chain → issue 1301.
