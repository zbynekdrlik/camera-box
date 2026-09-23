#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup script owns strict mode (set -euo pipefail) and fail()
# obs-box-baseline.sh (issue 1357) -- the ONE shared OBS-box appliance baseline; extended header below.
#
# WHY THIS EXISTS: the Linux OBS boxes were provisioned by two separately grown scripts. imag's
# (scripts/setup-imag.sh) carried the appliance baseline -- a bare openbox kiosk on plain Xorg, the
# low-latency kernel, the de-jitter masks, max-performance persistence, the power envelope -- while
# strih-lx's (scripts/setup-strih.sh) re-implemented a subset differently and ran a full GNOME Wayland
# desktop, so every fix on one box never reached the other (the owner's rulings on issue 1357:
# "imag is the reference starting position", "a difference between boxes is a defect").
#
# WHAT IT IS: the imag box-level steps, moved VERBATIM out of setup-imag.sh into one function per
# baseline item. setup-imag.sh AND setup-strih.sh call the same functions, so strih-lx gets the imag
# appliance by construction and the two can never diverge again. Box facts are ARGUMENTS derived per
# box by the caller, never hard-coded imag values:
#   BOX           the file/unit-name prefix (imag | strih): /etc/<BOX>-isolated-cpus.conf,
#                 <BOX>-maxperf.service, 50-<BOX>-autologin.conf, ... (imag's names are unchanged)
#   DESKTOP_USER  the kiosk autologin / OBS user
#   NIC           the ONE rig NDI interface (resolved by the caller)
#   SERIES        the Ubuntu release (obs_box_kernel_series: 24.04 imag, 26.04 strih-lx)
#   OBS_CFG       the OBS config dir (ProcessPriority=High)
#   PL1_W         the CPU's sustainable RAPL PL1 wattage (power envelope)
#   FETCH         the caller's `FETCH REPO_RELPATH DEST` installer (gh api on imag, the checkout on strih)
#
# The items, in the order both callers run them (imag's step numbers in brackets):
#   obs_box_network_tuning [2]   obs_box_max_performance [4]   obs_box_never_sleep [5]
#   obs_box_boot_safety_net [6]  obs_box_lowlatency_kernel [7] obs_box_cpu_affinity [8]
#   obs_box_nvidia_prime [9]     obs_box_dejitter [14] (+ obs_box_crash_popups_off)
#   obs_box_kiosk [15]           openbox autostart [16]: the caller writes its role autostart (which
#                                OBS unit, which projector layout) starting with the
#                                obs_box_openbox_autostart_preamble lines + the obs_box_openbox_menu_xml
#   obs_box_power_envelope [22]  obs_box_touchpad [25]         obs_box_maxperf_persistence [26]
# One grader for every item: scripts/lib/obs-box-baseline-verify.sh (verify-imag.sh + verify-strih.sh).
#
# NOT in the baseline (issue 1357 design, comment 5793075833): a realtime (rtprio) grant for the genlock
# render tick. Its SCHED_FIFO pin assumes a reserved core, and the FIFO policy + affinity leak to every
# NDI thread OBS spawns from it -- so the baseline keeps rtprio OFF until that vendored defect is fixed.
#
# Source-only: defines functions, runs nothing. The bodies keep setup-imag.sh's column-0 layout on
# purpose -- their heredocs must stay byte-identical to what imag has always written. The functions
# call the CALLER's fail() and colour vars (YELLOW/NC) and need root at run time.
#
# Split in two files only to keep each readable: THIS file is the system half (network, performance,
# boot safety, kernel, CPU affinity, GPU, power envelope) + the pure helpers; the kiosk-session half
# (never-sleep, de-jitter + crash popups, kiosk, touchpad, openbox autostart preamble + menu) is
# obs-box-kiosk.sh, sourced right here so callers source ONE entry point.
# shellcheck source=scripts/lib/obs-box-kiosk.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/obs-box-kiosk.sh"

# obs_box_cpu_isolation_plan  (stdin: one "CPU SIBLINGS_LIST" line per logical CPU, numerically
# ordered — i.e. cpuN + the contents of its topology/thread_siblings_list) -> THREE lines:
#   1. the CPUs to ISOLATE for the OBS thread pool   (isolcpus=)
#   2. the CPUs to run tickless                      (nohz_full=)
#   3. the CPUs left for housekeeping                (irqaffinity=)
#
# #483 tuned these by hand on the original 16-thread notebook (6 SMT P-cores + 4 E-cores) and
# BAKED THE RESULT IN AS LITERALS. #816 derives the same decision from the topology instead, so a
# replacement notebook with a different core count is provisioned correctly rather than being
# handed CPU numbers it does not have. The decision itself is UNCHANGED and reproduces the old
# box's values byte-for-byte:
#   - an SMT-PAIRED CPU is a P-core thread, an UNPAIRED one is an E-core (verified live on both
#     boxes via thread_siblings_list, never lscpu's flat count);
#   - P-core0 stays for openbox/Xorg + sshd/MCP, together with EVERY E-core -> housekeeping/IRQs;
#   - every other P-core thread is isolated for OBS (~106 threads, ~3 cores of real work);
#   - nohz_full covers ONLY the LAST isolated P-core pair — the one that hosts the SCHED_FIFO
#     genlock render tick (#484). Spreading it over the whole block would remove load-balancing
#     signal (#303).
obs_box_cpu_isolation_plan() {
    local cpu sibs i found
    local -a pair_key=() pair_cpus=() ecores=()
    while read -r cpu sibs; do
        [ -n "$cpu" ] || continue
        case "$sibs" in
            *,*|*-*)                      # SMT-paired -> a P-core thread
                found=-1
                for i in "${!pair_key[@]}"; do
                    if [ "${pair_key[$i]}" = "$sibs" ]; then found="$i"; break; fi
                done
                if [ "$found" -lt 0 ]; then
                    pair_key+=("$sibs"); pair_cpus+=("$cpu")
                else
                    pair_cpus[$found]="${pair_cpus[$found]},$cpu"
                fi
                ;;
            *) ecores+=("$cpu") ;;        # unpaired -> an E-core
        esac
    done
    local n="${#pair_key[@]}"
    [ "$n" -ge 3 ] || fail "obs_box_cpu_isolation_plan: found only $n SMT-paired P-core(s) — an OBS box needs one for housekeeping plus at least two to isolate for the OBS thread pool"
    local isolated=""
    for ((i = 1; i < n; i++)); do
        isolated="${isolated:+$isolated,}${pair_cpus[$i]}"
    done
    local house="${pair_cpus[0]}"
    for i in "${ecores[@]+"${ecores[@]}"}"; do house="${house},${i}"; done
    printf '%s\n%s\n%s\n' "$isolated" "${pair_cpus[$((n - 1))]}" "$house"
}

# obs_box_has_discrete_nvidia  (stdin: `lspci -nn` output) -> exit 0 when a DISCRETE NVIDIA display
# adapter is present, non-zero otherwise. #816: the NVIDIA driver step (#500) was mandatory and
# fail-hard, which aborts provisioning on a perfectly good box that simply has no dGPU (the
# replacement notebook is Intel-UHD-only). Match only real display-class devices so an NVIDIA
# audio/USB function on the same card can never masquerade as a GPU.
obs_box_has_discrete_nvidia() {
    grep -Eiq '(vga compatible controller|3d controller|display controller).*nvidia'
}

# obs_box_same_unit LINK UNIT -> exit 0 when LINK resolves to the SAME systemd unit file as UNIT.
# #823: the old check compared `readlink -f <link>` against the LITERAL "/lib/systemd/system/
# lightdm.service". On usrmerge Ubuntu /lib IS a symlink to /usr/lib, so readlink -f always answers
# /usr/lib/... and the compare could never pass — a perfectly correct kiosk DM aborted provisioning
# on its last assertion (.187, 2026-07-27). Canonicalise BOTH sides.
obs_box_same_unit() {
    local a b
    a="$(readlink -f "$1" 2>/dev/null)" || return 1
    b="$(readlink -f "$2" 2>/dev/null)" || return 1
    [ -n "$a" ] && [ "$a" = "$b" ]
}

# obs_box_kernel_series [OS_RELEASE_FILE] -> the box's Ubuntu release series (os-release VERSION_ID,
# e.g. 24.04 on imag, 26.04 on strih-lx) on stdout. The boot-safety kernel hold and the lowlatency
# meta package are both named by it (linux-*-hwe-<series>), so the ONE baseline serves both releases
# instead of hard-coding noble. Fails (rc 1, a message on stderr, nothing on stdout) when the file has
# no NN.NN VERSION_ID -- never a guessed series.
obs_box_kernel_series() {
    local f="${1:-/etc/os-release}" v
    # shellcheck source=/dev/null  # the box's os-release (or a test fixture), read in a subshell
    v="$( . "$f" 2>/dev/null; printf '%s' "${VERSION_ID:-}" )" || v=""
    if ! [[ "$v" =~ ^[0-9][0-9]\.[0-9][0-9]$ ]]; then
        echo "obs_box_kernel_series: no NN.NN VERSION_ID in ${f}" >&2
        return 1
    fi
    printf '%s\n' "$v"
}

# safe_grub_regen -- the #295 safe-grub mechanism (side-effecting: root + filesystem, so it is not
# one of the pure helpers above). GUARANTEES every
# installed kernel has an initrd BEFORE update-grub runs (a kernel without one bricked CAM3/CAM4,
# #295), then refuses to trust the regenerated grub.cfg if its default menu entry lacks a kernel
# image or an initrd -- never a raw ad-hoc grub edit. Reused by BOTH the #482 (lowlatency/
# preempt=full) and #483 (CPU isolation) grub.d drops below, called ONCE after both are written so
# update-grub only runs a single time for this pair of changes. Mirrors setup-device.sh STEP 10's
# initrd-guarantee + post-update-grub validation.
safe_grub_regen() {
    local vmlinuz kver
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

# obs_box_network_tuning NIC BOX -- the imag step 2 (#486) network tuning: sysctl buffers/BBR/nodelay/
# IPv6-off + EEE off / flow control advertised on the ONE rig NDI NIC (never every interface).
obs_box_network_tuning() {
    local NIC="${1:?obs_box_network_tuning: NIC required}" BOX="${2:?obs_box_network_tuning: BOX required}"
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

# EEE (Green Ethernet) off + flow-control ADVERTISED (issue 1234), scoped to $NIC only. Two
# mechanisms, belt-and-suspenders (some USB-ethernet chipsets don't implement these ioctls at
# all — `|| true` throughout): (1) a networkd-dispatcher hook for interface-routable/hotplug
# events, and (2) an immediate one-time apply now (re-applied at every boot via the governor
# step's rc.local).
mkdir -p /etc/networkd-dispatcher/routable.d
cat > /etc/networkd-dispatcher/routable.d/optimize-nic <<NICEOF
#!/bin/bash
# Disable EEE (Green Ethernet); advertise flow control for low latency (issue 1234) — scoped to ${BOX}'s NDI NIC only.
if [ "\$IFACE" = "${NIC}" ]; then
    ethtool --set-eee "${NIC}" eee off 2>/dev/null || true
    ethtool -A "${NIC}" rx on tx on 2>/dev/null || true
fi
NICEOF
chmod +x /etc/networkd-dispatcher/routable.d/optimize-nic
ethtool --set-eee "$NIC" eee off 2>/dev/null || true
ethtool -A "$NIC" rx on tx on 2>/dev/null || true
echo "  sysctl: buffers+BBR+nodelay+IPv6-off applied; EEE off, flow-control advertised on $NIC"
}

# obs_box_max_performance NIC BOX -- the imag step 4 governor (cpu-performance.service) + rc.local boot
# tuning: governor, USB autosuspend off (USB NIC), NIC powersave off, EEE/flow-control re-applied.
obs_box_max_performance() {
    local NIC="${1:?obs_box_max_performance: NIC required}" BOX="${2:?obs_box_max_performance: BOX required}"
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
# ${BOX} boot tuning (fleet parity): governor + USB autosuspend off (USB NIC!) + NIC powersave off
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "\$g"; done
for u in /sys/bus/usb/devices/*/power/control; do echo on > "\$u" 2>/dev/null; done
for n in /sys/class/net/*/device/power/control; do echo on > "\$n" 2>/dev/null; done
# #486/#1234: EEE off, flow-control advertised on the rig NDI NIC — reapplied every boot
# (belt-and-suspenders alongside step 2's networkd-dispatcher hook; some USB-ethernet
# chipsets reset EEE/pause state on power cycle).
ethtool --set-eee ${NIC} eee off 2>/dev/null || true
ethtool -A ${NIC} rx on tx on 2>/dev/null || true
exit 0
EOF
chmod +x /etc/rc.local
systemctl daemon-reload
systemctl enable --now cpu-performance.service >/dev/null 2>&1
bash /etc/rc.local
grep -q performance /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor || fail "governor not performance"
}

# obs_box_boot_safety_net SERIES BOX -- the imag step 6 (#487) boot safety net: hold the INSTALLED
# generic-HWE kernel packages of the box's release SERIES (24.04 / 26.04 -- obs_box_kernel_series), an
# unattended-upgrades kernel blacklist + Automatic-Reboot=false, and the initrd-guarantee hook.
obs_box_boot_safety_net() {
    local SERIES="${1:?obs_box_boot_safety_net: release series required}" BOX="${2:?obs_box_boot_safety_net: BOX required}"
# Ports setup-device.sh's #295 brick-prevention stack onto the OBS box (imag-nb first), run BEFORE the lowlatency
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
local KERNEL_HOLD_OK=1 p
# #820: hold ONLY packages this box actually has installed. Holding a NOT-installed name is not a
# no-op — apt then refuses any later install that would pull it in, and step 7's
# linux-lowlatency-hwe-<series> depends on exactly these HWE packages ("E: Held packages were changed
# and -y was used without --allow-change-held-packages"). Provisioning held itself out of its own
# next step on the replacement notebook (.187, 2026-07-27).
local KERNEL_HOLD_PKGS=()
for p in "linux-image-generic-hwe-${SERIES}" "linux-headers-generic-hwe-${SERIES}" "linux-generic-hwe-${SERIES}" \
    "linux-headers-$(uname -r)" "linux-image-$(uname -r)"; do
    dpkg -s "$p" >/dev/null 2>&1 && KERNEL_HOLD_PKGS+=("$p")
done
if [ "${#KERNEL_HOLD_PKGS[@]}" -eq 0 ]; then
    KERNEL_HOLD_OK=0
    echo "  WARNING: no installed generic-kernel package found to hold — kernel NOT pinned"
else
    apt-mark hold "${KERNEL_HOLD_PKGS[@]}" >/dev/null 2>&1 \
        || { KERNEL_HOLD_OK=0; echo "  WARNING: apt-mark hold of the generic kernel packages failed"; }
fi
cat > "/etc/apt/apt.conf.d/51${BOX}-kernel-lockdown" <<'EOF'
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
# CAM3/CAM4 on the fleet -- the same class of failure this hook prevents on every OBS box).
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
}

# obs_box_lowlatency_kernel SERIES -- the imag step 7 (#482) preempt=full via the lowlatency-kernel
# CONFIG package pulled by linux-lowlatency-hwe-<SERIES> (zero kernel downgrade); fails loud when the
# config drop-in is missing or lacks preempt=full. Takes effect on the next boot.
obs_box_lowlatency_kernel() {
    local SERIES="${1:?obs_box_lowlatency_kernel: release series required}"
# LIVE-VERIFIED FINDING (#482): there is NO lowlatency kernel IMAGE at the 6.17 line (the newest
# lowlatency images are 6.8/6.11 -- installing one would be a DOWNGRADE, losing 13th-gen
# CPU/iGPU/USB-NIC support). But the 6.17 generic kernel already IS PREEMPT_DYNAMIC, so
# linux-lowlatency-hwe-<series> on the release is a META package: it keeps the generic kernel image and
# only pulls in the `lowlatency-kernel` CONFIG package, which drops
# /etc/default/grub.d/99-lowlatency.cfg = GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full
# rcu_nocbs=all" -- full preemption on the NEWEST kernel, zero downgrade. This is a plain apt
# install (not a hand-authored grub.d file), so it needs no idempotent-append logic of its own --
# `apt-get install` on an already-installed package is already a no-op.
if ! dpkg -s lowlatency-kernel >/dev/null 2>&1; then
    apt-get update -qq
    # #820: --allow-change-held-packages so step 6's own kernel hold can never block this install
    # (the lowlatency meta depends on the very HWE packages step 6 pins). Step 6 re-holds nothing
    # here; the hold is restored on the next provisioning pass, and the lowlatency packages get
    # their own hold right below.
    DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-change-held-packages "linux-lowlatency-hwe-${SERIES}" >/dev/null \
        || fail "linux-lowlatency-hwe-${SERIES} install failed"
fi
[ -f /etc/default/grub.d/99-lowlatency.cfg ] \
    || fail "#482: lowlatency-kernel config package installed but /etc/default/grub.d/99-lowlatency.cfg is missing"
grep -q 'preempt=full' /etc/default/grub.d/99-lowlatency.cfg \
    || fail "#482: 99-lowlatency.cfg does not carry preempt=full — refuse to trust the config package"
# #487: never a raw ad-hoc grub edit -- hold the newly-installed kernel-config packages too, same
# as the generic kernel packages held in step 6, so an upgrade can't silently swap this config out.
apt-mark hold lowlatency-kernel "linux-lowlatency-hwe-${SERIES}" >/dev/null 2>&1 \
    || echo "  WARNING: apt-mark hold of the lowlatency-kernel config packages failed"
echo "  #482: lowlatency-kernel config installed (preempt=full on the 6.17 generic kernel, no downgrade)"
echo "  NOTE: preempt=full takes effect on the NEXT boot — this script does not reboot the box"
}

# obs_box_cpu_affinity BOX -- the imag step 8 (#483/#842) AFFINITY-ONLY OBS core reservation: derive the
# P-core block from THIS box's thread_siblings_list topology, persist it to /etc/<BOX>-isolated-cpus.conf
# (the OBS launcher's taskset fallback) and self-heal a leftover kernel-isolation grub.d drop-in.
# Exports OBS_BOX_ISOLATION_PLAN + OBS_BOX_ISOLATED_CPUS (global) for the caller's later steps.
# issue 1357: NO rtprio grant here -- the render-tick SCHED_FIFO pin assumes a reserved core and its
# FIFO+affinity leaks to every NDI thread OBS spawns from it (issue comment 5793075833), so the
# baseline keeps rtprio OFF.
obs_box_cpu_affinity() {
    local BOX="${1:?obs_box_cpu_affinity: BOX required}"
# #842 (recurrence of #784, live-diagnosed 2026-07-28): isolcpus= REMOVES the listed CPUs from the
# kernel scheduler's load-balancing DOMAINS -- it exists for explicit PER-THREAD pinning, never
# for handing a whole range mask to a many-threaded process. imag's OBS is a ~106-119-thread
# consumer (6x NDI decode + render + genlock + audio); under the OLD isolcpus=<block> cmdline the
# scheduler placed 114 of those 119 threads on ONE core while the other isolated cores sat at 0%
# busy -- NDI receive dropped from 60fps to ~53fps with 7-10 underruns/s (measured on 10.77.9.187;
# identical signature to #784's original 2026-07-15 finding on the incumbent .182 box, hand-fixed
# there by deleting this exact grub.d drop-in -- a fix that was never ported to THIS script, so
# #816's topology-derived rewrite reproduced the defect verbatim on the replacement notebook).
#
# FIX: stop writing isolcpus=/nohz_full=/irqaffinity= to the kernel cmdline AT ALL. The taskset
# AFFINITY pin below (the persisted-config file consumed by imag-obs-start.sh's
# `taskset -c "$IMAG_ISOLATED_CPUS"`) is UNCHANGED and stays -- a plain CPU affinity mask
# restricts WHICH cores a process may run on but does NOT remove those cores from the scheduler's
# load-balancing domain, so threads still migrate freely WITHIN the mask. Live-verified after a
# real reboot with a clean cmdline: threads spread 19/16/24/26/12/17 across cpu2-7, receive back
# to 60.15-60.20fps / 0-2 underruns -- identical to .182. Restricting OBS to 6 cores is harmless;
# *isolating* them is what broke it.
#
# nohz_full/irqaffinity are DROPPED TOO, not kept as a partial config -- deliberate decision (see
# the #842 design comment on the issue for the full reasoning): both existed ONLY in service of
# the isolation scheme. nohz_full was scoped to the one core pair meant to host a FUTURE SCHED_FIFO
# genlock render-tick thread (#483/#484); irqaffinity pushed default IRQ affinity off the isolated
# block. That render-tick thread does not exist today (its pin, when it ships, requests SCHED_FIFO
# via sched_setscheduler() + an rtprio ulimit grant below -- neither needs a kernel-cmdline flag).
# Keeping either as a stray, unpaired cmdline token once isolcpus is gone would be exactly the
# "half-finished polotovar" #784 already called out ("izolácia... LEN s explicitným per-thread
# pinningom") -- if/when the SCHED_FIFO pin needs kernel-level tick support, that is its OWN new,
# explicit, tested design, not a leftover flag surviving this fix.
#
# `obs_box_cpu_isolation_plan` (was imag_cpu_isolation_plan) is UNCHANGED -- its ISOLATED output is still the affinity mask; its
# nohz_full/housekeeping outputs go unused now (no cmdline write consumes them). HT pairs verified
# LIVE via thread_siblings_list (not lscpu's flat count): cpu0=0-1, cpu2=2-3, cpu4=4-5, cpu6=6-7,
# cpu8=8-9, cpu10=10-11 (all P-core HT pairs), cpu12-15 = E-cores (no HT pairing).
local f c
OBS_BOX_ISOLATION_PLAN="$(
    for f in /sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list; do
        [ -r "$f" ] || continue
        c="${f#/sys/devices/system/cpu/cpu}"; c="${c%%/*}"
        printf '%s %s\n' "$c" "$(cat "$f")"
    done | sort -n -k1,1 | obs_box_cpu_isolation_plan
)" || exit 1
OBS_BOX_ISOLATED_CPUS="$(printf '%s\n' "$OBS_BOX_ISOLATION_PLAN" | sed -n 1p)"
[ -n "$OBS_BOX_ISOLATED_CPUS" ] \
    || fail "#816: could not derive the CPU affinity plan from this box's topology"
# #841: persist the SAME derived value imag-obs-start.sh falls back to for a manual "Spustit OBS"
# invocation (no IMAG_ISOLATED_CPUS env set) -- ONE source of truth for the taskset affinity pin,
# the boot autostart's env export (step 16), and the wrapper's own fallback. Never a second
# hardcoded literal in the wrapper.
printf '%s\n' "$OBS_BOX_ISOLATED_CPUS" > "/etc/${BOX}-isolated-cpus.conf"
# #842 self-heal: a leftover kernel-isolation grub.d drop-in from a previous provisioning run (or
# a hand-applied #483/#816-era config) must be removed and grub regenerated -- the same self-heal
# discipline every other drift-prone config in this script already applies. This also covers the
# case where a box is being RE-provisioned after previously carrying the #842 defect.
if [ -f "/etc/default/grub.d/98-${BOX}-isolation.cfg" ]; then
    echo -e "  ${YELLOW}#842: removing leftover /etc/default/grub.d/98-${BOX}-isolation.cfg -- kernel isolcpus/nohz_full is the #784/#842 regression, affinity-only pin stays${NC}"
    rm -f "/etc/default/grub.d/98-${BOX}-isolation.cfg"
    # #295/#487: never a raw ad-hoc grub edit -- guarantee every kernel has an initrd, regenerate
    # grub.cfg, then refuse to trust it if the default entry lacks a kernel image or an initrd.
    safe_grub_regen
    echo "  #842: leftover kernel-isolation drop-in removed + grub regenerated"
fi
echo "  #483/#842: OBS core reservation is AFFINITY-ONLY (taskset ${OBS_BOX_ISOLATED_CPUS} via /etc/${BOX}-isolated-cpus.conf) -- no kernel isolcpus/nohz_full/irqaffinity written"
}

# obs_box_nvidia_prime BOX -- the imag step 9 (#500/#816/#841) GPU step: on a box with a DISCRETE NVIDIA
# GPU install nvidia-driver-595-open (the Blackwell-capable flavour on 24.04 AND 26.04) + PRIME
# nvidia-primary so Xorg renders on the dGPU; on an iGPU-only box the <BOX>-igpu-maxperf frequency pin.
obs_box_nvidia_prime() {
    local BOX="${1:?obs_box_nvidia_prime: BOX required}"
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
# #816: the whole step is GATED on a discrete NVIDIA GPU actually being present. It was
# mandatory + fail-hard, which aborts provisioning on a replacement notebook that simply has no
# dGPU (live: the i5-13420H box is Intel-UHD-only). On such a box the HDMI program output is
# driven by the iGPU directly — there is no PRIME to select and no driver to install.
if ! lspci -nn | obs_box_has_discrete_nvidia; then
    echo "  #816: no discrete NVIDIA GPU on this box — skipping the driver + PRIME step (iGPU drives HDMI directly)"
    # #841: the incumbent box's anti-stutter display tuning (nvidia-settings
    # ForceFullCompositionPipeline=On + GPUPowerMizerMode=1) is NVIDIA-only and has no direct
    # counterpart here -- but "TearFree" (the naive intel-DDX-style analog) does NOT apply on
    # THIS driver stack, confirmed LIVE on 10.77.9.187 rather than assumed: `Option "TearFree"
    # "true"` under `Driver "modesetting"` produced the Xorg.0.log line
    # `(WW) modeset(0): Option "TearFree" is not used`, and `strings modesetting_drv.so` contains
    # no "TearFree"/"Tear" text at all -- TearFree is a feature of the LEGACY xf86-video-intel DDX
    # (installed here but never
    # matched -- Xorg autoconfigures the built-in `modesetting` driver for this PCI id, confirmed
    # `(==) Matched modesetting as autoconfigured driver 0`), not of `modesetting`+glamor. Shipping
    # a dead option would be exactly the cargo-culted-NVIDIA-semantics-onto-Intel mistake this
    # ticket warns against, so it is NOT written. What this stack actually already provides
    # tear-free, verified live in the SAME log: `Present`+`DRI3` init cleanly and
    # `modeset(0): glamor X acceleration enabled`, with `PageFlip`/`Atomic` compiled into the
    # driver (`strings` confirms) -- a full-screen client (the OBS Program projector, no
    # compositor running) gets direct page-flipped scanout via Present by default, which is the
    # real tear-free mechanism on this stack, not an xorg.conf.d option. VRR (`Option
    # "VariableRefresh"`, also `strings`-confirmed real and X-property-visible as `VariableRefresh:
    # disabled` in the log) was considered too, but the HDMI-1 projector output itself reports
    # `vrr_capable: 0` (only the eDP-1 laptop panel does) -- not applicable to the affected output.
    #
    # The genuinely-applicable Intel/i915 equivalent to GPUPowerMizerMode=1 IS real: the iGPU
    # actively DVFS-scales (gt_cur_freq_mhz observed cycling well below its own gt_RP0_freq_mhz
    # ceiling under live 6-camera render load) -- the same ramp-hitch class of stutter
    # GPUPowerMizerMode=1 avoids on NVIDIA. i915 has no PowerMizer; pin the frequency FLOOR to the
    # hardware's own reported ceiling (gt_RP0_freq_mhz, never a hardcoded MHz literal -- a future
    # Intel notebook's ceiling will differ) instead, so it stops idling down and ramping back up
    # under load. Sysfs values reset on reboot, so this is reapplied every boot via a dedicated
    # systemd oneshot unit, mirroring the existing cpu-performance.service convention (step 4)
    # rather than a provisioning-time-only write.
    sed "s/@BOX@/${BOX}/g" > "/usr/local/bin/${BOX}-igpu-maxperf.sh" <<'IGPU_EOF'
#!/usr/bin/env bash
# camera-box #841: pin the Intel iGPU's frequency floor to its own reported max (gt_RP0_freq_mhz)
# so it never idles down and ramps back up under load -- the DVFS ramp-up is what caused the
# intermittent stutter on fast motion in the fullscreen OBS Program projector (the same problem
# GPUPowerMizerMode=1 solves on the NVIDIA box; i915 has no PowerMizer, but raising gt_min_freq to
# the hardware's own real max gets the same "always at max clock" outcome). Runs at every boot
# (systemd, root) because sysfs values reset on reboot -- never a hardcoded MHz literal, a future
# Intel notebook's ceiling will differ.
set -euo pipefail
for card in /sys/class/drm/card[0-9]; do
    [ -w "$card/gt_min_freq_mhz" ] || continue
    max="$(cat "$card/gt_RP0_freq_mhz" 2>/dev/null)"
    [ -n "$max" ] || continue
    echo "$max" > "$card/gt_min_freq_mhz"
    echo "$max" > "$card/gt_boost_freq_mhz" 2>/dev/null || true
    echo "@BOX@-igpu-maxperf: pinned $card gt_min_freq_mhz -> ${max}MHz (was DVFS-scaled down at idle)"
    exit 0
done
echo "@BOX@-igpu-maxperf: no writable i915 gt_min_freq_mhz sysfs node found -- nothing to pin" >&2
exit 0
IGPU_EOF
    chmod 755 "/usr/local/bin/${BOX}-igpu-maxperf.sh"
    sed "s/@BOX@/${BOX}/g" > "/etc/systemd/system/${BOX}-igpu-maxperf.service" <<'SVC_EOF'
[Unit]
Description=camera-box #841: pin Intel iGPU to max frequency (avoid DVFS ramp stutter, @BOX@ HDMI program projector)
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/@BOX@-igpu-maxperf.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
SVC_EOF
    systemctl daemon-reload
    systemctl enable --now "${BOX}-igpu-maxperf.service" >/dev/null 2>&1 \
        || echo "  WARNING: could not enable ${BOX}-igpu-maxperf.service"
    echo "  #841: iGPU max-frequency-pin service provisioned (no xorg.conf.d change -- TearFree does not exist on this driver, live-verified; Present+PageFlip already gives tear-free full-screen scanout without a compositor)"
elif ! dpkg -s nvidia-driver-595-open 2>/dev/null | grep '^Status: install ok installed' >/dev/null; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y nvidia-driver-595-open >/dev/null \
        || fail "nvidia-driver-595-open install failed"
fi
# PRIME nvidia-primary: on-demand PRIME mode left the HDMI dGPU output dead (live-verified) --
# nvidia must be the PRIMARY renderer so BOTH the HDMI output and the laptop's own eDP panel run
# on the RTX 5050.
if lspci -nn | obs_box_has_discrete_nvidia; then
    command -v prime-select >/dev/null 2>&1 || fail "prime-select missing after nvidia-driver-595-open install"
    prime-select nvidia || fail "prime-select nvidia failed"
fi
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
if lspci -nn | obs_box_has_discrete_nvidia; then
    echo "  #500: nvidia-driver-595-open installed, prime-select nvidia set, grub/initrd re-verified"
    echo "  NOTE: the PRIME GPU mode + the new DKMS module take full effect on the NEXT boot — this script does not reboot the box"
fi
}

# obs_box_power_envelope PL1_W FETCH -- the imag step 22 (#1040) power/thermal envelope: purge thermald,
# pin the MMIO RAPL PL1 long-term constraint to PL1_W watts at boot (imag-power-envelope.service) and
# supervise it with the ~45 s guard timer. PL1_W is a per-box fact (the CPU's sustainable wattage);
# FETCH is the caller's `FETCH REPO_RELPATH DEST` installer function. The on-box tool + unit names stay
# imag-power-envelope* on every box: the shared gather/verdict lib (scripts/lib/imag-power-envelope.sh)
# grades exactly those names.
obs_box_power_envelope() {
    local IMAG_PL1_W="${1:?obs_box_power_envelope: PL1 watts required}" FETCH="${2:?obs_box_power_envelope: fetch function required}"
# The imag render regression (issues 799/880/1029/1030) was a HARDWARE power clamp: thermald's
# DPTF policy programmed the MMIO RAPL PL1 long-term constraint to 25 W, starving the iGPU to
# gt_act_freq 600-850 MHz while every software freq knob sat at 1400. The durable fix pins PL1 to a
# sustainable 45 W (#1162 re-baseline for the replacement i7-13620H — 29 W starved it; 29 W was the
# original i5 unit's value) + slpc_ignore_eff_freq=1 at boot, PURGES thermald (the actor that programmed
# 25 W -- a minimalist appliance purges a competing policy engine, same discipline the sole-
# timesync-authority gate enforces; PROCHOT stays as the hardware backstop), and supervises the
# envelope with a LOUD root guard that alerts dev1-side instead of silently degrading. Env knobs
# below are baked into the units so a re-provision keeps the same envelope.

# thermald PURGED (not masked) -- its adaptive DPTF surface is opaque and moves across upgrades.
DEBIAN_FRONTEND=noninteractive apt-get purge -y thermald >/dev/null 2>&1 || true
# Self-heal any leftover HAND-PLACED temporary guard from a prior live hotfix -- the source-script
# fix here supersedes it (a hand-fix must never linger past its source-script fix). Best-effort by
# the conventional temp names; the live removal on the incumbent box is done at integration.
systemctl disable --now imag-power-envelope-temp-guard.timer imag-power-envelope-temp-guard.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/imag-power-envelope-temp-guard.* /usr/local/bin/imag-power-envelope-temp-guard.sh 2>/dev/null || true

# The shared verdict/decision lib (source-only) -- installed so the on-box scripts source it, via the
# caller's FETCH function (setup-imag.sh: gh api; setup-strih.sh: the repo checkout -- issue 1357).
mkdir -p /usr/local/lib
"$FETCH" scripts/lib/imag-power-envelope.sh /usr/local/lib/imag-power-envelope.sh \
    || fail "could not fetch scripts/lib/imag-power-envelope.sh via ${FETCH}"
chmod 644 /usr/local/lib/imag-power-envelope.sh

"$FETCH" scripts/imag-power-envelope.sh /usr/local/bin/imag-power-envelope.sh \
    || fail "could not fetch scripts/imag-power-envelope.sh via ${FETCH}"
chmod 755 /usr/local/bin/imag-power-envelope.sh

"$FETCH" scripts/imag-power-envelope-guard.sh /usr/local/bin/imag-power-envelope-guard.sh \
    || fail "could not fetch scripts/imag-power-envelope-guard.sh via ${FETCH}"
chmod 755 /usr/local/bin/imag-power-envelope-guard.sh

# ROOT system units (sysfs writes need root, unlike the user-level imag-obs.service). Env knobs
# baked in at provisioning time (the caller's PL1 argument; setup-imag.sh passes ${IMAG_PL1_W:-45}).
cat > /etc/systemd/system/imag-power-envelope.service <<PE_SVC_EOF
[Unit]
Description=camera-box #1040: pin imag-nb MMIO RAPL PL1 + slpc power envelope (sustainable 60fps render)
After=multi-user.target

[Service]
Type=oneshot
Environment=IMAG_PL1_W=${IMAG_PL1_W:-45}
ExecStart=/usr/local/bin/imag-power-envelope.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
PE_SVC_EOF

cat > /etc/systemd/system/imag-power-envelope-guard.service <<PE_GUARD_EOF
[Unit]
Description=camera-box #1040: imag-nb power-envelope runtime guard (thermal step-down + foreign re-assert)
After=imag-power-envelope.service

[Service]
Type=oneshot
Environment=IMAG_PL1_W=${IMAG_PL1_W:-45}
Environment=IMAG_PL1_STEPDOWN_W=${IMAG_PL1_STEPDOWN_W:-25}
Environment=IMAG_TCPU_STEPDOWN_C=${IMAG_TCPU_STEPDOWN_C:-93}
Environment=IMAG_TCPU_RESTORE_C=${IMAG_TCPU_RESTORE_C:-85}
ExecStart=/usr/local/bin/imag-power-envelope-guard.sh
PE_GUARD_EOF

cat > /etc/systemd/system/imag-power-envelope-guard.timer <<'PE_TMR_EOF'
[Unit]
Description=camera-box #1040: run the imag-nb power-envelope guard every ~45s

[Timer]
OnBootSec=60
OnUnitActiveSec=45
AccuracySec=5s

[Install]
WantedBy=timers.target
PE_TMR_EOF

# #1162/#784 self-heal: remove any leftover hand-applied PL1 override drop-in from the live
# re-baseline. The sustainable wattage is now source-controlled (each unit's Environment= above +
# the shared lib default), so a lingering .service.d/override.conf hand-fix must NOT persist to MASK
# a future source re-pin (the #784 lesson, mirroring the #842 grub.d self-heal). Idempotent: absent
# -> no-op. Runs BEFORE daemon-reload so the base unit's Environment wins on reload.
local _pe_dropin
for _pe_dropin in \
    /etc/systemd/system/imag-power-envelope.service.d/override.conf \
    /etc/systemd/system/imag-power-envelope-guard.service.d/override.conf; do
    if [ -f "$_pe_dropin" ]; then
        echo -e "  ${YELLOW}#1162: removing leftover hand-applied PL1 drop-in ${_pe_dropin} — PL1 wattage is source-controlled now (unit Environment= + shared lib default)${NC}"
        rm -f "$_pe_dropin"
        rmdir "$(dirname "$_pe_dropin")" 2>/dev/null || true
    fi
done

systemctl daemon-reload
systemctl enable --now imag-power-envelope.service >/dev/null 2>&1 \
    || fail "could not enable imag-power-envelope.service -- the boot power envelope would not be pinned"
systemctl enable --now imag-power-envelope-guard.timer >/dev/null 2>&1 \
    || fail "could not enable imag-power-envelope-guard.timer -- the envelope would be unsupervised"
echo "  #1040: thermald purged, PL1=${IMAG_PL1_W:-45}W envelope pinned at boot + supervised by the ~45s guard timer"
}

# obs_box_maxperf_persistence BOX -- the imag step 26 (issue 756/#791) full max-performance persistence:
# EPP/turbo/platform-profile/usbcore/PCI runtime-PM enforced at every boot by <BOX>-maxperf.service and on
# every hotplug by 99-<BOX>-maxperf-pm.rules. The generated files carry @BOX@ placeholders that the
# `sed` expands (their quoted heredocs keep every `$` literal for the boot-time script).
obs_box_maxperf_persistence() {
    local BOX="${1:?obs_box_maxperf_persistence: BOX required}"
# The incumbent's full performance persistence lived in imag-maxperf.service (issue 756) ->
# /usr/local/sbin/imag-maxperf.sh, plus a hotplug-persistent udev rule -- NEVER tracked in the repo
# (a live audit's `grep -rn imag-maxperf scripts/ tests/` returned nothing), hand-placed and never
# ported to this generator (the same "provisioning gap hidden by a hand patch" class issue 840
# documented for imag-obs-start.sh, issue 841 for the NVIDIA tuning, issue 858 for remoteos-mcp).
# Step 4 (cpu-performance.service + rc.local) persists ONLY the governor + per-device USB/NET
# power/control; EPP / intel_pstate no_turbo=0 / platform_profile / usbcore autosuspend / all-PCI
# runtime-PM off / the hotplug udev rule were absent entirely -- exactly the EPP-persistence gap the
# 2026-07-18 audit on this ticket demanded be folded in. Reproduce the live trio so a fresh box is
# IDENTICAL to today's imag (the ticket mandate). The governor is set redundantly with
# cpu-performance.service; that redundancy exists on the live box today and reproducing it is the
# correct parity choice -- NOT a defect and NOT deferred work: consolidating the two units was the
# explicitly REJECTED alternative (it would change the live box's own unit topology, so it is out of
# scope for a parity fix). Every knob is [ -f ]/command -v guarded so it stays hardware-agnostic
# (#816): a box lacking intel_pstate/
# platform_profile simply skips those writes. verify-imag.sh check (y) reads the service/script/udev
# presence AND the runtime STATE back and fails loud on any drift.
mkdir -p /usr/local/sbin
sed "s/@BOX@/${BOX}/g" > "/usr/local/sbin/${BOX}-maxperf.sh" <<'MAXPERF_EOF'
#!/usr/bin/env bash
# airuleset:script-ok boot enforcement must continue past missing knobs; every failure is logged loudly
# @BOX@ max-perf boot enforcement (idempotent) -- issue 756 / #791 reprovision parity.
set -u
log(){ echo "@BOX@-maxperf: $*"; }
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g" 2>/dev/null || log "governor write FAILED: $g"; done
for e in /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference; do [ -f "$e" ] && { echo performance > "$e" 2>/dev/null || log "EPP write FAILED: $e"; }; done
[ -f /sys/devices/system/cpu/intel_pstate/no_turbo ] && { echo 0 > /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || log "no_turbo write FAILED"; }
[ -f /sys/firmware/acpi/platform_profile ] && { echo performance > /sys/firmware/acpi/platform_profile 2>/dev/null || log "platform_profile write FAILED"; }
command -v powerprofilesctl >/dev/null && { powerprofilesctl set performance 2>/dev/null || log "powerprofilesctl FAILED (daemon not up yet?)"; }
[ -f /sys/module/usbcore/parameters/autosuspend ] && { echo -1 > /sys/module/usbcore/parameters/autosuspend 2>/dev/null || log "usb autosuspend write FAILED"; }
for p in /sys/bus/pci/devices/*/power/control; do echo on > "$p" 2>/dev/null || log "pci runtime-pm write FAILED: $p"; done
log "applied: governor=$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | tr '\n' ' ') profile=$(cat /sys/firmware/acpi/platform_profile 2>/dev/null)"
MAXPERF_EOF
chmod 755 "/usr/local/sbin/${BOX}-maxperf.sh"
sed "s/@BOX@/${BOX}/g" > "/etc/systemd/system/${BOX}-maxperf.service" <<'MAXPERF_SVC_EOF'
[Unit]
Description=Force full max-performance (CPU/platform/USB/PCI) -- @BOX@ issue 756
After=multi-user.target power-profiles-daemon.service
[Service]
Type=oneshot
ExecStart=/usr/local/sbin/@BOX@-maxperf.sh
RemainAfterExit=yes
[Install]
WantedBy=multi-user.target
MAXPERF_SVC_EOF
sed "s/@BOX@/${BOX}/g" > "/etc/udev/rules.d/99-${BOX}-maxperf-pm.rules" <<'MAXPERF_UDEV_EOF'
# @BOX@ max-perf (issue 756 / #791): force runtime PM OFF (power/control=on) on device add -- NDI
# NICs/peripherals must never power-dip; makes the boot-time write survive USB/PCI hotplug.
ACTION=="add", SUBSYSTEM=="pci", ATTR{power/control}="on"
ACTION=="add", SUBSYSTEM=="usb", TEST=="power/control", ATTR{power/control}="on"
MAXPERF_UDEV_EOF
udevadm control --reload-rules 2>/dev/null || true
systemctl daemon-reload
systemctl enable --now "${BOX}-maxperf.service" \
    || fail "issue 756/#791: could not enable+start ${BOX}-maxperf.service — the full max-performance persistence (EPP/turbo/PCI-PM) would not survive a reboot"
# Type=oneshot + RemainAfterExit=yes: an ACTIVE unit proves ExecStart (the enforcement script) ran
# to completion -- a stronger proof than re-checking the governor, which step 4's own
# cpu-performance.service already set (so a governor grep would pass even if <BOX>-maxperf never ran).
systemctl is-active --quiet "${BOX}-maxperf.service" \
    || fail "issue 756/#791: ${BOX}-maxperf.service is not active after enable --now — the boot-enforcement script did not run"
echo "  issue 756/#791: full max-performance persistence provisioned (${BOX}-maxperf.service active + udev rule)"
}
