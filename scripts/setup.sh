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
        avahi-daemon \
        ca-certificates

    log "Cleaning up..."
    apt-get autoremove -y
    apt-get clean

    log "System updated"
}

# Configure system for appliance mode
configure_system() {
    header "Configuring System"

    # --- Set timezone ---
    log "Setting timezone to UTC..."
    timedatectl set-timezone UTC || true

    # --- Disable unnecessary services ---
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
# Camera-box performance tuning

# Reduce swappiness
vm.swappiness = 10

# Increase inotify watches
fs.inotify.max_user_watches = 524288

# Network tuning
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
EOF
    sysctl -p /etc/sysctl.d/99-camera-box.conf 2>/dev/null || true

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
ExecStart=/usr/local/bin/dantesync
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

    # Install systemd service if present
    if [[ -f "$tmp_dir/camera-box.service" ]]; then
        install -m 644 "$tmp_dir/camera-box.service" /etc/systemd/system/
    else
        log "Creating camera-box service..."
        cat > /etc/systemd/system/camera-box.service << 'EOF'
[Unit]
Description=Camera-Box NDI Streaming
After=network.target avahi-daemon.service
Wants=avahi-daemon.service

[Service]
Type=simple
ExecStart=/usr/local/bin/camera-box
Restart=always
RestartSec=5
Environment="NDI_RUNTIME_DIR_V6=/usr/lib/ndi"
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
    fi

    rm -rf "$tmp_dir"

    # Create config directory
    mkdir -p "$CONFIG_DIR"
    if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
        cat > "$CONFIG_DIR/config.toml" << 'EOF'
# Camera-Box Configuration

# NDI source name (appears on network)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"
EOF
        log "Created config at $CONFIG_DIR/config.toml"
    fi

    # Create NDI directory
    mkdir -p "$NDI_DIR"

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
    set_hostname
    expand_disk
    update_system
    configure_system
    install_dantesync
    install_camera_box
    check_ndi_sdk
    show_status

    log "Setup completed successfully!"
}

main "$@"
