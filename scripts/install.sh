#!/bin/bash
set -euo pipefail

# Camera-Box USB Installer
# Usage: curl -fsSL https://github.com/zbynekdrlik/camera-box/releases/latest/download/install.sh | sudo bash -s /dev/sdX

REPO="zbynekdrlik/camera-box"
TARGET_DEVICE="${1:-}"
HOSTNAME="${2:-camera-box}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

# Check requirements
check_requirements() {
    if [[ $EUID -ne 0 ]]; then
        error "This script must be run as root (use sudo)"
    fi

    if [[ -z "$TARGET_DEVICE" ]]; then
        echo ""
        echo "Camera-Box USB Installer"
        echo "========================"
        echo ""
        echo "Usage: $0 <device> [hostname]"
        echo ""
        echo "Available devices:"
        lsblk -d -o NAME,SIZE,MODEL | grep -E "^sd|^nvme"
        echo ""
        error "Please specify target device (e.g., /dev/sdb)"
    fi

    if [[ ! -b "$TARGET_DEVICE" ]]; then
        error "Device not found: $TARGET_DEVICE"
    fi

    # Confirm dangerous operation
    echo ""
    echo -e "${RED}WARNING: This will ERASE ALL DATA on $TARGET_DEVICE${NC}"
    echo ""
    lsblk "$TARGET_DEVICE"
    echo ""
    read -p "Are you sure you want to continue? (yes/no): " confirm
    if [[ "$confirm" != "yes" ]]; then
        error "Aborted by user"
    fi

    for cmd in curl xz dd; do
        if ! command -v "$cmd" &>/dev/null; then
            error "Required command not found: $cmd"
        fi
    done
}

get_latest_release() {
    log "Fetching latest release..."
    RELEASE_URL=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | \
        grep -o '"browser_download_url": *"[^"]*camera-box-image.img.xz"' | \
        cut -d'"' -f4)

    if [[ -z "$RELEASE_URL" ]]; then
        error "Failed to find release image"
    fi

    CHECKSUM_URL="${RELEASE_URL}.sha256"
    log "Release found: $RELEASE_URL"
}

download_and_write() {
    local TMP_DIR="/tmp/camera-box-install"
    mkdir -p "$TMP_DIR"

    log "Downloading image (this may take a while)..."
    curl -fsSL "$RELEASE_URL" -o "$TMP_DIR/image.img.xz"
    curl -fsSL "$CHECKSUM_URL" -o "$TMP_DIR/image.img.xz.sha256"

    log "Verifying checksum..."
    cd "$TMP_DIR"
    sha256sum -c image.img.xz.sha256 || error "Checksum verification failed!"

    log "Writing image to $TARGET_DEVICE..."
    xz -dc image.img.xz | dd of="$TARGET_DEVICE" bs=4M status=progress conv=fsync

    sync
    log "Image written successfully!"

    # Clean up
    rm -rf "$TMP_DIR"
}

configure_hostname() {
    log "Configuring hostname: $HOSTNAME"

    # Wait for partitions to appear
    sleep 2
    partprobe "$TARGET_DEVICE" 2>/dev/null || true
    sleep 1

    # Mount overlay partition (partition 3)
    local OVERLAY_PART="${TARGET_DEVICE}3"
    if [[ "$TARGET_DEVICE" == *"nvme"* ]]; then
        OVERLAY_PART="${TARGET_DEVICE}p3"
    fi

    if [[ ! -b "$OVERLAY_PART" ]]; then
        warn "Could not find overlay partition, skipping hostname configuration"
        return
    fi

    local MOUNT_DIR="/tmp/camera-box-overlay"
    mkdir -p "$MOUNT_DIR"
    mount "$OVERLAY_PART" "$MOUNT_DIR"

    # Create upper directory structure
    mkdir -p "$MOUNT_DIR/upper/etc/camera-box"
    mkdir -p "$MOUNT_DIR/upper/etc"

    # Set hostname
    echo "$HOSTNAME" > "$MOUNT_DIR/upper/etc/hostname"

    # Update config
    cat > "$MOUNT_DIR/upper/etc/camera-box/config.toml" << EOF
# Camera-Box Configuration
hostname = "$HOSTNAME"
device = "auto"
EOF

    umount "$MOUNT_DIR"
    rmdir "$MOUNT_DIR"

    log "Hostname configured: $HOSTNAME"
}

download_ndi_sdk() {
    echo ""
    echo "NDI SDK Installation"
    echo "===================="
    echo ""
    echo "Camera-Box requires the NDI SDK to function."
    echo "The NDI SDK is proprietary software from Vizrt."
    echo ""
    echo "To download the SDK:"
    echo "1. Visit: https://ndi.video/download-ndi-sdk/"
    echo "2. Download the Linux SDK"
    echo "3. Extract and copy libndi.so to /usr/lib/ndi/ on the camera-box device"
    echo ""
    warn "The camera-box will not stream until NDI SDK is installed!"
}

main() {
    echo ""
    echo "╔═══════════════════════════════════════╗"
    echo "║     Camera-Box USB Installer          ║"
    echo "╚═══════════════════════════════════════╝"
    echo ""

    check_requirements
    get_latest_release
    download_and_write
    configure_hostname
    download_ndi_sdk

    echo ""
    log "Installation complete!"
    echo ""
    echo "Next steps:"
    echo "1. Download NDI SDK from https://ndi.video/download-ndi-sdk/"
    echo "2. Boot from USB drive"
    echo "3. Copy NDI SDK to /usr/lib/ndi/ on the device"
    echo "4. Connect USB capture card"
    echo "5. The NDI stream will appear as 'usb' on the network"
    echo ""
}

main "$@"
