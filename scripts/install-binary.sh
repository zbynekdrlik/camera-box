#!/bin/bash
set -euo pipefail

# Camera-Box Binary Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/install-binary.sh | sudo bash

REPO="zbynekdrlik/camera-box"
INSTALL_DIR="/usr/local/bin"
NDI_DIR="/usr/lib/ndi"

log() { echo "[INFO] $*"; }
error() { echo "[ERROR] $*" >&2; exit 1; }

if [[ $EUID -ne 0 ]]; then
    error "This script must be run as root (use sudo)"
fi

log "Fetching latest release..."
RELEASE_URL=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | \
    grep -o '"browser_download_url": *"[^"]*camera-box-linux-amd64.tar.gz"' | \
    head -1 | cut -d'"' -f4)

if [[ -z "$RELEASE_URL" ]]; then
    error "Failed to find release. Check https://github.com/${REPO}/releases"
fi

log "Downloading from: $RELEASE_URL"
TMP_DIR=$(mktemp -d)
curl -fsSL "$RELEASE_URL" -o "$TMP_DIR/camera-box.tar.gz"

log "Installing to $INSTALL_DIR..."
tar -xzf "$TMP_DIR/camera-box.tar.gz" -C "$TMP_DIR"
install -m 755 "$TMP_DIR/camera-box" "$INSTALL_DIR/"

if [[ -f "$TMP_DIR/camera-box.service" ]]; then
    install -m 644 "$TMP_DIR/camera-box.service" /etc/systemd/system/
    systemctl daemon-reload
    log "Systemd service installed"
fi

rm -rf "$TMP_DIR"

# Check for NDI library
if [[ ! -f "$NDI_DIR/libndi.so.6" ]] && [[ ! -f "$NDI_DIR/libndi.so" ]]; then
    echo ""
    echo "WARNING: NDI library not found at $NDI_DIR"
    echo "Camera-box requires the NDI SDK to function."
    echo ""
    echo "Install NDI SDK:"
    echo "  1. Download from https://ndi.video/download-ndi-sdk/"
    echo "  2. Extract and copy libndi.so* to $NDI_DIR/"
    echo ""
fi

log "Installation complete!"
echo ""
echo "Usage: camera-box [--device /dev/video0]"
echo "Service: systemctl start camera-box"
