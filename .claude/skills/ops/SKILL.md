---
name: ops
description: >
  Camera-box rig operations — DanteSync cluster clock, device deployment, and rig recovery.
  Load when: deploying camera-box to CAM1-4, diagnosing time-sync issues, checking DanteSync
  status, recovering wedged cameras, or any infra work on the cam/strih/stream cluster.
---

# Camera-box Ops

## DanteSync Cluster Clock

DanteSync (`~/devel/dantetimesync`) is the cluster wall-clock basis for genlock (#8/#42).

**Topology:** strih.lan (10.77.9.202) = NTP master (`ntp_server_mode`, stratum 3).
All nodes sync to strih: stream, cam2 (`--ntp-server strih.lan`), dev1.

**PTP (primary):** grandmaster 10.77.9.184 — all nodes LOCK to µs-grade parity.
**NTP fallback** (when GM absent): ±0.3-1 ms stepping sawtooth — genlock must degrade gracefully.

**Verified node status (2026-06-15 — do NOT re-doubt):**

| Node | IP | Version | Mode | Measured offset to strih |
|---|---|---|---|---|
| cam1 | 10.77.9.61 | v1.8.11 | LOCK | 0.50 ms |
| cam2 | 10.77.9.62 | v1.8.11 | NANO | 0.23 ms |
| cam3 | 10.77.9.63 | v1.8.16 | LOCK | 0.07 ms |
| cam4 | 10.77.9.64 | v1.8.11 | — | — |
| dev1 | — | v1.8.16 | LOCK | 0.16 ms |

`systemctl is-active dantesync` = active+enabled on all.

**HARD RULE:** DanteSync OWNS the clock on cam boxes AND dev1. NEVER install/enable
chrony / ptp4l / systemd-timesyncd / any other NTP/PTP tool, and NEVER run
`timedatectl set-ntp true` (DanteSync's `install.sh` disables them).

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

## Rig Recovery Policy

The camera-box rig (cam1-4, strih.lan, stream.lan) is a **dev rig, not production**.
Fix infra breakage autonomously — including host reboot — without asking.

When the stream box GPU wedges (`DXGI_ERROR_DEVICE_REMOVED` / TDR on the RTX 4060),
an OBS restart alone often does NOT clear it; a full reboot does.
User directive: "Nie sme v produkcii — mas to opravit ci uz tak alebo onak kludne aj
restartom pc a pokracovat vo vyvoji!!!"

Do NOT gate, classify, or ask before recovering dev rig infra.
The user interrupts Claude when the rig is needed live.

## #265/#266 — NDI-receive STUCK state + the detect+alert watchdog

THE STUCK STATE (intermittent, after long uptime): the cam→OBS NDI receive on a broadcast box
(strih/stream) collapses to ~10 fps (genlock STARVED, huge underruns) while `dantesync.exe` pegs
~1 of 16 cores. Restarting the dantesync service does NOT fix it — only a full PC reboot does
(2026-06-26: cam1→strih ~10→30.2 fps, underruns 290k→0 after reboot). Recovery = reboot (#265).

`scripts/stuck-watchdog.py` (+ `tests/python/test_stuck_watchdog.py`, run with
`PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest tests/python`) is the DETECT+ALERT check. It
parses the genlock-fifo audit line (`received/consumed/underruns/overruns`, ~5 s cadence) + a
dantesync CPU% and Discord-alerts via `airuleset notify`. Key classification gotchas (#266 review):

- **STARVED (alert, even UNNAMED):** received-fps < floor (incl. frozen ~0) with **underruns
  climbing on a drained FIFO, overruns flat**. A frozen-0-fps broadcast input in default scan mode
  is the WORST case — never skip it as "idle".
- **`overruns` is the idle discriminator:** a PARKED source (added, not on program) RECEIVES frames
  it never consumes → its FIFO OVERFLOWS → **overruns dominate the underrun churn** → alive, not
  starved → skip (#70). A source with NO activity (nothing arriving, nothing starving) is dormant →
  skip.
- **NAMED (`--source`) inputs:** always alert below floor; a declared input absent / <2 audit lines
  = VANISHED → alert (don't let `last_two_per_source` silently drop it).
- Delivery FAILS LOUDLY (exit 3) if `airuleset notify` returns non-zero; missing NDI audit data in
  default mode stays exit 2 even with `--dantesync-cpu`; counter deltas are normalized per dt.
