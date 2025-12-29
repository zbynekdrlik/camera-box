#!/bin/bash
set -euo pipefail

# Create Ubuntu Server VM image for USB deployment
# This creates a master image that can be dd'd to any USB

ISO_PATH="/home/newlevel/Downloads/ubuntu-server-24.04.iso"
DISK_IMAGE="/home/newlevel/Downloads/ubuntu-usb-master.img"
DISK_SIZE="32G"

log() { echo "[+] $1"; }
error() { echo "[ERROR] $1"; exit 1; }

# Check ISO exists
[[ -f "$ISO_PATH" ]] || error "ISO not found: $ISO_PATH"

# Create disk image
log "Creating ${DISK_SIZE} disk image..."
qemu-img create -f raw "$DISK_IMAGE" "$DISK_SIZE"

log "Starting QEMU VM for installation..."
log "=== INSTRUCTIONS ==="
log "1. Select 'Try or Install Ubuntu Server'"
log "2. Choose language, keyboard"
log "3. For install type: Choose 'Ubuntu Server (minimized)'"
log "4. Network: Use DHCP (default)"
log "5. Storage: Use entire disk (the virtual disk)"
log "6. Username: newlevel, Password: newlevel"
log "7. Install OpenSSH server: YES"
log "8. Featured snaps: Skip (select none)"
log "9. Wait for install, then reboot"
log "10. After reboot, login and run: sudo poweroff"
log "===================="
echo ""
read -p "Press Enter to start VM..."

# Run QEMU with the ISO
qemu-system-x86_64 \
    -enable-kvm \
    -m 4096 \
    -smp 2 \
    -cpu host \
    -drive file="$DISK_IMAGE",format=raw,if=virtio \
    -cdrom "$ISO_PATH" \
    -boot d \
    -net nic,model=virtio \
    -net user \
    -vga virtio \
    -display gtk

log "VM closed. If installation completed, the image is ready."
log "Image location: $DISK_IMAGE"
log ""
log "To write to USB:"
log "  sudo dd if=$DISK_IMAGE of=/dev/sdX bs=4M status=progress"
