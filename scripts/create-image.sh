#!/bin/bash
#
# Create a camera-box disk image from a running device
# Usage: ./create-image.sh SOURCE_IP OUTPUT_FILE
# Example: ./create-image.sh 10.77.9.61 camera-box-v1.0.0.img
#

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SOURCE_IP="${1:-}"
OUTPUT_FILE="${2:-camera-box.img}"
SSH_PASS="${SSH_PASS:-newlevel}"

if [ -z "$SOURCE_IP" ]; then
    echo -e "${RED}Usage: $0 SOURCE_IP [OUTPUT_FILE]${NC}"
    echo ""
    echo "Example: $0 10.77.9.61 camera-box-v1.0.0.img"
    echo ""
    echo "Environment variables:"
    echo "  SSH_PASS - SSH password (default: newlevel)"
    exit 1
fi

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Camera-Box Image Creator${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Source device: ${YELLOW}${SOURCE_IP}${NC}"
echo -e "Output file:   ${YELLOW}${OUTPUT_FILE}${NC}"
echo ""

# Check if sshpass is installed
if ! command -v sshpass &> /dev/null; then
    echo -e "${RED}Error: sshpass is required. Install with: sudo apt install sshpass${NC}"
    exit 1
fi

# Get source disk info
echo -e "${GREEN}[1/4] Getting source disk information...${NC}"
DISK_INFO=$(sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no root@$SOURCE_IP "lsblk -b -d -o NAME,SIZE | grep -E '^(sd|nvme|mmcblk)' | head -1")
DISK_NAME=$(echo "$DISK_INFO" | awk '{print $1}')
DISK_SIZE=$(echo "$DISK_INFO" | awk '{print $2}')
DISK_SIZE_GB=$((DISK_SIZE / 1024 / 1024 / 1024))

echo "  Source disk: /dev/$DISK_NAME (${DISK_SIZE_GB}GB)"

# Confirm
echo ""
echo -e "${YELLOW}Warning: This will create a ${DISK_SIZE_GB}GB image file${NC}"
read -p "Continue? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 1
fi

# Clean up source device before imaging
echo ""
echo -e "${GREEN}[2/4] Preparing source device...${NC}"
sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no root@$SOURCE_IP "
    # Clear logs to reduce image size
    journalctl --vacuum-size=10M
    # Clear temp files
    rm -rf /tmp/*
    # Sync filesystem
    sync
"
echo "  Source device prepared"

# Create image
echo ""
echo -e "${GREEN}[3/4] Creating disk image (this may take a while)...${NC}"
sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no root@$SOURCE_IP "dd if=/dev/$DISK_NAME bs=4M status=progress" > "$OUTPUT_FILE"
echo "  Image created: $OUTPUT_FILE"

# Compress image
echo ""
echo -e "${GREEN}[4/4] Compressing image...${NC}"
xz -v -T0 "$OUTPUT_FILE"
COMPRESSED_FILE="${OUTPUT_FILE}.xz"
COMPRESSED_SIZE=$(ls -lh "$COMPRESSED_FILE" | awk '{print $5}')
echo "  Compressed: $COMPRESSED_FILE ($COMPRESSED_SIZE)"

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Image creation complete!${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo "Output file: $COMPRESSED_FILE"
echo ""
echo "To write to USB:"
echo "  xzcat $COMPRESSED_FILE | sudo dd of=/dev/sdX bs=4M status=progress"
echo ""
echo "After booting new device, run setup script to configure:"
echo "  ./setup-device.sh CAM2 10.77.9.62 cam2"
