#!/bin/bash
#
# imag-nb One-Shot Setup (#458) — provision a Linux notebook as the 60fps IMAG OBS box
#
# Runs ON the imag box as root (same model as setup-device.sh). Idempotent — safe to re-run.
# After it finishes, run scripts/imag_scenes.py from dev1 to seed the OBS profile/scenes over
# WebSocket (and later `imag_scenes.py --projector` once the HDMI monitor is connected).
#
# Usage (on the box):
#   sudo CAM_PW=<fleet-pw> GH_TOKEN=<gh-pat-with-repo-scope> ./setup-imag.sh [--yes]
#
# GH_TOKEN (repo-read scope) is required for the genlock hot-swap step: imag-nb runs the CUSTOMIZED genlock
# OBS+DistroAV build (#460), hot-swapped over the PPA base — the artifacts are GitHub Actions
# workflow artifacts on this PRIVATE repo, which `gh run download` needs auth to fetch.
#
# Topology (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md):
#   6× cam box NDI 1080p60 -> imag-nb OBS (1080p60 low-latency IMAG, genlock build #460) ->
#   HDMI program projector
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
TOTAL_STEPS=17

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

# --- PURE functions (no network, no root, no side effects — sourced + unit-tested from
# tests/setup_imag_pure_functions.rs against synthetic fixtures; the BASH_SOURCE guard below
# skips the destructive provisioning flow when sourced) ---------------------------------------

# manifest_sha_for_path MANIFEST RELPATH -> the #120 manifest's recorded sha256 for RELPATH, or
# fails loud if RELPATH isn't listed. Narrow, single-purpose lookup (NOT a full bundle-completeness
# walk like scripts/genlock-manifest.sh --check) — setup-imag.sh only ever needs THREE specific
# files verified (libobs.so.30 + distroav.so + #499 bin/obs), never all ~1600 bundle files, so
# hashing everything would be pure waste (the bundle also carries the Qt frontend + locale data +
# default plugins, none of which this script installs). setup-imag.sh is copied to the box
# standalone (no sibling scripts/genlock-manifest.sh checked out there), so this cannot shell out
# to the repo's own tool.
manifest_sha_for_path() {
    local manifest="$1" relpath="$2" sha
    sha="$(jq -r --arg p "$relpath" '.files[] | select(.path == $p) | .sha256' "$manifest")" \
        || fail "cannot parse $manifest"
    [ -n "$sha" ] || fail "#460 manifest lists no entry for $relpath — refuse to trust an unverifiable file"
    printf '%s\n' "$sha"
}

# verify_file_sha FILE EXPECTED_SHA LABEL -> fail loud on any mismatch (corrupted/tampered file).
verify_file_sha() {
    local f="$1" want="$2" label="$3" got
    [ -f "$f" ] || fail "$label: file missing at $f"
    got="$(sha256sum "$f" | awk '{print $1}')"
    [ "$got" = "$want" ] || fail "$label: sha256 mismatch (want $want got $got) — corrupted/tampered artifact"
}

# --- source-guard: when sourced (the unit tests), stop here — never run the destructive
# provisioning flow below. Same convention as scripts/launch-obs-genlock.sh /
# scripts/genlock-manifest.sh / scripts/drift-guard.sh. -----------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0
fi

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
step 2 "Network performance tuning (#486): sysctl + EEE/flow-control off on the NDI NIC"
# =============================================================================
# imag aggregates 6x concurrent NDI 1080p60 streams over a single USB-ethernet NIC on stock
# buffers/EEE — exactly the jitter the cam fleet already tuned away (setup-device.sh STEP 14).
# Scoped to the ONE $NIC resolved in step 1 above — NOT a for-every-interface loop (imag also
# carries Wi-Fi/other adapters that must stay untouched).
cat > /etc/sysctl.d/99-network-performance.conf <<'EOF'
# Network performance optimizations for low-latency streaming (mirrors setup-device.sh STEP 14)

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

# EEE (Green Ethernet) + flow-control off, scoped to $NIC only. Two mechanisms, belt-and-
# suspenders (some USB-ethernet chipsets don't implement these ioctls at all — `|| true`
# throughout): (1) a networkd-dispatcher hook for interface-routable/hotplug events, and
# (2) an immediate one-time apply now (re-applied at every boot via the governor step's rc.local).
mkdir -p /etc/networkd-dispatcher/routable.d
cat > /etc/networkd-dispatcher/routable.d/optimize-nic <<NICEOF
#!/bin/bash
# Disable EEE (Green Ethernet) and flow control for low latency — scoped to imag's NDI NIC only.
if [ "\$IFACE" = "${NIC}" ]; then
    ethtool --set-eee "${NIC}" eee off 2>/dev/null || true
    ethtool -A "${NIC}" rx off tx off 2>/dev/null || true
fi
NICEOF
chmod +x /etc/networkd-dispatcher/routable.d/optimize-nic
ethtool --set-eee "$NIC" eee off 2>/dev/null || true
ethtool -A "$NIC" rx off tx off 2>/dev/null || true
echo "  sysctl: buffers+BBR+nodelay+IPv6-off applied; EEE/flow-control off on $NIC"

# =============================================================================
step 3 "DanteSync (#479): pin imag's system clock to the cluster master (genlock needs it)"
# =============================================================================
# imag's genlock render tick + FIFO ts-align read clock_gettime(CLOCK_REALTIME) — the system
# clock. Without cluster clock discipline, imag free-runs vs the dante-disciplined cameras and
# the genlock FIFO ts_head_skew drifts unbounded (live-proven 2026-07-04: skew drifted
# 474->541ms/80min, CAM underruns climbing). DanteSync OWNS the clock (ops skill hard rule) —
# NEVER timesyncd/chrony/ptp4l alongside it. Pin the NIC ($NIC, resolved in step 1) since imag is
# a notebook with other network interfaces that must not be mistaken for the rig link.
DANTESYNC_REPO="zbynekdrlik/dantesync"
if [ ! -x /usr/local/bin/dantesync ]; then
    DANTESYNC_URL="$(curl -fsSL "https://api.github.com/repos/${DANTESYNC_REPO}/releases/latest" 2>/dev/null \
        | grep -o '"browser_download_url": *"[^"]*dantesync-linux-amd64"' \
        | grep -o 'https://[^"]*' | head -1 || true)"
    if [ -n "$DANTESYNC_URL" ]; then
        curl -fsSL "$DANTESYNC_URL" -o /usr/local/bin/dantesync || fail "dantesync download failed"
    elif [ -n "${CAM_PW:-}" ]; then
        command -v sshpass >/dev/null 2>&1 || apt-get install -y sshpass >/dev/null
        sshpass -p "$CAM_PW" scp -O -o StrictHostKeyChecking=no \
            "${DESKTOP_USER}@${NDI_PEER}:/usr/local/bin/dantesync" /usr/local/bin/dantesync \
            || fail "dantesync copy from cam1 fallback failed"
    else
        fail "dantesync: no GitHub release asset found and CAM_PW unset for the cam-box fallback copy"
    fi
    chmod +x /usr/local/bin/dantesync
fi
[ -x /usr/local/bin/dantesync ] || fail "dantesync binary missing after install attempt"

cat > /etc/systemd/system/dantesync.service <<EOF
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantesync -i ${NIC} --ntp-server strih.lan
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

# DanteSync OWNS the clock — MASK systemd-timesyncd so nothing else ever disciplines it (ops
# skill hard rule: NEVER chrony/ptp4l/timesyncd alongside dantesync). Mask BEFORE enabling
# dantesync so the two clock sources can never race even for one boot cycle.
timedatectl set-ntp false 2>/dev/null || true
systemctl disable --now systemd-timesyncd >/dev/null 2>&1 || true
systemctl mask systemd-timesyncd >/dev/null 2>&1 || true

systemctl daemon-reload
systemctl enable dantesync >/dev/null 2>&1 || true
# #491: stamp the restart instant, then FORCE a restart so the running daemon matches the
# just-(re)installed binary+unit, and verify a FRESH post-restart lock. A re-provision restarts an
# already-locked dantesync; PTP re-acquisition then takes 30-70s while the pre-restart LOCK lines
# scroll out of a `-n 50` window — the old 60s / `-n 50` check false-failed even though dantesync
# re-locked fine. The journal read is ANCHORED to the restart instant (--since) so a stale
# pre-restart LOCK can't satisfy it and a fresh one can't be scrolled out; budget is 150s.
DANTESYNC_RESTART_EPOCH="$(date +%s)"
systemctl restart dantesync

# Verify PTP/NTP lock — the Linux equivalent of the ops-skill journalctl check. Accepts either
# PTP LOCK/NANO or the NTP-fallback offset line (grandmaster may be transiently absent).
DANTESYNC_LOCKED=0
for i in $(seq 1 75); do
    if journalctl -u dantesync --no-pager --since "@$DANTESYNC_RESTART_EPOCH" 2>/dev/null | grep -qE '\[PTP\][[:space:]]+(LOCK|NANO)|\[NTP\] offset'; then
        DANTESYNC_LOCKED=1
        break
    fi
    sleep 2
done
[ "$DANTESYNC_LOCKED" -eq 1 ] || fail "dantesync did not report PTP/NTP lock within 150s of restart — genlock clock discipline not established (check journalctl -u dantesync)"
echo "  dantesync locked to strih.lan via $NIC (timesyncd masked)"

# =============================================================================
step 4 "Max performance: governor + no USB/NIC powersave (USB-ethernet feeds the NDI!)"
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
cat > /etc/rc.local <<EOF
#!/bin/bash
# imag-nb boot tuning (fleet parity): governor + USB autosuspend off (USB NIC!) + NIC powersave off
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "\$g"; done
for u in /sys/bus/usb/devices/*/power/control; do echo on > "\$u" 2>/dev/null; done
for n in /sys/class/net/*/device/power/control; do echo on > "\$n" 2>/dev/null; done
# #486: EEE/flow-control off on the rig NDI NIC — reapplied every boot (belt-and-suspenders
# alongside step 2's networkd-dispatcher hook; some USB-ethernet chipsets reset EEE state).
ethtool --set-eee ${NIC} eee off 2>/dev/null || true
ethtool -A ${NIC} rx off tx off 2>/dev/null || true
exit 0
EOF
chmod +x /etc/rc.local
systemctl daemon-reload
systemctl enable --now cpu-performance.service >/dev/null 2>&1
bash /etc/rc.local
grep -q performance /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor || fail "governor not performance"

# =============================================================================
step 5 "Never sleep: lid ignore + sleep masked + GNOME idle/blank/lock off"
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
step 6 "Boot safety net (#487): kernel apt-hold + initrd-guarantee hook + unattended-upgrade lockdown"
# =============================================================================
# Ports setup-device.sh's #295 brick-prevention stack onto imag-nb, run BEFORE the lowlatency
# kernel (#482) and CPU-isolation (#483) grub.d drops below so both are safe to apply: a kernel
# that silently gains a new image via unattended-upgrades, or one whose initrd never got generated
# before grub picked it as the default, is exactly what bricked CAM3/CAM4 (#295). Unlike the cam
# fleet's appliance policy (setup-device.sh STEP 15 fully disables unattended-upgrades), imag does
# NOT disable it wholesale — step 14 below deliberately keeps security updates flowing (only their
# schedule is pinned, #485). So here we pin the KERNEL specifically (apt-mark hold + an
# Unattended-Upgrade package-blacklist entry) and lock Automatic-Reboot to false, rather than
# masking the whole service.
# Found in review: a bare `cmd || echo WARNING` is correctly non-fatal, but the step's closing
# summary echo below must NOT unconditionally claim "kernel pinned" when the hold actually
# failed -- track the real outcome so the summary line reflects reality instead of asserting
# success next to (or instead of) the WARNING.
KERNEL_HOLD_OK=1
apt-mark hold linux-image-generic-hwe-24.04 linux-headers-generic-hwe-24.04 \
    linux-generic-hwe-24.04 "linux-headers-$(uname -r)" "linux-image-$(uname -r)" \
    >/dev/null 2>&1 \
    || { KERNEL_HOLD_OK=0; echo "  WARNING: apt-mark hold of the generic kernel packages failed"; }
cat > /etc/apt/apt.conf.d/51imag-kernel-lockdown <<'EOF'
// #487: the kernel is pinned (apt-mark hold) -- never let unattended-upgrades touch it, and never
// let it reboot the box unattended. Automatic-Reboot is already Ubuntu's default (false); pinning
// it here explicitly means a future distro/package default change can never silently flip it.
Unattended-Upgrade::Package-Blacklist {
    "linux-image";
    "linux-headers";
    "linux-generic";
    "linux-lowlatency";
    "lowlatency-kernel";
};
Unattended-Upgrade::Automatic-Reboot "false";
EOF
# #295: any FUTURE kernel install must always get an initrd. This /etc/kernel/postinst.d hook sorts
# before grub's own `zz-update-grub` hook, so a missing initrd is regenerated BEFORE grub is
# updated -- identical mechanism to setup-device.sh STEP 15/16, ported here verbatim.
mkdir -p /etc/kernel/postinst.d
cat > /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee << 'EOF'
#!/bin/sh
# #295/#487: guarantee every installed kernel has an initrd (a kernel without one bricked
# CAM3/CAM4 on the fleet -- the same class of failure this hook prevents on imag-nb).
set -e
version="$1"
[ -n "$version" ] || exit 0
if [ ! -e "/boot/initrd.img-${version}" ]; then
    update-initramfs -c -k "${version}"
fi
EOF
chmod +x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee
if [ "$KERNEL_HOLD_OK" -eq 1 ]; then
    echo "  #487: kernel pinned (apt-mark hold), unattended-upgrades kernel-blacklisted + Automatic-Reboot=false, initrd hook installed"
else
    echo "  #487: unattended-upgrades kernel-blacklisted + Automatic-Reboot=false, initrd hook installed -- kernel apt-mark hold FAILED, see WARNING above"
fi

# safe_grub_regen -- the #295 safe-grub mechanism (side-effecting: root + filesystem, so this
# does NOT belong in the pure-functions section at the top of this file). GUARANTEES every
# installed kernel has an initrd BEFORE update-grub runs (a kernel without one bricked CAM3/CAM4,
# #295), then refuses to trust the regenerated grub.cfg if its default menu entry lacks a kernel
# image or an initrd -- never a raw ad-hoc grub edit. Reused by BOTH the #482 (lowlatency/
# preempt=full) and #483 (CPU isolation) grub.d drops below, called ONCE after both are written so
# update-grub only runs a single time for this pair of changes. Mirrors setup-device.sh STEP 10's
# initrd-guarantee + post-update-grub validation.
safe_grub_regen() {
    for vmlinuz in /boot/vmlinuz-*; do
        [ -e "$vmlinuz" ] || continue
        kver="${vmlinuz#/boot/vmlinuz-}"
        if [ ! -e "/boot/initrd.img-${kver}" ]; then
            echo -e "  ${YELLOW}#295: kernel ${kver} has no initrd — generating before grub${NC}"
            update-initramfs -c -k "${kver}"
        fi
    done
    update-grub
    local grub_cfg="/boot/grub/grub.cfg"
    if [ -f "$grub_cfg" ]; then
        local default_entry
        default_entry="$(awk '/^[[:space:]]*menuentry /{c++} c==1{print} c==2{exit}' "$grub_cfg")"
        if ! echo "$default_entry" | grep -qE '(vmlinuz|[[:space:]]linux )' \
            || ! echo "$default_entry" | grep -q 'initrd'; then
            fail "#295: grub default entry lacks a kernel image or initrd — aborting to avoid a brick"
        fi
    fi
}

# =============================================================================
step 7 "Low-latency kernel (#482): preempt=full via lowlatency-kernel config — zero downgrade"
# =============================================================================
# LIVE-VERIFIED FINDING (#482): there is NO lowlatency kernel IMAGE at the 6.17 line (the newest
# lowlatency images are 6.8/6.11 -- installing one would be a DOWNGRADE, losing 13th-gen
# CPU/iGPU/USB-NIC support). But the 6.17 generic kernel already IS PREEMPT_DYNAMIC, so
# linux-lowlatency-hwe-24.04 on 24.04 is a META package: it keeps the generic kernel image and
# only pulls in the `lowlatency-kernel` CONFIG package, which drops
# /etc/default/grub.d/99-lowlatency.cfg = GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full
# rcu_nocbs=all" -- full preemption on the NEWEST kernel, zero downgrade. This is a plain apt
# install (not a hand-authored grub.d file), so it needs no idempotent-append logic of its own --
# `apt-get install` on an already-installed package is already a no-op.
if ! dpkg -s lowlatency-kernel >/dev/null 2>&1; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y linux-lowlatency-hwe-24.04 >/dev/null \
        || fail "linux-lowlatency-hwe-24.04 install failed"
fi
[ -f /etc/default/grub.d/99-lowlatency.cfg ] \
    || fail "#482: lowlatency-kernel config package installed but /etc/default/grub.d/99-lowlatency.cfg is missing"
grep -q 'preempt=full' /etc/default/grub.d/99-lowlatency.cfg \
    || fail "#482: 99-lowlatency.cfg does not carry preempt=full — refuse to trust the config package"
# #487: never a raw ad-hoc grub edit -- hold the newly-installed kernel-config packages too, same
# as the generic kernel packages held in step 6, so an upgrade can't silently swap this config out.
apt-mark hold lowlatency-kernel linux-lowlatency-hwe-24.04 >/dev/null 2>&1 \
    || echo "  WARNING: apt-mark hold of the lowlatency-kernel config packages failed"
echo "  #482: lowlatency-kernel config installed (preempt=full on the 6.17 generic kernel, no downgrade)"
echo "  NOTE: preempt=full takes effect on the NEXT boot — this script does not reboot the box"

# =============================================================================
step 8 "CPU isolation (#483): P-core block for OBS + safe-grub regen"
# =============================================================================
# imag's OBS is a ~106-thread ~3-core consumer (6x NDI decode + render + genlock + audio), NOT the
# cam-box's single lean thread -- isolate the whole P-core BLOCK cpu2-11 (10 threads) for OBS, keep
# P-core0 (cpu0,1) for GNOME/Xorg + sshd/MCP housekeeping, and park default IRQs there too.
# nohz_full is scoped to ONLY the one core pair (10,11) that will host the future SCHED_FIFO
# genlock render-tick thread (#484) -- spreading it across the whole block would remove
# load-balancing signal (#303). rcu_nocbs=all already comes from 99-lowlatency.cfg above (covers
# 10,11). irqaffinity= is a BOOT-TIME default (not a runtime /proc/irq write) -- the USB-ethernet
# NDI IRQ is managed-MSI and rejects runtime affinity changes (#303). HT pairs verified LIVE via
# thread_siblings_list (not lscpu's flat count): cpu0=0-1, cpu2=2-3, cpu4=4-5, cpu6=6-7, cpu8=8-9,
# cpu10=10-11 (all P-core HT pairs), cpu12-15 = E-cores (no HT pairing).
cat > /etc/default/grub.d/98-imag-isolation.cfg <<'EOF'
# camera-box #483: reserve the P-core block cpu2-11 for the OBS thread pool (keep P-core0=cpu0,1
# + the E-cores cpu12-15 for GNOME/sshd/MCP/housekeeping); nohz_full only on cpu10,11 (the future
# genlock render-tick core, #484). rcu_nocbs=all is already set by 99-lowlatency.cfg (covers 10,11,
# which nohz_full requires). irqaffinity keeps IRQs off the isolated block.
GRUB_CMDLINE_LINUX_DEFAULT="$GRUB_CMDLINE_LINUX_DEFAULT isolcpus=2,3,4,5,6,7,8,9,10,11 nohz_full=10,11 irqaffinity=0,1,12,13,14,15"
EOF
# #295/#487: never a raw ad-hoc grub edit -- guarantee every kernel has an initrd, regenerate
# grub.cfg, then refuse to trust it if the default entry lacks a kernel image or an initrd. ONE
# call covers BOTH this drop-in and #482's 99-lowlatency.cfg above (they are only ever picked up
# by grub-mkconfig, which runs once here).
safe_grub_regen
echo "  #483: isolcpus=2-11 nohz_full=10,11 irqaffinity=0,1,12,13,14,15 written + grub regenerated"
echo "  NOTE: CPU isolation takes effect on the NEXT boot — this script does not reboot the box"

# =============================================================================
step 9 "NVIDIA dGPU driver (#500): nvidia-driver-595-open + PRIME nvidia-primary"
# =============================================================================
# imag-nb's HDMI program-projector output is physically wired through the NVIDIA dGPU (an RTX
# 5050 Laptop / Blackwell, PCI 10de:2dd8), NOT the Intel iGPU -- live-verified: the HDMI connector
# showed `disconnected` on every output until the dGPU was actually initialized. The PLAIN
# proprietary `nvidia-driver-595` package does NOT init Blackwell (`NVRM: RmInitAdapter failed!
# (0x22:0x56:1017)`, live-reproduced on imag-nb) -- it needs the OPEN kernel-modules flavor.
# `ubuntu-drivers devices` (live-checked on imag-nb) recommends plain `nvidia-driver-595` for this
# PCI id -- that recommendation is WRONG for this GPU; the `-open` variant is the deliberate,
# verified-working choice. `apt-cache search nvidia-driver` (live-checked) lists nothing newer
# than the 595 line as of this writing. Driver-upgrade freedom is explicitly wanted by the user
# ("pravdaze drivere musia byt upgradovane... nikto netvrdi ze musis pouzivat nejake stare lts") --
# re-check `ubuntu-drivers devices` / `apt-cache search nvidia-driver` for a newer `-open` release
# before reusing this pin verbatim; prefer the newest available `-open` flavor over 595 if one has
# since shipped.
# Found in review: a bare `dpkg -s <pkg> >/dev/null 2>&1` exit code alone is NOT a reliable
# "is it installed" check — dpkg -s exits 0 even for a package that was `apt remove`d (not purged)
# and now sits in "deinstall ok config-files" state (live-verified on this box: `dpkg -s
# alsa-base` exits 0 with `Status: deinstall ok config-files`). If the driver package were ever
# removed-not-purged between provisioning runs, that bare exit-code check would wrongly conclude
# "already installed", skip the apt-get install, and still run prime-select + safe_grub_regen on a
# box with no actual driver files. Check the Status field content instead (no `-q` on the piped
# grep — dpkg -s output is tiny, but this matches the same safe-read convention used elsewhere in
# this script rather than mixing conventions).
if ! dpkg -s nvidia-driver-595-open 2>/dev/null | grep '^Status: install ok installed' >/dev/null; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y nvidia-driver-595-open >/dev/null \
        || fail "nvidia-driver-595-open install failed"
fi
# PRIME nvidia-primary: on-demand PRIME mode left the HDMI dGPU output dead (live-verified) --
# nvidia must be the PRIMARY renderer so BOTH the HDMI output and the laptop's own eDP panel run
# on the RTX 5050.
command -v prime-select >/dev/null 2>&1 || fail "prime-select missing after nvidia-driver-595-open install"
prime-select nvidia || fail "prime-select nvidia failed"
# #295/#487: a DKMS driver install regenerates initramfs for the running kernel -- never trust
# that blindly. Reuse the SAME safe_grub_regen helper the #482/#483 grub.d drops call above
# (defined earlier in step 6): guarantee every kernel has an initrd, regenerate grub.cfg, and
# refuse to trust it if the default entry lacks a kernel image or an initrd.
safe_grub_regen
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
    echo "  #500: nvidia-smi already enumerates: $(nvidia-smi -L | head -1)"
else
    echo "  #500: nvidia-smi not yet enumerating the GPU (expected pre-reboot on a fresh driver install)"
fi
echo "  #500: nvidia-driver-595-open installed, prime-select nvidia set, grub/initrd re-verified"
echo "  NOTE: the PRIME GPU mode + the new DKMS module take full effect on the NEXT boot — this script does not reboot the box"

# =============================================================================
step 10 "NDI runtime 6.3.2 from cam1 -> ${NDI_DIR} (fleet-identical)"
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
# DistroAV's Linux loader (plugin-main.cpp load_ndilib) scans ONLY /usr/lib, /usr/lib64,
# /usr/local/lib (non-recursive — NOT the multiarch dir, NOT the ld cache) for libndi.so.<N>.
# Live-proven on imag-nb: without this symlink DistroAV logs ERR-404 despite a valid ld cache.
ln -sf "$(readlink -f "$NDI_DIR"/libndi.so.6)" /usr/local/lib/libndi.so.6
apt-get install -y avahi-daemon >/dev/null 2>&1 || true
systemctl enable --now avahi-daemon >/dev/null 2>&1

# =============================================================================
step 11 "OBS Studio (official PPA, 32.x) — base install; libobs.so.30 gets genlock hot-swapped next"
# =============================================================================
if ! command -v obs >/dev/null 2>&1; then
    add-apt-repository -y ppa:obsproject/obs-studio >/dev/null
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y obs-studio >/dev/null
fi
obs --version 2>/dev/null || true

# =============================================================================
step 12 "Genlock hot-swap (#460): deploy patched libobs.so.30 + distroav.so over the PPA base"
# =============================================================================
# imag-nb MUST run the CUSTOMIZED genlock OBS+DistroAV, not stock DistroAV (user directive,
# #458 comment 2026-07-03) — the stock-bootstrap path this step used to run is dead. The PPA
# obs-studio package installed in the prior step ships libobs.so.30 with SONAME "libobs.so.30"
# (live-verified on imag-nb) — IDENTICAL to the genlock build's own SONAME — so the genlock
# libobs.so.30 hot-swaps cleanly over it, exactly mirroring the Windows obs.dll hot-swap (see
# .claude/skills/genlock). libobs.so.30 (the genlock render-tick/ts-align patches live in
# obs-source.c/obs-video.c, both inside libobs core), distroav.so, AND the FRONTEND executable
# /usr/bin/obs are all swapped — obs-frontend-api/obs-opengl/obs-scripting (the shared LIBRARIES)
# are untouched by the genlock patches and stay PPA-stock. No stock DistroAV .deb is installed any
# more — the genlock-built distroav.so IS the plugin.
#
# #499: /usr/bin/obs (the frontend EXECUTABLE, compiled from vendor/obs-studio/frontend/) MUST
# also be swapped — skipping it leaves a half-stock box. The multiview render-budget decouple
# (#276 obs_display_set_render_divisor / #278 adaptive EWMA skip / #293 anti-starvation floor) AND
# the "newlevel.media" window title both live in the frontend EXE, NOT in libobs.so.30 — exactly
# the "frontend lives in the exe, not the DLL" gotcha already documented for Windows
# (.claude/skills/genlock/SKILL.md). A genlock deploy that only swaps libobs.so.30/distroav.so
# leaves the stock frontend running: the multiview code path that calls
# obs_display_set_render_divisor() never runs, and multiview chokes the program render (live-
# proven 2026-07-04: 16fps/59ms with the stock frontend vs 60fps/1.7ms once bin/obs was swapped).
GENLOCK_REPO="zbynekdrlik/camera-box"
GENLOCK_WORKFLOW="linux-genlock.yml"
GENLOCK_MARKER_DIR="/opt/obs-genlock"
GENLOCK_BACKUP_ROOT="/opt/obs-backup"
LIBOBS_REAL="/usr/lib/x86_64-linux-gnu/libobs.so.30"
DISTROAV_REAL="/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so"
OBS_FRONTEND_REAL="/usr/bin/obs"

command -v jq >/dev/null 2>&1 || apt-get install -y jq >/dev/null 2>&1 || fail "jq install failed (needed for #460 manifest verify)"

# manifest_sha_for_path() and verify_file_sha() are defined at the TOP of this file (pure
# functions, no root/network needed) — see the source-guard block near the top for why: they are
# sourced + unit-tested directly against synthetic fixtures from
# tests/setup_imag_pure_functions.rs, which needs the destructive flow below to be skippable.

DEPLOYED_SHA=""
[ -f "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" ] && DEPLOYED_SHA="$(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt")"

if ! command -v gh >/dev/null 2>&1; then
    # Capture curl's output into a variable FIRST, then grep it — never `curl | grep | head -1`
    # as a live pipe (the same early-pipe-closure class the SONAME check below documents; a
    # materialised string can't truncate a still-writing upstream process under pipefail).
    GH_RELEASE_JSON="$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest)" \
        || fail "could not reach api.github.com/repos/cli/cli/releases/latest"
    # `|| true` is load-bearing: if BOTH greps find zero matches (e.g. the GitHub API response
    # shape ever changes), `grep` exits non-zero, and under pipefail that propagates to this bare
    # assignment — `set -e` would abort the script HERE, before the very next line's intended
    # `fail "no gh CLI ... asset found"` ever runs, with NO diagnostic at all. Same footgun class
    # (and same fix) as the LATEST_LOG lookup in the verify step — found in review.
    GH_DEB_URL="$(printf '%s' "$GH_RELEASE_JSON" \
        | grep -oE '"browser_download_url": *"[^"]*linux_amd64\.deb"' | grep -oE 'https[^"]*' | head -1 || true)"
    [ -n "$GH_DEB_URL" ] || fail "no gh CLI linux_amd64 .deb asset found on latest cli/cli release"
    curl -fsSL -o /tmp/gh-cli.deb "$GH_DEB_URL"
    DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/gh-cli.deb >/dev/null \
        || dpkg -i /tmp/gh-cli.deb || fail "gh CLI install failed"
fi
command -v gh >/dev/null 2>&1 || fail "gh CLI still missing after install attempt"
[ -n "${GH_TOKEN:-}" ] || fail "GH_TOKEN env required (repo-read scope) to download the private #460 CI artifact"

# GENLOCK_RUN_ID overrides run resolution (pin a specific build). Default: the latest SUCCESSFUL
# linux-genlock.yml run on the dev branch — the workflow triggers on push to dev, but ALSO carries
# a bare `workflow_dispatch:` with no branch restriction, so an unfiltered `gh run list` could pick
# up an experimental manual dispatch on some other branch as "latest successful" (`--branch main`
# is wrong for a different reason: linux-genlock.yml never runs ON main — `gh run list -w
# linux-genlock.yml -b main` is EMPTY, live-verified — a push-triggered run's headBranch stays
# "dev" even after the commit later merges to main). `--branch dev` is the correct filter: it
# matches every push-to-dev run AND any workflow_dispatch explicitly run against dev, while
# excluding a stray dispatch against a feature/experimental branch.
RUN_ID="${GENLOCK_RUN_ID:-}"
if [ -z "$RUN_ID" ]; then
    # `-q '.[0].databaseId'` on an EMPTY result list yields the literal 4-char text "null" (jq's
    # normal behaviour indexing a nonexistent array element), which would silently pass the
    # `[ -n "$RUN_ID" ]` guard below as if it were a real id. `// empty` collapses that case to a
    # genuinely empty string so the guard actually fires.
    RUN_ID="$(gh run list --repo "$GENLOCK_REPO" --workflow "$GENLOCK_WORKFLOW" --branch dev \
        --status success --limit 1 --json databaseId -q '.[0].databaseId // empty')"
    [ -n "$RUN_ID" ] || fail "no successful $GENLOCK_WORKFLOW run found on $GENLOCK_REPO (branch dev)"
fi

# Idempotency check BEFORE any download: `gh run view --json headSha` is a single cheap API call
# (the workflow's own GENLOCK_BUILD_SHA.txt is just `git rev-parse HEAD` at build time, i.e. the
# SAME commit as the run's headSha — no need to pull the ~90MB bundle just to learn this). Skips
# the whole download+verify+install cycle on every no-op re-run (setup-imag.sh may be re-run for
# unrelated reasons, and linux-genlock.yml fires on every dev push touching vendor/**).
NEW_SHA="$(gh run view "$RUN_ID" --repo "$GENLOCK_REPO" --json headSha -q .headSha)"
[ -n "$NEW_SHA" ] || fail "could not read headSha for run $RUN_ID"

NOOP_VALID=0
if [ "$DEPLOYED_SHA" = "$NEW_SHA" ] && [ -f "$LIBOBS_REAL" ] && [ -f "$DISTROAV_REAL" ] && [ -f "$OBS_FRONTEND_REAL" ]; then
    NOOP_VALID=1
    # #472 defense-in-depth (PR #471 review, deliberately deferred): the SHA-marker + file
    # *existence* check above trusts the on-disk marker without re-verifying the installed
    # BYTES. If the deployed libobs.so.30/distroav.so/bin-obs were ever silently reverted (e.g. an
    # unattended `apt upgrade` slipping past the apt-mark hold, or manual tampering), a no-op
    # re-run would wrongly report "already deployed" and skip re-swapping — the verify step's runtime
    # log-verify would still catch it eventually, but only after a confusing failure. Re-verify
    # the CURRENTLY INSTALLED files against the manifest cached locally on the LAST successful
    # swap (pure local sha256 compare, zero network cost, only paid on the already-rare re-run
    # path) and fall through to a fresh re-install on any mismatch.
    CACHED_MANIFEST="$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json"
    if [ -f "$CACHED_MANIFEST" ]; then
        WANT_LIBOBS_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'lib/x86_64-linux-gnu/libobs.so.30')"
        GOT_LIBOBS_SHA_CACHED="$(sha256sum "$LIBOBS_REAL" | awk '{print $1}')"
        WANT_DISTROAV_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
        GOT_DISTROAV_SHA_CACHED="$(sha256sum "$DISTROAV_REAL" | awk '{print $1}')"
        WANT_OBS_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'bin/obs')"
        GOT_OBS_SHA_CACHED="$(sha256sum "$OBS_FRONTEND_REAL" | awk '{print $1}')"
        if [ "$GOT_LIBOBS_SHA_CACHED" != "$WANT_LIBOBS_SHA_CACHED" ] || [ "$GOT_DISTROAV_SHA_CACHED" != "$WANT_DISTROAV_SHA_CACHED" ] \
            || [ "$GOT_OBS_SHA_CACHED" != "$WANT_OBS_SHA_CACHED" ]; then
            echo "  WARNING: installed genlock bytes do not match the cached manifest — forcing re-install"
            NOOP_VALID=0
        fi
    else
        echo "  WARNING: no cached $CACHED_MANIFEST to re-verify against — trusting the deployed-SHA marker"
    fi
fi

if [ "$NOOP_VALID" -eq 1 ]; then
    echo "  genlock build $NEW_SHA already deployed — no-op (skipped download)"
else
    GENLOCK_TMP="$(mktemp -d)"
    trap 'rm -rf "$GENLOCK_TMP"' EXIT

    gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n obs-genlock-linux-x86_64 --dir "$GENLOCK_TMP/bundle" \
        || fail "download of obs-genlock-linux-x86_64 failed (run $RUN_ID)"
    gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n distroav-linux-fast-so --dir "$GENLOCK_TMP/fast" \
        || fail "download of distroav-linux-fast-so failed (run $RUN_ID)"

    [ -f "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt" ] || fail "bundle missing GENLOCK_BUILD_SHA.txt"
    BUNDLE_SHA="$(cat "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt")"
    [ "$BUNDLE_SHA" = "$NEW_SHA" ] || \
        fail "bundle build SHA ($BUNDLE_SHA) != resolved run headSha ($NEW_SHA) — refuse to deploy"
    [ -f "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt" ] || fail "distroav-linux-fast-so missing DISTROAV_BUILD_SHA.txt"
    FAST_SHA="$(cat "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt")"
    [ "$FAST_SHA" = "$NEW_SHA" ] || \
        fail "distroav-linux-fast-so build SHA ($FAST_SHA) != resolved run headSha ($NEW_SHA) — mismatched artifacts, refuse to deploy"

    BUNDLE_LIBOBS="$GENLOCK_TMP/bundle/lib/x86_64-linux-gnu/libobs.so.30"
    FAST_DISTROAV="$GENLOCK_TMP/fast/distroav.so"
    BUNDLE_OBS="$GENLOCK_TMP/bundle/bin/obs"
    [ -f "$FAST_DISTROAV" ] || fail "distroav-linux-fast-so missing distroav.so"
    [ -f "$BUNDLE_OBS" ] || fail "bundle missing bin/obs (#499: the frontend executable)"
    [ -f "$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json" ] || fail "bundle missing BUNDLE_MANIFEST.json (#120)"
    MANIFEST="$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json"

    # #120 sha256 verify — libobs.so.30 against the bundle's own manifest (both on disk).
    # NOTE: the manifest lookup MUST be a bare `VAR="$(...)"` assignment on its OWN line, never
    # embedded as one of several arguments to another command — `set -e` only aborts the script on
    # a subshell's failing exit status when that substitution IS the entire simple command (a bare
    # assignment); a `fail()`/`exit` inside a function called as `cmd "$(func)" other-arg` only
    # kills the command-substitution SUBSHELL, and `cmd` still runs with that argument silently
    # empty — live-verified with a minimal repro during review of this exact PR.
    WANT_LIBOBS_SHA="$(manifest_sha_for_path "$MANIFEST" 'lib/x86_64-linux-gnu/libobs.so.30')"
    verify_file_sha "$BUNDLE_LIBOBS" "$WANT_LIBOBS_SHA" "bundle libobs.so.30"
    # distroav.so has NO manifest of its own in the distroav-linux-fast-so artifact (it ships only
    # DISTROAV_BUILD_SHA.txt — a commit id, not a content hash) — cross-check it against the
    # BUNDLE's manifest entry for the SAME file instead (both jobs build distroav.so from the
    # identical commit; live-verified byte-identical across both jobs' outputs). Without this, the
    # one file actually plugged into OBS as the NDI-carrying plugin had ZERO integrity check.
    WANT_DISTROAV_SHA="$(manifest_sha_for_path "$MANIFEST" 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
    verify_file_sha "$FAST_DISTROAV" "$WANT_DISTROAV_SHA" \
        "distroav-linux-fast-so distroav.so (cross-checked against bundle manifest)"
    # #499: bin/obs (the frontend executable) ships IN the same bundle as libobs.so.30 (both are
    # part of the staged OBS rundir), so it has its OWN manifest entry — verify it the same way.
    WANT_OBS_SHA="$(manifest_sha_for_path "$MANIFEST" 'bin/obs')"
    verify_file_sha "$BUNDLE_OBS" "$WANT_OBS_SHA" "bundle bin/obs (frontend)"

    mkdir -p "$GENLOCK_MARKER_DIR" "$GENLOCK_BACKUP_ROOT"
    # Exactly TWO bounded backup dirs (never accumulate one per re-run — #185's unbounded target/
    # growth is the cautionary precedent): a permanent STOCK backup made once on the very first
    # swap ever (the forever rollback-to-PPA-stock path) and a PREVIOUS backup overwritten on every
    # swap (quick rollback to the immediately-prior deployed build). #499: the frontend gets the
    # SAME stock/previous treatment as libobs/distroav — a bare file under $GENLOCK_BACKUP_ROOT
    # (live-verified path, hand-created 2026-07-04) rather than nested in stock-pre-genlock/, since
    # it is a standalone executable, not a plugin library pair.
    STOCK_BACKUP="$GENLOCK_BACKUP_ROOT/stock-pre-genlock"
    PREV_BACKUP="$GENLOCK_BACKUP_ROOT/previous"
    OBS_FRONTEND_STOCK_BACKUP="$GENLOCK_BACKUP_ROOT/obs.stock"
    if [ ! -d "$STOCK_BACKUP" ]; then
        mkdir -p "$STOCK_BACKUP"
        [ -f "$LIBOBS_REAL" ] && cp -a "$LIBOBS_REAL" "$STOCK_BACKUP/libobs.so.30"
        [ -f "$DISTROAV_REAL" ] && cp -a "$DISTROAV_REAL" "$STOCK_BACKUP/distroav.so"
    fi
    if [ ! -f "$OBS_FRONTEND_STOCK_BACKUP" ]; then
        [ -f "$OBS_FRONTEND_REAL" ] && cp -a "$OBS_FRONTEND_REAL" "$OBS_FRONTEND_STOCK_BACKUP"
    fi
    rm -rf "$PREV_BACKUP"
    mkdir -p "$PREV_BACKUP"
    [ -f "$LIBOBS_REAL" ] && cp -a "$LIBOBS_REAL" "$PREV_BACKUP/libobs.so.30"
    [ -f "$DISTROAV_REAL" ] && cp -a "$DISTROAV_REAL" "$PREV_BACKUP/distroav.so"
    [ -f "$OBS_FRONTEND_REAL" ] && cp -a "$OBS_FRONTEND_REAL" "$PREV_BACKUP/obs"
    [ -n "$DEPLOYED_SHA" ] && echo "$DEPLOYED_SHA" > "$PREV_BACKUP/GENLOCK_BUILD_SHA.txt"

    install -m 0644 -o root -g root "$BUNDLE_LIBOBS" "$LIBOBS_REAL" || fail "libobs.so.30 hot-swap install failed"
    install -m 0644 -o root -g root "$FAST_DISTROAV" "$DISTROAV_REAL" || fail "distroav.so hot-swap install failed"
    install -m 0755 -o root -g root "$BUNDLE_OBS" "$OBS_FRONTEND_REAL" || fail "frontend obs hot-swap install failed (#499)"
    ldconfig

    # SONAME sanity check (no `-q` on a piped external command under pipefail — same early-close
    # SIGPIPE footgun documented for the step-4 ldconfig check; read the full small output instead).
    readelf -d "$LIBOBS_REAL" 2>/dev/null | grep 'SONAME.*\[libobs\.so\.30\]' >/dev/null \
        || fail "post-swap libobs.so.30 SONAME check failed — refuse a mismatched ABI"

    # #499 post-swap build-proof: the stock PPA frontend never references
    # obs_display_set_render_divisor (the #276/#278/#293 multiview render-budget decouple symbol)
    # — live-verified `nm -D -u` shows it as an UNDEFINED (U) symbol only on the genlock-built
    # frontend. A missing reference here means the wrong/stock binary got installed.
    # No `-q` on a piped external command under pipefail (same early-close SIGPIPE footgun as the
    # SONAME check above): `nm -D -u` on this binary emits ~2900 lines (~170KB, live-measured) and
    # the target symbol sits at line ~286 — `grep -q` would exit right after that early match,
    # SIGPIPE-ing `nm` mid-write, and under `set -euo pipefail` that would wrongly `fail()` a
    # CORRECT build. Read the full output instead.
    nm -D -u "$OBS_FRONTEND_REAL" 2>/dev/null | grep 'obs_display_set_render_divisor' >/dev/null \
        || fail "post-swap /usr/bin/obs does not reference obs_display_set_render_divisor — refuse a stock/wrong frontend binary (#499: multiview render-budget decouple would be missing)"

    echo "$NEW_SHA" > "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt"
    echo "$FAST_SHA" > "$GENLOCK_MARKER_DIR/DISTROAV_BUILD_SHA.txt"
    cp -a "$MANIFEST" "$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json"
    date -Is > "$GENLOCK_MARKER_DIR/DEPLOYED_AT"

    # Prevent an unattended `apt upgrade` from silently reverting libobs.so.30 back to PPA-stock
    # bytes behind the operator's back (dpkg still tracks obs-studio) — drift must be a deliberate
    # re-run of this script, never a background package update. distroav is NOT held: this rework
    # removed the stock DistroAV .deb install entirely, so `distroav` is no longer a dpkg package
    # at all (distroav.so is installed via a bare `install`, outside dpkg) — `apt upgrade` cannot
    # touch it regardless, holding it would be a no-op. Any hold failure is LOGGED, never silent —
    # it is a real (if soft) drift-protection guarantee, not a cosmetic nicety.
    if ! apt-mark hold obs-studio >/dev/null 2>&1; then
        echo "  WARNING: apt-mark hold obs-studio failed — an unattended apt upgrade could revert this deploy"
    fi

    # A swap while OBS is already running (a later re-run onto a NEWER build) needs OBS to
    # relaunch to pick up the new .so — mirrors the Windows force-kill-then-relaunch convention
    # (launch-obs-genlock.sh's --force uses Stop-Process -Force = SIGKILL, never a bare SIGTERM).
    # The "Launch OBS" step's own `if ! pgrep -x obs` relaunch guard would otherwise SKIP relaunching a
    # still-exiting (SIGTERM'd but not yet dead) OBS, silently leaving the OLD build's process
    # resident even though the NEW build's bytes + marker are already on disk — so SIGKILL and
    # WAIT for actual death here, failing loud if it won't die within budget.
    if pgrep -x obs >/dev/null 2>&1; then
        pkill -9 -x obs || true
        for _ in $(seq 1 10); do
            pgrep -x obs >/dev/null 2>&1 || break
            sleep 1
        done
        pgrep -x obs >/dev/null 2>&1 && fail "old obs64 would not die after SIGKILL — cannot safely relaunch onto the new build"
    fi
    echo "  genlock build $NEW_SHA deployed (was: ${DEPLOYED_SHA:-none, PPA stock})"
fi

# =============================================================================
step 13 "OBS pre-seed: WebSocket :4455 no-auth + SaveProjectors + no first-run wizard"
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
step 14 "Desktop de-jitter (#485): mask background jitter sources + OBS ProcessPriority=High"
# =============================================================================
# imag is a single-app OBS kiosk — no human ever browses, mails, or searches files on it. All
# masks below are low-risk + reversible; security updates stay ON (only their SCHEDULE is
# pinned, Automatic-Reboot is already false by Ubuntu default and is deliberately left untouched).

# systemd-oomd: known to kill WHOLE GNOME sessions (incl. OBS) on transient PSI memory-pressure
# spikes even with GB of RAM free — kernel OOM remains the real backstop.
systemctl disable --now systemd-oomd.service systemd-oomd.socket >/dev/null 2>&1 || true
systemctl mask systemd-oomd.service systemd-oomd.socket >/dev/null 2>&1 || true

# File indexer + groupware factories: no files worth indexing, no mail/calendar account, ever.
DESKTOP_UID="$(id -u "$DESKTOP_USER")"
u_systemctl() {
    sudo -u "$DESKTOP_USER" \
        XDG_RUNTIME_DIR="/run/user/${DESKTOP_UID}" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${DESKTOP_UID}/bus" \
        systemctl --user "$@" >/dev/null 2>&1 || true
}
u_systemctl mask tracker-miner-fs-3.service tracker-miner-fs-control-3.service \
    tracker-writeback-3.service tracker-xdg-portal-3.service
sudo -u "$DESKTOP_USER" tracker3 reset -s >/dev/null 2>&1 || true
u_systemctl mask evolution-source-registry.service evolution-calendar-factory.service \
    evolution-addressbook-factory.service evolution-user-prompter.service evolution-alarm-notify.service

# apport/whoopsie: apport writes multi-GB core dumps right when OBS already crashed (worst-time
# disk spike); whoopsie phones crash reports home — neither has value on a kiosk appliance.
systemctl disable --now apport.service whoopsie.service >/dev/null 2>&1 || true
systemctl mask apport.service whoopsie.service >/dev/null 2>&1 || true

# snapd: hold auto-refresh forever (unused firefox/snap-store snaps) — a mid-service "restart to
# update" banner popping over the fullscreen program output is the failure mode this avoids.
snap refresh --hold=forever >/dev/null 2>&1 || true

# apt-daily-upgrade.timer: pin the SCHEDULE to a fixed off-hours time via a drop-in — security
# updates themselves stay fully enabled, never disabled here.
mkdir -p /etc/systemd/system/apt-daily-upgrade.timer.d
cat > /etc/systemd/system/apt-daily-upgrade.timer.d/imag-offhours.conf <<'EOF'
[Timer]
OnCalendar=
OnCalendar=*-*-* 04:00
RandomizedDelaySec=30min
EOF
systemctl daemon-reload
systemctl restart apt-daily-upgrade.timer >/dev/null 2>&1 || true

# GNOME animations off — one less compositor cost on the fullscreen program output.
gs org.gnome.desktop.interface enable-animations false

# OBS-native: ProcessPriority=High is OBS's own render-starvation knob (zero cost; ships Normal
# by default). global.ini was just seeded above — flip the value in place if present, else
# append a [General] section (same duplicate-section convention seed_ini already uses for
# LastVersion; Qt's ini backend merges duplicate group headers).
if grep -q '^ProcessPriority=' "$OBS_CFG/global.ini" 2>/dev/null; then
    sed -i 's/^ProcessPriority=.*/ProcessPriority=High/' "$OBS_CFG/global.ini"
else
    printf '\n[General]\nProcessPriority=High\n' >> "$OBS_CFG/global.ini"
fi
chown "$DESKTOP_USER:$DESKTOP_USER" "$OBS_CFG/global.ini"
echo "  de-jitter: oomd/tracker/evolution/apport/whoopsie masked, snapd held, apt-daily pinned 04:00, animations off, OBS ProcessPriority=High"

# =============================================================================
step 15 "Desktop icon + autostart (reboot lands cutting-ready)"
# =============================================================================
APP_DESKTOP=$(ls /usr/share/applications/com.obsproject.Studio.desktop 2>/dev/null || true)
mkdir -p "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"
if [ -n "$APP_DESKTOP" ]; then
    cp -f "$APP_DESKTOP" "$USER_HOME/.config/autostart/obs.desktop"
    cp -f "$APP_DESKTOP" "$USER_HOME/Desktop/obs.desktop"
    chmod +x "$USER_HOME/Desktop/obs.desktop"
    # #483: pin the AUTOSTART launch to the OBS P-core block -- WITHOUT this, isolcpus (once the
    # #483 grub change takes effect on the next boot) would STARVE OBS onto cpu0,1,12-15 (the
    # block reserved for GNOME/housekeeping/E-cores). The Desktop icon (double-click launch) is
    # deliberately left plain `Exec=obs` -- only the autostart path is pinned (live-verified).
    sed -i 's/^Exec=obs$/Exec=taskset -c 2-11 obs/' "$USER_HOME/.config/autostart/obs.desktop"
    sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        gio set "$USER_HOME/Desktop/obs.desktop" metadata::trusted true 2>/dev/null || true
fi
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"

# =============================================================================
step 16 "Launch OBS on the desktop session (X11 :0)"
# =============================================================================
# Clear stale OBS crash sentinels BEFORE relaunching — mirrors the Windows
# launch-obs-genlock.sh convention (Remove-Item .sentinel\* before Start-Process obs64). On a
# genlock RE-deploy, the genlock hot-swap step force-restarts (SIGKILL) a running OBS to load the
# swapped libobs.so.30/distroav.so; without clearing the sentinel here first, the relaunched OBS
# pops the "Crash or unclean shutdown detected" recovery modal and hangs headless — WebSocket
# :4455 never comes up, and the verify step fails "not listening" even though the swap succeeded
# (hit live 2026-07-04 during a #463 re-deploy to imag-nb; recovered by hand).
rm -rf "${OBS_CFG}/.sentinel"/* 2>/dev/null || true
if ! pgrep -x obs >/dev/null; then
    # #483: pin this provisioning-time launch to the P-core block too, matching the autostart
    # entry above -- without taskset here, a provisioning launch (before the isolcpus grub change
    # is active, i.e. before the first post-provision reboot) still lands unpinned; after reboot,
    # an un-pinned `obs` would be STARVED onto the tiny cpu0,1,12-15 remainder once isolcpus takes
    # effect. `nice -n -5` was deliberately NOT added -- the desktop user lacks CAP_SYS_NICE
    # (live-confirmed, #483 comment).
    sudo -u "$DESKTOP_USER" DISPLAY=:0 DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        nohup taskset -c 2-11 obs >/tmp/obs-launch.log 2>&1 &
    sleep 8
fi
pgrep -x obs >/dev/null || fail "OBS did not start (see /tmp/obs-launch.log)"

# =============================================================================
step 17 "Verify: WebSocket :4455 + genlock render tick + DistroAV/NDI loaded"
# =============================================================================
for i in $(seq 1 15); do
    if (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; then exec 3>&-; echo "  WS :4455 up"; break; fi
    [ "$i" -eq 15 ] && fail "OBS WebSocket :4455 not listening"
    sleep 2
done

# Genlock log verify — the Linux equivalent of scripts/launch-obs-genlock.sh's Windows
# log-verify (#257 proof): the OBS log is the AUTHORITATIVE runtime signal a stock/wrong build
# cannot fake. Same regex family as scripts/drift-guard.sh genlock_capability_from_log.
OBS_LOG_DIR="$OBS_CFG/logs"
# `|| true` is load-bearing: under `set -euo pipefail`, `ls` on a non-matching glob exits non-zero
# even though `head` succeeds with empty output, and pipefail propagates that failure to the bare
# assignment — `set -e` would abort the script HERE, before the very next line's intended
# `fail "no OBS log found..."` ever runs (same convention already used for the desktop-icon step's
# `APP_DESKTOP=$(ls ... 2>/dev/null || true)`).
LATEST_LOG="$(ls -t "$OBS_LOG_DIR"/*.txt 2>/dev/null | head -1 || true)"
[ -n "$LATEST_LOG" ] || fail "no OBS log found in $OBS_LOG_DIR — cannot verify the genlock build"
LOG_TEXT="$(cat "$LATEST_LOG")"
echo "$LOG_TEXT" | grep -iE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' >/dev/null \
    || fail "OBS log shows NO genlock capability marker in '$LATEST_LOG' — NOT the genlock build (check the #460 hot-swap step)"
echo "  genlock render tick ENABLED (#460 build proof)"
if echo "$LOG_TEXT" | grep -i '\[distroav\] plugin loaded' >/dev/null; then
    echo "  DistroAV plugin loaded"
else
    echo "  WARNING: no '[distroav] plugin loaded' line yet (may log lazily on first NDI activation)"
fi
if echo "$LOG_TEXT" | grep -i 'NDI library initialized' >/dev/null; then
    echo "  NDI runtime loaded"
else
    echo "  WARNING: no 'NDI library initialized' line yet"
fi

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}imag-nb base provisioning DONE (genlock build: $(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" 2>/dev/null || echo unknown))${NC}"
echo -e "${GREEN}NEXT (from dev1): scripts/imag_scenes.py --host ${STATIC_IP}   # profile+scenes${NC}"
echo -e "${GREEN}       then:      scripts/imag_scenes.py --host ${STATIC_IP} --projector   # once HDMI monitor connected${NC}"
echo -e "${GREEN}========================================${NC}"
