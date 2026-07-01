#!/bin/bash
set -euo pipefail

# Simple Ubuntu 24.04 USB installer
# Creates bootable USB with SSH + DHCP only
# User: newlevel, Password: newlevel, Root SSH enabled

DEVICE="${1:-}"
MOUNT_ROOT="/mnt/usb-root"
MOUNT_EFI="/mnt/usb-efi"
SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Check requirements
check_requirements() {
    log "Checking requirements..."

    [[ $EUID -eq 0 ]] || error "Must run as root"
    [[ -n "$DEVICE" ]] || error "Usage: $0 /dev/sdX"
    [[ -b "$DEVICE" ]] || error "$DEVICE is not a block device"

    # Safety check - don't allow sda
    [[ "$DEVICE" != "/dev/sda" ]] || error "Refusing to write to /dev/sda (system disk)"

    # Check required tools
    for cmd in debootstrap parted mkfs.vfat mkfs.ext4 mount chroot; do
        command -v $cmd &>/dev/null || error "Missing required tool: $cmd"
    done
}

# Confirm with user
confirm_device() {
    log "Target device: $DEVICE"
    lsblk "$DEVICE" -o NAME,SIZE,MODEL,MOUNTPOINT
    echo ""
    warn "ALL DATA ON $DEVICE WILL BE DESTROYED!"
    read -p "Type 'yes' to continue: " confirm
    [[ "$confirm" == "yes" ]] || error "Aborted by user"
}

# Unmount any existing partitions
cleanup_mounts() {
    log "Cleaning up existing mounts..."

    # Unmount any partitions on the device
    for part in "${DEVICE}"*; do
        if mountpoint -q "$part" 2>/dev/null || grep -q "$part" /proc/mounts 2>/dev/null; then
            umount -l "$part" 2>/dev/null || true
        fi
    done

    # Unmount our mount points
    for mp in "$MOUNT_ROOT/boot/efi" "$MOUNT_ROOT/dev/pts" "$MOUNT_ROOT/dev" "$MOUNT_ROOT/proc" "$MOUNT_ROOT/sys" "$MOUNT_ROOT" "$MOUNT_EFI"; do
        if mountpoint -q "$mp" 2>/dev/null; then
            umount -l "$mp" 2>/dev/null || true
        fi
    done

    sleep 1
}

# Partition the drive
partition_drive() {
    log "Partitioning $DEVICE..."

    # Wipe existing partition table
    wipefs -a "$DEVICE"

    # Create GPT partition table
    parted -s "$DEVICE" mklabel gpt

    # Create EFI partition (512MB)
    parted -s "$DEVICE" mkpart "EFI" fat32 1MiB 513MiB
    parted -s "$DEVICE" set 1 esp on

    # Create root partition (rest of disk, min 30GB)
    parted -s "$DEVICE" mkpart "root" ext4 513MiB 100%

    # Wait for kernel to recognize partitions
    partprobe "$DEVICE"
    sleep 2

    # Determine partition names (handle nvme style too)
    if [[ "$DEVICE" == *"nvme"* ]]; then
        PART_EFI="${DEVICE}p1"
        PART_ROOT="${DEVICE}p2"
    else
        PART_EFI="${DEVICE}1"
        PART_ROOT="${DEVICE}2"
    fi

    log "Creating filesystems..."
    mkfs.vfat -F32 -n "EFI" "$PART_EFI"
    mkfs.ext4 -L "ubuntu-root" "$PART_ROOT"
}

# Mount filesystems
mount_filesystems() {
    log "Mounting filesystems..."

    mkdir -p "$MOUNT_ROOT" "$MOUNT_EFI"
    mount "$PART_ROOT" "$MOUNT_ROOT"
    mkdir -p "$MOUNT_ROOT/boot/efi"
    mount "$PART_EFI" "$MOUNT_ROOT/boot/efi"
}

# Install base system
install_base() {
    log "Installing Ubuntu 24.04 base system (this takes a few minutes)..."

    debootstrap --arch=amd64 noble "$MOUNT_ROOT" http://archive.ubuntu.com/ubuntu/
}

# Configure the system
configure_system() {
    log "Configuring system..."

    # Mount virtual filesystems for chroot
    mount --bind /dev "$MOUNT_ROOT/dev"
    mount --bind /dev/pts "$MOUNT_ROOT/dev/pts"
    mount --bind /proc "$MOUNT_ROOT/proc"
    mount --bind /sys "$MOUNT_ROOT/sys"

    # Set up apt sources
    cat > "$MOUNT_ROOT/etc/apt/sources.list" << 'EOF'
deb http://archive.ubuntu.com/ubuntu/ noble main restricted universe multiverse
deb http://archive.ubuntu.com/ubuntu/ noble-updates main restricted universe multiverse
deb http://archive.ubuntu.com/ubuntu/ noble-security main restricted universe multiverse
EOF

    # Set hostname
    echo "camera-box" > "$MOUNT_ROOT/etc/hostname"
    cat > "$MOUNT_ROOT/etc/hosts" << 'EOF'
127.0.0.1   localhost
127.0.1.1   camera-box
EOF

    # Configure fstab
    ROOT_UUID=$(blkid -s UUID -o value "$PART_ROOT")
    EFI_UUID=$(blkid -s UUID -o value "$PART_EFI")

    cat > "$MOUNT_ROOT/etc/fstab" << EOF
UUID=$ROOT_UUID /         ext4  errors=remount-ro 0 1
UUID=$EFI_UUID  /boot/efi vfat  umask=0077        0 1
tmpfs           /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0
EOF

    # Create setup script to run inside chroot
    cat > "$MOUNT_ROOT/tmp/setup.sh" << 'SETUP_EOF'
#!/bin/bash
set -e

export DEBIAN_FRONTEND=noninteractive

# Update package list
apt-get update

# Install essential packages
# Note: systemd-networkd has built-in DHCP, dhcpcd is backup
apt-get install -y \
    linux-image-generic \
    grub-efi-amd64 \
    openssh-server \
    sudo \
    vim \
    less \
    dhcpcd-base \
    cloud-guest-utils

# #362: bake the NDI/audio RUNTIME deps into the base image so a fresh clone can RUN camera-box
# without hand-provisioning. The fresh CAM3 clone (#301 re-image) booted but camera-box crash-looped
# because libndi.so could not dlopen: the ALSA runtime + the avahi client/common libs were absent,
# /usr/lib/ndi was not on the dynamic-linker path, and no avahi-daemon ran for the mDNS NDI-source
# discovery libndi performs. avahi-daemon is mDNS only — no conflict with DanteSync's clock ownership
# (cam4 runs both). These are public Ubuntu packages (main/universe), installable in the chroot.
apt-get install -y \
    libasound2t64 \
    libavahi-client3 \
    libavahi-common3 \
    avahi-daemon \
    avahi-utils

# Put /usr/lib/ndi on the dynamic-linker path so dlopen("libndi.so") resolves once the (licensing-
# restricted) NDI lib is copied in — without it a fresh box fails on "libndi.so: cannot open shared
# object file" even though the lib is present at /usr/lib/ndi.
echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
ldconfig

# libndi browses mDNS via avahi-daemon for NDI source discovery — enable it so a fresh --display
# receiver can find sources (with no daemon, NDI find() returns nothing and the display stays black).
systemctl enable avahi-daemon

# #295/#307: harden the appliance kernel at the SOURCE. This builds the "clean Ubuntu + SSH" base
# image that setup.sh later hardens, so there is a narrow first-boot window (before setup.sh runs)
# where the original brick exposure exists. Pin the kernel so a surprise kernel can never be
# installed, and disable unattended upgrades — an active unattended-upgrades auto-installed the
# initrd-less kernel that bricked CAM3/CAM4. (Same idiom as setup.sh harden_appliance_kernel.)
apt-mark hold linux-image-generic linux-headers-generic linux-generic 2>/dev/null || true

cat > /etc/apt/apt.conf.d/20auto-upgrades << 'AUTOUPG_EOF'
// Camera-box appliance: never auto-update. An unattended kernel install without an initrd bricked
// CAM3/CAM4 (#295). Kernels are pinned with `apt-mark hold`; updates are operator-driven.
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
AUTOUPG_EOF

# Belt-and-braces: even if unattended-upgrades is ever re-enabled, kernels stay blacklisted.
cat > /etc/apt/apt.conf.d/51camera-box-no-kernel-autoupgrade << 'NOKERNEL_EOF'
// #295: never let unattended-upgrades touch the kernel on the appliance.
Unattended-Upgrade::Package-Blacklist {
    "linux-image";
    "linux-headers";
    "linux-generic";
};
NOKERNEL_EOF

# Create user newlevel
useradd -m -s /bin/bash -G sudo newlevel
echo "newlevel:newlevel" | chpasswd

# Set root password
echo "root:newlevel" | chpasswd

# Configure SSH - enable root login and password auth
# Handle both commented and uncommented lines
sed -i 's/^#*PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config
sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config

# Generate SSH host keys NOW (not at first boot)
ssh-keygen -A

# Enable SSH service
systemctl enable ssh

# Configure netplan for DHCP on all ethernet interfaces
mkdir -p /etc/netplan
cat > /etc/netplan/01-netcfg.yaml << 'NETEOF'
network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        driver: "*"
      dhcp4: true
NETEOF

# Set correct permissions on netplan config
chmod 600 /etc/netplan/01-netcfg.yaml

# Generate networkd config from netplan
netplan generate

# Enable networkd (netplan uses it as renderer)
systemctl enable systemd-networkd
systemctl enable systemd-resolved

# Link resolv.conf to systemd-resolved
ln -sf /run/systemd/resolve/stub-resolv.conf /etc/resolv.conf

# Configure GRUB for physical console (NOT serial)
# #295/#307: GRUB_DEFAULT=saved pins the default to an explicitly-saved known-good kernel rather
# than whatever is "newest" — a boot-newest default is how an initrd-less auto-installed kernel
# became the default and bricked CAM3/CAM4.
cat > /etc/default/grub << 'GRUBEOF'
GRUB_DEFAULT=saved
GRUB_TIMEOUT=3
GRUB_DISTRIBUTOR="Ubuntu"
GRUB_CMDLINE_LINUX_DEFAULT=""
GRUB_CMDLINE_LINUX="console=tty0"
GRUB_TERMINAL="console"
# #344: do NOT let os-prober add cross-disk stowaway entries (the build host's
# other disks) to this image's menu.
GRUB_DISABLE_OS_PROBER=true
GRUBEOF

# Install GRUB. grub-install lays down the /boot/grub/x86_64-efi modules + grubx64,
# but its --removable BOOTX64.EFI core is BROKEN on Ubuntu 24.04 (a live-media probe
# drops it to the grub> rescue prompt and the USB never boots — #344). So after
# grub-install we OVERWRITE that core with a clean grub-mkimage core that chains
# straight to grub.cfg by root fs UUID (see /tmp/install-grub-efi.sh).
source /tmp/install-grub-efi.sh
grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=ubuntu --removable
update-grub
# #295/#307: pin index 0 (the single freshly-installed known-good kernel, with its initrd) as the
# saved default so the base image boots deterministically the kernel it was provisioned with.
grub-set-default 0
# #344: replace the broken removable core with a clean, bootable one.
build_grub_efi_core /boot/efi "$(grub-probe --target=fs_uuid /)"

# Clean up
apt-get clean
rm -rf /var/lib/apt/lists/*

# Verify critical files exist
echo "Verifying installation..."
ERRORS=0

# Check SSH host keys
if [ ! -f /etc/ssh/ssh_host_ed25519_key ]; then
    echo "ERROR: SSH host keys missing!"
    ERRORS=$((ERRORS+1))
fi

# Check SSH config
if ! grep -q "^PermitRootLogin yes" /etc/ssh/sshd_config; then
    echo "ERROR: PermitRootLogin not enabled!"
    ERRORS=$((ERRORS+1))
fi

# Check netplan config
if [ ! -f /etc/netplan/01-netcfg.yaml ]; then
    echo "ERROR: Netplan config missing!"
    ERRORS=$((ERRORS+1))
fi

# Check kernel
if [ ! -f /boot/vmlinuz-* ]; then
    echo "ERROR: Kernel not installed!"
    ERRORS=$((ERRORS+1))
fi

# Check GRUB
if [ ! -f /boot/efi/EFI/BOOT/BOOTX64.EFI ]; then
    echo "ERROR: GRUB EFI not installed!"
    ERRORS=$((ERRORS+1))
fi

# Check user exists
if ! id newlevel &>/dev/null; then
    echo "ERROR: User newlevel not created!"
    ERRORS=$((ERRORS+1))
fi

if [ $ERRORS -gt 0 ]; then
    echo "FAILED: $ERRORS errors found!"
    exit 1
fi

echo "All verifications passed!"
echo "Setup complete!"
SETUP_EOF

    chmod +x "$MOUNT_ROOT/tmp/setup.sh"

    # #344: make the shared GRUB EFI core installer available inside the chroot
    # (setup.sh sources it to overwrite the broken --removable core).
    cp "$SCRIPT_DIR/lib/install-grub-efi.sh" "$MOUNT_ROOT/tmp/install-grub-efi.sh"

    log "Running configuration inside chroot..."
    chroot "$MOUNT_ROOT" /tmp/setup.sh

    # Clean up setup script
    rm -f "$MOUNT_ROOT/tmp/setup.sh" "$MOUNT_ROOT/tmp/install-grub-efi.sh"

    # #369: install auto-grow-root first-boot service into the rw-root image.
    # growpart (from cloud-guest-utils, installed above) expands root to fill the disk on first boot
    # so a fresh USB key always uses the full disk. cam4 shipped 3.5G/92%-full on a 57G disk.
    # Fault-tolerant: script writes the marker even when grow/resize fails (non-fatal exit).
    mkdir -p "$MOUNT_ROOT/usr/local/sbin"
    install -m 0755 "$SCRIPT_DIR/lib/camera-box-grow-root.sh" \
        "$MOUNT_ROOT/usr/local/sbin/camera-box-grow-root.sh"
    install -m 0644 "$SCRIPT_DIR/../systemd/camera-box-grow-root.service" \
        "$MOUNT_ROOT/etc/systemd/system/camera-box-grow-root.service"
    chroot "$MOUNT_ROOT" systemctl enable camera-box-grow-root.service
}

# Cleanup and unmount
cleanup() {
    log "Cleaning up..."

    # Unmount in reverse order
    umount "$MOUNT_ROOT/dev/pts" 2>/dev/null || true
    umount "$MOUNT_ROOT/dev" 2>/dev/null || true
    umount "$MOUNT_ROOT/proc" 2>/dev/null || true
    umount "$MOUNT_ROOT/sys" 2>/dev/null || true
    umount "$MOUNT_ROOT/boot/efi" 2>/dev/null || true
    umount "$MOUNT_ROOT" 2>/dev/null || true

    sync
}

# Main
main() {
    check_requirements
    confirm_device
    cleanup_mounts
    partition_drive
    mount_filesystems
    install_base
    configure_system
    cleanup

    echo ""
    log "========================================="
    log "USB Linux installation complete!"
    log "========================================="
    log "User: newlevel"
    log "Password: newlevel"
    log "Root SSH: enabled (password: newlevel)"
    log "Network: DHCP on all ethernet interfaces"
    log ""
    log "You can now remove the USB and boot from it."
}

# Run with cleanup on error
trap cleanup EXIT
main
