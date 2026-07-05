# Camera-Box Device Setup Guide

This guide documents how to set up a new camera-box device (CAM1, CAM2, etc.).

## Overview

**Process for creating a new camera device:**

1. Install Ubuntu onto a USB drive (`scripts/create-usb-linux.sh`)
2. Boot new device from USB
3. SSH into device and run setup script
4. Copy NDI library from CAM1
5. Reboot - device is ready

## Device Registry

| Device | Hostname | IP Address | VBAN Stream | Status |
|--------|----------|------------|-------------|--------|
| CAM1 | CAM1 | 10.77.9.61 | cam1 | Active (READ-ONLY) |
| CAM2 | CAM2 | 10.77.9.62 | cam2 | Active |
| CAM3 | CAM3 | 10.77.9.63 | cam3 | Active |

## Network Configuration

- **Network**: `10.77.8.0/23`
- **Gateway**: `10.77.8.1`
- **DNS**: `10.77.8.1`

---

## Step 1: Install Ubuntu Onto a USB Drive

`scripts/create-usb-linux.sh` is the sole, canonical one-shot installer (#448) — it debootstraps
a fresh Ubuntu 24.04 appliance rootfs (SSH + DHCP only) directly onto the target disk. There is
no separate "master image" file to build first.

```bash
# On dev machine, connect USB drive
cd /home/newlevel/devel/camera-box

# Check USB device name
lsblk -d -o NAME,SIZE,MODEL | grep -E '^sd'

# Install onto the USB drive (replace /dev/sdX with your USB device; refuses /dev/sda for safety)
sudo ./scripts/create-usb-linux.sh /dev/sdX

# Non-interactive (skip the 'type yes' confirmation): add --yes
# Running FROM a box's own live-USB, targeting its internal disk (even /dev/sda): use
# --target-disk /dev/sdX instead of the positional form.
```

## Step 2: Boot New Device

1. Insert USB into new camera PC
2. Power on and boot from USB (may need BIOS/UEFI boot menu)
3. **Wait ~5 minutes for first boot** - the fresh install takes longer on first boot
4. Device will get DHCP IP initially

**SSH Connection Details (fresh install):**
- Username: `root`
- Password: `newlevel`

## Step 3: Get Device IP

The user must provide the device's current DHCP IP address.

**DO NOT scan the network** - ask user for the IP.

## Step 4: Run Setup Script

SSH into the device and run the setup script:

```bash
# From dev machine - copy setup script to device
sshpass -p 'newlevel' scp scripts/setup-device.sh root@<DEVICE_IP>:/root/

# SSH into device
sshpass -p 'newlevel' ssh root@<DEVICE_IP>

# On the device, run setup script:
# Usage: ./setup-device.sh DEVICE_NAME DEVICE_IP VBAN_STREAM
./setup-device.sh CAM2 10.77.9.62 cam2
```

### What the Setup Script Does (15 steps):

1. **Set hostname** - e.g., CAM2
2. **Configure static IP** - e.g., 10.77.9.62
3. **Install camera-box binary** - Downloads from GitHub releases
4. **Setup NDI library directory** - /usr/lib/ndi
5. **Create camera-box config** - /etc/camera-box/config.toml
6. **Create systemd service** - camera-box.service
7. **Set binary capabilities** - Real-time priority, memory lock
8. **Disable GRUB timeout** - Fast boot (0 seconds)
9. **Reduce network wait** - 5 second timeout
10. **Disable power button shutdown** - Used for mute toggle instead
11. **Disable power saving** - No sleep/suspend, CPU performance mode
12. **Optimize network** - Large buffers, BBR congestion, disable IPv6
13. **Disable unnecessary services** - snapd, cloud-init, bluetooth, cups, etc.
14. **Install required packages** - avahi-daemon, v4l-utils, alsa-utils
15. **Summary** - Shows what was configured

## Cluster clock synchronization (genlock prerequisite) — #8

The software genlock in `src/ndi.rs` aligns every camera's NDI send timecode to **absolute
wall-clock frame boundaries** (`wait_for_next_boundary_100ns`). That only produces a *common*
boundary across cameras if their wall clocks are synchronized to the **same reference**. If a
node's clock is offset, every node computes a *different* boundary and the cameras are silently
NOT genlocked. So cluster clock sync is a hard prerequisite, not an optimization.

### Mechanism (the decision)

- **DanteSync** is the chosen mechanism (chosen over plain chrony/NTP because the broadcast rig
  needs sub-ms, PTP-grade alignment, and DanteSync already disciplines the Windows OBS boxes).
- **Master / reference clock: strih = `10.77.9.202`** (the DanteTime master; NTP anchor + PTP
  fine servo). The setup scripts use the **IP, not `strih.lan`**: per `CLAUDE.md`/`targets.md`,
  `.lan` DNS may not resolve on a freshly-provisioned read-only-rootfs camera, and a failed
  resolve makes dantesync fall back to its public-pool default and silently desync.
- Every Linux camera runs `dantesync --ntp-server 10.77.9.202` as an enabled systemd service,
  written by `scripts/setup.sh` / `scripts/setup-device.sh` so it survives the read-only rootfs
  and a reboot. **The `--ntp-server 10.77.9.202` arg is essential** — a bare `dantesync` defaults
  to a *public* NTP pool (e.g. `time.cloudflare.com`), which would discipline the camera to a
  clock *different* from the rest of the cluster and break genlock. Do not hand-edit the unit; fix
  the setup script (Script Failure Policy).
- The Windows OBS boxes (strih = master, stream) run DanteSync too, configured on those hosts.

### Measured baseline (evidence, 2026-06-15, read-only) — full cluster cam1-4

Steady-state absolute NTP offset reported by each node's DanteSync, all PTP NANO-locked,
captured by `scripts/clock-offset-guard.sh` from dev1:

| Node | Absolute offset | State |
|------|-----------------|-------|
| cam1 (10.77.9.61) | ~+21..+66 µs | PTP NANO lock (drift within ±1 µs/s) |
| cam2 (10.77.9.62) | ~+371..+382 µs | PTP NANO lock (drift within ±1 µs/s) |
| cam3 (10.77.9.63) | ~+14..+24 µs | PTP NANO lock (drift within ±1 µs/s) |
| cam4 (10.77.9.64) | ~0..+9 µs | PTP NANO lock (drift within ±1 µs/s) |
| strih (master→GM 10.77.9.184) | ~+1249 µs | PTP NANO lock, settled |

All four cameras are enrolled, NTP-disciplined to master `10.77.9.202` and PTP NANO-locked to
the same grandmaster (`10.77.9.184`) with sub-µs/s drift. Full-cluster guard pass (cam3 in
read-only rootfs, production state):

```
== clock-offset-guard (#8): bound 2000 us (|offset| must stay within) ==
   master = strih (DanteSync NTP anchor + PTP servo); frame period @60fps = 16667 us
  cam1           OK       (offset 21 us, |21| <= 2000)
  cam2           OK       (offset 382 us, |382| <= 2000)
  cam3           OK       (offset 24 us, |24| <= 2000)
  cam4           OK       (offset 2 us, |2| <= 2000)
ALL CLEAR — 4 node(s) within the 2000 us offset bound. Genlock clock assumption holds.
```

**cam3 enrollment note (2026-06-15):** cam3 came online running a stale *bare* `dantesync` unit
(`ExecStart=/usr/local/bin/dantesync`, dantesync 1.8.2) — its NTP path was failing (defaulting to
the public pool it could not reach), so it had no readable absolute offset even though PTP held it
NANO-locked. Re-enrolled to the cluster standard (latest dantesync binary + the
`--ntp-server 10.77.9.202` unit produced by `scripts/setup.sh`'s `install_dantesync`); NTP
immediately converged (−24/+4/+11/+24 µs) and the unit is `enabled` so it survives reboot + the
read-only-rootfs remount cycle. No script change was needed — the setup path already writes the
correct unit; cam3 just predated the fix.

### Offset bound + the regression guard

- **Bound: ±2000 µs (2 ms).** Rationale: the 60 fps frame period is 16.7 ms (16667 µs); a clock
  offset that large would put a camera a whole frame off the common genlock boundary, and the
  unsynced failure mode is tens-to-hundreds of ms. **2 ms is ~8× under the frame period** (so any
  boundary divergence stays well within a frame) yet comfortably **above** the legitimate
  steady-state offsets above (notably strih's ~1.25 ms master-to-grandmaster offset), so the
  guard does not false-positive on a healthy cluster while still catching real drift.
- **Regression check: `scripts/clock-offset-guard.sh`** queries each reachable camera's DanteSync
  offset over SSH and exits non-zero if any node exceeds the bound (or is unreachable / unknown —
  never a silent pass). Run it from dev1:

  ```bash
  scripts/clock-offset-guard.sh                 # default: cam1-4, ±2000 µs bound
  scripts/clock-offset-guard.sh --bound-us 1000 # tighter bound
  ```

  Exit codes: `0` all within bound, `20` drift, `11` a node unreachable/unknown, `1` usage error.
  The Windows OBS boxes report the same `ntp_offset_us` signal as JSON on `\\.\pipe\dantesync`,
  parsed read-only via the win-* MCP tools (the guard's `offset_us_from_pipe_json` is the shared
  comparator). The parsing + threshold logic is unit-tested in `tests/clock_offset_guard.rs`.

### Genlock boundary agreement across cameras (acceptance evidence)

`wait_for_next_boundary_100ns(fps)` (`src/ndi.rs`) computes each camera's next frame boundary
**purely from that camera's own wall clock** (`get_wall_clock_100ns` → `(now/second)*second +
frame_n*10_000_000/fps`). The boundary is therefore a deterministic function of the wall clock
alone: **two cameras whose wall clocks agree to within Δ compute the same absolute frame boundary
to within that same Δ.** There is no other input (no per-node phase term, no random jitter in the
boundary math), so the cross-camera boundary spread is bounded by — and equal to — the cross-camera
clock spread.

This means the boundary agreement is established directly by the offset evidence above (no separate
fabricated frame-tap measurement is claimed here — a cross-camera NDI tap is Phase-2/#7 work that
does not yet exist):

- **Before** (cam3 on the stale bare unit, NTP failing): cam3 had no shared wall-time anchor on
  the NTP path; only PTP held it. The genlock boundary cam3 computed from its wall clock could not
  be certified against the cluster (offset UNKNOWN — guard exit 11). Cross-camera boundary
  agreement was unverifiable for cam3.
- **After** (cam3 re-enrolled to master `10.77.9.202`): the four cameras' wall clocks agree within
  a measured spread of **max 380 µs** (cam2 +382 µs to cam4 +2 µs), all PTP NANO-locked. By the
  argument above, their genlock frame boundaries therefore agree to within **≤ 380 µs** — about
  **44× tighter than the 16.7 ms (60 fps) frame period** (and the worst single-node offset, cam2's
  382 µs against UTC, is itself ~44× under one frame). The cluster's wall-clock genlock boundary
  divergence is well inside one frame on every camera; the genlock clock assumption stated in
  `src/ndi.rs:62-65` holds across cam1-4.

The continuous guard (`scripts/clock-offset-guard.sh`, exit non-zero if any node exceeds ±2 ms)
keeps this bound — and therefore the boundary agreement — from silently regressing.

## Step 5: Copy NDI Library

NDI library cannot be distributed - must copy from existing device:

```bash
# On the new device:
scp root@10.77.9.61:/usr/lib/ndi/* /usr/lib/ndi/
```

## Step 6: Apply Network and Reboot

```bash
# On the device:
netplan apply
reboot
```

## Step 7: Verify

```bash
# Connect at new static IP
sshpass -p 'newlevel' ssh root@10.77.9.62

# Check service status
systemctl status camera-box

# Watch logs
journalctl -u camera-box -f
```

---

## Quick Reference Commands

### For CAM2 Setup:
```bash
# 1. Install onto USB (on dev machine)
sudo ./scripts/create-usb-linux.sh /dev/sdb

# 2. After boot, user provides IP (e.g., 10.77.8.164)

# 3. Copy and run setup script
sshpass -p 'newlevel' scp scripts/setup-device.sh root@10.77.8.164:/root/
sshpass -p 'newlevel' ssh root@10.77.8.164 "./setup-device.sh CAM2 10.77.9.62 cam2"

# 4. Copy NDI library
sshpass -p 'newlevel' ssh root@10.77.8.164 "scp root@10.77.9.61:/usr/lib/ndi/* /usr/lib/ndi/"

# 5. Apply network and reboot
sshpass -p 'newlevel' ssh root@10.77.8.164 "netplan apply && reboot"

# 6. Verify at new IP
sshpass -p 'newlevel' ssh root@10.77.9.62 "systemctl status camera-box"
```

---

## Troubleshooting

### Service won't start
```bash
journalctl -u camera-box -n 50 --no-pager
```

### No video capture
```bash
v4l2-ctl --list-devices
ls -la /dev/video*
```

### No NDI output
```bash
ls -la /usr/lib/ndi/
ldd /usr/local/bin/camera-box | grep ndi
```

### Network issues
```bash
ip addr show
ip route
ping 10.77.8.1
```

### Check boot time
```bash
systemd-analyze
systemd-analyze blame | head -20
```

---

## Files Reference

| File | Purpose |
|------|---------|
| `/etc/hostname` | Device hostname |
| `/etc/netplan/01-netcfg.yaml` | Static IP configuration |
| `/etc/camera-box/config.toml` | Camera-box app config |
| `/etc/systemd/system/camera-box.service` | Systemd service |
| `/usr/local/bin/camera-box` | Application binary |
| `/usr/lib/ndi/libndi.so.6` | NDI library |
| `/etc/default/grub` | GRUB timeout settings |
| `/etc/sysctl.d/99-network-performance.conf` | Network optimizations |
| `/etc/systemd/logind.conf.d/disable-power-button.conf` | Power button config |

---

## Deploying Updates to Existing Cameras

**IMPORTANT:** Use IP addresses, not `.lan` hostnames (DNS may not resolve).

```bash
# Build release on dev machine
cargo build --release

# Deploy to a camera (replace X with camera number: 1, 2, 3, or 4)
sshpass -p 'newlevel' ssh root@10.77.9.6X "mount -o remount,rw / && systemctl stop camera-box"
sshpass -p 'newlevel' scp target/release/camera-box root@10.77.9.6X:/usr/local/bin/
sshpass -p 'newlevel' ssh root@10.77.9.6X "systemctl start camera-box && mount -o remount,ro / 2>/dev/null; true"
```

**Notes:**
- `rw-mode`/`ro-mode` scripts may not exist on all devices - use `mount -o remount,rw /` instead
- The `mount -o remount,ro` may show "mount point is busy" warning - this is harmless
- Password for all devices: `newlevel`

---

## Important Notes

- **CAM1 is READ-ONLY** - Do not modify CAM1, it's the production reference
- **Never scan network** - Always ask user for device IP
- **Master image** = Clean Ubuntu + SSH only (NOT a clone of CAM1)
- **Setup script** = Does ALL configuration (installs apps, optimizes system)
- **NDI library** = Must be copied manually (licensing)

---

## Boot Hardening — "it must be impossible to brick a cam box again" (#295)

Two cam boxes (CAM3 + CAM4) were bricked when an `update-grub` defaulted to an auto-installed
`6.8.0-124` kernel that had **no generated initrd** → the kernel could not mount root. The trigger
chain: an active `unattended-upgrades` auto-installed a new kernel; a full 100M `/var/cache` tmpfs
broke apt with `ENOSPC` so the initrd was never generated; a later `update-grub` happily made that
initrd-less kernel the default boot entry.

The provisioning scripts (`scripts/setup.sh` and `scripts/setup-device.sh`) now make this
**impossible to recreate on a re-provision** — never a one-off live edit (live grub edits are what
bricked the boxes). What they do, enforced by `tests/appliance_boot_hardening.rs`:

1. **Pin the kernel** — `apt-mark hold linux-image-generic linux-headers-generic linux-generic`.
   An appliance must never silently gain a new kernel.
2. **Disable automatic upgrades** — `/etc/apt/apt.conf.d/20auto-upgrades` sets
   `APT::Periodic::Unattended-Upgrade "0"` (plus a kernel blacklist and the masked
   `unattended-upgrades.service`). This is *how* the bad kernel auto-installed.
3. **Guarantee an initrd for every kernel before grub** — provisioning runs
   `update-initramfs -c -k <ver>` for any kernel missing one *before* `update-grub`, and installs
   `/etc/kernel/postinst.d/zz-camera-box-initrd-guarantee` so any future kernel install regenerates a
   missing initrd before grub's own hook.
4. **Pin a safe grub default** — `GRUB_DEFAULT=saved` + `grub-set-default` to the running known-good
   kernel, with a guard that validates the generated default entry references both a kernel image AND
   an initrd (and aborts loudly otherwise — never ship a brickable default).
5. **Size `/var/cache` ≥512M uniformly** — so apt can never `ENOSPC` and leave a kernel without its
   initrd. (Drift was 100M on some boxes, 500M on others.)

### Long-term target — the read-only-root + overlay image (`scripts/build-image.sh`)

The deployed boxes were provisioned manually with a **read-write root**, so they are exposed to
power-loss corruption and to the brick above. `scripts/build-image.sh` already builds the durable
appliance image: a **read-only root filesystem** with an **overlay** partition for writes (and
tmpfs for `/var/log`, `/tmp`, …), with the kernel pinned and `GRUB_DEFAULT=saved`. The long-term
plan is to re-image the fleet onto that ro-root + overlay image. That live re-image is an
**operational step (tracked separately, #301)** — it is NOT done by this hardening change; this
change makes the *provisioning* safe so the brick cannot recur whichever path is used.
