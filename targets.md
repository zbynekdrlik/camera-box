# Deployment Targets

## Windows Targets (DanteSync)

| Host | IP Address | Status | Notes |
|------|------------|--------|-------|
| stagebox1 | 10.77.9.237 | Active | SSH: newlevel/newlevel |
| strih | 10.77.9.202 | Active | SSH: newlevel/newlevel |
| ableton-foh | 10.77.9.230 | Active | SSH: master/master |
| mbc | 10.77.7.232 | Active | SSH: newlevel/newlevel — Master Broadcast Console: Ableton DAW doing the FINAL stream audio mastering; plugin latency deliberately aligned to EXACTLY 1s (the reason stream PGM's genlock hold ≈ 1000 − camera-path ≈ 925ms); the A/V-sync mic feeds INTO an Ableton channel here (was found muted 2026-07-12 — check this channel first when the measurement audio is silent). IP MOVED 2026-07-13: was 10.77.9.232 (a ping to the OLD IP falsely reads as "box off" — it is normally ON); `mbc.lan` resolves correctly, verify with `getent hosts mbc.lan` before declaring it down |
| stream | 10.77.9.204 | Active | SSH: newlevel/newlevel |
| bridge | 10.77.9.201 | Active | SSH: newlevel/newlevel |
| resolume-snv | (per `getent hosts resolume.lan`) | Maintenance target (traveling) | SSH: newlevel/newlevel — RESOLUME-SNV, the CG / graphics PC (Resolume Arena → strih via Spout/NDI, plus a `cg-obs`). DanteSync **maintenance** target (#811): brought under the fleet clock-discipline + version-parity umbrella so its NDI feeds (`cg` / `NDI obs hudba` / `RESOLUME-SNV (cg-obs)`) stop drifting (#800 saw ~+65 ms/h all day, no dantesync). **Not in the E2E `[0/8]` version gate** — it is not a measured source in the cam→strih→stream recording path, and it is a traveling box often powered off/away between events; roll + version-check it as a STANDALONE maintenance step (`.claude/skills/ops`). **IP is NOT pinned here:** `resolume.lan` currently resolves to `10.77.9.201` — the SAME IP `bridge` lists above (an event-LAN DHCP drift/collision) — so always `getent hosts resolume.lan` and confirm the box identity (DHCP lease / its OBS profile name, never "the shared OBS-WS password worked" — `.claude/rules/rig-state-inspection.md` §2) before deploying |
| iem | 10.77.9.231 | Active | SSH: iem/iem |
| songs | 10.77.9.212 | Active | SSH: newlevel/newlevel |
| piano | 10.77.9.236 | Offline | SSH: newlevel/newlevel |

### RESOLUME-SNV — dev1 fleet membership + the `:8899` on-box install (#1296)

RESOLUME-SNV is registered in the ONE declared managed-OBS-box list
`scripts/lib/obs-fleet.sh` (`OBS_FLEET`, class `windows-genlock`, host `resolume.lan`, home-check
`traveling`), from which the dev1-side watchdogs derive their box rosters
(`.claude/rules/obs-fleet-list.md`). Where it is watched:

- **bundle-state** (`:8899` health + auto-restart) and **obs-liveness** (`render_advanced`) — carries
  the facet; obs-liveness polls it only while `obs_fleet_is_home resolume` (OBS-WS `:4455` answers).
- **network-reach** — REPORT-ONLY by default (a traveling box's absence is normal), PROMOTED to a
  paging node only while it is home (`obs_fleet_is_home`).
- **NOT** audio-lag / av-step / vb-matrix (no mbc audio, no VB-Matrix on the CG box).
- **version-integrity-gate** — surfaced REPORT-ONLY (`--win-state-report-only resolume=…`); it stays
  OUT of the `[0/8]` blocking set (a traveling box, not a measured source). **rig-health-audit** —
  genlock-build + bundle-state facets rendered when present, rate-EXEMPT (#787).

**SUPERVISOR on-box install checklist — the `:8899` BundleStateServer on RESOLUME-SNV** (mirrors the
strih/stream install in `.claude/skills/genlock` / `.claude/skills/ops`; a rig step, NOT a code-PR
step). Run it when the box is home + its identity is CONFIRMED (`getent hosts resolume.lan` + its OBS
profile name, `.claude/rules/rig-state-inspection.md` §2 — it currently resolves to `10.77.9.201`,
which collides with `bridge`):

1. Deploy `C:\ProgramData\camera-box\{bundle-state-server.py, bundle_state_gather.py, obs_phase2.py,
   run-bundle-state-server.ps1}` by having the box `Invoke-WebRequest` each raw file at a pinned
   commit SHA (`https://raw.githubusercontent.com/zbynekdrlik/camera-box/<sha>/scripts/<file>`) — never
   transfer file content through an agent's context. Keep `run-bundle-state-server.ps1` pure ASCII
   (`grep -nP '[^\x00-\x7F]'` before deploy — the em-dash parse trap).
2. `FileWrite` the OBS-WS password to `C:\ProgramData\camera-box\obs-ws-password.txt` (one line, the
   local `rig-obs-ws-credentials` memory value) — never fetched from GitHub, never committed.
3. Register the Scheduled Task `BundleStateServer` (ONSTART trigger, InteractiveToken as `newlevel`,
   session-agnostic — mirrors the existing `StartOBS` / the strih/stream `BundleStateServer` task),
   whose action runs `run-bundle-state-server.ps1` (the restart-loop supervisor).
4. Verify from dev1: `curl http://resolume.lan:8899/bundle-state.json` returns the facets (OBS
   identity / `obs_process_count` / `genlock_build_sha`), and a forced `obs64` kill pages via the
   existing obs-liveness / bundle-state path within 2 passes + the auto-restart brings `:8899` back.

**dantesync on RESOLUME-SNV (issue 1297).** The box runs dantesync (1.8.53 = fleet pin) and answers
`:8898/status` whenever up. Config lives at `C:\ProgramData\dantesync\config.json`; `ntp_server`
pointed at `strih.lan` (unresolvable while strih is off) so NTP phase discipline is dead
(`ntp_failed=true`, 0 samples, a −14 ms phase walk) and `system.phase_slew` was ABSENT (the box
STEPS, not slews). The on-box fix = the SUPERVISOR runbook in `.claude/rules/resolume-dantesync.md`
(emit the PS apply program with `scripts/dantesync_config_patch.py --emit-apply`, run it via the
`win-resolume` MCP, read back `:8898/status`); the NTP-failover feature is zbynekdrlik/dantesync#111.
Version-parity + lock/NTP/phase are checked STANDALONE (never `[0/8]`): `scripts/dantesync-version-gate.sh
--win "resolume=newlevel@<ip>"` and `scripts/dantesync-maintenance-gate.sh --box resolume` (REPORT-ONLY,
SKIP while away). A fleet roll adds it via `scripts/dantesync-fleet-upgrade.sh --win` using
`dantesync_resolume_win_spec "$(getent hosts resolume.lan | awk 'NR==1{print $1}')"`.

## Camera Targets (camera-box)

| Device | IP Address | Status | Notes |
|--------|------------|--------|-------|
| CAM1 | 10.77.9.61 | Active | SSH: root/newlevel (ro-root appliance like the whole fleet — deploy normally; the old "READ-ONLY reference" note meant the ro filesystem, NOT "skip deploys") |
| CAM2 | 10.77.9.62 | Active | SSH: root/newlevel |
| CAM3 | 10.77.9.63 | Active | SSH: root/newlevel |
| CAM4 | 10.77.9.64 | Active | SSH: root/newlevel |
| CAM5 | 10.77.9.65 | Active | SSH: root/newlevel; fleet grew 4->6 (#451), fully provisioned |
| CAM6 | 10.77.9.66 | Active | SSH: root/newlevel; fleet grew 4->6 (#451), fully provisioned |
| CAM7 | 10.77.9.67 | Active | SSH: root/newlevel; BUILT 2026-07-14 (M.2 internal disk, setup-device.sh CAM7, verify-device ALL CLEAR 21/21); NOT yet wired into strih OBS (no 'NDI cam7' input/scene) nor CAMERA_SET/sweep — integration is the follow-up |

### Grabber cards — LIVE fleet assignment (verified 2026-07-12 via V4L2 `card` string, #728)

**A physical card can move between boxes without the hostname changing — this table can drift.**
`grabber_model_for_hostname` in `src/capture_rate_health.rs` is the OPERATIONAL/historical
convention only; the code no longer trusts it blindly — `capture_rate_health::resolve_grabber_model`
resolves the ACTUAL runtime card via `capture::query_card_name` (VIDIOC_QUERYCAP) at every boot and
prefers that over this table whenever it's available. Re-verify with
`v4l2-ctl -d /dev/videoN --info | grep 'Card type'` (or read `/sys/class/video4linux/videoN/name`)
before trusting this table for anything operational.

| Device | Grabber model | Capture node | Notes |
|--------|---------------|--------------|-------|
| CAM1 | Elgato 4K S | /dev/video0 | Swapped in 2026-07-12 (was ShadowCast 2, #728); capture node RENUMBERED from /dev/video1 -> /dev/video0 by 2026-07-13 (#744) — exactly the "can drift" case above; `scripts/recording-e2e.sh` no longer hardcodes a node, see `scripts/lib/v4l2-neutral.sh` |
| CAM2 | Elgato Cam Link 4K (owner swap 2026-09-03; was ShadowCast 2 — the hostname→model table is stale, runtime detection is authoritative, #728/#729) | /dev/video0 | Swapped |
| CAM3 | ShadowCast 2 | /dev/video0 | Unchanged |
| CAM4 | NZXT Signal HD60 | /dev/video0 | Unchanged, no V4L2 picture controls exposed |
| CAM5 | ShadowCast 2 | /dev/video0 | Swapped in 2026-07-12 (was Elgato 4K S, #728) — this is the SAME physical unit that used to sit in CAM1 |
| CAM6 | Elgato 4K S | /dev/video0 | Renumbered from /dev/video1 (#744, verified live 2026-07-13, same class of drift as CAM1) |
| CAM7 | Elgato 4K S | /dev/video0 | New box built 2026-07-14 |

## Linux OBS Targets (camera-box, #458)

| Host | IP Address | Status | Notes |
|------|------------|--------|-------|
| imag-nb | 10.77.9.182 | Active | SSH: newlevel/newlevel (sudo needs pw); 60fps IMAG OBS box, genlock hot-swap over PPA base (#460); dev1 also has headless key-based SSH (`~/.ssh/id_ed25519`) for `scripts/drift-guard.sh --check-imag` (#541), installed by `setup-imag.sh` step 19 |

## Important Notes

- **Always use IP addresses**, not `.lan` hostnames (DNS may not resolve)
- Camera devices use `mount -o remount,rw /` (not `rw-mode` command)
- Windows targets use `newlevel` user (not `root`)
- imag-nb (Linux OBS) also uses `newlevel` user (live-verified SSH login), same as most Windows targets
