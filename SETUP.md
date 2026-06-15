# Camera-Box Device Setup Guide

This guide documents how to set up a new camera-box device (CAM1, CAM2, etc.).

## Overview

**Process for creating a new camera device:**

1. Write clean Ubuntu master image to USB
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

## Step 1: Write Master Image to USB

The master image is a clean Ubuntu Server with only SSH configured.

```bash
# On dev machine, connect USB drive
cd /home/newlevel/devel/camera-box

# Check USB device name
lsblk -d -o NAME,SIZE,MODEL | grep -E '^sd'

# Write image (replace /dev/sdX with your USB device)
sudo ./scripts/write-image.sh /home/newlevel/Downloads/ubuntu-usb-master.img /dev/sdX

# Script automatically unmounts - safe to remove when done
```

## Step 2: Boot New Device

1. Insert USB into new camera PC
2. Power on and boot from USB (may need BIOS/UEFI boot menu)
3. **Wait ~5 minutes for first boot** - the master image takes longer on first boot
4. Device will get DHCP IP initially

**SSH Connection Details (master image):**
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

### Measured baseline (evidence, 2026-06-15, read-only)

Steady-state absolute NTP offset reported by each node's DanteSync, all PTP NANO-locked:

| Node | Absolute offset | State |
|------|-----------------|-------|
| cam1 (10.77.9.61) | ~+85..+333 µs | PTP NANO lock |
| cam2 (10.77.9.62) | ~+300..+351 µs | PTP NANO lock |
| cam4 (10.77.9.64) | ~−40 µs | PTP NANO lock |
| strih (master→GM 10.77.9.184) | ~+1249 µs | PTP NANO lock, settled |

(cam3 was powered down at measurement time — enroll/verify it when it is back online.)

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
# 1. Write image (on dev machine)
sudo ./scripts/write-image.sh /home/newlevel/Downloads/ubuntu-usb-master.img /dev/sdb

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
