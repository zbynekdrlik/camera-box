#!/bin/bash
# scripts/lib/camera-box-grow-root.sh — auto-grow root partition on first boot (#369).
#
# Installed into cam-box images by both builders (create-usb-linux.sh + build-image.sh) at
# /usr/local/sbin/camera-box-grow-root.sh, activated by camera-box-grow-root.service.
#
# WHY: cam4 shipped with a 3.5G/92%-full root partition on a 57G disk because the root
# partition written by the image builder was never expanded to fill the physical disk.
# This script runs once on first boot (via the systemd service) and expands the root partition.
#
# FAULT-TOLERANT: exits 0 even when growpart/resize2fs fails (e.g. the overlay-image layout
# has 3 partitions — EFI + root + overlay — so root is NOT the last partition; growpart will
# refuse to expand it and return non-zero, which we suppress). The marker is always written so
# subsequent boots skip this script entirely. Boot is never blocked.
#
# Supports standard partition naming: /dev/sda2, /dev/nvme0n1p2, /dev/mmcblk0p2.
set -uo pipefail
# NOTE: -e (exit-on-error) is deliberately ABSENT. Without it, growpart/resize2fs
# failures are non-fatal and the unconditional `touch "$MARKER"` at the end always
# runs. Do NOT add -e here — it would break the fault-tolerant "always write marker"
# contract and risk blocking boot on the 3-partition overlay image layout.

MARKER=/var/lib/camera-box/grow-root.done
[ -f "$MARKER" ] && exit 0

ROOT_DEV=$(findmnt -n -o SOURCE /) || {
    mkdir -p /var/lib/camera-box && touch "$MARKER"
    exit 0
}
DISK_DEV=$(echo "$ROOT_DEV" | sed -E 's/p?[0-9]+$//')
PART_NUM=$(echo "$ROOT_DEV" | grep -oE '[0-9]+$')
DISK_SIZE=$(lsblk -b -n -o SIZE "$DISK_DEV" 2>/dev/null | head -1)
PART_SIZE=$(lsblk -b -n -o SIZE "$ROOT_DEV" 2>/dev/null | head -1)
if [[ -n "$DISK_SIZE" && -n "$PART_SIZE" && $(( DISK_SIZE - PART_SIZE )) -gt 1073741824 ]]; then
    echo "[camera-box-grow-root] Growing root $ROOT_DEV ..."
    growpart "$DISK_DEV" "$PART_NUM" 2>/dev/null && resize2fs "$ROOT_DEV" 2>/dev/null \
        && echo "[camera-box-grow-root] Success" \
        || echo "[camera-box-grow-root] grow/resize skipped (non-fatal)"
else
    echo "[camera-box-grow-root] Already at capacity — skipping"
fi
mkdir -p /var/lib/camera-box
touch "$MARKER"
