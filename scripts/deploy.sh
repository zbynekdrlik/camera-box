#!/bin/bash
set -euo pipefail

# Camera-Box Deployment Script
# Deploys camera-box setup + NDI SDK to a target device
#
# Usage: ./deploy.sh <hostname-or-ip> [device-name]
# Example: ./deploy.sh 10.77.8.119 CAM1
# Example: ./deploy.sh 192.168.1.100 CAM2

TARGET="${1:-}"
DEVICE_NAME="${2:-camera-box}"
SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-newlevel}"
NDI_SDK_PATH="${NDI_SDK_PATH:-/usr/lib/ndi}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

usage() {
    cat << EOF
Camera-Box Deployment Script

Usage: $(basename "$0") <target-ip> [device-name]

Arguments:
    target-ip     IP address or hostname of the target device
    device-name   Name for the device (default: camera-box)

Environment Variables:
    SSH_USER      SSH username (default: root)
    SSH_PASS      SSH password (default: newlevel)
    NDI_SDK_PATH  Path to local NDI SDK (default: /usr/lib/ndi)

Examples:
    $(basename "$0") 10.77.8.119 CAM1
    $(basename "$0") 192.168.1.100 CAM2
    SSH_USER=admin SSH_PASS=secret $(basename "$0") 10.77.8.50 CAM3

EOF
    exit 0
}

check_requirements() {
    if [[ -z "$TARGET" ]]; then
        usage
    fi

    # Check sshpass
    if ! command -v sshpass &>/dev/null; then
        error "sshpass is required. Install with: sudo apt-get install sshpass"
    fi

    # Check NDI SDK exists locally
    if [[ ! -f "$NDI_SDK_PATH/libndi.so.6" ]]; then
        error "NDI SDK not found at $NDI_SDK_PATH/libndi.so.6
Download from https://ndi.video/download-ndi-sdk/ and install locally first."
    fi

    # Test SSH connection
    info "Testing connection to $TARGET..."
    if ! sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 "$SSH_USER@$TARGET" "echo ok" &>/dev/null; then
        error "Cannot connect to $TARGET. Check IP address and credentials."
    fi
    log "Connection successful"
}

run_setup() {
    log "Running setup script on $TARGET with hostname: $DEVICE_NAME"

    sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no "$SSH_USER@$TARGET" \
        "curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/setup.sh | bash -s $DEVICE_NAME"
}

copy_ndi_sdk() {
    log "Copying NDI SDK to $TARGET..."

    # Create directory on target
    sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no "$SSH_USER@$TARGET" \
        "mkdir -p /usr/lib/ndi"

    # Copy NDI library
    sshpass -p "$SSH_PASS" scp -o StrictHostKeyChecking=no \
        "$NDI_SDK_PATH/libndi.so.6" \
        "$SSH_USER@$TARGET:/usr/lib/ndi/"

    # Create symlink and update ldconfig
    sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no "$SSH_USER@$TARGET" \
        "ln -sf /usr/lib/ndi/libndi.so.6 /usr/lib/ndi/libndi.so && ldconfig"

    log "NDI SDK installed"
}

start_services() {
    log "Starting camera-box service..."

    sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no "$SSH_USER@$TARGET" \
        "systemctl restart camera-box && sleep 2 && systemctl status camera-box --no-pager | head -15"
}

show_status() {
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  Deployment Complete: $DEVICE_NAME${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════${NC}"
    echo ""

    sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no "$SSH_USER@$TARGET" "
echo 'Hostname:' \$(hostname)
echo 'IP:' \$(hostname -I | awk '{print \$1}')
echo ''
echo 'Services:'
systemctl is-active dantesync 2>/dev/null && echo '  dantesync: running' || echo '  dantesync: stopped'
systemctl is-active camera-box 2>/dev/null && echo '  camera-box: running' || echo '  camera-box: stopped'
systemctl is-active avahi-daemon 2>/dev/null && echo '  avahi-daemon: running' || echo '  avahi-daemon: stopped'
echo ''
echo 'Video devices:'
v4l2-ctl --list-devices 2>/dev/null | head -10 || echo '  None detected'
"
    echo ""
    log "Device $DEVICE_NAME is ready!"
    echo ""
    echo "SSH: ssh $SSH_USER@$TARGET"
    echo "Logs: ssh $SSH_USER@$TARGET journalctl -u camera-box -f"
    echo ""
}

main() {
    echo ""
    echo "╔═══════════════════════════════════════════════════════════════╗"
    echo "║              Camera-Box Deployment Script                     ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    echo ""

    check_requirements
    run_setup
    copy_ndi_sdk
    start_services
    show_status
}

main "$@"
