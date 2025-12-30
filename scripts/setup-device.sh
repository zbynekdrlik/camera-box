#!/bin/bash
#
# Camera-Box Device Setup Script
# Sets up a clean Ubuntu installation as a camera-box appliance
#
# Usage: ./setup-device.sh DEVICE_NAME DEVICE_IP VBAN_STREAM
# Example: ./setup-device.sh CAM2 10.77.9.62 cam2
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# GitHub repo for downloading binary
GITHUB_REPO="zbynekdrlik/camera-box"
BINARY_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/camera-box-linux-amd64.tar.gz"

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}Error: Please run as root${NC}"
    exit 1
fi

# Parse arguments
DEVICE_NAME="${1:-}"
DEVICE_IP="${2:-}"
VBAN_STREAM="${3:-}"

if [ -z "$DEVICE_NAME" ] || [ -z "$DEVICE_IP" ] || [ -z "$VBAN_STREAM" ]; then
    echo -e "${RED}Usage: $0 DEVICE_NAME DEVICE_IP VBAN_STREAM${NC}"
    echo ""
    echo "Examples:"
    echo "  $0 CAM1 10.77.9.61 cam1"
    echo "  $0 CAM2 10.77.9.62 cam2"
    exit 1
fi

TOTAL_STEPS=16

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Camera-Box Device Setup${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Device Name:  ${YELLOW}${DEVICE_NAME}${NC}"
echo -e "Device IP:    ${YELLOW}${DEVICE_IP}${NC}"
echo -e "VBAN Stream:  ${YELLOW}${VBAN_STREAM}${NC}"
echo ""

# Confirm
read -p "Continue with setup? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 1
fi

# =============================================================================
# STEP 1: Set hostname
# =============================================================================
echo ""
echo -e "${GREEN}[1/${TOTAL_STEPS}] Setting hostname...${NC}"
echo "$DEVICE_NAME" > /etc/hostname
hostnamectl set-hostname "$DEVICE_NAME"
sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_NAME/" /etc/hosts
echo "  Hostname set to: $DEVICE_NAME"

# =============================================================================
# STEP 2: Configure static IP
# =============================================================================
echo ""
echo -e "${GREEN}[2/${TOTAL_STEPS}] Configuring static IP...${NC}"
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
chmod 600 /etc/netplan/01-netcfg.yaml
rm -f /etc/netplan/50-cloud-init.yaml
echo "  Static IP configured: $DEVICE_IP"

# =============================================================================
# STEP 3: Install camera-box binary
# =============================================================================
echo ""
echo -e "${GREEN}[3/${TOTAL_STEPS}] Installing camera-box binary...${NC}"
if curl -fsSL "$BINARY_URL" -o /tmp/camera-box.tar.gz; then
    tar -xzf /tmp/camera-box.tar.gz -C /usr/local/bin/
    chmod +x /usr/local/bin/camera-box
    rm -f /tmp/camera-box.tar.gz
    echo "  Binary installed from GitHub (v1.0.0)"
else
    echo -e "  ${YELLOW}Warning: Could not download binary from GitHub${NC}"
    echo "  Please install manually to /usr/local/bin/camera-box"
fi

# =============================================================================
# STEP 4: Install NDI library
# =============================================================================
echo ""
echo -e "${GREEN}[4/${TOTAL_STEPS}] Setting up NDI library...${NC}"
mkdir -p /usr/lib/ndi
# Add NDI library path to ldconfig
echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
# NDI library must be copied from dev machine due to licensing
# This is done separately before running this script:
#   scp /usr/lib/ndi/* root@<DEVICE_IP>:/usr/lib/ndi/
if [ -f /usr/lib/ndi/libndi.so.6 ]; then
    ldconfig
    echo "  NDI library: present and configured"
else
    echo -e "  ${YELLOW}NDI library not found${NC}"
    echo "  Copy from dev machine BEFORE running this script:"
    echo "  scp /usr/lib/ndi/* root@<DEVICE_IP>:/usr/lib/ndi/"
fi

# =============================================================================
# STEP 5: Configure ALSA for USB headset
# =============================================================================
echo ""
echo -e "${GREEN}[5/${TOTAL_STEPS}] Configuring ALSA audio...${NC}"
cat > /etc/asound.conf << 'ALSAEOF'
# Asymmetric config: stereo output, mono input
# USB headset on card 1 (CSCTEK USB Audio and HID)
pcm.!default {
    type asym
    playback.pcm {
        type plug
        slave {
            pcm "hw:1,0"
            channels 2
        }
    }
    capture.pcm {
        type plug
        slave {
            pcm "hw:1,0"
            channels 1
        }
    }
}

ctl.!default {
    type hw
    card 1
}
ALSAEOF
echo "  ALSA config: /etc/asound.conf (USB headset on card 1)"

# =============================================================================
# STEP 6: Create camera-box config
# =============================================================================
echo ""
echo -e "${GREEN}[6/${TOTAL_STEPS}] Creating camera-box config...${NC}"
mkdir -p /etc/camera-box
cat > /etc/camera-box/config.toml << EOF
# Camera-Box Configuration
# Device: ${DEVICE_NAME}
# Generated: $(date -Iseconds)

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
echo "  Config: /etc/camera-box/config.toml"

# =============================================================================
# STEP 7: Create systemd service
# =============================================================================
echo ""
echo -e "${GREEN}[7/${TOTAL_STEPS}] Creating systemd service...${NC}"
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
systemctl daemon-reload
systemctl enable camera-box
echo "  Service created and enabled"

# =============================================================================
# STEP 8: Set binary capabilities
# =============================================================================
echo ""
echo -e "${GREEN}[8/${TOTAL_STEPS}] Setting binary capabilities...${NC}"
if [ -f /usr/local/bin/camera-box ]; then
    setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box
    echo "  Capabilities set (real-time priority, memory lock)"
else
    echo -e "  ${YELLOW}Skipped - binary not installed${NC}"
fi

# =============================================================================
# STEP 9: Disable GRUB timeout (fast boot)
# =============================================================================
echo ""
echo -e "${GREEN}[9/${TOTAL_STEPS}] Disabling GRUB timeout...${NC}"
sed -i 's/GRUB_TIMEOUT=.*/GRUB_TIMEOUT=0/' /etc/default/grub
sed -i 's/GRUB_TIMEOUT_STYLE=.*/GRUB_TIMEOUT_STYLE=hidden/' /etc/default/grub
grep -q "GRUB_TIMEOUT_STYLE" /etc/default/grub || echo "GRUB_TIMEOUT_STYLE=hidden" >> /etc/default/grub
update-grub 2>/dev/null || true
echo "  GRUB timeout: 0 seconds"

# =============================================================================
# STEP 10: Reduce network wait timeout
# =============================================================================
echo ""
echo -e "${GREEN}[10/${TOTAL_STEPS}] Reducing network wait timeout...${NC}"
mkdir -p /etc/systemd/system/systemd-networkd-wait-online.service.d
cat > /etc/systemd/system/systemd-networkd-wait-online.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/lib/systemd/systemd-networkd-wait-online --timeout=5
EOF
echo "  Network wait timeout: 5 seconds"

# =============================================================================
# STEP 11: Disable power button shutdown
# =============================================================================
echo ""
echo -e "${GREEN}[11/${TOTAL_STEPS}] Disabling power button shutdown...${NC}"
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/disable-power-button.conf << EOF
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF
echo "  Power button: ignored (used for mute toggle)"

# =============================================================================
# STEP 12: Disable all power saving / sleep
# =============================================================================
echo ""
echo -e "${GREEN}[12/${TOTAL_STEPS}] Disabling power saving...${NC}"
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
# Disable CPU frequency scaling (use performance governor)
for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo "performance" > "$cpu" 2>/dev/null || true
done
# Make it persistent
cat > /etc/systemd/system/cpu-performance.service << 'EOF'
[Unit]
Description=Set CPU to performance mode
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/bin/bash -c 'for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > $cpu; done'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable cpu-performance.service 2>/dev/null || true
echo "  Sleep/suspend: disabled"
echo "  CPU governor: performance"

# =============================================================================
# STEP 13: Optimize network for performance
# =============================================================================
echo ""
echo -e "${GREEN}[13/${TOTAL_STEPS}] Optimizing network performance...${NC}"
cat > /etc/sysctl.d/99-network-performance.conf << 'EOF'
# Network performance optimizations for low-latency streaming

# Increase network buffer sizes
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 1048576
net.core.wmem_default = 1048576
net.core.netdev_max_backlog = 5000

# TCP optimizations
net.ipv4.tcp_rmem = 4096 1048576 134217728
net.ipv4.tcp_wmem = 4096 1048576 134217728
net.ipv4.tcp_congestion_control = bbr
net.ipv4.tcp_fastopen = 3

# Reduce latency
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_nodelay = 1

# Disable IPv6 if not needed
net.ipv6.conf.all.disable_ipv6 = 1
net.ipv6.conf.default.disable_ipv6 = 1
EOF
sysctl -p /etc/sysctl.d/99-network-performance.conf 2>/dev/null || true
echo "  Network buffers: optimized"
echo "  TCP congestion: BBR"
echo "  IPv6: disabled"

# =============================================================================
# STEP 14: Disable unnecessary services
# =============================================================================
echo ""
echo -e "${GREEN}[14/${TOTAL_STEPS}] Disabling unnecessary services...${NC}"
# Snap
systemctl disable --now snapd.service snapd.socket snapd.seeded.service 2>/dev/null || true
systemctl mask snapd.service 2>/dev/null || true
# Cloud-init
systemctl disable --now cloud-init.service cloud-init-local.service cloud-config.service cloud-final.service 2>/dev/null || true
touch /etc/cloud/cloud-init.disabled 2>/dev/null || true
# Auto updates
systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer 2>/dev/null || true
# ModemManager (not needed)
systemctl disable --now ModemManager.service 2>/dev/null || true
# Bluetooth (not needed)
systemctl disable --now bluetooth.service 2>/dev/null || true
# Printing (not needed)
systemctl disable --now cups.service cups-browsed.service 2>/dev/null || true
echo "  Disabled: snapd, cloud-init, auto-updates, ModemManager, bluetooth, cups"

# =============================================================================
# STEP 15: Install required packages
# =============================================================================
echo ""
echo -e "${GREEN}[15/${TOTAL_STEPS}] Installing required packages...${NC}"
apt-get update -qq
apt-get install -y -qq avahi-daemon libavahi-client3 v4l-utils alsa-utils 2>/dev/null || true
systemctl enable avahi-daemon
echo "  Installed: avahi-daemon, libavahi-client3, v4l-utils, alsa-utils"

# =============================================================================
# STEP 16: Summary
# =============================================================================
echo ""
echo -e "${GREEN}[16/${TOTAL_STEPS}] Setup Complete!${NC}"
echo "=========================================="
echo ""
echo "Configuration:"
echo "  Hostname:    $DEVICE_NAME"
echo "  IP Address:  $DEVICE_IP"
echo "  VBAN Stream: $VBAN_STREAM"
echo "  NDI Name:    usb"
echo ""
echo "Optimizations applied:"
echo "  - GRUB timeout: 0s"
echo "  - Network wait: 5s"
echo "  - Power button: mute toggle (not shutdown)"
echo "  - Sleep/suspend: disabled"
echo "  - CPU governor: performance"
echo "  - Network: optimized for streaming"
echo "  - Unnecessary services: disabled"
echo ""
if [ ! -f /usr/lib/ndi/libndi.so.6 ]; then
    echo -e "${YELLOW}ACTION REQUIRED:${NC}"
    echo "  Copy NDI library: scp root@10.77.9.61:/usr/lib/ndi/* /usr/lib/ndi/"
    echo ""
fi
echo -e "${YELLOW}Next steps:${NC}"
echo "  1. Apply network config: netplan apply"
echo "  2. Reboot: reboot"
echo ""
echo -e "${GREEN}After reboot, connect via: ssh root@${DEVICE_IP}${NC}"
