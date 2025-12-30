#!/bin/bash
#
# Camera-Box Device Setup Script
# Usage: ./setup-device.sh DEVICE_NAME DEVICE_IP VBAN_STREAM
# Example: ./setup-device.sh CAM1 10.77.9.61 cam1
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

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

echo ""
echo -e "${GREEN}[1/7] Setting hostname...${NC}"
echo "$DEVICE_NAME" > /etc/hostname
hostnamectl set-hostname "$DEVICE_NAME"
sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_NAME/" /etc/hosts
echo "  Hostname set to: $DEVICE_NAME"

echo ""
echo -e "${GREEN}[2/7] Configuring static IP...${NC}"
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
# Remove cloud-init config if exists
rm -f /etc/netplan/50-cloud-init.yaml
echo "  Static IP configured: $DEVICE_IP"

echo ""
echo -e "${GREEN}[3/7] Creating camera-box config...${NC}"
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
echo "  Config created: /etc/camera-box/config.toml"

echo ""
echo -e "${GREEN}[4/7] Creating systemd service...${NC}"
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

echo ""
echo -e "${GREEN}[5/7] Setting capabilities...${NC}"
if [ -f /usr/local/bin/camera-box ]; then
    setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box
    echo "  Capabilities set on binary"
else
    echo -e "  ${YELLOW}Warning: Binary not found at /usr/local/bin/camera-box${NC}"
    echo "  Please install the binary first"
fi

echo ""
echo -e "${GREEN}[6/7] Creating NDI directory...${NC}"
mkdir -p /usr/lib/ndi
echo "  NDI directory: /usr/lib/ndi"
if [ ! -f /usr/lib/ndi/libndi.so.6 ]; then
    echo -e "  ${YELLOW}Warning: NDI library not found${NC}"
    echo "  Please copy libndi.so.6 to /usr/lib/ndi/"
fi

echo ""
echo -e "${GREEN}[7/7] Summary${NC}"
echo "=========================================="
echo "Device setup complete!"
echo ""
echo "Configuration:"
echo "  Hostname:    $DEVICE_NAME"
echo "  IP Address:  $DEVICE_IP"
echo "  VBAN Stream: $VBAN_STREAM"
echo "  NDI Name:    usb"
echo ""
echo "Files created:"
echo "  /etc/hostname"
echo "  /etc/netplan/01-netcfg.yaml"
echo "  /etc/camera-box/config.toml"
echo "  /etc/systemd/system/camera-box.service"
echo ""
echo -e "${YELLOW}Next steps:${NC}"
echo "  1. Install camera-box binary to /usr/local/bin/"
echo "  2. Copy NDI library to /usr/lib/ndi/"
echo "  3. Apply network config: netplan apply"
echo "  4. Reboot to apply all changes: reboot"
echo ""
echo -e "${GREEN}After reboot, connect via: ssh root@${DEVICE_IP}${NC}"
