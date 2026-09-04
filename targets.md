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
