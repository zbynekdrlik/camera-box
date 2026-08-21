#!/bin/bash
set -euo pipefail

# Ubuntu 24.04 (noble) one-shot cam-box installer.
# Debootstraps a fresh appliance rootfs to the target disk with SSH + DHCP only.
# User: newlevel, Password: newlevel, Root SSH enabled.
#
# ONE-SHOT CONTRACT (#448): a fresh box installs → boots → is SSH-reachable with NO manual
# post-install patching. The boot-critical steps that make this true (all of which bit us live
# on cam5 + cam6, 2026-07-03):
#   * FAIL-LOUD dep check (check_required_files) BEFORE any disk write — the script copies sibling
#     files (lib/install-grub-efi.sh, lib/camera-box-grow-root.sh, ../systemd/…grow-root.service)
#     into the target; a MISSING sibling once broke the install mid-way AFTER partitioning (cam6).
#   * MASK systemd-networkd-wait-online in the chroot — unbounded, it stalled boot before
#     multi-user.target so sshd never started (cam5/cam6 pinged but :22 was dead).
#   * NAMED NVRAM UEFI boot entry (create_efi_boot_entry, host side) — grub-install --removable
#     writes only \EFI\BOOT\BOOTX64.EFI and NO NVRAM entry, so firmware could boot the live-USB or
#     a stale Windows entry instead of the fresh install.
#   * MASK the ro-root grub units (grub-common + grub-initrd-fallback) so they don't land FAILED.

# Args:
#   /dev/sdX                positional target (refuses /dev/sda for safety)
#   --target-disk /dev/sdX  EXPLICIT target, ALLOWED even for /dev/sda — safe when run
#                           ON a box's live-USB where /dev/sda is the internal target disk
#                           (removes the per-install guard-patch hack; #448 unified method)
#   --yes | -y              non-interactive: skip the 'type yes' confirmation
DEVICE=""
FORCE_TARGET=0
ASSUME_YES=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target-disk) DEVICE="${2:-}"; FORCE_TARGET=1; shift 2 ;;
        --yes|-y)      ASSUME_YES=1; shift ;;
        -*)            echo "Unknown option: $1" >&2; exit 1 ;;
        *)             DEVICE="$1"; shift ;;
    esac
done
MOUNT_ROOT="/mnt/usb-root"
MOUNT_EFI="/mnt/usb-efi"
SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

# shellcheck source=scripts/lib/log-bound.sh
. "$SCRIPT_DIR/lib/log-bound.sh"  # log_bound_logrotate_config/log_bound_timer_dropin (#679) --
                                  # SAME source of truth as setup-device.sh/verify-device.sh

# shellcheck source=scripts/lib/log-diet.sh
. "$SCRIPT_DIR/lib/log-diet.sh"  # log_diet_journald_dropin (#762) -- SAME source of truth as
                                 # setup-device.sh/verify-device.sh

# shellcheck source=scripts/lib/udev-camera-box.sh
. "$SCRIPT_DIR/lib/udev-camera-box.sh"  # udev_camera_box_rules_content/
                                        # udev_camera_box_helper_script_content (#894) -- SAME
                                        # source of truth as setup-device.sh/verify-device.sh

# shellcheck source=scripts/lib/dscp-nft.sh
. "$SCRIPT_DIR/lib/dscp-nft.sh"  # dscp_nft_ruleset_content/dscp_nft_service_unit_content
                                 # (dantesync issue 52) -- SAME source of truth as
                                 # setup-device.sh/verify-device.sh for the NTP-client DSCP
                                 # nftables OUTPUT-mangle rule (udp dport 123 -> dscp ef) + oneshot

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

    # Safety: refuse /dev/sda UNLESS explicitly targeted via --target-disk (see arg parsing)
    if [[ "$DEVICE" == "/dev/sda" && "$FORCE_TARGET" -ne 1 ]]; then
        error "Refusing to write to /dev/sda (system disk). Use --target-disk /dev/sda if that IS the intended target (e.g. running on a box's live-USB where the internal disk is /dev/sda)."
    fi

    # Check required tools
    for cmd in debootstrap parted mkfs.vfat mkfs.ext4 mount chroot; do
        command -v $cmd &>/dev/null || error "Missing required tool: $cmd"
    done

    # #448: FAIL LOUD if any required sibling file is missing — BEFORE any disk write. Never
    # partition a disk when a file a LATER step needs is absent (see check_required_files).
    check_required_files
}

# #448: verify every sibling file this script copies into the target rootfs exists, BEFORE the
# first destructive disk op. On cam6, lib/camera-box-grow-root.sh was ABSENT when the script ran
# from a copied dir → the `install` of that file failed mid-way AFTER partition_drive had already
# wiped + partitioned the disk, leaving a half-installed, unbootable box. This guard turns that
# into an early, safe, loud failure that never touches the disk.
# Referenced (kept in sync with configure_system's copies):
#   $SCRIPT_DIR/lib/install-grub-efi.sh              (sourced in the chroot, #344)
#   $SCRIPT_DIR/lib/camera-box-grow-root.sh          (installed to /usr/local/sbin, #369)
#   $SCRIPT_DIR/../systemd/camera-box-grow-root.service (installed as a systemd unit, #369)
check_required_files() {
    log "Checking required sibling files..."
    local f
    for f in \
        "$SCRIPT_DIR/lib/install-grub-efi.sh" \
        "$SCRIPT_DIR/lib/camera-box-grow-root.sh" \
        "$SCRIPT_DIR/../systemd/camera-box-grow-root.service"; do
        [[ -f "$f" ]] || error "Missing required file: $f — refusing to partition. Run \
create-usb-linux.sh from a full repo checkout (its lib/ and systemd/ siblings MUST be present)."
    done
    log "All required sibling files present."
}

# Confirm with user
confirm_device() {
    log "Target device: $DEVICE"
    lsblk "$DEVICE" -o NAME,SIZE,MODEL,MOUNTPOINT
    echo ""
    warn "ALL DATA ON $DEVICE WILL BE DESTROYED!"
    if [[ "$ASSUME_YES" -eq 1 ]]; then
        log "Non-interactive (--yes): proceeding."
    else
        read -rp "Type 'yes' to continue: " confirm
        [[ "$confirm" == "yes" ]] || error "Aborted by user"
    fi
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

    # #679: bound /var/log against runaway growth from ANY chatty logger, baked in from first
    # boot -- this closes the narrow window before setup-device.sh later converts /var/log to a
    # fixed 50MB tmpfs (setup-device.sh STEP 18 writes the identical two files onto that tmpfs;
    # this base image already carries them so a box is protected even before setup-device.sh ever
    # runs). See scripts/lib/log-bound.sh for the full incident writeup.
    mkdir -p "$MOUNT_ROOT$(dirname "$LOG_BOUND_TIMER_DROPIN_PATH")"
    log_bound_logrotate_config > "$MOUNT_ROOT$LOG_BOUND_LOGROTATE_PATH"
    log_bound_timer_dropin > "$MOUNT_ROOT$LOG_BOUND_TIMER_DROPIN_PATH"

    # #762: cap journald's own RuntimeMaxUse from first boot too (the rsyslog PURGE itself runs
    # inside the chroot below, alongside the #591/#597 competing-daemon purge -- this file write
    # just needs to land before that chroot script runs). See scripts/lib/log-diet.sh.
    mkdir -p "$MOUNT_ROOT$(dirname "$LOG_DIET_JOURNALD_DROPIN_PATH")"
    log_diet_journald_dropin > "$MOUNT_ROOT$LOG_DIET_JOURNALD_DROPIN_PATH"

    # #894: bake the SAME conditional hotplug-restart + autosuspend-reapply udev rule setup-device.sh
    # installs (see scripts/lib/udev-camera-box.sh) into the base image too -- a fresh USB build must
    # never regress to the old fleet's UNCONDITIONAL "restart camera-box.service on hotplug" rule
    # (traced to the retired scripts/setup.sh, #563) for even one boot before a re-provisioning pass.
    # Plain file writes, no chroot needed (nothing here calls apt/systemctl).
    mkdir -p "$MOUNT_ROOT/etc/udev/rules.d" "$MOUNT_ROOT/usr/local/bin"
    udev_camera_box_rules_content > "$MOUNT_ROOT/etc/udev/rules.d/99-camera-box.rules"
    udev_camera_box_helper_script_content > "$MOUNT_ROOT/usr/local/bin/camera-box-udev-video-add.sh"
    chmod +x "$MOUNT_ROOT/usr/local/bin/camera-box-udev-video-add.sh"

    # dantesync issue 52 (camera-box provisioning half): bake the NTP-client DSCP nftables rule +
    # its boot oneshot into the base image too (same dual-bake as the udev/log-diet writes above),
    # so a freshly-imaged box marks its NTP requests EF from first boot -- before setup-device.sh
    # ever re-runs. dantesync's rsntp Linux client has no socket handle to setsockopt(IP_TOS); a
    # dedicated `table ip dantesync_dscp` + oneshot closes the request half (scripts/lib/dscp-nft.sh).
    # Plain host-side file writes (like the udev bake above); the `nftables` install + `systemctl
    # enable dantesync-dscp` happen inside the chroot setup.sh below (they need apt/systemctl).
    mkdir -p "$MOUNT_ROOT$(dirname "$DSCP_NFT_RULESET_PATH")" "$MOUNT_ROOT$(dirname "$DSCP_NFT_SERVICE_PATH")"
    dscp_nft_ruleset_content > "$MOUNT_ROOT$DSCP_NFT_RULESET_PATH"
    dscp_nft_service_unit_content > "$MOUNT_ROOT$DSCP_NFT_SERVICE_PATH"

    # #448 (2026-07-18 rescope, event finding #8): force-load the Intel iGPU DRM module at boot so a
    # HEADLESS first boot (no monitor attached) still brings up /dev/dri + /dev/fb0 for the painter /
    # cameraman-monitor framebuffer chain. On cam5-class hardware `i915` is only udev-probed when a
    # display is present, so a monitor-less box came up with no framebuffer (no cameraman monitor);
    # cam5 was recovered live by pinning `i915` in modules-load.d. systemd-modules-load.service (a
    # static, always-run unit) reads /etc/modules-load.d/*.conf and force-loads each named module --
    # no enable step needed. Plain host-side write (no chroot; nothing here calls apt/systemctl),
    # mirroring the udev/log-bound bakes above. The OPERATOR half -- first PHYSICAL boot of a GPU box
    # WITH a monitor connected so the connector/framebuffer initializes -- is documented in SETUP.md.
    mkdir -p "$MOUNT_ROOT/etc/modules-load.d"
    cat > "$MOUNT_ROOT/etc/modules-load.d/i915.conf" << 'MODLOAD_EOF'
# camera-box (#448): force the Intel iGPU DRM module at boot so a headless first boot (no monitor)
# still brings up /dev/dri + /dev/fb0 for the painter / cameraman monitor. A future non-Intel cam
# box swaps in its platform's DRM module name here (one module name per line).
i915
MODLOAD_EOF

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
    nftables \
    cloud-guest-utils

# #362: bake the NDI/audio RUNTIME deps into the base image so a fresh clone can RUN camera-box
# without hand-provisioning. The fresh CAM3 clone (#301 re-image) booted but camera-box crash-looped
# because libndi.so could not dlopen: the ALSA runtime + the avahi client/common libs were absent,
# /usr/lib/ndi was not on the dynamic-linker path, and no avahi-daemon ran for the mDNS NDI-source
# discovery libndi performs. avahi-daemon is mDNS only — no conflict with DanteSync's clock ownership
# (cam4 runs both). These are public Ubuntu packages (main/universe), installable in the chroot.
# #743: psmisc (provides `fuser`) joins the SAME dual-bake here + setup-device.sh -- a fresh
# cam2 clone (2026-07-13) had no `fuser` at all, false-FAILing rig-mode.sh's #464 KMS-held check
# AND silently no-op'ing recording-e2e.sh's capture-release busy-wait (`fuser` exits 127 ->
# the `while` loop's condition reads false immediately, same as "already released").
# #782: alsa-utils (provides `amixer`/`alsactl`) joins the SAME dual-bake -- the oldest boxes
# (cam1/cam3) were provisioned before it was in either list, so their interkom Mic/PCM gain was
# neither readable nor persisted across boot. Baked into the base image AND applied by
# setup-device.sh STEP 16 (per-box gain via `amixer` + `alsactl store`). The asound.conf/mixer
# BAKE stays in setup-device.sh, not here -- it needs the live HID card present, which the
# chroot base-image build does not have.
apt-get install -y \
    libasound2t64 \
    alsa-utils \
    libavahi-client3 \
    libavahi-common3 \
    avahi-daemon \
    avahi-utils \
    psmisc

# Put /usr/lib/ndi on the dynamic-linker path so dlopen("libndi.so") resolves once the (licensing-
# restricted) NDI lib is copied in — without it a fresh box fails on "libndi.so: cannot open shared
# object file" even though the lib is present at /usr/lib/ndi.
echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
ldconfig

# libndi browses mDNS via avahi-daemon for NDI source discovery — enable it so a fresh --display
# receiver can find sources (with no daemon, NDI find() returns nothing and the display stays black).
systemctl enable avahi-daemon

# #295/#307: harden the appliance kernel at the SOURCE. This builds the "clean Ubuntu + SSH" base
# image that setup-device.sh later hardens, so there is a narrow first-boot window (before
# setup-device.sh runs) where the original brick exposure exists. Pin the kernel so a surprise
# kernel can never be installed, and disable unattended upgrades — an active unattended-upgrades
# auto-installed the initrd-less kernel that bricked CAM3/CAM4. (Same idiom as setup-device.sh's
# kernel-pin step; scripts/setup.sh, which used to carry this too, was retired in #563.)
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

# #591: dantesync is the rig's SOLE clock authority (installed later by setup-device.sh). The base
# debootstrap image ships systemd-timesyncd; a minimalist cambox/imag appliance must run NO other
# timesync daemon (cam5/cam6 ran systemd-timesyncd alongside dantesync -> a real 5.28s clock
# desync). PURGE it from the image so a freshly-imaged box never ships a 2nd timesync daemon, then
# mask it as a backstop. chrony/ntp/ntpsec/openntpd are not in the base image but are purged too as
# belt-and-suspenders (a no-op when absent).
for _ts in systemd-timesyncd chrony ntp ntpsec openntpd; do
    systemctl disable --now "$_ts" 2>/dev/null || true
    apt-get purge -y "$_ts" 2>/dev/null \
        || dpkg --purge --force-depends "$_ts" 2>/dev/null || true
    systemctl mask "$_ts" 2>/dev/null || true
done
# #597: same purge for linuxptp (ptp4l/phc2sys) -- a rogue PTP daemon would fight dantesync's own
# PTP servo directly. Package name ("linuxptp") differs from unit names ("ptp4l"/"phc2sys"), not
# in the base image today but purged as belt-and-suspenders (a no-op when absent). Same
# disable -> purge -> mask order as the loop above.
for _u in ptp4l phc2sys; do
    systemctl disable --now "$_u" 2>/dev/null || true
done
apt-get purge -y linuxptp 2>/dev/null \
    || dpkg --purge --force-depends linuxptp 2>/dev/null || true
for _u in ptp4l phc2sys; do
    systemctl mask "$_u" 2>/dev/null || true
done

# #762: rsyslog is REDUNDANT on this appliance -- journald already captures everything, and
# nothing reads /var/log/syslog on a read-only appliance with no operator logging in. A live
# incident (cam1, 2026-07-15) showed rsyslogd enter a write-error feedback loop once the 50MB
# /var/log tmpfs filled, burning 42.8% CPU and starving the camera-box send path. Same
# disable -> purge -> mask discipline as the #591/#597 stanzas above -- purge, not just mask.
# The journald RuntimeMaxUse drop-in itself is already written onto the base image above
# (before this chroot script runs); this only needs to purge the redundant daemon.
systemctl disable --now rsyslog 2>/dev/null || true
apt-get purge -y rsyslog 2>/dev/null \
    || dpkg --purge --force-depends rsyslog 2>/dev/null || true
systemctl mask rsyslog 2>/dev/null || true

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

# Configure netplan for DHCP on the PCI LAN NIC (enp*) only. #1155: pin the LAN stanza by NIC
# NAME, never the driver wildcard -- a driver-wildcard match also claims a USB CDC-NCM camera
# link (bkshading, issue 808) and gives it the box IP + a duplicate default route, making the box
# PTP-deaf (cam1 live incident 2026-08-20). setup-device.sh STEP 2 later pins the same name: match.
mkdir -p /etc/netplan
cat > /etc/netplan/01-netcfg.yaml << 'NETEOF'
network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        name: "enp*"
      dhcp4: true
NETEOF

# Set correct permissions on netplan config
chmod 600 /etc/netplan/01-netcfg.yaml

# Generate networkd config from netplan
netplan generate

# Enable networkd (netplan uses it as renderer)
systemctl enable systemd-networkd
systemctl enable systemd-resolved

# dantesync issue 52 (camera-box provisioning half): enable the boot-time oneshot that applies the
# NTP-client DSCP nftables rule (its unit + ruleset file were baked into the image host-side above;
# the `nftables` package is installed above). Enable-only -- the rule applies on first boot. rsntp's
# Linux client cannot setsockopt(IP_TOS), so this marks the request direction (scripts/lib/dscp-nft.sh).
systemctl enable dantesync-dscp

# #448: MASK systemd-networkd-wait-online. The base debootstrap pulls it in with NO
# `--interface`/`--any` bound, so on first boot it waits for EVERY interface to be fully
# configured; ordered before network-online.target it stalls the boot BEFORE multi-user.target,
# so sshd never starts — cam5 AND cam6 pinged (link up) but :22 was dead and needed a console
# `systemctl restart ssh` to recover. A cam box needs DHCP-when-it-arrives, not a boot-blocking
# wait. Masking removes the wait entirely. Idempotent (ln -sf overwrites any prior link).
ln -sf /dev/null /etc/systemd/system/systemd-networkd-wait-online.service

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

# #448: MASK the two grub systemd units that assume a package-managed rw-root grub install.
# On the appliance image they land in FAILED state on every boot (grub-initrd-fallback tries a
# fallback initrd the pinned image doesn't use; grub-common expects an apt-driven grub upgrade
# path) — noise in `systemctl --failed` and a false "degraded" boot state. The bootloader is
# already installed + pinned by hand above (grub-install + build_grub_efi_core + grub-set-default),
# so these units have no job to do. Mask them for a clean boot. Idempotent (ln -sf).
ln -sf /dev/null /etc/systemd/system/grub-common.service
ln -sf /dev/null /etc/systemd/system/grub-initrd-fallback.service

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

# #448: create a NAMED NVRAM UEFI boot entry for the freshly-installed disk so firmware boots the
# INSTALL, not the live-USB or a stale Windows entry. This MUST run on the HOST (efivars are not
# mounted inside the debootstrap chroot), so it lives here, called from main() after grub is laid
# down. grub-install --removable wrote ONLY \EFI\BOOT\BOOTX64.EFI and NO NVRAM entry — on cam5 the
# box re-booted the installer/dead-Windows entry until an efibootmgr entry was added by hand.
#
# GUARDED: only acts when the host booted UEFI (/sys/firmware/efi/efivars present). If efivars are
# absent (BIOS/CSM boot, or not mounted) it logs that the operator must REMOVE the live-USB so
# firmware falls back to the internal disk's \EFI\BOOT\BOOTX64.EFI, and does nothing else.
# Idempotent: removes any prior `cam-box` entries before creating a fresh one. Non-fatal — the
# --removable BOOTX64.EFI fallback still boots if this can't write NVRAM.
create_efi_boot_entry() {
    if [[ ! -d /sys/firmware/efi/efivars ]]; then
        warn "Host has no EFI vars (/sys/firmware/efi/efivars absent) — booted in BIOS/CSM mode or"
        warn "efivars not mounted. NOT creating an NVRAM boot entry. After install completes, REMOVE"
        warn "the live-USB so firmware boots the internal disk's \\EFI\\BOOT\\BOOTX64.EFI fallback."
        return 0
    fi
    if ! command -v efibootmgr &>/dev/null; then
        warn "efibootmgr not installed on the host live-USB — skipping the NVRAM boot entry. REMOVE"
        warn "the live-USB after install so firmware boots \\EFI\\BOOT\\BOOTX64.EFI on $DEVICE."
        return 0
    fi

    log "Creating named UEFI boot entry 'cam-box' for $DEVICE (ESP = partition 1)..."

    # Idempotent: delete any existing cam-box entries so re-runs don't stack duplicates.
    local existing bn
    # NB: [0-9A-Fa-f]+ (not {4}) — portable across mawk (Ubuntu default) which lacks interval exprs.
    existing=$(efibootmgr 2>/dev/null | awk \
        '$2=="cam-box" && $1 ~ /^Boot[0-9A-Fa-f]+\*?$/ {n=$1; sub(/^Boot/,"",n); sub(/\*$/,"",n); print n}') || true
    for bn in $existing; do
        efibootmgr -b "$bn" -B >/dev/null 2>&1 || true
    done

    # Create the entry pointing at the REAL removable core (grub-install --removable + #344 wrote
    # \EFI\BOOT\BOOTX64.EFI; the \EFI\ubuntu\grubx64.efi path does NOT exist with --removable).
    # efibootmgr -c PREPENDS the new entry to BootOrder, so it becomes first automatically.
    if efibootmgr -c -d "$DEVICE" -p 1 -L cam-box -l '\EFI\BOOT\BOOTX64.EFI' >/dev/null; then
        log "UEFI boot entry 'cam-box' created and set first in BootOrder for $DEVICE."
    else
        warn "efibootmgr failed to write the NVRAM entry — REMOVE the live-USB after install so"
        warn "firmware boots the internal disk's \\EFI\\BOOT\\BOOTX64.EFI fallback."
    fi
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
    check_requirements       # incl. check_required_files — fail loud BEFORE any disk write (#448)
    confirm_device
    cleanup_mounts
    partition_drive
    mount_filesystems
    install_base
    configure_system         # lays down grub + masks networkd-wait-online / ro-root grub units (#448)
    create_efi_boot_entry    # host-side named NVRAM UEFI entry, guarded on efivars (#448)
    cleanup

    echo ""
    log "========================================="
    log "USB Linux installation complete!"
    log "========================================="
    log "User: newlevel"
    log "Password: newlevel"
    log "Root SSH: enabled (password: newlevel)"
    log "Network: DHCP on all ethernet interfaces"
    log "Boot: sshd starts on first boot (networkd-wait-online masked #448)"
    if [[ -d /sys/firmware/efi/efivars ]]; then
        log "Boot: named 'cam-box' UEFI entry created — this box will boot the internal disk."
    else
        warn "Boot: host has no EFI vars — REMOVE the USB so firmware boots the internal disk's"
        warn "      \\EFI\\BOOT\\BOOTX64.EFI (no NVRAM entry could be created)."
    fi
    log ""
    log "You can now remove the USB and boot from it."
}

# Run with cleanup on error — UNLESS sourced for unit tests (CREATE_USB_SOURCE_ONLY=1), which
# loads the functions (check_required_files, create_efi_boot_entry, arg parsing) WITHOUT executing
# the installer. Normal invocation (env var unset) runs exactly as before.
if [[ "${CREATE_USB_SOURCE_ONLY:-0}" != "1" ]]; then
    trap cleanup EXIT
    main
fi
