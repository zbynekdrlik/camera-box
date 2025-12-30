#!/bin/bash
#
# Write camera-box image to USB drive
# Usage: ./write-image.sh IMAGE_FILE USB_DEVICE
# Example: ./write-image.sh camera-box-v1.0.0.img.xz /dev/sdb
#

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Default to ubuntu-usb-master.img in images directory
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEFAULT_IMAGE="${SCRIPT_DIR}/../images/ubuntu-usb-master.img"

IMAGE_FILE="${1:-$DEFAULT_IMAGE}"
USB_DEVICE="${2:-}"

if [ -z "$USB_DEVICE" ]; then
    echo -e "${RED}Usage: $0 [IMAGE_FILE] USB_DEVICE${NC}"
    echo ""
    echo "Example: $0 /dev/sdb                    # Uses default image"
    echo "Example: $0 custom.img /dev/sdb         # Uses custom image"
    echo ""
    echo "Default image: $DEFAULT_IMAGE"
    echo ""
    echo "Available USB devices:"
    lsblk -d -o NAME,SIZE,MODEL | grep -E '^sd'
    exit 1
fi

# Safety check - don't write to system disk
if [[ "$USB_DEVICE" == "/dev/sda" ]] || [[ "$USB_DEVICE" == "/dev/nvme0n1" ]]; then
    echo -e "${RED}Error: Refusing to write to system disk $USB_DEVICE${NC}"
    exit 1
fi

# Check if device exists
if [ ! -b "$USB_DEVICE" ]; then
    echo -e "${RED}Error: Device $USB_DEVICE does not exist${NC}"
    exit 1
fi

# Get device info
DEVICE_INFO=$(lsblk -d -o NAME,SIZE,MODEL "$USB_DEVICE" 2>/dev/null | tail -1)

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Camera-Box Image Writer${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Image file:  ${YELLOW}${IMAGE_FILE}${NC}"
echo -e "USB device:  ${YELLOW}${USB_DEVICE}${NC}"
echo -e "Device info: ${DEVICE_INFO}"
echo ""
echo -e "${RED}WARNING: All data on $USB_DEVICE will be destroyed!${NC}"
echo ""
read -p "Type 'YES' to continue: " CONFIRM
if [ "$CONFIRM" != "YES" ]; then
    echo "Aborted."
    exit 1
fi

# Unmount any partitions
echo ""
echo -e "${GREEN}[1/4] Unmounting partitions...${NC}"
umount ${USB_DEVICE}* 2>/dev/null || true

# Write image
echo ""
echo -e "${GREEN}[2/4] Writing image to USB...${NC}"
if [[ "$IMAGE_FILE" == *.xz ]]; then
    xzcat "$IMAGE_FILE" | dd of="$USB_DEVICE" bs=4M status=progress conv=fsync
elif [[ "$IMAGE_FILE" == *.gz ]]; then
    zcat "$IMAGE_FILE" | dd of="$USB_DEVICE" bs=4M status=progress conv=fsync
else
    dd if="$IMAGE_FILE" of="$USB_DEVICE" bs=4M status=progress conv=fsync
fi

# Sync and unmount
echo ""
echo -e "${GREEN}[3/4] Syncing...${NC}"
sync

echo ""
echo -e "${GREEN}[4/4] Unmounting USB (safe to remove)...${NC}"
umount ${USB_DEVICE}* 2>/dev/null || true
sync
echo -e "${GREEN}USB safely unmounted - you can remove it now${NC}"

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Image written successfully!${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo "Next steps:"
echo "  1. Remove USB and insert into new camera PC"
echo "  2. Boot from USB"
echo "  3. After boot, connect via SSH (it will have CAM1's IP initially)"
echo "  4. Run setup script: ./setup-device.sh CAM2 10.77.9.62 cam2"
echo "  5. Reboot to apply new IP"
