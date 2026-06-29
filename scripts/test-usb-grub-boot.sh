#!/bin/bash
#
# Regression test for #344 — the USB/image GRUB EFI core must actually BOOT.
#
# Builds a minimal GPT loopback image (ESP + ext4 root with a tiny grub.cfg
# that prints a marker), installs the removable EFI core via the shared
# scripts/lib/install-grub-efi.sh `build_grub_efi_core`, boots it headless in
# qemu + OVMF over a USB-storage controller (exactly how a cam-box boots its
# USB), and asserts the marker reaches the serial console — i.e. GRUB chained
# to the real grub.cfg instead of dropping to the `grub>` rescue prompt.
#
# This is an ON-DEMAND integration test (needs root + losetup + qemu-system +
# OVMF). It is NOT part of the Tier-0 unit CI (privileged + needs firmware).
# Run it on dev1 when touching the image/grub install path:
#     sudo bash scripts/test-usb-grub-boot.sh
# Exit 0 = booted (marker seen). Exit 1 = did NOT boot (regression). Exit 2 =
# prerequisites missing (cannot run here — never silently passes).
#
set -euo pipefail

MARKER="MARKER_GRUB_BOOTED_OK_344"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/install-grub-efi.sh
source "$HERE/lib/install-grub-efi.sh"

OVMF_CODE="$(ls /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd 2>/dev/null | head -1 || true)"
OVMF_VARS="$(ls /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/OVMF/OVMF_VARS.fd 2>/dev/null | head -1 || true)"

# --- prerequisites (fail loudly, never silently skip) ---
miss=""
[ "$(id -u)" -eq 0 ] || miss="$miss root(losetup/mount)"
command -v qemu-system-x86_64 >/dev/null || miss="$miss qemu-system-x86_64"
command -v grub-mkimage       >/dev/null || miss="$miss grub-mkimage"
command -v losetup            >/dev/null || miss="$miss losetup"
command -v mkfs.vfat          >/dev/null || miss="$miss dosfstools"
# build_grub_efi_core reads modules from here; a missing grub-efi-amd64-bin must be PREREQ (exit 2),
# not a false boot regression (exit 1).
[ -d /usr/lib/grub/x86_64-efi ] || miss="$miss grub-efi-amd64-bin(/usr/lib/grub/x86_64-efi)"
[ -n "$OVMF_CODE" ] && [ -n "$OVMF_VARS" ] || miss="$miss ovmf"
if [ -n "$miss" ]; then
    echo "PREREQ MISSING:$miss" >&2
    echo "This on-demand test needs root + qemu + OVMF + losetup. Run on dev1: sudo bash scripts/test-usb-grub-boot.sh" >&2
    exit 2
fi

WORK="$(mktemp -d)"
IMG="$WORK/usb.img"
VARS="$WORK/ovmf_vars.fd"
SERIAL="$WORK/serial.log"
MNT="$WORK/mnt"
LOOP=""
cleanup() {
    set +e
    [ -n "${QPID:-}" ] && kill -9 "$QPID" 2>/dev/null
    mountpoint -q "$MNT/boot/efi" && umount "$MNT/boot/efi"
    mountpoint -q "$MNT" && umount "$MNT"
    [ -n "$LOOP" ] && losetup -d "$LOOP" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

echo "[1/4] build minimal GPT image (ESP + ext4 root)"
truncate -s 256M "$IMG"
parted -s "$IMG" mklabel gpt \
    mkpart ESP fat32 1MiB 65MiB set 1 esp on \
    mkpart root ext4 65MiB 100%
LOOP="$(losetup --find --show -P "$IMG")"
# wait for partition nodes — fail loudly if losetup -P never created them (harness/env
# failure, NOT a grub boot regression → exit 2 "cannot run here", not exit 1)
part_ok=0
for _ in $(seq 1 20); do [ -b "${LOOP}p2" ] && { part_ok=1; break; }; sleep 0.3; done
[ "$part_ok" -eq 1 ] || { echo "PREREQ/HARNESS FAIL: partition node ${LOOP}p2 never appeared (losetup -P)" >&2; exit 2; }
mkfs.vfat -F32 "${LOOP}p1" >/dev/null
mkfs.ext4 -q "${LOOP}p2"
ROOT_UUID="$(blkid -s UUID -o value "${LOOP}p2")"

echo "[2/4] populate /boot/grub/grub.cfg (root UUID=$ROOT_UUID)"
mkdir -p "$MNT"
mount "${LOOP}p2" "$MNT"
mkdir -p "$MNT/boot/efi" "$MNT/boot/grub/x86_64-efi"
mount "${LOOP}p1" "$MNT/boot/efi"
mkdir -p "$MNT/boot/efi/EFI/BOOT"
# minimal config: route output to serial, print the marker, halt
cat > "$MNT/boot/grub/grub.cfg" <<CFG
serial --unit=0 --speed=115200
terminal_output serial console
echo "$MARKER root=$ROOT_UUID"
sleep 5
halt
CFG

echo "[3/4] install EFI core via build_grub_efi_core (the unit under test)"
build_grub_efi_core "$MNT/boot/efi" "$ROOT_UUID" /usr/lib/grub/x86_64-efi || true
test -f "$MNT/boot/efi/EFI/BOOT/BOOTX64.EFI" || { echo "FAIL: no BOOTX64.EFI produced"; exit 1; }
sync
umount "$MNT/boot/efi"
umount "$MNT"
losetup -d "$LOOP"; LOOP=""

echo "[4/4] boot image in qemu + OVMF (USB-storage), capture serial"
cp "$OVMF_VARS" "$VARS"
# Prefer KVM; fall back to TCG (qemu's accel list does this automatically). Software TCG boots
# the OVMF firmware several times slower, so scale the boot-wait budget when KVM is unavailable.
ACCEL="kvm:tcg"; BOOT_WAIT=45
[ -r /dev/kvm ] || { ACCEL="tcg"; BOOT_WAIT=150; }
qemu-system-x86_64 \
    -machine "q35,accel=$ACCEL" -m 1024 -smp 1 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$VARS" \
    -device qemu-xhci,id=xhci \
    -drive "if=none,id=usbstick,format=raw,file=$IMG,snapshot=on" \
    -device usb-storage,bus=xhci.0,drive=usbstick \
    -net none -display none -serial "file:$SERIAL" \
    >/dev/null 2>&1 &
QPID=$!

booted=0
for _ in $(seq 1 "$BOOT_WAIT"); do
    if grep -qa "$MARKER" "$SERIAL" 2>/dev/null; then booted=1; break; fi
    # grub.cfg halts after printing the marker, so a successful boot makes qemu EXIT. If the
    # process is gone, do one final marker check (it may have printed then halted between polls)
    # and stop waiting — a dead qemu emits no further serial, so looping on it is the silent-death
    # trap. qemu gone WITHOUT the marker = it never booted = regression.
    if ! kill -0 "$QPID" 2>/dev/null; then
        grep -qa "$MARKER" "$SERIAL" 2>/dev/null && booted=1
        break
    fi
    sleep 1
done
kill -9 "$QPID" 2>/dev/null || true; QPID=""

if [ "$booted" -eq 1 ]; then
    echo "PASS: image booted — GRUB chained to grub.cfg (marker on serial)"
    exit 0
fi
echo "FAIL: image did NOT boot — GRUB never reached grub.cfg (rescue prompt / no marker)"
echo "--- serial tail ---"
tr -d '\000' < "$SERIAL" | sed 's/\x1b\[[0-9;?=]*[a-zA-Z]//g' | grep -avE '^\s*$' | tail -20 || true
exit 1
