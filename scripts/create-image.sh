#!/bin/bash
set -euo pipefail

# Camera-Box USB Image Creator
# Creates a bootable Ubuntu Server 24.04 UEFI/GPT image for USB deployment

# Configuration
IMAGE_SIZE="${IMAGE_SIZE:-4G}"
IMAGE_PATH="${IMAGE_PATH:-./camera-box-image.img}"
ISO_PATH="${ISO_PATH:-}"
HOSTNAME="${HOSTNAME:-camera-box}"
USERNAME="${USERNAME:-newlevel}"
PASSWORD="${PASSWORD:-newlevel}"

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK_DIR="${WORK_DIR:-/tmp/camera-box-build}"
OVMF_CODE="/usr/share/OVMF/OVMF_CODE_4M.fd"
OVMF_VARS="/usr/share/OVMF/OVMF_VARS_4M.fd"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Create a bootable Ubuntu Server 24.04 USB image for camera-box.

Options:
    -i, --iso PATH       Path to Ubuntu Server 24.04 ISO (required)
    -o, --output PATH    Output image path (default: ./camera-box-image.img)
    -s, --size SIZE      Image size (default: 4G)
    -h, --hostname NAME  Hostname (default: camera-box)
    -u, --username USER  Username (default: newlevel)
    -p, --password PASS  Password (default: newlevel)
    --help               Show this help message

Example:
    $(basename "$0") -i ubuntu-server-24.04.iso -o camera-box.img

EOF
    exit 0
}

check_dependencies() {
    log "Checking dependencies..."

    local missing=()

    command -v qemu-system-x86_64 >/dev/null || missing+=("qemu-system-x86_64")
    command -v qemu-img >/dev/null || missing+=("qemu-img")
    command -v genisoimage >/dev/null || missing+=("genisoimage")

    [[ -f "$OVMF_CODE" ]] || missing+=("ovmf")

    if [[ ${#missing[@]} -gt 0 ]]; then
        error "Missing dependencies: ${missing[*]}
Install with: sudo apt-get install qemu-system-x86 qemu-utils genisoimage ovmf"
    fi

    log "All dependencies found"
}

generate_password_hash() {
    # Generate SHA-512 password hash
    openssl passwd -6 "$1"
}

create_autoinstall_config() {
    log "Creating autoinstall configuration..."

    mkdir -p "$WORK_DIR"

    local password_hash
    password_hash=$(generate_password_hash "$PASSWORD")

    # Create user-data (autoinstall config)
    cat > "$WORK_DIR/user-data" << EOF
#cloud-config
autoinstall:
  version: 1
  shutdown: poweroff
  locale: en_US.UTF-8
  keyboard:
    layout: us
  identity:
    hostname: ${HOSTNAME}
    username: ${USERNAME}
    password: "${password_hash}"
  ssh:
    install-server: true
    allow-pw: true
  network:
    version: 2
    ethernets:
      all-en:
        match:
          name: "en*"
        dhcp4: true
      all-eth:
        match:
          name: "eth*"
        dhcp4: true
  storage:
    layout:
      name: direct
  packages:
    - vim
    - less
    - cloud-guest-utils
  late-commands:
    - echo '${USERNAME} ALL=(ALL) NOPASSWD:ALL' > /target/etc/sudoers.d/${USERNAME}
    - chmod 440 /target/etc/sudoers.d/${USERNAME}
    - sed -i 's/^#*PermitRootLogin.*/PermitRootLogin yes/' /target/etc/ssh/sshd_config
    - sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication yes/' /target/etc/ssh/sshd_config
    - echo 'root:${PASSWORD}' | chroot /target chpasswd
    - |
      cat > /target/etc/netplan/01-netcfg.yaml << 'NETPLAN'
      network:
        version: 2
        renderer: networkd
        ethernets:
          all-ethernet:
            match:
              driver: "*"
            dhcp4: true
      NETPLAN
    - chmod 600 /target/etc/netplan/01-netcfg.yaml
EOF

    # Create meta-data
    cat > "$WORK_DIR/meta-data" << EOF
instance-id: ${HOSTNAME}-001
local-hostname: ${HOSTNAME}
EOF

    chmod 644 "$WORK_DIR/user-data" "$WORK_DIR/meta-data"

    log "Autoinstall configuration created"
}

create_seed_iso() {
    log "Creating cloud-init seed ISO..."

    genisoimage -output "$WORK_DIR/seed.iso" \
        -volid cidata \
        -joliet -rock \
        "$WORK_DIR/user-data" "$WORK_DIR/meta-data" 2>/dev/null

    log "Seed ISO created: $WORK_DIR/seed.iso"
}

extract_kernel() {
    log "Extracting kernel and initrd from ISO..."

    local mount_point="$WORK_DIR/iso-mount"
    mkdir -p "$mount_point"

    sudo mount -o loop,ro "$ISO_PATH" "$mount_point"

    cp "$mount_point/casper/vmlinuz" "$WORK_DIR/vmlinuz"
    cp "$mount_point/casper/initrd" "$WORK_DIR/initrd"

    sudo umount "$mount_point"

    log "Kernel and initrd extracted"
}

create_disk_image() {
    log "Creating ${IMAGE_SIZE} disk image..."

    rm -f "$IMAGE_PATH"
    qemu-img create -f raw "$IMAGE_PATH" "$IMAGE_SIZE"

    log "Disk image created: $IMAGE_PATH"
}

run_installation() {
    log "Starting QEMU installation (this may take 5-10 minutes)..."

    # Copy UEFI vars (writable)
    cp "$OVMF_VARS" "$WORK_DIR/OVMF_VARS.fd"

    local output_log="$WORK_DIR/install.log"

    # Run QEMU with autoinstall
    qemu-system-x86_64 \
        -enable-kvm \
        -m 4096 \
        -smp 2 \
        -cpu host \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$WORK_DIR/OVMF_VARS.fd" \
        -drive file="$IMAGE_PATH",format=raw,if=virtio,id=disk0 \
        -drive file="$ISO_PATH",format=raw,if=virtio,media=cdrom,readonly=on,id=cdrom0 \
        -drive file="$WORK_DIR/seed.iso",format=raw,if=virtio,id=seed \
        -kernel "$WORK_DIR/vmlinuz" \
        -initrd "$WORK_DIR/initrd" \
        -append "autoinstall console=ttyS0,115200 ---" \
        -net nic,model=virtio \
        -net user \
        -nographic \
        -serial mon:stdio 2>&1 | tee "$output_log" &

    local qemu_pid=$!

    log "QEMU started (PID: $qemu_pid), waiting for installation to complete..."

    # Wait for QEMU to exit (installation complete + poweroff)
    wait $qemu_pid || true

    # Check if installation succeeded
    if grep -q "reboot: Power down" "$output_log"; then
        log "Installation completed successfully!"
    else
        error "Installation may have failed. Check $output_log for details."
    fi
}

cleanup() {
    log "Cleaning up..."
    sudo umount "$WORK_DIR/iso-mount" 2>/dev/null || true
    rm -rf "$WORK_DIR"
    log "Cleanup complete"
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -i|--iso)
            ISO_PATH="$2"
            shift 2
            ;;
        -o|--output)
            IMAGE_PATH="$2"
            shift 2
            ;;
        -s|--size)
            IMAGE_SIZE="$2"
            shift 2
            ;;
        -h|--hostname)
            HOSTNAME="$2"
            shift 2
            ;;
        -u|--username)
            USERNAME="$2"
            shift 2
            ;;
        -p|--password)
            PASSWORD="$2"
            shift 2
            ;;
        --help)
            usage
            ;;
        *)
            error "Unknown option: $1"
            ;;
    esac
done

# Validate required arguments
[[ -z "$ISO_PATH" ]] && error "ISO path is required. Use -i or --iso"
[[ -f "$ISO_PATH" ]] || error "ISO not found: $ISO_PATH"

# Main
trap cleanup EXIT

log "Camera-Box Image Creator"
log "========================"
log "ISO: $ISO_PATH"
log "Output: $IMAGE_PATH"
log "Size: $IMAGE_SIZE"
log "Hostname: $HOSTNAME"
log "Username: $USERNAME"
log ""

check_dependencies
create_autoinstall_config
create_seed_iso
extract_kernel
create_disk_image
run_installation

log ""
log "========================================="
log "Image created successfully: $IMAGE_PATH"
log "========================================="
log ""
log "To write to USB:"
log "  sudo dd if=$IMAGE_PATH of=/dev/sdX bs=4M status=progress conv=fsync"
log ""
log "After booting, expand the partition with:"
log "  sudo growpart /dev/sda 2 && sudo resize2fs /dev/sda2"
log ""
log "SSH access:"
log "  ssh ${USERNAME}@<ip-address>  (password: ${PASSWORD})"
log "  ssh root@<ip-address>         (password: ${PASSWORD})"
