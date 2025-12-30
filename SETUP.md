# Camera-Box Device Setup Guide

This guide documents how to set up a new camera-box device (CAM1, CAM2, etc.).

## Quick Start (Image-Based Deployment)

The fastest way to deploy a new camera is using the master USB image.

### Step 1: Write Image to USB

```bash
# From dev machine, with USB drive connected
cd /home/newlevel/devel/camera-box

# Check available USB devices
lsblk -d -o NAME,SIZE,MODEL | grep -E '^sd'

# Write image to USB (replace /dev/sdX with your USB device)
sudo ./scripts/write-image.sh /home/newlevel/Downloads/ubuntu-usb-master.img /dev/sdX

# Script will automatically unmount when done - safe to remove
```

### Step 2: Boot New Device

1. Insert USB into new camera PC
2. Boot from USB (may need to change BIOS boot order)
3. Wait for system to boot (will have CAM1 settings initially)

### Step 3: Configure Device via SSH

```bash
# Get the device IP (it boots with DHCP or CAM1's old IP)
# User provides the IP

# SSH into the new device
ssh root@<DEVICE_IP>   # password: newlevel

# Run setup script with new device settings
./setup-device.sh CAM2 10.77.9.62 cam2

# Reboot to apply
reboot
```

### Step 4: Verify

```bash
# Connect at new IP
ssh root@10.77.9.62

# Check service
systemctl status camera-box
journalctl -u camera-box -f
```

---

## Device-Specific Configuration

Each camera device requires the following unique settings:

| Setting | CAM1 | CAM2 | Location |
|---------|------|------|----------|
| Hostname | `CAM1` | `CAM2` | `/etc/hostname`, `/etc/hosts` |
| Static IP | `10.77.9.61` | `10.77.9.62` | `/etc/netplan/01-netcfg.yaml` |
| VBAN Stream | `cam1` | `cam2` | `/etc/camera-box/config.toml` |
| NDI Name | `usb` | `usb` | `/etc/camera-box/config.toml` (same for all) |

## Network Information

- **Network**: `10.77.8.0/23`
- **Gateway**: `10.77.8.1`
- **DNS**: Uses systemd-resolved (default)

## Setup Steps

### 1. Install Base System

Install Ubuntu Server (minimal) on the device.

### 2. Set Hostname

```bash
# Set hostname (replace CAM1 with CAM2, etc.)
DEVICE_NAME="CAM1"

echo "$DEVICE_NAME" > /etc/hostname
hostnamectl set-hostname "$DEVICE_NAME"

# Update /etc/hosts
sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_NAME/" /etc/hosts
```

### 3. Configure Static IP

Create/edit `/etc/netplan/01-netcfg.yaml`:

```bash
# For CAM1: IP = 10.77.9.61
# For CAM2: IP = 10.77.9.62
DEVICE_IP="10.77.9.61"

cat > /etc/netplan/01-netcfg.yaml << EOF
network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        driver: "*"
      addresses:
        - ${DEVICE_IP}/23
      routes:
        - to: default
          via: 10.77.8.1
      nameservers:
        addresses:
          - 10.77.8.1
EOF

# Apply network configuration
netplan apply
```

### 4. Install camera-box Binary

```bash
# Download latest release
curl -fsSL https://github.com/zbynekdrlik/camera-box/releases/latest/download/camera-box -o /usr/local/bin/camera-box
chmod +x /usr/local/bin/camera-box

# Or copy from build machine
scp /path/to/camera-box root@DEVICE_IP:/usr/local/bin/
```

### 5. Install NDI SDK

```bash
# Create NDI directory
mkdir -p /usr/lib/ndi

# Copy NDI SDK libraries (from another device or download)
# Required files: libndi.so.6
```

### 6. Create Configuration File

```bash
# For CAM1: stream = "cam1"
# For CAM2: stream = "cam2"
VBAN_STREAM="cam1"

mkdir -p /etc/camera-box

cat > /etc/camera-box/config.toml << EOF
# Camera-Box Configuration

# NDI source name (appears on network)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"

# VBAN Intercom Configuration
[intercom]
stream = "${VBAN_STREAM}"
target = "strih.lan"
sample_rate = 48000
channels = 1
EOF
```

### 7. Create Systemd Service

```bash
cat > /etc/systemd/system/camera-box.service << 'EOF'
[Unit]
Description=Camera Box - USB Video Capture to NDI
Documentation=https://github.com/zbynekdrlik/camera-box
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/camera-box --display "STRIH-SNV (interkom)"
Restart=always
RestartSec=3

# Run with real-time priority for low latency
Nice=-10
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

# Environment for NDI SDK
Environment=NDI_RUNTIME_DIR_V6=/usr/lib/ndi

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=camera-box

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadOnlyPaths=/
ReadWritePaths=/dev /sys /run

# Allow access to video devices
SupplementaryGroups=video

[Install]
WantedBy=multi-user.target
EOF

# Enable and start service
systemctl daemon-reload
systemctl enable camera-box
systemctl start camera-box
```

### 8. Set Capabilities (for real-time priority)

```bash
setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box
```

### 9. Verify Setup

```bash
# Check service status
systemctl status camera-box

# Check logs
journalctl -u camera-box -f

# Verify network
ip addr show
ping -c 3 strih.lan
```

## Quick Setup Script

For convenience, here's a one-liner setup script (run as root):

```bash
# Set these variables for each device
DEVICE_NAME="CAM1"      # CAM1, CAM2, etc.
DEVICE_IP="10.77.9.61"  # 10.77.9.61, 10.77.9.62, etc.
VBAN_STREAM="cam1"      # cam1, cam2, etc.

# Then run setup
curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/setup-device.sh | bash -s -- "$DEVICE_NAME" "$DEVICE_IP" "$VBAN_STREAM"
```

## Device Registry

| Device | Hostname | IP Address | VBAN Stream | Status |
|--------|----------|------------|-------------|--------|
| CAM1 | CAM1 | 10.77.9.61 | cam1 | Active |
| CAM2 | CAM2 | 10.77.9.62 | cam2 | Planned |

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
# Check NDI library
ls -la /usr/lib/ndi/
ldd /usr/local/bin/camera-box | grep ndi
```

### Network issues
```bash
ip addr show
ip route
ping 10.77.8.1
```
