#!/bin/bash
#
# imag-nb One-Shot Setup (#458) — provision a Linux notebook as the 60fps IMAG OBS box
#
# Runs ON the imag box as root (same model as setup-device.sh). Idempotent — safe to re-run.
# After it finishes, run scripts/imag_scenes.py from dev1 to seed the OBS profile/scenes over
# WebSocket (and later `imag_scenes.py --projector` once the HDMI monitor is connected).
#
# Usage (on the box):
#   sudo CAM_PW=<fleet-pw> ./setup-imag.sh [--yes]
#
# Topology (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md):
#   6× cam box NDI 1080p60 -> imag-nb OBS (1080p60 low-latency IMAG) -> HDMI program projector
#
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

STATIC_IP="10.77.9.182"
PREFIX="23"
NDI_PEER="10.77.9.61"            # cam1 — fleet NDI runtime source (6.3.2)
NDI_DIR="/usr/lib/ndi"
DESKTOP_USER="newlevel"
USER_HOME="/home/${DESKTOP_USER}"
OBS_CFG="${USER_HOME}/.config/obs-studio"
TOTAL_STEPS=10

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

[ "$EUID" -eq 0 ] || fail "run as root (sudo)"
id "$DESKTOP_USER" >/dev/null 2>&1 || fail "user $DESKTOP_USER missing"

ASSUME_YES=0
[ "${1:-}" = "--yes" ] || [ "${1:-}" = "-y" ] && ASSUME_YES=1
if [ "$ASSUME_YES" -ne 1 ]; then
    read -p "Provision this box as imag-nb (${STATIC_IP})? (y/N) " -n 1 -r; echo
    [[ $REPLY =~ ^[Yy]$ ]] || { echo "Aborted."; exit 1; }
fi

# Pre-flight: curl + CA certs BEFORE first use (the cam5/#450 lesson — a base image without
# curl makes every download step fail silently mid-run; ensure it up-front, fail loud).
if ! command -v curl >/dev/null 2>&1; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates >/dev/null \
        || fail "cannot install curl — network/apt broken"
fi

# =============================================================================
step 1 "Static IP ${STATIC_IP}/${PREFIX} (NetworkManager — desktop Ubuntu)"
# =============================================================================
# The box got .182 via DHCP; pin the SAME address static so it never drifts (the cam6
# lesson: a DHCP lease change looks like a dead box). Same IP -> nmcli re-apply is safe
# over this very ssh session.
if command -v nmcli >/dev/null 2>&1; then
    NIC=$(ip -4 route get "$NDI_PEER" | grep -oP 'dev \K\S+' | head -1)
    [ -n "$NIC" ] || fail "cannot resolve NIC toward the rig LAN"
    CON=$(nmcli -t -f NAME,DEVICE con show --active | awk -F: -v d="$NIC" '$2==d{print $1; exit}')
    [ -n "$CON" ] || fail "no active NetworkManager connection on $NIC"
    GW=$(ip -4 route show default | grep -oP 'via \K\S+' | head -1)
    DNS=$(resolvectl dns "$NIC" 2>/dev/null | grep -oP '(\d+\.){3}\d+' | head -2 | tr '\n' ' ')
    [ -n "$DNS" ] || DNS="$GW"
    nmcli con mod "$CON" ipv4.method manual \
        ipv4.addresses "${STATIC_IP}/${PREFIX}" ipv4.gateway "$GW" ipv4.dns "${DNS// /,}"
    nmcli con up "$CON" >/dev/null || true   # same IP — session survives
    echo "  static ${STATIC_IP}/${PREFIX} gw=$GW dns=$DNS on $NIC ($CON)"
else
    fail "nmcli missing — desktop Ubuntu expected (netplan-only path not implemented)"
fi

# =============================================================================
step 2 "Max performance: governor + no USB/NIC powersave (USB-ethernet feeds the NDI!)"
# =============================================================================
cat > /etc/systemd/system/cpu-performance.service <<'EOF'
[Unit]
Description=Set CPU governor to performance
After=multi-user.target
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done'
[Install]
WantedBy=multi-user.target
EOF
cat > /etc/rc.local <<'EOF'
#!/bin/bash
# imag-nb boot tuning (fleet parity): governor + USB autosuspend off (USB NIC!) + NIC powersave off
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done
for u in /sys/bus/usb/devices/*/power/control; do echo on > "$u" 2>/dev/null; done
for n in /sys/class/net/*/device/power/control; do echo on > "$n" 2>/dev/null; done
exit 0
EOF
chmod +x /etc/rc.local
systemctl daemon-reload
systemctl enable --now cpu-performance.service >/dev/null 2>&1
bash /etc/rc.local
grep -q performance /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor || fail "governor not performance"

# =============================================================================
step 3 "Never sleep: lid ignore + sleep masked + GNOME idle/blank/lock off"
# =============================================================================
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/99-imag-no-sleep.conf <<'EOF'
[Login]
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
IdleAction=ignore
EOF
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target >/dev/null 2>&1 || true
systemctl restart systemd-logind
UBUS="unix:path=/run/user/$(id -u $DESKTOP_USER)/bus"
gs() { sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" gsettings set "$@" 2>/dev/null || true; }
gs org.gnome.desktop.session idle-delay 0
gs org.gnome.settings-daemon.plugins.power sleep-inactive-ac-type "'nothing'"
gs org.gnome.settings-daemon.plugins.power sleep-inactive-battery-type "'nothing'"
gs org.gnome.desktop.screensaver lock-enabled false
gs org.gnome.desktop.screensaver idle-activation-enabled false

# =============================================================================
step 4 "NDI runtime 6.3.2 from cam1 -> ${NDI_DIR} (fleet-identical)"
# =============================================================================
if [ ! -e "${NDI_DIR}/libndi.so.6" ]; then
    [ -n "${CAM_PW:-}" ] || fail "CAM_PW env required to fetch NDI runtime from cam1"
    command -v sshpass >/dev/null 2>&1 || apt-get install -y sshpass >/dev/null
    mkdir -p "$NDI_DIR"
    sshpass -p "$CAM_PW" scp -O -o StrictHostKeyChecking=no \
        "${DESKTOP_USER}@${NDI_PEER}:/usr/lib/ndi/libndi.so.*.*.*" "$NDI_DIR/" \
        || fail "NDI copy from cam1 failed"
    ( cd "$NDI_DIR" && REAL=$(ls libndi.so.*.*.* | head -1) && ln -sf "$REAL" libndi.so.6 && ln -sf libndi.so.6 libndi.so )
fi
echo "$NDI_DIR" > /etc/ld.so.conf.d/ndi.conf
ldconfig
# no `grep -q` on a pipe under pipefail — -q's early close SIGPIPEs ldconfig and fails the pipeline
ldconfig -p | grep libndi >/dev/null || fail "libndi not in linker cache"
apt-get install -y avahi-daemon >/dev/null 2>&1 || true
systemctl enable --now avahi-daemon >/dev/null 2>&1

# =============================================================================
step 5 "OBS Studio (official PPA, 32.x) — same major as the genlock base"
# =============================================================================
if ! command -v obs >/dev/null 2>&1; then
    add-apt-repository -y ppa:obsproject/obs-studio >/dev/null
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y obs-studio >/dev/null
fi
obs --version 2>/dev/null || true

# =============================================================================
step 6 "DistroAV NDI plugin (stock bootstrap — the genlock Linux build #460 swaps in later)"
# =============================================================================
if [ ! -e /usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so ] && [ ! -e /usr/lib/obs-plugins/distroav.so ]; then
    DEB_URL=$(curl -fsSL https://api.github.com/repos/DistroAV/DistroAV/releases/latest \
        | grep -oE '"browser_download_url": *"[^"]*x86_64-linux[^"]*\.deb"' | grep -oE 'https[^"]*' | head -1)
    [ -n "$DEB_URL" ] || fail "no DistroAV linux .deb asset found on latest release"
    curl -fsSL -o /tmp/distroav.deb "$DEB_URL"
    DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/distroav.deb >/dev/null \
        || dpkg -i /tmp/distroav.deb || fail "DistroAV install failed"
fi

# =============================================================================
step 7 "OBS pre-seed: WebSocket :4455 no-auth + SaveProjectors + no first-run wizard"
# =============================================================================
mkdir -p "$OBS_CFG"
seed_ini() {  # seed_ini FILE  — append our sections only if the file has no [OBSWebSocket] yet
    local f="$1"
    touch "$f"
    if ! grep -q '^\[OBSWebSocket\]' "$f"; then
        cat >> "$f" <<'EOF'

[OBSWebSocket]
ServerEnabled=true
ServerPort=4455
AuthRequired=false
FirstLoad=false
EOF
    fi
    if ! grep -q '^SaveProjectors=' "$f"; then
        printf '\n[BasicWindow]\nSaveProjectors=true\n' >> "$f"
    fi
    if ! grep -q '^LastVersion=' "$f"; then
        printf '\n[General]\nLastVersion=536936450\n' >> "$f"   # 32.1.2 — suppress first-run wizard
    fi
}
seed_ini "$OBS_CFG/global.ini"
seed_ini "$OBS_CFG/user.ini"
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$OBS_CFG"

# =============================================================================
step 8 "Desktop icon + autostart (reboot lands cutting-ready)"
# =============================================================================
APP_DESKTOP=$(ls /usr/share/applications/com.obsproject.Studio.desktop 2>/dev/null || true)
mkdir -p "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"
if [ -n "$APP_DESKTOP" ]; then
    cp -f "$APP_DESKTOP" "$USER_HOME/.config/autostart/obs.desktop"
    cp -f "$APP_DESKTOP" "$USER_HOME/Desktop/obs.desktop"
    chmod +x "$USER_HOME/Desktop/obs.desktop"
    sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        gio set "$USER_HOME/Desktop/obs.desktop" metadata::trusted true 2>/dev/null || true
fi
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"

# =============================================================================
step 9 "Launch OBS on the desktop session (X11 :0)"
# =============================================================================
if ! pgrep -x obs >/dev/null; then
    sudo -u "$DESKTOP_USER" DISPLAY=:0 DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        nohup obs >/tmp/obs-launch.log 2>&1 &
    sleep 8
fi
pgrep -x obs >/dev/null || fail "OBS did not start (see /tmp/obs-launch.log)"

# =============================================================================
step 10 "Verify: WebSocket :4455 reachable"
# =============================================================================
for i in $(seq 1 15); do
    if (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; then exec 3>&-; echo "  WS :4455 up"; break; fi
    [ "$i" -eq 15 ] && fail "OBS WebSocket :4455 not listening"
    sleep 2
done

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}imag-nb base provisioning DONE${NC}"
echo -e "${GREEN}NEXT (from dev1): scripts/imag_scenes.py --host ${STATIC_IP}   # profile+scenes${NC}"
echo -e "${GREEN}       then:      scripts/imag_scenes.py --host ${STATIC_IP} --projector   # once HDMI monitor connected${NC}"
echo -e "${GREEN}========================================${NC}"
