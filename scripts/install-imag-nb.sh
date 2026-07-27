#!/bin/bash
set -euo pipefail

# One-shot OS installer for an imag notebook (#815, part of #791).
#
# Run this ON the new notebook, from its own Ubuntu 24.04 live-USB session. It installs a clean
# desktop Ubuntu onto the notebook's internal disk so that `scripts/setup-imag.sh` can take over
# right after the first boot — the imag equivalent of `scripts/create-usb-linux.sh` for the cam
# fleet. Swapping the imag NB must never again be manual work (user directive, 2026-07-27).
#
# WHY squashfs LAYERS and not a copy of the running live session: an Ubuntu 24.04 live ISO stacks
#   minimal.squashfs  ->  minimal.standard.squashfs  ->  minimal.standard.live.squashfs
# and the TOP layer is the live session itself (casper, the `ubuntu` live user, the installer
# snap). Copying `/` would install all of that. Copying only the lower layers — exactly what the
# real installer does — yields a genuinely installed system.
#
# Usage (on the box, as root):
#   sudo ./install-imag-nb.sh --target-disk /dev/nvme0n1 --ip 10.77.9.187 --hostname imag-nb --yes
#
# After it finishes: remove the USB stick (or let the NVRAM entry take over) and reboot. The box
# comes up on the static IP with SSH open, ready for `setup-imag.sh`.

DEVICE=""
FORCE_TARGET=0
ASSUME_YES=0
CASPER_DIR="/cdrom/casper"
IMAG_IP=""
IMAG_PREFIX="23"
IMAG_GW=""
IMAG_DNS=""
IMAG_HOSTNAME="imag-nb"
DESKTOP_USER="newlevel"
DESKTOP_PW="newlevel"
NM_CON_ID="imag-lan"
MOUNT_ROOT="/mnt/imag-root"
MOUNT_SRC="/mnt/imag-src"
LAYER_MNT_BASE="/mnt/imag-layer"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target-disk) DEVICE="${2:-}"; FORCE_TARGET=1; shift 2 ;;
        --yes|-y)      ASSUME_YES=1; shift ;;
        --casper-dir)  CASPER_DIR="${2:-}"; shift 2 ;;
        --ip)          IMAG_IP="${2:-}"; shift 2 ;;
        --prefix)      IMAG_PREFIX="${2:-}"; shift 2 ;;
        --gateway)     IMAG_GW="${2:-}"; shift 2 ;;
        --dns)         IMAG_DNS="${2:-}"; shift 2 ;;
        --hostname)    IMAG_HOSTNAME="${2:-}"; shift 2 ;;
        --user)        DESKTOP_USER="${2:-}"; shift 2 ;;
        --password)    DESKTOP_PW="${2:-}"; shift 2 ;;
        -*)            echo "Unknown option: $1" >&2; exit 1 ;;
        *)             DEVICE="$1"; shift ;;
    esac
done

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
log()   { echo -e "${GREEN}[+]${NC} $1"; }
warn()  { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1" >&2; exit 1; }

# --- PURE functions (no root, no disk writes, no network — sourced + unit-tested from
# tests/install_imag_nb_pure_functions.rs; the BASH_SOURCE guard at the bottom skips the
# destructive install flow when this file is sourced) ----------------------------------------

# imag_layer_chain CASPER_DIR SECUREBOOT_STATE -> ordered squashfs layer paths, LOWEST FIRST.
#
# The install chain is minimal -> minimal.standard (+ the enhanced-secureboot delta on top when
# Secure Boot is enabled). DELIBERATELY excluded: `*.live.squashfs` (the live session itself —
# casper + the `ubuntu` user; installing it is the classic "my install is just a live clone" bug),
# every per-language delta (`minimal.de.squashfs`, `minimal.standard.fr.squashfs`, …) and the
# `minimal.enhanced-secureboot.squashfs` branch (a delta on `minimal`, not on `minimal.standard`
# — the wrong branch of the chain).
imag_layer_chain() {
    local casper="${1:-}" sb="${2:-disabled}" f
    [ -n "$casper" ] || { echo "imag_layer_chain: casper dir required" >&2; return 2; }

    local base="${casper}/minimal.squashfs"
    local std="${casper}/minimal.standard.squashfs"
    local sbl="${casper}/minimal.standard.enhanced-secureboot.squashfs"

    for f in "$base" "$std"; do
        [ -f "$f" ] || { echo "imag_layer_chain: required layer missing: $f" >&2; return 3; }
    done
    printf '%s\n%s\n' "$base" "$std"

    if [ "$sb" = "enabled" ]; then
        [ -f "$sbl" ] || { echo "imag_layer_chain: Secure Boot is on but layer missing: $sbl" >&2; return 3; }
        printf '%s\n' "$sbl"
    fi
}

# imag_part_name DISK INDEX -> the partition device node. NVMe/mmc need a `p` infix
# (/dev/nvme0n1 -> /dev/nvme0n1p1), SATA/USB do not (/dev/sda -> /dev/sda1). Getting this wrong
# formats the wrong node.
imag_part_name() {
    local disk="${1:-}" idx="${2:-}"
    [ -n "$disk" ] && [ -n "$idx" ] || { echo "imag_part_name: disk and index required" >&2; return 2; }
    case "$disk" in
        *[0-9]) printf '%sp%s\n' "$disk" "$idx" ;;
        *)      printf '%s%s\n'  "$disk" "$idx" ;;
    esac
}

# imag_fstab ROOT_UUID ESP_UUID -> the target's /etc/fstab. UUID-based on purpose: a device-path
# fstab breaks the moment a USB stick shifts the kernel's disk naming.
imag_fstab() {
    local root_uuid="${1:-}" esp_uuid="${2:-}"
    [ -n "$root_uuid" ] && [ -n "$esp_uuid" ] || { echo "imag_fstab: both UUIDs required" >&2; return 2; }
    cat <<EOF
# imag notebook — written by scripts/install-imag-nb.sh (#815)
UUID=${root_uuid} / ext4 defaults,relatime,errors=remount-ro 0 1
UUID=${esp_uuid} /boot/efi vfat umask=0077 0 1
EOF
}

# imag_nm_keyfile ID IP PREFIX GATEWAY DNS -> a NetworkManager keyfile pinning the static address.
# setup-imag.sh REQUIRES NetworkManager (it fails loud without `nmcli`), and the box must come up
# on its intended address by itself — never via a manual nmcli session after first boot.
imag_nm_keyfile() {
    local id="${1:-}" ip="${2:-}" prefix="${3:-}" gw="${4:-}" dns="${5:-}"
    [ -n "$id" ] && [ -n "$ip" ] && [ -n "$prefix" ] || { echo "imag_nm_keyfile: id/ip/prefix required" >&2; return 2; }
    cat <<EOF
[connection]
id=${id}
type=ethernet
autoconnect=true
autoconnect-priority=100

[ipv4]
method=manual
address1=${ip}/${prefix},${gw}
dns=${dns};
may-fail=false

[ipv6]
method=ignore
EOF
}

# --- destructive install flow (skipped when this file is SOURCED) ----------------------------

check_requirements() {
    log "Checking requirements..."
    [[ $EUID -eq 0 ]] || error "Must run as root"
    [[ -n "$DEVICE" ]] || error "Usage: $0 --target-disk /dev/nvme0n1 --ip <addr> [--hostname imag-nb] [--yes]"
    [[ -b "$DEVICE" ]] || error "$DEVICE is not a block device"
    [[ -n "$IMAG_IP" ]] || error "--ip is required (the box must come up on a known address)"

    # Same safety guard as create-usb-linux.sh: a bare positional /dev/sda is refused; targeting a
    # system disk needs the explicit --target-disk form.
    if [[ "$DEVICE" == "/dev/sda" && "$FORCE_TARGET" -ne 1 ]]; then
        error "Refusing a bare /dev/sda. Use --target-disk /dev/sda if that IS the internal disk."
    fi

    # Never install onto the disk the live session itself is running from.
    local live_src
    live_src=$(findmnt -no SOURCE /cdrom 2>/dev/null || true)
    if [ -n "$live_src" ]; then
        local live_disk
        live_disk="/dev/$(lsblk -no PKNAME "$live_src" 2>/dev/null || true)"
        [ "$live_disk" != "$DEVICE" ] || error "$DEVICE is the live-USB itself — refusing to install onto it"
    fi

    local t
    for t in rsync sfdisk mkfs.ext4 mkfs.vfat blkid chroot efibootmgr lsblk findmnt; do
        command -v "$t" >/dev/null 2>&1 || error "missing required tool: $t"
    done
    [ -d "$CASPER_DIR" ] || error "casper dir not found: $CASPER_DIR (run this from the live-USB session)"
    [ -d /sys/firmware/efi ] || error "not booted in UEFI mode — this installer only does UEFI/GPT"
}

secureboot_state() {
    if command -v mokutil >/dev/null 2>&1 && mokutil --sb-state 2>/dev/null | grep -qi 'SecureBoot enabled'; then
        echo enabled
    else
        echo disabled
    fi
}

confirm_device() {
    echo
    lsblk -o NAME,SIZE,TYPE,MODEL "$DEVICE" || true
    warn "ALL DATA on $DEVICE will be destroyed."
    if [[ "$ASSUME_YES" -eq 1 ]]; then
        log "Non-interactive (--yes): proceeding."
        return
    fi
    read -r -p "Type 'yes' to continue: " a
    [[ "$a" == "yes" ]] || error "aborted by user"
}

cleanup_mounts() {
    local m
    for m in "$MOUNT_ROOT/dev/pts" "$MOUNT_ROOT/dev" "$MOUNT_ROOT/proc" "$MOUNT_ROOT/sys" \
             "$MOUNT_ROOT/run" "$MOUNT_ROOT/boot/efi" "$MOUNT_ROOT" "$MOUNT_SRC"; do
        mountpoint -q "$m" 2>/dev/null && umount -l "$m" || true
    done
    for m in "${LAYER_MNT_BASE}"*; do
        [ -d "$m" ] || continue
        mountpoint -q "$m" 2>/dev/null && umount -l "$m" || true
    done
}

partition_disk() {
    log "Partitioning $DEVICE (GPT: 1G ESP + ext4 root)"
    swapoff -a || true
    wipefs -a "$DEVICE" >/dev/null
    sfdisk "$DEVICE" <<'EOF'
label: gpt
,1GiB,U
,,L
EOF
    partprobe "$DEVICE" || true
    udevadm settle || true
    ESP_PART=$(imag_part_name "$DEVICE" 1)
    ROOT_PART=$(imag_part_name "$DEVICE" 2)
    [ -b "$ESP_PART" ] && [ -b "$ROOT_PART" ] || error "partitions did not appear ($ESP_PART / $ROOT_PART)"
    mkfs.vfat -F32 -n IMAG-ESP "$ESP_PART" >/dev/null
    mkfs.ext4 -F -L imag-root "$ROOT_PART" >/dev/null
    log "ESP=$ESP_PART root=$ROOT_PART"
}

mount_target() {
    mkdir -p "$MOUNT_ROOT"
    mount "$ROOT_PART" "$MOUNT_ROOT"
    mkdir -p "$MOUNT_ROOT/boot/efi"
    mount "$ESP_PART" "$MOUNT_ROOT/boot/efi"
}

copy_rootfs() {
    local sb chain i n=0 lower=""
    sb=$(secureboot_state)
    log "Secure Boot: $sb — resolving install layers"
    chain=$(imag_layer_chain "$CASPER_DIR" "$sb") || error "cannot resolve the ISO's squashfs layer chain"

    # Mount each layer read-only, then stack them as ONE read-only overlay. lowerdir is TOP-FIRST,
    # so the chain (lowest first) is reversed here. The overlay resolves whiteouts natively —
    # sequential unsquashfs would leave whiteout char devices behind in the installed system.
    while IFS= read -r i; do
        [ -n "$i" ] || continue
        local mp="${LAYER_MNT_BASE}${n}"
        mkdir -p "$mp"
        mount -o ro,loop "$i" "$mp" || error "cannot mount layer $i"
        if [ -z "$lower" ]; then lower="$mp"; else lower="${mp}:${lower}"; fi
        log "  layer $n: $(basename "$i")"
        n=$((n + 1))
    done <<< "$chain"
    [ "$n" -ge 2 ] || error "expected at least 2 layers, got $n"

    mkdir -p "$MOUNT_SRC"
    mount -t overlay imag-src -o "ro,lowerdir=${lower}" "$MOUNT_SRC" \
        || error "cannot stack the layer overlay"

    log "Copying root filesystem to $MOUNT_ROOT (this takes a few minutes)"
    rsync -aHAXS --numeric-ids --info=progress2 \
        --exclude='/boot/efi/*' \
        "$MOUNT_SRC/" "$MOUNT_ROOT/" || error "rsync of the root filesystem failed"

    # Prove a real rootfs landed. NOTE: do NOT gate on a kernel here — the install layers carry
    # NONE (live-verified on a real 24.04.2 desktop ISO: minimal.squashfs has an empty /boot and no
    # /lib/modules at all; the kernel sits in the `.live` layer we deliberately skip, and in the ISO
    # pool). The chroot step installs it from apt.
    [ -f "$MOUNT_ROOT/etc/os-release" ] || error "copied rootfs has no /etc/os-release — wrong layers?"
    [ -x "$MOUNT_ROOT/usr/bin/apt-get" ] || error "copied rootfs has no apt — wrong layers?"
}

prepare_chroot() {
    mount --bind /dev "$MOUNT_ROOT/dev"
    mount --bind /dev/pts "$MOUNT_ROOT/dev/pts"
    mount -t proc proc "$MOUNT_ROOT/proc"
    mount -t sysfs sys "$MOUNT_ROOT/sys"
    mount -t tmpfs tmpfs "$MOUNT_ROOT/run"
    mkdir -p "$MOUNT_ROOT/run/systemd/resolve"
    cp -f /etc/resolv.conf "$MOUNT_ROOT/run/systemd/resolve/stub-resolv.conf" 2>/dev/null || true
    cp -fL /etc/resolv.conf "$MOUNT_ROOT/etc/resolv.conf" 2>/dev/null || true
}

write_target_config() {
    local root_uuid esp_uuid
    root_uuid=$(blkid -s UUID -o value "$ROOT_PART") || error "cannot read root UUID"
    esp_uuid=$(blkid -s UUID -o value "$ESP_PART") || error "cannot read ESP UUID"
    imag_fstab "$root_uuid" "$esp_uuid" > "$MOUNT_ROOT/etc/fstab"

    echo "$IMAG_HOSTNAME" > "$MOUNT_ROOT/etc/hostname"
    cat > "$MOUNT_ROOT/etc/hosts" <<EOF
127.0.0.1	localhost
127.0.1.1	${IMAG_HOSTNAME}
::1	ip6-localhost ip6-loopback
EOF

    # Default gateway/DNS from the live session when not given explicitly.
    [ -n "$IMAG_GW" ]  || IMAG_GW=$(ip -4 route show default | grep -oP 'via \K\S+' | head -1 || true)
    [ -n "$IMAG_DNS" ] || IMAG_DNS="$IMAG_GW"
    [ -n "$IMAG_GW" ] || error "cannot determine the default gateway — pass --gateway"

    mkdir -p "$MOUNT_ROOT/etc/NetworkManager/system-connections"
    imag_nm_keyfile "$NM_CON_ID" "$IMAG_IP" "$IMAG_PREFIX" "$IMAG_GW" "$IMAG_DNS" \
        > "$MOUNT_ROOT/etc/NetworkManager/system-connections/${NM_CON_ID}.nmconnection"
    chmod 600 "$MOUNT_ROOT/etc/NetworkManager/system-connections/${NM_CON_ID}.nmconnection"

    # A fresh install must get its OWN machine-id (a copied one makes two boxes share a DHCP
    # identity and journal namespace).
    : > "$MOUNT_ROOT/etc/machine-id"
    rm -f "$MOUNT_ROOT/var/lib/dbus/machine-id"
}

configure_in_chroot() {
    log "Configuring the installed system (user, ssh, grub)"
    cat > "$MOUNT_ROOT/tmp/imag-chroot.sh" <<CHROOT
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

id -u "${DESKTOP_USER}" >/dev/null 2>&1 || useradd -m -s /bin/bash -G sudo,adm,video,audio,plugdev "${DESKTOP_USER}"
echo "${DESKTOP_USER}:${DESKTOP_PW}" | chpasswd
echo "root:${DESKTOP_PW}" | chpasswd

apt-get update -qq
apt-get install -y --no-install-recommends openssh-server network-manager grub-efi-amd64 grub-efi-amd64-signed shim-signed >/dev/null

# The ISO's install layers ship NO kernel (see copy_rootfs) — pull one from apt. \`linux-generic\`
# brings image + modules + headers (headers matter for any later DKMS driver). setup-imag.sh may
# swap in the lowlatency HWE kernel afterwards; this is the bootable baseline.
if ! ls /boot/vmlinuz-* >/dev/null 2>&1; then
    apt-get install -y linux-generic >/dev/null || exit 1
fi
ls /boot/vmlinuz-* >/dev/null 2>&1 || { echo "no kernel installed in the chroot" >&2; exit 1; }

# noble gotcha: openssh-server ships socket-activated; the appliance wants the plain service so a
# box is reachable the moment multi-user is up, independent of socket activation quirks.
systemctl disable --now ssh.socket >/dev/null 2>&1 || true
systemctl enable ssh.service >/dev/null 2>&1 || systemctl enable ssh >/dev/null 2>&1 || true
systemctl enable NetworkManager >/dev/null 2>&1 || true
# The cam-fleet lesson (#547): unbounded network-online waits stall boot before sshd starts.
systemctl mask systemd-networkd-wait-online.service >/dev/null 2>&1 || true

systemd-machine-id-setup >/dev/null 2>&1 || true

# grub: a NAMED bootloader id AND the removable fallback path — firmware that ignores NVRAM still
# finds \EFI\BOOT\BOOTX64.EFI (the documented cam-fleet gotcha).
grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=imag-nb --recheck
grub-install --target=x86_64-efi --efi-directory=/boot/efi --removable --recheck
update-grub

for k in /boot/vmlinuz-*; do
    v=\${k#/boot/vmlinuz-}
    [ -f "/boot/initrd.img-\${v}" ] || update-initramfs -c -k "\${v}"
done
CHROOT
    chroot "$MOUNT_ROOT" bash /tmp/imag-chroot.sh || error "chroot configuration failed"
    rm -f "$MOUNT_ROOT/tmp/imag-chroot.sh"

    # Belt and braces: the install is worthless if it cannot boot or cannot be reached.
    ls "$MOUNT_ROOT"/boot/initrd.img-* >/dev/null 2>&1 || error "no initrd in the installed system"
    [ -f "$MOUNT_ROOT/boot/grub/grub.cfg" ] || error "grub.cfg missing in the installed system"
    [ -f "$MOUNT_ROOT/boot/efi/EFI/BOOT/BOOTX64.EFI" ] || error "removable EFI fallback missing"
}

create_efi_boot_entry() {
    local disk="$DEVICE" part=1
    efibootmgr | grep -q 'imag-nb' && log "NVRAM entry 'imag-nb' already present" && return 0
    efibootmgr -c -d "$disk" -p "$part" -L "imag-nb" -l '\EFI\imag-nb\shimx64.efi' >/dev/null 2>&1 \
        || efibootmgr -c -d "$disk" -p "$part" -L "imag-nb" -l '\EFI\imag-nb\grubx64.efi' >/dev/null 2>&1 \
        || warn "could not create an NVRAM entry — the removable fallback still boots"
}

main() {
    trap cleanup_mounts EXIT
    check_requirements
    confirm_device
    cleanup_mounts
    partition_disk
    mount_target
    copy_rootfs
    prepare_chroot
    write_target_config
    configure_in_chroot
    create_efi_boot_entry
    sync
    cleanup_mounts
    log "DONE — ${IMAG_HOSTNAME} installed on ${DEVICE} (${IMAG_IP}/${IMAG_PREFIX}, user ${DESKTOP_USER})"
    log "Next: remove the live-USB, reboot, then run scripts/setup-imag.sh on the box."
}

# Sourced (unit tests) -> define the pure functions only. Executed -> run the install.
if [ -n "${BASH_SOURCE[0]:-}" ] && [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0 2>/dev/null || true
fi
main "$@"
