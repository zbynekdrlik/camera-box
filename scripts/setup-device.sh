#!/bin/bash
#
# Camera-Box Device Setup Script
# Sets up a clean Ubuntu installation as a camera-box appliance
#
# Usage: ./setup-device.sh [--binary <url|path>] DEVICE_NAME
# Example: ./setup-device.sh CAM5        (case-insensitive; cam5 works too)
#
# DEVICE_NAME is resolved via scripts/camera-set.sh (#24/#451 -- the single source of truth for
# the cam1-7 fleet map): IP address / VBAN stream name / genlock emit-rate are all DERIVED from
# it, never passed as free-text positional args (#450). An unknown name fails loudly through
# camera-set.sh's fail-closed `case` -- never silently provisions the wrong box.
#

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# fail MSG -- print in red to stderr and exit non-zero. This is a ONE-SHOT provisioner
# (script-failure-policy): every install step that could otherwise leave the box
# half-configured (binary/NDI/ALSA/dantesync) fails loud here instead of warn-and-continue (#450).
fail() {
    echo -e "${RED}FAIL: $1${NC}" >&2
    exit 1
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve() -- NAME -> IP / VBAN stream / genlock FPS (#450)

# GitHub repo + CI dev-build channel for installing the fleet-matching binary (#457 -- the fleet
# runs CI dev-builds, e.g. 1.7.0-dev.157, never a GitHub release; see STEP 3 below).
GITHUB_REPO="zbynekdrlik/camera-box"
CI_BRANCH="${CAMERA_BOX_CI_BRANCH:-dev}"

# Fleet NDI runtime source -- the licensed .so is never built by CI, so a fresh box fetches it
# from a known-good fleet peer instead of requiring a manual per-box scp copy (STEP 4, #457).
NDI_PEER="${CAMERA_BOX_NDI_PEER:-10.77.9.61}"
NDI_PEER_PW="${CAM_PW:-newlevel}"

# --- PURE functions (no root, no network, no side effects -- sourced + unit-tested from
# tests/setup_device_pure_functions.rs; the BASH_SOURCE guard below skips the destructive
# provisioning flow when sourced. Same convention as scripts/setup-imag.sh.) --------------------

# resolve_device_name NAME -- resolves a single fleet camera name (case-insensitive: CAM5, cam5,
# Cam5 all equivalent -- the historical hostname convention is uppercase) to DEVICE_NAME
# (uppercase hostname, e.g. CAM5) / DEVICE_IP / VBAN_STREAM (lowercase stream name, e.g. cam5) /
# CAMERA_GENLOCK_FPS (per-cam emit rate, #451), via scripts/camera-set.sh's camera_resolve().
# Fails loud on an unknown/empty name: camera-set.sh's own fail-closed `case` already rejects it
# and prints why; this just turns that nonzero return into a hard exit instead of letting a
# careless caller ignore it and provision the wrong box.
resolve_device_name() {
    local raw="${1:-}"
    local lc
    lc="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"
    camera_resolve "$lc" || exit 1
    DEVICE_NAME="$(printf '%s' "$CAMERA_NAME" | tr '[:lower:]' '[:upper:]')"
    DEVICE_IP="$CAMERA_IP"
    VBAN_STREAM="$CAMERA_NAME"
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the destructive
# provisioning flow below. Same convention as scripts/setup-imag.sh / scripts/genlock-manifest.sh.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0
fi

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    fail "please run as root"
fi

# Parse arguments. --binary <url|path> may appear anywhere; DEVICE_NAME is the sole positional
# argument (#450 -- name-resolved single-arg invocation; IP/stream/genlock-fps are all DERIVED
# from it via camera-set.sh, replacing the old free-text 3-positional-arg form).
BINARY_ARG=""
POSITIONAL=()
while [ $# -gt 0 ]; do
    case "$1" in
        --binary)
            BINARY_ARG="${2:?--binary needs a URL or local path}"
            shift 2
            ;;
        *)
            POSITIONAL+=("$1")
            shift
            ;;
    esac
done
set -- "${POSITIONAL[@]}"

DEVICE_NAME_ARG="${1:-}"

if [ -z "$DEVICE_NAME_ARG" ]; then
    echo -e "${RED}Usage: $0 [--binary <url|path>] DEVICE_NAME${NC}"
    echo ""
    echo "DEVICE_NAME is resolved via scripts/camera-set.sh (cam1-7) -- case-insensitive."
    echo ""
    echo "Examples:"
    echo "  $0 CAM5"
    echo "  $0 --binary ./dist/camera-box CAM2"
    exit 1
fi

resolve_device_name "$DEVICE_NAME_ARG"

TOTAL_STEPS=19

# GitHub repos
DANTESYNC_REPO="zbynekdrlik/dantesync"

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Camera-Box Device Setup${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Device Name:  ${YELLOW}${DEVICE_NAME}${NC}"
echo -e "Device IP:    ${YELLOW}${DEVICE_IP}${NC}"
echo -e "VBAN Stream:  ${YELLOW}${VBAN_STREAM}${NC}"
echo ""

# Confirm
read -p "Continue with setup? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 1
fi

# =============================================================================
# Pre-flight: ensure curl + CA certificates BEFORE first use
# =============================================================================
# STEP 3 (binary) and STEP 17 (dantesync) download via curl, but the minimal
# create-usb base image ships WITHOUT curl — and STEP 16 (which installs packages)
# runs AFTER STEP 3. On cam5 this made the binary download silently fail. Install
# curl up-front, fail loud if it can't be obtained, and stay idempotent (skip when
# already present).
echo ""
echo -e "${GREEN}[pre-flight] Ensuring curl + CA certificates...${NC}"
if command -v curl >/dev/null 2>&1; then
    echo "  curl already present"
else
    echo "  curl missing — installing (base image ships without it)"
    apt-get update -qq
    apt-get install -y -qq curl ca-certificates
    command -v curl >/dev/null 2>&1 || {
        echo -e "  ${RED}Error: curl still missing after install — cannot download binary/dantesync${NC}"
        exit 1
    }
    echo "  curl installed"
fi

# =============================================================================
# STEP 1: Set hostname
# =============================================================================
echo ""
echo -e "${GREEN}[1/${TOTAL_STEPS}] Setting hostname...${NC}"
echo "$DEVICE_NAME" > /etc/hostname
hostnamectl set-hostname "$DEVICE_NAME"
sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_NAME/" /etc/hosts
echo "  Hostname set to: $DEVICE_NAME"

# =============================================================================
# STEP 2: Configure static IP
# =============================================================================
echo ""
echo -e "${GREEN}[2/${TOTAL_STEPS}] Configuring static IP...${NC}"
cat > /etc/netplan/01-netcfg.yaml << EOF
network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        driver: "*"
      addresses:
        - ${DEVICE_IP}/23
      routes:
        - to: default
          via: 10.77.8.1
      nameservers:
        addresses:
          - 10.77.8.1
EOF
chmod 600 /etc/netplan/01-netcfg.yaml
rm -f /etc/netplan/50-cloud-init.yaml
echo "  Static IP configured: $DEVICE_IP"

# =============================================================================
# STEP 3: Install camera-box binary
# =============================================================================
echo ""
echo -e "${GREEN}[3/${TOTAL_STEPS}] Installing camera-box binary...${NC}"
# The fleet runs CI dev-builds (e.g. 1.7.0-dev.157), never a GitHub release -- installing from
# releases/latest silently version-drifted a fresh box from the fleet (cam6, #457). Resolution
# order:
#   1. --binary <local path>                  - use the file directly (already fetched elsewhere)
#   2. --binary <url> / CAMERA_BOX_BINARY_URL  - curl this exact URL (a raw camera-box binary)
#   3. default                                 - gh run download the latest successful CI
#      artifact on $CI_BRANCH, mirroring scripts/deploy-fleet.sh's own mechanism, so a fresh box
#      matches the fleet with no manual copy.
BINARY_SRC="${BINARY_ARG:-${CAMERA_BOX_BINARY_URL:-}}"
if [ -n "$BINARY_SRC" ] && [ -f "$BINARY_SRC" ]; then
    echo "  Using local binary: $BINARY_SRC"
    install -m 0755 "$BINARY_SRC" /usr/local/bin/camera-box
    echo "  Binary installed: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
elif [ -n "$BINARY_SRC" ]; then
    echo "  Downloading binary from: $BINARY_SRC"
    curl -fsSL "$BINARY_SRC" -o /usr/local/bin/camera-box \
        || fail "could not download binary from $BINARY_SRC"
    chmod +x /usr/local/bin/camera-box
    echo "  Binary installed: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
elif command -v gh >/dev/null 2>&1 && [ -n "${GH_TOKEN:-}" ]; then
    echo "  Fetching latest CI dev-build artifact (branch: $CI_BRANCH)..."
    RUN_ID="$(gh run list --repo "$GITHUB_REPO" --branch "$CI_BRANCH" --workflow ci.yml \
        --status success --limit 1 --json databaseId -q '.[0].databaseId // empty' 2>/dev/null || true)"
    [ -n "$RUN_ID" ] || fail "no successful CI run found on branch '$CI_BRANCH' -- install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
    DIST_DIR="$(mktemp -d)"
    if gh run download "$RUN_ID" --repo "$GITHUB_REPO" -n camera-box-linux-amd64 --dir "$DIST_DIR" 2>/dev/null \
        && [ -f "$DIST_DIR/camera-box" ]; then
        install -m 0755 "$DIST_DIR/camera-box" /usr/local/bin/camera-box
        echo "  Binary installed from CI run $RUN_ID: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
    else
        rm -rf "$DIST_DIR"
        fail "gh run download failed for run $RUN_ID -- install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
    fi
    rm -rf "$DIST_DIR"
else
    fail "gh CLI unavailable or GH_TOKEN unset -- cannot auto-fetch the fleet dev-build. Install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
fi

# =============================================================================
# STEP 4: Install NDI library
# =============================================================================
echo ""
echo -e "${GREEN}[4/${TOTAL_STEPS}] Setting up NDI library...${NC}"
mkdir -p /usr/lib/ndi
# Add NDI library path to ldconfig
echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
if [ -f /usr/lib/ndi/libndi.so.6 ]; then
    ldconfig
    echo "  NDI library: present and configured"
elif [ "$DEVICE_IP" = "$NDI_PEER" ]; then
    echo -e "  ${YELLOW}This box IS the fleet NDI source ($NDI_PEER) -- nothing to fetch${NC}"
    echo "  Copy libndi.so.* onto it manually before re-running this script"
else
    # NDI is a licensed runtime -- CI never builds it, so fetch it from a known-good fleet peer
    # (cam1) instead of requiring a manual per-box scp copy (#457). Mirrors the already-proven
    # setup-imag.sh step-10 dance: scp the versioned .so, then symlink libndi.so.6/libndi.so onto it.
    echo "  NDI library not found locally -- fetching from fleet peer $NDI_PEER..."
    command -v sshpass >/dev/null 2>&1 || apt-get install -y -qq sshpass >/dev/null 2>&1 || true
    if command -v sshpass >/dev/null 2>&1 \
        && sshpass -p "$NDI_PEER_PW" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
            "root@${NDI_PEER}:/usr/lib/ndi/libndi.so.*.*.*" /usr/lib/ndi/ 2>/dev/null; then
        REAL="$(cd /usr/lib/ndi && ls libndi.so.*.*.* 2>/dev/null | head -1 || true)"
        [ -n "$REAL" ] || fail "NDI fetch from $NDI_PEER produced no file -- copy manually: scp root@${NDI_PEER}:/usr/lib/ndi/libndi.so.* /usr/lib/ndi/"
        ln -sf "$REAL" /usr/lib/ndi/libndi.so.6
        ln -sf libndi.so.6 /usr/lib/ndi/libndi.so
        ldconfig
        echo "  NDI library fetched from $NDI_PEER and configured ($REAL)"
    else
        fail "could not fetch NDI library from fleet peer $NDI_PEER -- copy manually: scp root@${NDI_PEER}:/usr/lib/ndi/libndi.so.* /usr/lib/ndi/"
    fi
fi

# =============================================================================
# STEP 5: Configure ALSA for USB headset
# =============================================================================
echo ""
echo -e "${GREEN}[5/${TOTAL_STEPS}] Configuring ALSA audio...${NC}"

# Auto-detect USB headset card (CSCTEK USB Audio and HID). `|| true` on each pipeline guards
# against `set -o pipefail` aborting the whole script on a no-match `grep` (#458 footgun #1) --
# `head`/`awk` succeed on empty input, but pipefail takes the RIGHTMOST *failing* command's exit
# code, which is `grep`'s nonzero when nothing matched.
USB_CARD=$(cat /proc/asound/cards 2>/dev/null | grep -E 'HID.*USB Audio|USB Audio.*HID' | head -1 | awk '{print $1}' || true)
if [ -z "$USB_CARD" ]; then
    # Fallback: try to find any USB audio device
    USB_CARD=$(cat /proc/asound/cards 2>/dev/null | grep -i 'usb.*audio\|audio.*usb' | head -1 | awk '{print $1}' || true)
fi
# #450: fail loud instead of silently defaulting to card 1 -- a wrong hardcoded card would
# silently misconfigure the intercom on hardware whose USB audio device enumerates differently.
[ -n "$USB_CARD" ] || fail "could not auto-detect a USB headset on /proc/asound/cards -- refusing to silently default to card 1"
echo "  Detected USB headset on card $USB_CARD"

cat > /etc/asound.conf << ALSAEOF
# Asymmetric config: stereo output, mono input
# USB headset on card $USB_CARD (auto-detected)
pcm.!default {
    type asym
    playback.pcm {
        type plug
        slave {
            pcm "hw:$USB_CARD,0"
            channels 2
        }
    }
    capture.pcm {
        type plug
        slave {
            pcm "hw:$USB_CARD,0"
            channels 1
        }
    }
}

ctl.!default {
    type hw
    card $USB_CARD
}
ALSAEOF
echo "  ALSA config: /etc/asound.conf (card $USB_CARD)"

# =============================================================================
# STEP 6: Create camera-box config
# =============================================================================
echo ""
echo -e "${GREEN}[6/${TOTAL_STEPS}] Creating camera-box config...${NC}"
mkdir -p /etc/camera-box
cat > /etc/camera-box/config.toml << EOF
# Camera-Box Configuration
# Device: ${DEVICE_NAME}
# Generated: $(date -Iseconds)

# NDI source name (appears on network)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"

# VBAN Intercom Configuration
[intercom]
stream = "${VBAN_STREAM}"
target = "strih.lan"
sample_rate = 48000
channels = 1
EOF
echo "  Config: /etc/camera-box/config.toml"

# =============================================================================
# STEP 7: Create systemd service
# =============================================================================
echo ""
echo -e "${GREEN}[7/${TOTAL_STEPS}] Creating systemd service...${NC}"
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

# #289 + #11 systemd drop-ins: the realtime CPU-isolation + genlock emit-rate
# overrides live in drop-ins (not the base unit) so they can be re-applied / tuned
# without rewriting the whole unit. Today NO script created these — every box has
# been a manual SSH edit that drifted (30<->60 across boxes; a reinstall came up
# free-running/uncapped and with the grab NOT pinned to the isolated core). Writing
# them here makes a fresh box match the fleet in one run. Idempotent: re-running
# overwrites with identical content, and the daemon-reload below picks them up.
mkdir -p /etc/systemd/system/camera-box.service.d
# #289 — pin the SCHED_FIFO grab onto the isolcpus=3 reserved core. Soft (inherited)
# CPUAffinity mask, NOT a hard cpuset: the per-thread sched_setaffinity calls in the
# binary still move painter / --display / intercom threads OFF onto the general cores
# and re-pin capture+emit to the /sys-derived isolated core (src/affinity.rs, the
# authoritative path). This static value just matches the fleet's isolcpus=3.
cat > /etc/systemd/system/camera-box.service.d/cpu-affinity.conf << 'EOF'
[Service]
# #289: pin grab to the isolated core (isolcpus=3) so box load never starves capture/emit
CPUAffinity=3
EOF
# #11/#450 — NDI emit rate, from the per-cam CAMERA_GENLOCK_FPS table (scripts/camera-set.sh,
# #451) rather than a hardcoded literal. Every fleet camera emits 60fps today (the stream box
# decimates 60->30 downstream), so this resolves to the same value as before -- but a future
# per-camera divergence now needs only a camera-set.sh edit, not a setup-device.sh edit too.
cat > /etc/systemd/system/camera-box.service.d/genlock.conf << EOF
[Service]
Environment=CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}
EOF

systemctl daemon-reload
systemctl enable camera-box
echo "  Service created and enabled"
echo "  Drop-ins: cpu-affinity.conf (CPUAffinity=3, isolcpus core) + genlock.conf (CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS})"

# =============================================================================
# STEP 8: Configure auto-login on tty1
# =============================================================================
echo ""
echo -e "${GREEN}[8/${TOTAL_STEPS}] Configuring auto-login...${NC}"
mkdir -p /etc/systemd/system/getty@tty1.service.d
cat > /etc/systemd/system/getty@tty1.service.d/autologin.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear %I $TERM
EOF
systemctl daemon-reload
echo "  Auto-login: root on tty1"

# =============================================================================
# STEP 9: Set binary capabilities
# =============================================================================
echo ""
echo -e "${GREEN}[9/${TOTAL_STEPS}] Setting binary capabilities...${NC}"
if [ -f /usr/local/bin/camera-box ]; then
    setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box
    echo "  Capabilities set (real-time priority, memory lock)"
else
    echo -e "  ${YELLOW}Skipped - binary not installed${NC}"
fi

# =============================================================================
# STEP 10: GRUB — fast boot + #295 brick-proofing (pin a known-good kernel)
# =============================================================================
echo ""
echo -e "${GREEN}[10/${TOTAL_STEPS}] Configuring GRUB (fast + safe boot)...${NC}"
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
# #289/#303: reserve core 3 for the SCHED_FIFO capture/emit path (isolcpus=3) and quiet it
# the rest of the way — nohz_full=3 stops the periodic scheduler tick on it, rcu_nocbs=3
# offloads RCU callbacks off it, and irqaffinity=0-2 defaults ALL boot IRQs onto the general
# cores (the only lever that moves managed MSI xhci IRQs, which reject runtime
# /proc/irq/<n>/smp_affinity writes — #289's ExecStartPre logs those as non-fatal and they
# stay put without this). Each flag is appended to GRUB_CMDLINE_LINUX IDEMPOTENTLY (never
# duplicated), one shared loop for all four so there is exactly one append code path. This
# edit lives INSIDE this #295 initrd-guaranteed grub step (the update-grub + abort-if-no-initrd
# guard below) so the cmdline change can never strand a box on an initrd-less kernel the way
# an ad-hoc grub edit did.
for flag_tag in "isolcpus=3:#289" "nohz_full=3:#303" "rcu_nocbs=3:#303" "irqaffinity=0-2:#303"; do
    flag="${flag_tag%%:*}"
    tag="${flag_tag##*:}"
    if ! grep -qE "^GRUB_CMDLINE_LINUX=.*${flag}([\" ]|\$)" /etc/default/grub; then
        if grep -q '^GRUB_CMDLINE_LINUX=' /etc/default/grub; then
            # Append inside the existing double-quoted value.
            sed -i "s/^\(GRUB_CMDLINE_LINUX=\"[^\"]*\)\"/\1 ${flag}\"/" /etc/default/grub
            # Normalise a leading space when GRUB_CMDLINE_LINUX was previously empty ("" -> " isolcpus=3").
            sed -i 's/^GRUB_CMDLINE_LINUX="  */GRUB_CMDLINE_LINUX="/' /etc/default/grub
        else
            echo "GRUB_CMDLINE_LINUX=\"${flag}\"" >> /etc/default/grub
        fi
        echo "  Kernel cmdline: ${flag} added to GRUB_CMDLINE_LINUX [${tag}]"
    else
        echo "  Kernel cmdline: ${flag} already present [${tag}]"
    fi
done
# #295: GUARANTEE every installed kernel has an initrd BEFORE regenerating grub. A kernel without an
# initrd that becomes the grub default cannot mount root — that bricked CAM3 + CAM4.
for vmlinuz in /boot/vmlinuz-*; do
    [ -e "$vmlinuz" ] || continue
    kver="${vmlinuz#/boot/vmlinuz-}"
    if [ ! -e "/boot/initrd.img-${kver}" ]; then
        echo -e "  ${YELLOW}#295: kernel ${kver} has no initrd — generating before grub${NC}"
        update-initramfs -c -k "${kver}"
    fi
done
update-grub
# #295: refuse to leave a default boot entry without a kernel image AND an initrd, then pin the
# saved default to the running (known-good) kernel.
GRUB_CFG="/boot/grub/grub.cfg"
if [ -f "$GRUB_CFG" ]; then
    DEFAULT_ENTRY="$(awk '/^[[:space:]]*menuentry /{c++} c==1{print} c==2{exit}' "$GRUB_CFG")"
    if ! echo "$DEFAULT_ENTRY" | grep -qE '(vmlinuz|[[:space:]]linux )' \
        || ! echo "$DEFAULT_ENTRY" | grep -q 'initrd'; then
        echo -e "${RED}#295: grub default entry lacks a kernel image or initrd — aborting to avoid a brick${NC}"
        exit 1
    fi
    # The default entry (index 0) is now proven to carry both a kernel image and an initrd. Pin it
    # explicitly as the saved default so it boots deterministically. The kernel is held (apt-mark
    # hold), so index 0 is the single known-good kernel on a freshly-provisioned appliance.
    grub-set-default 0
fi
echo "  GRUB: timeout 0s, default pinned to known-good kernel with initrd [#295]"

# =============================================================================
# STEP 11: Don't block boot on the network
# #547: MASK systemd-networkd-wait-online. Unmasked it timed out at 120s and delayed
# network-online.target -> camera-box started ~123s late (observed on cam3). The box has a static
# IP (STEP 2), and camera-box (After=network-online.target + its own retry) never needs to wait for
# the link to be "online", so masking is safe and cuts boot-to-stream to a few seconds.
# =============================================================================
echo ""
echo -e "${GREEN}[11/${TOTAL_STEPS}] Removing the network-wait boot stall...${NC}"
systemctl mask systemd-networkd-wait-online.service 2>/dev/null || true
# Keep a short-timeout override too (belt-and-braces): harmless while masked, and if the unit is
# ever unmasked it still caps the wait at 5s instead of the 120s default.
mkdir -p /etc/systemd/system/systemd-networkd-wait-online.service.d
cat > /etc/systemd/system/systemd-networkd-wait-online.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/lib/systemd/systemd-networkd-wait-online --timeout=5
EOF
echo "  Network wait: masked (no 120s boot stall) + 5s override fallback"

# =============================================================================
# STEP 12: Disable power button shutdown
# =============================================================================
echo ""
echo -e "${GREEN}[12/${TOTAL_STEPS}] Disabling power button shutdown...${NC}"
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/disable-power-button.conf << EOF
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF
echo "  Power button: ignored (used for mute toggle)"

# =============================================================================
# STEP 13: Disable all power saving / sleep
# =============================================================================
echo ""
echo -e "${GREEN}[13/${TOTAL_STEPS}] Disabling power saving...${NC}"
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
# Disable CPU frequency scaling (use performance governor)
for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo "performance" > "$cpu" 2>/dev/null || true
done
# Make it persistent
cat > /etc/systemd/system/cpu-performance.service << 'EOF'
[Unit]
Description=Set CPU to performance mode
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/bin/bash -c 'for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > $cpu; done'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable cpu-performance.service 2>/dev/null || true
echo "  Sleep/suspend: disabled"
echo "  CPU governor: performance"

# =============================================================================
# STEP 14: Optimize network for performance
# =============================================================================
echo ""
echo -e "${GREEN}[14/${TOTAL_STEPS}] Optimizing network performance...${NC}"
cat > /etc/sysctl.d/99-network-performance.conf << 'EOF'
# Network performance optimizations for low-latency streaming

# Increase network buffer sizes
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 1048576
net.core.wmem_default = 1048576
net.core.netdev_max_backlog = 5000

# TCP optimizations
net.ipv4.tcp_rmem = 4096 1048576 134217728
net.ipv4.tcp_wmem = 4096 1048576 134217728
net.ipv4.tcp_congestion_control = bbr
net.ipv4.tcp_fastopen = 3

# Reduce latency
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_nodelay = 1

# Disable IPv6 if not needed
net.ipv6.conf.all.disable_ipv6 = 1
net.ipv6.conf.default.disable_ipv6 = 1
EOF
sysctl -p /etc/sysctl.d/99-network-performance.conf 2>/dev/null || true

# Disable EEE (Energy Efficient Ethernet / Green Ethernet) and flow control
# This reduces latency by preventing power-saving mode transitions
cat > /etc/networkd-dispatcher/routable.d/optimize-nic << 'NICEOF'
#!/bin/bash
# Disable EEE (Green Ethernet) and flow control for low latency
IFACE="$IFACE"
if [ -n "$IFACE" ] && [ "$IFACE" != "lo" ]; then
    # Disable Energy Efficient Ethernet
    ethtool --set-eee "$IFACE" eee off 2>/dev/null || true
    # Disable flow control (pause frames)
    ethtool -A "$IFACE" rx off tx off 2>/dev/null || true
fi
NICEOF
chmod +x /etc/networkd-dispatcher/routable.d/optimize-nic

# Apply to current interface
for iface in /sys/class/net/*/device; do
    IFACE=$(basename "$(dirname "$iface")")
    ethtool --set-eee "$IFACE" eee off 2>/dev/null || true
    ethtool -A "$IFACE" rx off tx off 2>/dev/null || true
done

echo "  Network buffers: optimized"
echo "  TCP congestion: BBR"
echo "  IPv6: disabled"
echo "  EEE (Green Ethernet): disabled"
echo "  Flow control: disabled"

# =============================================================================
# STEP 15: Disable unnecessary services
# =============================================================================
echo ""
echo -e "${GREEN}[15/${TOTAL_STEPS}] Disabling unnecessary services...${NC}"
# Snap
systemctl disable --now snapd.service snapd.socket snapd.seeded.service 2>/dev/null || true
systemctl mask snapd.service 2>/dev/null || true
# Cloud-init
systemctl disable --now cloud-init.service cloud-init-local.service cloud-config.service cloud-final.service 2>/dev/null || true
touch /etc/cloud/cloud-init.disabled 2>/dev/null || true
# Auto updates — #295: an active unattended-upgrades auto-installed a kernel without an initrd that
# bricked CAM3/CAM4. PIN the kernel and disable unattended upgrades entirely; an appliance must
# never silently gain a new kernel.
systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer \
    apt-daily.service apt-daily-upgrade.service 2>/dev/null || true
systemctl mask unattended-upgrades.service 2>/dev/null || true
apt-mark hold linux-image-generic linux-headers-generic linux-generic 2>/dev/null || true
cat > /etc/apt/apt.conf.d/20auto-upgrades << 'EOF'
// Camera-box appliance: never auto-update (#295 — kernels are pinned with apt-mark hold).
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
EOF
cat > /etc/apt/apt.conf.d/51camera-box-no-kernel-autoupgrade << 'EOF'
// #295: never let unattended-upgrades touch the kernel on the appliance.
Unattended-Upgrade::Package-Blacklist {
    "linux-image";
    "linux-headers";
    "linux-generic";
};
EOF
# #295: any FUTURE kernel install must always get an initrd. This /etc/kernel/postinst.d hook sorts
# before grub's own `zz-update-grub` hook, so a missing initrd is regenerated BEFORE grub is updated.
mkdir -p /etc/kernel/postinst.d
cat > /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee << 'EOF'
#!/bin/sh
# #295: guarantee every installed kernel has an initrd (a kernel without one bricked CAM3/CAM4).
# #547: on the read-only-root appliance, mkinitramfs' default build dir /var/tmp is a 50M tmpfs --
# far too small for a ~400M initramfs ("No space left on device" -> no initrd -> a half-installed,
# unbootable kernel). Build in /root/.itmp (the real ~51G disk) instead. A kernel install only ever
# runs inside a `mount -o remount,rw /` window, so /root (and /boot) are writable then.
set -e
version="$1"
[ -n "$version" ] || exit 0
if [ ! -e "/boot/initrd.img-${version}" ]; then
    mkdir -p /root/.itmp
    TMPDIR=/root/.itmp update-initramfs -c -k "${version}"
fi
EOF
chmod +x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee
echo "  #295: kernel pinned (apt-mark hold), unattended-upgrades disabled, initrd hook installed"
# fwupd — #547: PURGE it. On the read-only-root appliance fwupd holds an open write handle on
# /var/lib/fwupd/pending.db, which makes `mount -o remount,ro /` fail with EBUSY (blocked the ro
# conversion on cam1/cam4). The appliance never firmware-updates itself, so remove it outright.
systemctl disable --now fwupd.service fwupd-refresh.timer 2>/dev/null || true
apt-get purge -y fwupd fwupd-signed 2>/dev/null \
    || dpkg --purge --force-depends fwupd fwupd-signed 2>/dev/null \
    || systemctl mask fwupd.service 2>/dev/null || true
# ModemManager (not needed)
systemctl disable --now ModemManager.service 2>/dev/null || true
# Bluetooth (not needed)
systemctl disable --now bluetooth.service 2>/dev/null || true
# Printing (not needed)
systemctl disable --now cups.service cups-browsed.service 2>/dev/null || true
echo "  Disabled: snapd, cloud-init, auto-updates, fwupd (purged), ModemManager, bluetooth, cups"

# =============================================================================
# STEP 16: Install required packages
# =============================================================================
echo ""
echo -e "${GREEN}[16/${TOTAL_STEPS}] Installing required packages...${NC}"
apt-get update -qq
# #362: include the FULL NDI/audio runtime dep set so a fresh box can RUN camera-box (the CAM3
# clone crash-looped on missing libndi deps): libasound2t64 (ALSA, intercom), libavahi-common3
# (libndi links it alongside libavahi-client3), and avahi-utils (avahi-browse for diagnosis).
apt-get install -y -qq avahi-daemon libavahi-client3 libavahi-common3 avahi-utils libasound2t64 v4l-utils alsa-utils ethtool curl ca-certificates 2>/dev/null || true
systemctl enable avahi-daemon
echo "  Installed: avahi-daemon, libavahi-client3, libavahi-common3, avahi-utils, libasound2t64, v4l-utils, alsa-utils, ethtool, curl, ca-certificates"

# Create rc.local for power management settings (USB autosuspend, etc.)
cat > /etc/rc.local << 'RCEOF'
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
RCEOF
chmod +x /etc/rc.local
echo "  Created: /etc/rc.local (USB autosuspend off, CPU performance)"

# =============================================================================
# STEP 17: Install dantesync (PTP time synchronization)
# =============================================================================
echo ""
echo -e "${GREEN}[17/${TOTAL_STEPS}] Installing dantesync...${NC}"

DANTESYNC_INSTALLED=false

# Get latest release URL from GitHub
DANTESYNC_URL=$(curl -fsSL "https://api.github.com/repos/${DANTESYNC_REPO}/releases/latest" 2>/dev/null | \
    grep -o '"browser_download_url": *"[^"]*dantesync-linux-amd64"' | \
    grep -o 'https://[^"]*' | head -1) || true

# #450: fail loud -- dantesync disciplines the cluster wall-clock genlock depends on (#8); a box
# provisioned without it silently free-runs its own clock instead of the fleet's shared reference.
[ -n "$DANTESYNC_URL" ] || fail "could not get dantesync release URL from GitHub -- dantesync is required for cluster clock sync (#8), not optional"

curl -fsSL "$DANTESYNC_URL" -o /usr/local/bin/dantesync 2>/dev/null \
    || fail "failed to download dantesync from $DANTESYNC_URL"
chmod +x /usr/local/bin/dantesync

# Create systemd service
cat > /etc/systemd/system/dantesync.service << 'DANTEEOF'
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
DANTEEOF

systemctl daemon-reload
systemctl enable dantesync 2>/dev/null || true
DANTESYNC_INSTALLED=true
echo "  dantesync: installed and enabled"

# =============================================================================
# STEP 18: Configure read-only root filesystem
# =============================================================================
echo ""
echo -e "${GREEN}[18/${TOTAL_STEPS}] Configuring read-only filesystem...${NC}"

# Get the root partition UUID
ROOT_UUID=$(findmnt -n -o UUID /)

# Backup original fstab -- IDEMPOTENCY GUARD (#450): only back up ONCE. An unconditional `cp`
# here clobbers /etc/fstab.bak with the ALREADY-REWRITTEN (ro+tmpfs) fstab on a re-run,
# permanently losing the true pre-provisioning original -- including the EFI entry this step
# reads back out of it, below.
if [ ! -f /etc/fstab.bak ]; then
    cp /etc/fstab /etc/fstab.bak
    echo "  Backed up original fstab to /etc/fstab.bak"
else
    echo "  /etc/fstab.bak already exists -- keeping the original backup (idempotent re-run)"
fi

# Create new fstab with read-only root and tmpfs mounts
cat > /etc/fstab << FSTABEOF
# Root filesystem - read-only for reliability
UUID=${ROOT_UUID} / ext4 ro 0 1

# EFI partition (if exists)
$(grep '/boot/efi' /etc/fstab.bak 2>/dev/null || echo "# No EFI partition")

# tmpfs mounts for writable directories
tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0
tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0
tmpfs /var/tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=50M 0 0
# #295: size /var/cache >=512M (uniformly across the fleet) so apt can never ENOSPC and leave a
# freshly-installed kernel without its initrd (a 100M /var/cache filled up and did exactly that).
tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0
tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0
FSTABEOF

echo "  Root filesystem: read-only (ro)"
echo "  tmpfs mounts: /tmp, /var/log, /var/tmp, /var/cache, /var/spool"
echo "  To remount read-write: mount -o remount,rw /"

# =============================================================================
# STEP 19: Final verification + summary
# =============================================================================
echo ""
echo -e "${GREEN}[19/${TOTAL_STEPS}] Verifying installation...${NC}"

# #450: NEVER print "Setup Complete!" on a half-configured box. Every failure path above already
# fails loud at the point of failure, but this box being the fleet's own NDI source is a
# legitimate NON-fatal branch in STEP 4 (it fetches from itself) -- so it alone can still reach
# here without libndi.so.6 actually present. This is the belt-and-braces final gate.
MISSING=""
[ -f /usr/local/bin/camera-box ] || MISSING="${MISSING}camera-box binary (/usr/local/bin/camera-box) "
[ -f /usr/lib/ndi/libndi.so.6 ] || MISSING="${MISSING}NDI library (/usr/lib/ndi/libndi.so.6) "
if [ -n "$MISSING" ]; then
    fail "half-configured box -- missing: ${MISSING}-- refusing to report Setup Complete"
fi

echo -e "${GREEN}Setup Complete!${NC}"
echo "=========================================="
echo ""
echo "Configuration:"
echo "  Hostname:     $DEVICE_NAME"
echo "  IP Address:   $DEVICE_IP"
echo "  VBAN Stream:  $VBAN_STREAM"
echo "  Genlock FPS:  $CAMERA_GENLOCK_FPS"
echo "  NDI Name:     usb"
echo ""
echo "Optimizations applied:"
echo "  - GRUB timeout: 0s"
echo "  - Network wait: 5s"
echo "  - Power button: mute toggle (not shutdown)"
echo "  - Sleep/suspend: disabled"
echo "  - CPU governor: performance"
echo "  - CPU isolation: core 3 reserved + quieted (isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2) for the realtime grab [#289/#303]"
echo "  - Realtime pin: CPUAffinity=3 drop-in; NDI emit ${CAMERA_GENLOCK_FPS}fps (genlock.conf) [#289/#11/#451]"
echo "  - Network: optimized for streaming"
echo "  - Unnecessary services: disabled"
echo "  - Root filesystem: read-only (with tmpfs overlays)"
if [ "$DANTESYNC_INSTALLED" = true ]; then
    echo "  - Dante time sync: installed (PTP synchronization)"
fi
echo ""
echo -e "${YELLOW}Next steps:${NC}"
echo "  1. Apply network config: netplan apply"
echo "  2. Reboot: reboot"
echo ""
echo -e "${GREEN}After reboot, connect via: ssh root@${DEVICE_IP}${NC}"
