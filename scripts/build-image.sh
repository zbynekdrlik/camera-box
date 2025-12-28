#!/bin/bash
set -euo pipefail

# Camera-Box USB Image Builder
# Creates a bootable USB image with read-only root filesystem

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY_TARBALL="${1:-}"
OUTPUT_IMAGE="camera-box-image.img"
IMAGE_SIZE="4G"
WORK_DIR="/tmp/camera-box-build"

# Partition sizes
EFI_SIZE="256M"
ROOT_SIZE="3G"
OVERLAY_SIZE="512M"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"
}

error() {
    echo "[ERROR] $*" >&2
    exit 1
}

cleanup() {
    log "Cleaning up..."
    # Unmount in reverse order
    umount -R "${WORK_DIR}/rootfs" 2>/dev/null || true
    umount "${WORK_DIR}/efi" 2>/dev/null || true
    losetup -D 2>/dev/null || true
    rm -rf "${WORK_DIR}"
}

trap cleanup EXIT

# Check requirements
check_requirements() {
    local missing=()
    for cmd in debootstrap mksquashfs parted mkfs.fat mkfs.ext4 losetup; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        error "Missing required commands: ${missing[*]}"
    fi

    if [[ $EUID -ne 0 ]]; then
        error "This script must be run as root"
    fi

    if [[ -z "$BINARY_TARBALL" ]] || [[ ! -f "$BINARY_TARBALL" ]]; then
        error "Usage: $0 <camera-box-binary.tar.gz>"
    fi
}

create_image() {
    log "Creating ${IMAGE_SIZE} image file..."
    truncate -s "$IMAGE_SIZE" "$OUTPUT_IMAGE"

    log "Creating partition table..."
    parted -s "$OUTPUT_IMAGE" mklabel gpt
    parted -s "$OUTPUT_IMAGE" mkpart ESP fat32 1MiB "${EFI_SIZE}"
    parted -s "$OUTPUT_IMAGE" set 1 esp on
    parted -s "$OUTPUT_IMAGE" mkpart root ext4 "${EFI_SIZE}" "3328MiB"
    parted -s "$OUTPUT_IMAGE" mkpart overlay ext4 "3328MiB" "100%"

    log "Setting up loop device..."
    LOOP_DEV=$(losetup --find --show --partscan "$OUTPUT_IMAGE")
    log "Loop device: $LOOP_DEV"

    # Wait for partitions to appear
    sleep 2

    log "Formatting partitions..."
    mkfs.fat -F32 -n "EFI" "${LOOP_DEV}p1"
    mkfs.ext4 -L "root" "${LOOP_DEV}p2"
    mkfs.ext4 -L "overlay" "${LOOP_DEV}p3"
}

bootstrap_rootfs() {
    mkdir -p "${WORK_DIR}/rootfs" "${WORK_DIR}/efi"

    log "Mounting partitions..."
    mount "${LOOP_DEV}p2" "${WORK_DIR}/rootfs"
    mkdir -p "${WORK_DIR}/rootfs/boot/efi"
    mount "${LOOP_DEV}p1" "${WORK_DIR}/rootfs/boot/efi"

    log "Installing Debian base system..."
    debootstrap --variant=minbase --include=systemd,systemd-sysv,dbus,udev \
        bookworm "${WORK_DIR}/rootfs" http://deb.debian.org/debian

    log "Configuring base system..."

    # Set hostname
    echo "camera-box" > "${WORK_DIR}/rootfs/etc/hostname"

    # Configure fstab for read-only root with overlay
    cat > "${WORK_DIR}/rootfs/etc/fstab" << 'EOF'
# camera-box fstab - read-only root with overlay
tmpfs           /tmp            tmpfs   defaults,noatime,nosuid,nodev,mode=1777  0 0
tmpfs           /var/log        tmpfs   defaults,noatime,nosuid,nodev,size=64M   0 0
tmpfs           /var/tmp        tmpfs   defaults,noatime,nosuid,nodev,mode=1777  0 0
EOF

    # Install additional packages
    log "Installing additional packages..."
    chroot "${WORK_DIR}/rootfs" apt-get update
    chroot "${WORK_DIR}/rootfs" apt-get install -y --no-install-recommends \
        linux-image-amd64 \
        grub-efi-amd64 \
        avahi-daemon \
        v4l-utils \
        util-linux \
        iproute2 \
        openssh-server \
        curl \
        ca-certificates \
        ffmpeg

    # Clean up apt cache
    chroot "${WORK_DIR}/rootfs" apt-get clean
    rm -rf "${WORK_DIR}/rootfs/var/lib/apt/lists/"*
}

install_camera_box() {
    log "Installing camera-box binary..."
    tar -xzf "$BINARY_TARBALL" -C "${WORK_DIR}/rootfs/usr/local/bin/"
    chmod +x "${WORK_DIR}/rootfs/usr/local/bin/camera-box"

    log "Installing systemd service..."
    cp "${WORK_DIR}/rootfs/usr/local/bin/camera-box.service" \
       "${WORK_DIR}/rootfs/etc/systemd/system/" 2>/dev/null || \
    cp "${SCRIPT_DIR}/../systemd/camera-box.service" \
       "${WORK_DIR}/rootfs/etc/systemd/system/"

    # Enable services
    chroot "${WORK_DIR}/rootfs" systemctl enable camera-box.service
    chroot "${WORK_DIR}/rootfs" systemctl enable avahi-daemon.service
    chroot "${WORK_DIR}/rootfs" systemctl enable ssh.service

    # Disable conflicting services
    chroot "${WORK_DIR}/rootfs" systemctl mask systemd-timesyncd.service
    chroot "${WORK_DIR}/rootfs" systemctl mask apt-daily.timer
    chroot "${WORK_DIR}/rootfs" systemctl mask apt-daily-upgrade.timer
    chroot "${WORK_DIR}/rootfs" systemctl mask sleep.target
    chroot "${WORK_DIR}/rootfs" systemctl mask suspend.target
    chroot "${WORK_DIR}/rootfs" systemctl mask hibernate.target

    # Create config directory
    mkdir -p "${WORK_DIR}/rootfs/etc/camera-box"
    cat > "${WORK_DIR}/rootfs/etc/camera-box/config.toml" << 'EOF'
# Camera-Box Configuration
hostname = "camera-box"
device = "auto"
EOF
}

configure_power_button() {
    log "Disabling power button..."
    mkdir -p "${WORK_DIR}/rootfs/etc/systemd/logind.conf.d"
    cat > "${WORK_DIR}/rootfs/etc/systemd/logind.conf.d/disable-power-button.conf" << 'EOF'
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF
}

configure_network() {
    log "Configuring network (DHCP default)..."
    mkdir -p "${WORK_DIR}/rootfs/etc/systemd/network"
    cat > "${WORK_DIR}/rootfs/etc/systemd/network/20-wired.network" << 'EOF'
[Match]
Name=en* eth*

[Network]
DHCP=yes

[DHCPv4]
UseDNS=yes
UseNTP=yes
EOF

    chroot "${WORK_DIR}/rootfs" systemctl enable systemd-networkd.service
    chroot "${WORK_DIR}/rootfs" systemctl enable systemd-resolved.service
}

configure_overlay() {
    log "Configuring overlay filesystem..."

    # Create initramfs hook for overlay
    mkdir -p "${WORK_DIR}/rootfs/etc/initramfs-tools/scripts/init-bottom"
    cat > "${WORK_DIR}/rootfs/etc/initramfs-tools/scripts/init-bottom/overlay" << 'OVERLAY_SCRIPT'
#!/bin/sh
PREREQ=""
prereqs() { echo "$PREREQ"; }
case "$1" in
    prereqs) prereqs; exit 0 ;;
esac

# Mount overlay filesystem
mkdir -p /mnt/overlay
mount -t ext4 /dev/disk/by-label/overlay /mnt/overlay

mkdir -p /mnt/overlay/upper /mnt/overlay/work

# Move root to lower
mkdir -p /mnt/root-ro
mount --move ${rootmnt} /mnt/root-ro

# Mount overlay as new root
mount -t overlay overlay -o lowerdir=/mnt/root-ro,upperdir=/mnt/overlay/upper,workdir=/mnt/overlay/work ${rootmnt}

# Move mounts into new root
mkdir -p ${rootmnt}/mnt/root-ro ${rootmnt}/mnt/overlay
mount --move /mnt/root-ro ${rootmnt}/mnt/root-ro
mount --move /mnt/overlay ${rootmnt}/mnt/overlay
OVERLAY_SCRIPT
    chmod +x "${WORK_DIR}/rootfs/etc/initramfs-tools/scripts/init-bottom/overlay"

    # Update initramfs
    chroot "${WORK_DIR}/rootfs" update-initramfs -u
}

install_bootloader() {
    log "Installing GRUB bootloader..."

    # Install GRUB to EFI partition
    chroot "${WORK_DIR}/rootfs" grub-install --target=x86_64-efi \
        --efi-directory=/boot/efi --bootloader-id=camera-box --removable

    # Configure GRUB
    cat > "${WORK_DIR}/rootfs/etc/default/grub" << 'EOF'
GRUB_DEFAULT=0
GRUB_TIMEOUT=3
GRUB_DISTRIBUTOR="Camera-Box"
GRUB_CMDLINE_LINUX_DEFAULT="quiet"
GRUB_CMDLINE_LINUX=""
EOF

    chroot "${WORK_DIR}/rootfs" update-grub
}

install_ndi_library() {
    log "Installing NDI library..."

    # Check if NDI library exists on build system
    if [[ -f "/usr/lib/ndi/libndi.so.6" ]]; then
        mkdir -p "${WORK_DIR}/rootfs/usr/lib/ndi"
        cp /usr/lib/ndi/libndi.so* "${WORK_DIR}/rootfs/usr/lib/ndi/"
        log "NDI library copied from build system"
    elif [[ -n "${NDI_SDK_PATH:-}" ]] && [[ -d "$NDI_SDK_PATH" ]]; then
        mkdir -p "${WORK_DIR}/rootfs/usr/lib/ndi"
        cp "$NDI_SDK_PATH"/lib/x86_64-linux-gnu/libndi.so* "${WORK_DIR}/rootfs/usr/lib/ndi/"
        log "NDI library copied from NDI_SDK_PATH"
    else
        log "Warning: NDI library not found. Set NDI_SDK_PATH or install to /usr/lib/ndi/"
        log "The camera-box will not work until NDI library is installed on the device."
    fi
}

download_dantetimesync() {
    log "Downloading DanteTimeSync..."
    local DTS_URL="https://github.com/zbynekdrlik/dantetimesync/releases/latest/download/dantetimesync-linux-amd64"

    curl -fsSL "$DTS_URL" -o "${WORK_DIR}/rootfs/usr/local/bin/dantetimesync" || {
        log "Warning: Failed to download DanteTimeSync, skipping..."
        return
    }
    chmod +x "${WORK_DIR}/rootfs/usr/local/bin/dantetimesync"

    # Create systemd service
    cat > "${WORK_DIR}/rootfs/etc/systemd/system/dantetimesync.service" << 'EOF'
[Unit]
Description=Dante Time Sync
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantetimesync
Restart=always
RestartSec=5
Nice=-10
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

[Install]
WantedBy=multi-user.target
EOF

    chroot "${WORK_DIR}/rootfs" systemctl enable dantetimesync.service
}

finalize_image() {
    log "Finalizing image..."

    # Create factory reset script
    cat > "${WORK_DIR}/rootfs/usr/local/bin/camera-box-reset" << 'EOF'
#!/bin/bash
echo "Factory reset: clearing overlay partition..."
rm -rf /mnt/overlay/upper/* /mnt/overlay/work/*
echo "Done. Reboot to apply."
EOF
    chmod +x "${WORK_DIR}/rootfs/usr/local/bin/camera-box-reset"

    # Sync and unmount
    sync
    umount "${WORK_DIR}/rootfs/boot/efi"
    umount "${WORK_DIR}/rootfs"

    log "Detaching loop device..."
    losetup -d "$LOOP_DEV"

    log "Image created: $OUTPUT_IMAGE"
}

main() {
    check_requirements

    log "Starting Camera-Box image build..."
    mkdir -p "$WORK_DIR"

    create_image
    bootstrap_rootfs
    install_camera_box
    install_ndi_library
    configure_power_button
    configure_network
    configure_overlay
    download_dantetimesync
    install_bootloader
    finalize_image

    log "Build complete!"
}

main "$@"
