#!/bin/bash
set -euo pipefail

# Camera-Box Device Setup Script
# Usage: curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/setup.sh | sudo bash
# Usage: curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/setup.sh | sudo bash -s CAM1
#
# This script transforms a fresh Ubuntu installation into a fully configured camera-box device.

# Accept hostname as first argument, default to "camera-box"
DEVICE_HOSTNAME="${1:-camera-box}"

REPO="zbynekdrlik/camera-box"
DANTE_REPO="zbynekdrlik/dantesync"
INSTALL_DIR="/usr/local/bin"
NDI_DIR="/usr/lib/ndi"
CONFIG_DIR="/etc/camera-box"

# HDMI cameraman preview (#528, #557): scripts/camera-set.sh's CAMERA_DISPLAY_SOURCE table is the
# fleet's single source of truth for which NDI source a box's --display renders. This script (a
# standalone `curl | sudo bash` quick-install path with no local repo checkout) downloads the SAME
# camera-set.sh from this repo rather than keeping a second, hand-copied table -- the table stays
# the ONE source of truth (#557's fix). Overridable (URL or local path) for tests / local dry-runs.
CAMERA_SET_SOURCE="${CAMERA_SET_SOURCE:-https://raw.githubusercontent.com/${REPO}/main/scripts/camera-set.sh}"

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

header() {
    echo ""
    echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
    echo -e "${BLUE}  $*${NC}"
    echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
    echo ""
}

# --- PURE functions (no root, no network beyond fetch_camera_set's own download -- unit-tested
# from tests/setup_display_table_pure_functions.rs; the BASH_SOURCE guard at the bottom of this
# file skips the destructive `main "$@"` install flow when sourced. Same convention as
# scripts/setup-device.sh / scripts/setup-imag.sh.) -------------------------------------------

# fetch_camera_set DEST -- writes the CURRENT scripts/camera-set.sh (the #528/#557 single source
# of truth for the CAMERA_DISPLAY_SOURCE table) to DEST. CAMERA_SET_SOURCE may be a URL (curl) or
# a local path (cp) -- overridden by tests to point at the real local file so no network call
# happens; production curl-pipe installs use the default GitHub raw URL.
fetch_camera_set() {
    local dest="$1"
    case "$CAMERA_SET_SOURCE" in
        http://* | https://*) curl -fsSL "$CAMERA_SET_SOURCE" -o "$dest" ;;
        *) cp "$CAMERA_SET_SOURCE" "$dest" ;;
    esac
}

# resolve_display_source HOSTNAME -- resolves HOSTNAME (case-insensitive) to its
# CAMERA_DISPLAY_SOURCE table entry via the REAL scripts/camera-set.sh (fetched by
# fetch_camera_set, never duplicated here -- #557). Echoes the resolved source, or nothing for a
# box with no configured preview. Unlike setup-device.sh's resolve_device_name, an UNRECOGNIZED
# hostname is NOT fatal here: setup.sh's hostname argument defaults to "camera-box" and is not
# required to be a fleet camN name, so an unresolved name falls back to "no preview" -- the same
# safe default every box had before #528/#557 baked a universal preview into every ExecStart.
resolve_display_source() {
    local hostname_arg="${1:-}"
    local lc
    lc="$(printf '%s' "$hostname_arg" | tr '[:upper:]' '[:lower:]')"
    local tmp
    tmp="$(mktemp)"
    if fetch_camera_set "$tmp" 2>/dev/null; then
        # shellcheck source=scripts/camera-set.sh
        . "$tmp"
        rm -f "$tmp"
        if camera_resolve "$lc" 2>/dev/null; then
            printf '%s' "$CAMERA_DISPLAY_SOURCE"
            return 0
        fi
    else
        rm -f "$tmp"
    fi
    printf ''
}

# config_toml_display_section SOURCE -- byte-identical contract to setup-device.sh's function of
# the same name (#528/#557): emits an optional `[display]` config.toml section for a box's HDMI
# cameraman preview NDI source, or nothing when SOURCE is empty. Escapes `\` and `"` before
# interpolating into the TOML string literal (see setup-device.sh's copy for the full rationale).
# `fb_device` is deliberately omitted -- src/config.rs's DisplayConfig serde-defaults it to
# "/dev/fb0" when absent.
config_toml_display_section() {
    local source="${1:-}"
    [ -n "$source" ] || return 0
    local escaped="${source//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    printf '\n# HDMI cameraman preview (#528/#557 -- CAMERA_DISPLAY_SOURCE table, scripts/camera-set.sh)\n[display]\nsource = "%s"\n' "$escaped"
}

# Check if running as root
check_root() {
    if [[ $EUID -ne 0 ]]; then
        error "This script must be run as root (use sudo)"
    fi
}

# Set hostname
set_hostname() {
    header "Setting Hostname"

    local current_hostname
    current_hostname=$(hostname)

    if [[ "$current_hostname" == "$DEVICE_HOSTNAME" ]]; then
        info "Hostname already set to $DEVICE_HOSTNAME"
        return 0
    fi

    log "Setting hostname to: $DEVICE_HOSTNAME"

    # Set hostname
    hostnamectl set-hostname "$DEVICE_HOSTNAME"
    echo "$DEVICE_HOSTNAME" > /etc/hostname

    # Update /etc/hosts
    sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_HOSTNAME/" /etc/hosts
    if ! grep -q "127.0.1.1" /etc/hosts; then
        echo "127.0.1.1	$DEVICE_HOSTNAME" >> /etc/hosts
    fi

    log "Hostname set to: $DEVICE_HOSTNAME"
}

# Expand disk to use full storage
expand_disk() {
    header "Expanding Disk"

    # Find root partition
    local root_dev
    root_dev=$(findmnt -n -o SOURCE /)
    local disk_dev
    disk_dev=$(echo "$root_dev" | sed 's/[0-9]*$//' | sed 's/p$//')
    local part_num
    part_num=$(echo "$root_dev" | grep -o '[0-9]*$')

    info "Root partition: $root_dev"
    info "Disk device: $disk_dev"
    info "Partition number: $part_num"

    # Check if growpart is available
    if ! command -v growpart &>/dev/null; then
        log "Installing cloud-guest-utils..."
        apt-get update -qq
        apt-get install -y -qq cloud-guest-utils
    fi

    # Get current and potential sizes
    local current_size
    current_size=$(lsblk -b -n -o SIZE "$root_dev" | head -1)
    local disk_size
    disk_size=$(lsblk -b -n -o SIZE "$disk_dev" | head -1)

    info "Current partition size: $(numfmt --to=iec $current_size)"
    info "Disk size: $(numfmt --to=iec $disk_size)"

    # Only expand if there's significant space to gain (>1GB)
    local diff=$((disk_size - current_size))
    if [[ $diff -gt 1073741824 ]]; then
        log "Expanding partition..."
        if growpart "$disk_dev" "$part_num"; then
            log "Partition expanded"

            log "Expanding filesystem..."
            resize2fs "$root_dev"
            log "Filesystem expanded"
        else
            warn "Partition expansion not needed or failed"
        fi
    else
        info "Partition already at maximum size"
    fi

    # Show final size
    df -h /
}

# Update system packages
update_system() {
    header "Updating System"

    log "Updating package lists..."
    apt-get update

    log "Upgrading packages..."
    DEBIAN_FRONTEND=noninteractive apt-get upgrade -y -o Dpkg::Options::="--force-confdef" -o Dpkg::Options::="--force-confold"

    log "Installing essential packages..."
    # Detect correct ALSA package name (Ubuntu 24.04+ uses libasound2t64)
    ALSA_PKG="libasound2"
    if apt-cache show libasound2t64 &>/dev/null; then
        ALSA_PKG="libasound2t64"
    fi

    apt-get install -y \
        curl \
        wget \
        vim \
        htop \
        iotop \
        net-tools \
        ethtool \
        v4l-utils \
        libavahi-client3 \
        libavahi-common3 \
        avahi-daemon \
        avahi-utils \
        ca-certificates \
        "$ALSA_PKG" \
        alsa-utils

    log "Cleaning up..."
    apt-get autoremove -y
    apt-get clean

    log "System updated"
}

# #295: GUARANTEE every installed kernel has an initrd. A kernel without an initrd that becomes the
# grub default cannot mount root — that is exactly what bricked CAM3 + CAM4. Run this BEFORE every
# update-grub so grub can never produce a default entry that fails to boot.
camera_box_ensure_all_kernels_have_initrd() {
    local vmlinuz kver
    for vmlinuz in /boot/vmlinuz-*; do
        [[ -e "$vmlinuz" ]] || continue
        kver="${vmlinuz#/boot/vmlinuz-}"
        if [[ ! -e "/boot/initrd.img-${kver}" ]]; then
            warn "#295: kernel ${kver} has no initrd — generating one before grub"
            update-initramfs -c -k "${kver}"
        fi
    done
}

# #295: refuse to leave a grub default that cannot boot, then pin the saved default to the running
# (known-good) kernel. Validates the generated /boot/grub/grub.cfg default entry references BOTH a
# kernel image AND an initrd; aborts loudly if not (never ship a brickable default).
camera_box_pin_safe_grub_default() {
    local cfg="/boot/grub/grub.cfg" default_block
    [[ -f "$cfg" ]] || {
        warn "#295: $cfg not found — cannot validate grub default"
        return 0
    }
    # grub's default is its first top-level menuentry (saved_entry=0).
    default_block="$(awk '/^[[:space:]]*menuentry /{c++} c==1{print} c==2{exit}' "$cfg")"
    if ! grep -qE '(vmlinuz|[[:space:]]linux )' <<<"$default_block" \
        || ! grep -q 'initrd' <<<"$default_block"; then
        error "#295: grub default entry is missing a kernel image or initrd line — refusing to leave the box brickable"
    fi
    # The default entry (index 0) is now proven to carry both a kernel image and an initrd. Pin it
    # explicitly as the saved default so it boots deterministically. The kernel is held (apt-mark
    # hold), so on a freshly-provisioned appliance index 0 is the single known-good kernel.
    grub-set-default 0
}

# #295: PIN the appliance kernel and disable unattended upgrades. The brick was an auto-installed
# kernel (via active unattended-upgrades) without an initrd becoming the grub default. An appliance
# must never silently gain a new kernel.
harden_appliance_kernel() {
    header "Hardening Appliance Boot (#295)"

    log "Pinning kernel meta-packages (apt-mark hold)..."
    apt-mark hold linux-image-generic linux-headers-generic linux-generic 2>/dev/null || true

    log "Disabling automatic (unattended) upgrades..."
    cat > /etc/apt/apt.conf.d/20auto-upgrades << 'EOF'
// Camera-box appliance: never auto-update. An unattended kernel install without an initrd bricked
// CAM3/CAM4 (#295). Kernels are pinned with `apt-mark hold`; updates are operator-driven.
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
EOF
    # Belt-and-braces: even if unattended-upgrades is ever re-enabled, kernels stay blacklisted.
    cat > /etc/apt/apt.conf.d/51camera-box-no-kernel-autoupgrade << 'EOF'
// #295: never let unattended-upgrades touch the kernel on the appliance.
Unattended-Upgrade::Package-Blacklist {
    "linux-image";
    "linux-headers";
    "linux-generic";
};
EOF
    systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer \
        apt-daily.service apt-daily-upgrade.service 2>/dev/null || true
    systemctl mask unattended-upgrades.service 2>/dev/null || true

    # #295: any FUTURE kernel install must always get an initrd. This /etc/kernel/postinst.d hook
    # sorts before grub's own `zz-update-grub` hook ("zz-camera-box" < "zz-update-grub"), so a
    # missing initrd is regenerated BEFORE grub is updated.
    log "Installing initrd-guarantee kernel hook..."
    mkdir -p /etc/kernel/postinst.d
    cat > /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee << 'EOF'
#!/bin/sh
# #295: guarantee every installed kernel has an initrd (a kernel without one bricked CAM3/CAM4).
set -e
version="$1"
[ -n "$version" ] || exit 0
if [ ! -e "/boot/initrd.img-${version}" ]; then
    update-initramfs -c -k "${version}"
fi
EOF
    chmod +x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee

    log "Kernel pinned; auto-upgrades disabled; initrd guaranteed for every kernel"
}

# Configure system for appliance mode
configure_system() {
    header "Configuring System"

    # --- Set timezone ---
    log "Setting timezone to UTC..."
    timedatectl set-timezone UTC || true

    # --- Disable unnecessary services for fast boot ---
    log "Disabling unnecessary services..."
    local services_to_disable=(
        "apt-daily.timer"
        "apt-daily-upgrade.timer"
        "motd-news.timer"
        "man-db.timer"
    )
    for svc in "${services_to_disable[@]}"; do
        systemctl disable "$svc" 2>/dev/null || true
        systemctl stop "$svc" 2>/dev/null || true
    done

    # Mask slow boot services
    log "Masking slow boot services..."
    local services_to_mask=(
        "snapd.service"
        "snapd.socket"
        "snapd.seeded.service"
        "systemd-networkd-wait-online.service"
        "ModemManager.service"
        "thermald.service"
        "apport.service"
        "pollinate.service"
    )
    for svc in "${services_to_mask[@]}"; do
        systemctl mask "$svc" 2>/dev/null || true
    done

    # --- Disable power saving ---
    log "Disabling power savings..."

    # CPU performance mode
    for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
        [[ -f "$gov" ]] && echo "performance" > "$gov" 2>/dev/null || true
    done

    # USB autosuspend off
    for ctrl in /sys/bus/usb/devices/*/power/control; do
        [[ -f "$ctrl" ]] && echo "on" > "$ctrl" 2>/dev/null || true
    done

    # Network power management off
    for iface in /sys/class/net/*/device/power/control; do
        [[ -f "$iface" ]] && echo "on" > "$iface" 2>/dev/null || true
    done

    # Disable systemd sleep targets
    systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true

    # Disable power button
    mkdir -p /etc/systemd/logind.conf.d
    cat > /etc/systemd/logind.conf.d/disable-power-button.conf << 'EOF'
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF

    # Persistent power settings via rc.local
    cat > /etc/rc.local << 'EOF'
#!/bin/bash
# Camera-box power settings

# CPU performance mode
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -f "$gov" ] && echo "performance" > "$gov" 2>/dev/null
done

# USB autosuspend off
for ctrl in /sys/bus/usb/devices/*/power/control; do
    [ -f "$ctrl" ] && echo "on" > "$ctrl" 2>/dev/null
done

# Network power management off
for iface in /sys/class/net/*/device/power/control; do
    [ -f "$iface" ] && echo "on" > "$iface" 2>/dev/null
done

exit 0
EOF
    chmod +x /etc/rc.local

    # --- Configure kernel parameters ---
    log "Configuring kernel parameters..."
    cat > /etc/sysctl.d/99-camera-box.conf << 'EOF'
# Camera-box performance and reliability tuning

# Reduce swappiness
vm.swappiness = 10

# Increase inotify watches
fs.inotify.max_user_watches = 524288

# Network tuning
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216

# Auto-reboot on kernel panic (10 seconds)
kernel.panic = 10
kernel.panic_on_oops = 1

# Reduce console noise
kernel.printk = 3 4 1 3
EOF
    sysctl -p /etc/sysctl.d/99-camera-box.conf 2>/dev/null || true

    # --- Resilience features ---
    log "Configuring resilience features..."

    # Hardware watchdog - reboot if system hangs
    mkdir -p /etc/systemd/system.conf.d
    cat > /etc/systemd/system.conf.d/watchdog.conf << 'EOF'
[Manager]
RuntimeWatchdogSec=30
RebootWatchdogSec=10min
EOF

    # udev rule to restart camera-box on USB reconnect
    cat > /etc/udev/rules.d/99-camera-box.rules << 'EOF'
# Restart camera-box when USB video device is added
ACTION=="add", SUBSYSTEM=="video4linux", RUN+="/bin/systemctl restart camera-box.service"
EOF
    udevadm control --reload-rules 2>/dev/null || true

    # Force fsck on next boot after unclean shutdown
    tune2fs -c 1 "$(findmnt -n -o SOURCE /)" 2>/dev/null || true

    # --- Configure GRUB for fast + #295 brick-proof boot ---
    log "Configuring GRUB (fast + safe boot)..."
    if [[ -f /etc/default/grub ]]; then
        sed -i 's/GRUB_TIMEOUT=.*/GRUB_TIMEOUT=0/' /etc/default/grub
        sed -i 's/GRUB_TIMEOUT_STYLE=.*/GRUB_TIMEOUT_STYLE=hidden/' /etc/default/grub
        grep -q "GRUB_TIMEOUT_STYLE" /etc/default/grub || echo "GRUB_TIMEOUT_STYLE=hidden" >> /etc/default/grub
        grep -q "GRUB_RECORDFAIL_TIMEOUT" /etc/default/grub || echo "GRUB_RECORDFAIL_TIMEOUT=0" >> /etc/default/grub
        # #295: pin the default to the explicitly-saved known-good kernel, never "newest".
        if grep -q '^GRUB_DEFAULT=' /etc/default/grub; then
            sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' /etc/default/grub
        else
            echo 'GRUB_DEFAULT=saved' >> /etc/default/grub
        fi
        # #295: guarantee initrds BEFORE regenerating grub, then validate + pin a safe default.
        camera_box_ensure_all_kernels_have_initrd
        update-grub
        camera_box_pin_safe_grub_default
    fi

    # --- Configure autologin ---
    log "Configuring automatic console login..."
    mkdir -p /etc/systemd/system/getty@tty1.service.d
    cat > /etc/systemd/system/getty@tty1.service.d/autologin.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear %I $TERM
EOF

    # Add status display on login
    cat > /root/.bash_profile << 'EOF'
# Camera-Box Status on Login
echo ""
echo "╔═══════════════════════════════════════════════════════════════╗"
echo "║                    Camera-Box Device                          ║"
echo "╚═══════════════════════════════════════════════════════════════╝"
echo ""
echo "Hostname: $(hostname)"
echo "IP: $(hostname -I | awk '{print $1}')"
echo "Uptime: $(uptime -p)"
echo ""
echo "Services:"
systemctl is-active camera-box >/dev/null 2>&1 && echo "  camera-box: running" || echo "  camera-box: stopped"
systemctl is-active dantesync >/dev/null 2>&1 && echo "  dantesync: running" || echo "  dantesync: stopped"
systemctl is-active avahi-daemon >/dev/null 2>&1 && echo "  avahi-daemon: running" || echo "  avahi-daemon: stopped"
echo ""
EOF

    # --- Configure read-only filesystem ---
    log "Configuring read-only filesystem..."

    # Configure journald for volatile storage
    mkdir -p /etc/systemd/journald.conf.d
    cat > /etc/systemd/journald.conf.d/volatile.conf << 'EOF'
[Journal]
Storage=volatile
RuntimeMaxUse=30M
EOF

    # Create helper scripts for switching modes
    cat > /usr/local/bin/rw-mode << 'EOF'
#!/bin/bash
mount -o remount,rw /
echo "Root filesystem is now read-write"
EOF
    chmod +x /usr/local/bin/rw-mode

    cat > /usr/local/bin/ro-mode << 'EOF'
#!/bin/bash
sync
mount -o remount,ro /
echo "Root filesystem is now read-only"
EOF
    chmod +x /usr/local/bin/ro-mode

    # Configure fstab for read-only root and tmpfs mounts
    local root_uuid
    root_uuid=$(findmnt -n -o UUID /)

    cat > /etc/fstab << EOF
# Camera-Box filesystem configuration (read-only root)
UUID=$root_uuid / ext4 ro 0 1
tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0
tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0
tmpfs /var/tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=50M 0 0
# #295: size /var/cache >=512M (uniformly across the fleet) so apt can never ENOSPC and leave a
# freshly-installed kernel without its initrd (a 100M /var/cache filled up and did exactly that).
tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0
tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0
EOF

    # --- Enable avahi for mDNS ---
    log "Enabling Avahi (mDNS)..."
    systemctl enable avahi-daemon 2>/dev/null || true
    systemctl start avahi-daemon 2>/dev/null || true

    log "System configured"
}

# Install DanteTimeSync
install_dantesync() {
    header "Installing DanteSync"

    log "Fetching latest DanteSync release..."
    local dante_url
    dante_url=$(curl -fsSL "https://api.github.com/repos/${DANTE_REPO}/releases/latest" 2>/dev/null | \
        grep -o '"browser_download_url": *"[^"]*dantesync-linux-amd64"' | \
        head -1 | cut -d'"' -f4) || true

    if [[ -z "$dante_url" ]]; then
        warn "DanteSync release not found, skipping"
        return 0
    fi

    info "Downloading from: $dante_url"
    local tmp_dir
    tmp_dir=$(mktemp -d)

    if curl -fsSL "$dante_url" -o "$tmp_dir/dantesync"; then
        install -m 755 "$tmp_dir/dantesync" "$INSTALL_DIR/"
        rm -rf "$tmp_dir"

        log "Creating DanteSync service..."
        cat > /etc/systemd/system/dantesync.service << 'EOF'
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
# Sync to the cluster master strih (10.77.9.202), NOT the dantesync binary's default public NTP
# pool (sk.pool.ntp.org / time.cloudflare.com) — every node must discipline its clock to the SAME
# reference or the wall-clock genlock in src/ndi.rs silently diverges across cameras (#8). Use the
# IP, not strih.lan: per CLAUDE.md/targets.md .lan DNS may not resolve on a freshly-provisioned
# read-only-rootfs camera, and a failed resolve makes dantesync fall back to its public-pool
# default and silently desync — the exact regression #8 guards against.
ExecStart=/usr/local/bin/dantesync --ntp-server 10.77.9.202
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

        systemctl daemon-reload
        systemctl enable dantesync
        systemctl start dantesync

        log "DanteSync installed and started"
    else
        warn "Failed to download DanteSync"
    fi
}

# Install camera-box binary
install_camera_box() {
    header "Installing Camera-Box"

    log "Fetching latest camera-box release..."
    local release_url
    release_url=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null | \
        grep -o '"browser_download_url": *"[^"]*camera-box-linux-amd64.tar.gz"' | \
        head -1 | cut -d'"' -f4) || true

    if [[ -z "$release_url" ]]; then
        warn "No camera-box release found"
        info "You can build from source: cargo build --release"
        return 0
    fi

    info "Downloading from: $release_url"
    local tmp_dir
    tmp_dir=$(mktemp -d)

    curl -fsSL "$release_url" -o "$tmp_dir/camera-box.tar.gz"

    log "Extracting and installing..."
    tar -xzf "$tmp_dir/camera-box.tar.gz" -C "$tmp_dir"
    install -m 755 "$tmp_dir/camera-box" "$INSTALL_DIR/"

    # Create systemd service (always use our production config)
    log "Creating camera-box service..."
    cat > /etc/systemd/system/camera-box.service << 'EOF'
[Unit]
Description=Camera Box - USB Video Capture to NDI
Documentation=https://github.com/zbynekdrlik/camera-box
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/camera-box
Restart=always
RestartSec=3

# Run with real-time priority for low latency
Nice=-10
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

# Environment for NDI SDK
Environment=NDI_RUNTIME_DIR_V6=/usr/lib/ndi

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=camera-box

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadOnlyPaths=/
ReadWritePaths=/dev /sys /run

# Allow access to video devices
SupplementaryGroups=video

[Install]
WantedBy=multi-user.target
EOF

    rm -rf "$tmp_dir"

    # Create config directory
    mkdir -p "$CONFIG_DIR"
    if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
        # Generate stream name from hostname (lowercase)
        local stream_name
        stream_name=$(echo "$DEVICE_HOSTNAME" | tr '[:upper:]' '[:lower:]')

        cat > "$CONFIG_DIR/config.toml" << EOF
# Camera-Box Configuration - $DEVICE_HOSTNAME

# NDI source name (appears as "$DEVICE_HOSTNAME (usb)" on network)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"

# VBAN Intercom Configuration
[intercom]
stream = "$stream_name"
target = "strih.lan"
sample_rate = 48000
channels = 2
sidetone_gain = 100.0
mic_gain = 12.0
headphone_gain = 15.0
EOF
        # HDMI cameraman preview (#528/#557): a box with a CAMERA_DISPLAY_SOURCE table entry
        # (scripts/camera-set.sh) gets an appended [display] section here -- the SAME mechanism
        # setup-device.sh uses, resolved via the real camera-set.sh (never a hardcoded value, and
        # never baked into ExecStart -- see the #450 canonical-plain-ExecStart guard above). A box
        # with no table entry (every box except cam1 today) gets nothing appended.
        local display_source
        display_source="$(resolve_display_source "$DEVICE_HOSTNAME")"
        printf '%s' "$(config_toml_display_section "$display_source")" >> "$CONFIG_DIR/config.toml"
        if [[ -n "$display_source" ]]; then
            info "HDMI preview: $display_source (persists across reboot/redeploy, #528/#557)"
        fi
        log "Created config at $CONFIG_DIR/config.toml"
    fi

    # Create NDI directory
    mkdir -p "$NDI_DIR"
    # #362: put /usr/lib/ndi on the dynamic-linker path so dlopen("libndi.so") resolves once the
    # (licensing-restricted) NDI lib is copied in. Without this a fresh box crash-loops camera-box on
    # "libndi.so: cannot open shared object file" even though the lib is present at /usr/lib/ndi.
    echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
    ldconfig

    systemctl daemon-reload
    systemctl enable camera-box

    log "Camera-Box installed"
}

# Check NDI SDK
check_ndi_sdk() {
    header "Checking NDI SDK"

    if [[ -f "$NDI_DIR/libndi.so.6" ]] || [[ -f "$NDI_DIR/libndi.so" ]]; then
        log "NDI SDK found at $NDI_DIR"

        # Start camera-box if NDI is present
        systemctl start camera-box 2>/dev/null || true
    else
        warn "NDI SDK not found!"
        echo ""
        echo "Camera-Box requires the NDI SDK to stream video."
        echo ""
        echo "To install the NDI SDK:"
        echo "  1. Download from: https://ndi.video/download-ndi-sdk/"
        echo "  2. Extract the archive"
        echo "  3. Copy libndi.so.6 to $NDI_DIR/"
        echo ""
        echo "Example:"
        echo "  sudo mkdir -p $NDI_DIR"
        echo "  sudo cp /path/to/NDI\\ SDK/lib/x86_64-linux-gnu/libndi.so.6 $NDI_DIR/"
        echo ""
    fi
}

# Show final status
show_status() {
    header "Setup Complete"

    echo "System Information:"
    echo "  Hostname: $(hostname)"
    echo "  IP Address: $(hostname -I | awk '{print $1}')"
    echo "  Disk Usage: $(df -h / | awk 'NR==2 {print $3 "/" $2 " (" $5 " used)"}')"
    echo "  GLIBC: $(ldd --version | head -1 | awk '{print $NF}')"
    echo ""

    echo "Services:"
    for svc in dantesync camera-box avahi-daemon; do
        local status
        if systemctl is-active --quiet "$svc" 2>/dev/null; then
            status="${GREEN}running${NC}"
        else
            status="${YELLOW}stopped${NC}"
        fi
        echo -e "  $svc: $status"
    done
    echo ""

    echo "SSH Access:"
    echo "  ssh root@$(hostname -I | awk '{print $1}')"
    echo ""

    if [[ ! -f "$NDI_DIR/libndi.so.6" ]] && [[ ! -f "$NDI_DIR/libndi.so" ]]; then
        echo -e "${YELLOW}Note: NDI SDK not installed. Camera-Box will not stream until installed.${NC}"
        echo ""
    fi

    echo "Useful commands:"
    echo "  journalctl -u camera-box -f   # View camera-box logs"
    echo "  journalctl -u dantesync -f    # View time sync logs"
    echo "  v4l2-ctl --list-devices       # List video devices"
    echo ""
}

# Main
main() {
    echo ""
    echo "╔═══════════════════════════════════════════════════════════════╗"
    echo "║                    Camera-Box Setup                           ║"
    echo "║         Automated Device Configuration Script                 ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    echo ""
    info "Target hostname: $DEVICE_HOSTNAME"
    echo ""

    check_root
    expand_disk      # First! Before anything that needs disk space
    set_hostname
    # #295: PIN the kernel + disable auto-upgrades BEFORE update_system runs `apt-get upgrade -y`,
    # so provisioning can never silently install (and then boot) a new kernel. The box keeps the
    # known-good kernel it is already running.
    harden_appliance_kernel
    update_system
    configure_system
    install_dantesync
    install_camera_box
    check_ndi_sdk
    show_status

    log "Setup completed successfully!"
    echo ""
    warn "Rebooting in 5 seconds to apply all changes..."
    warn "After reboot, connect via: ssh root@<new-static-ip>"
    sleep 5
    reboot
}

# --- source-guard: skip main() ONLY when genuinely SOURCED (e.g. by the pure-function unit
# tests). This is NOT the same guard as setup-device.sh's `[ "${BASH_SOURCE[0]}" != "${0}" ]` --
# that form breaks THIS script's real invocation mode. setup.sh's documented usage is
# `curl -fsSL ... | sudo bash -s CAM1` (stdin-piped): under `bash -s`, BASH_SOURCE[0] is EMPTY
# and $0 is "bash", so a bare `!=` comparison ("" != "bash") would ALSO match and silently skip
# the whole install on every real curl-pipe run. Requiring BASH_SOURCE[0] to be non-empty AND
# different from $0 distinguishes true `source`/`.` (BASH_SOURCE[0] = the sourced path) from a
# stdin pipe (BASH_SOURCE[0] unset) -- verified against all three invocation modes (piped,
# direct-file, sourced) before landing this (#557).
if [ -n "${BASH_SOURCE[0]:-}" ] && [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0
fi

main "$@"
