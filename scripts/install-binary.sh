#!/bin/bash
set -euo pipefail

# Camera-Box Binary Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/zbynekdrlik/camera-box/main/scripts/install-binary.sh | sudo bash

REPO="zbynekdrlik/camera-box"
DANTE_REPO="zbynekdrlik/dantetimesync"
INSTALL_DIR="/usr/local/bin"
NDI_DIR="/usr/lib/ndi"
CONFIG_DIR="/etc/camera-box"

log() { echo "[INFO] $*"; }
warn() { echo "[WARN] $*"; }
error() { echo "[ERROR] $*" >&2; exit 1; }

if [[ $EUID -ne 0 ]]; then
    error "This script must be run as root (use sudo)"
fi

# --- Install Dependencies ---
log "Installing dependencies..."
apt-get update -qq
apt-get install -y -qq libavahi-client3 ethtool >/dev/null 2>&1 || true

# --- Disable Power Savings ---
log "Disabling power savings..."

# Disable CPU frequency scaling (set to performance)
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    if [[ -f "$gov" ]]; then
        echo "performance" > "$gov" 2>/dev/null || true
    fi
done

# Disable USB autosuspend
for ctrl in /sys/bus/usb/devices/*/power/control; do
    if [[ -f "$ctrl" ]]; then
        echo "on" > "$ctrl" 2>/dev/null || true
    fi
done

# Disable network card power management
for iface in /sys/class/net/*/device/power/control; do
    if [[ -f "$iface" ]]; then
        echo "on" > "$iface" 2>/dev/null || true
    fi
done

# Disable Wake-on-LAN power management via ethtool
for iface in $(ls /sys/class/net/ | grep -v lo); do
    if ethtool "$iface" 2>/dev/null | grep -q "Wake-on"; then
        ethtool -s "$iface" wol d 2>/dev/null || true
    fi
done

# Disable systemd sleep/suspend targets
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true

# Disable power button actions
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/disable-power-button.conf << 'EOF'
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF

# Make power settings persistent via rc.local
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

log "Power savings disabled"

# --- Install DanteTimeSync ---
log "Installing DanteTimeSync..."
DANTE_URL=$(curl -fsSL "https://api.github.com/repos/${DANTE_REPO}/releases/latest" 2>/dev/null | \
    grep -o '"browser_download_url": *"[^"]*dantetimesync[^"]*"' | \
    head -1 | cut -d'"' -f4) || true

if [[ -n "$DANTE_URL" ]]; then
    DANTE_TMP=$(mktemp -d)
    if curl -fsSL "$DANTE_URL" -o "$DANTE_TMP/dantetimesync" 2>/dev/null; then
        install -m 755 "$DANTE_TMP/dantetimesync" "$INSTALL_DIR/"
        rm -rf "$DANTE_TMP"

        # Create DanteTimeSync service
        cat > /etc/systemd/system/dantetimesync.service << 'EOF'
[Unit]
Description=Dante Time Sync
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantetimesync
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
        systemctl daemon-reload
        systemctl enable dantetimesync 2>/dev/null || true
        systemctl start dantetimesync 2>/dev/null || true
        log "DanteTimeSync installed and started"
    else
        warn "Failed to download DanteTimeSync"
    fi
else
    warn "DanteTimeSync release not found, skipping"
fi

# --- Install Camera-Box ---
log "Fetching latest camera-box release..."
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
    systemctl enable camera-box 2>/dev/null || true
    log "Systemd service installed and enabled"
fi

rm -rf "$TMP_DIR"

# Create default config if it doesn't exist
mkdir -p "$CONFIG_DIR"
if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
    cat > "$CONFIG_DIR/config.toml" << 'EOF'
# Camera-Box Configuration

# NDI source name (appears as "HOSTNAME (source_name)" in NDI)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"
EOF
    log "Created default config at $CONFIG_DIR/config.toml"
fi

# Check for NDI library
if [[ ! -f "$NDI_DIR/libndi.so.6" ]] && [[ ! -f "$NDI_DIR/libndi.so" ]]; then
    echo ""
    warn "NDI library not found at $NDI_DIR"
    echo "Camera-box requires the NDI SDK to function."
    echo ""
    echo "Install NDI SDK:"
    echo "  1. Download from https://ndi.video/download-ndi-sdk/"
    echo "  2. Extract and copy libndi.so* to $NDI_DIR/"
    echo ""
fi

# Start camera-box service
systemctl start camera-box 2>/dev/null || true

log "Installation complete!"
echo ""
echo "Configuration: $CONFIG_DIR/config.toml"
echo "Usage: camera-box [--device /dev/video0]"
echo "Service: systemctl status camera-box"
