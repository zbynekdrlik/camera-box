#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + two constants, no top-level side effects) --
# deliberately NOT `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it
# (the standing .claude/rules/ci-testing-gotchas.md rule); every caller owns its own strictness.
#
# scripts/lib/efi-boot-entry.sh -- #1066 D6: the ONE source of truth for the named `cam-box` UEFI
# NVRAM boot-entry decision logic, shared by create-usb-linux.sh (host-side guard: write the entry
# only when the target IS the builder's own boot disk), setup-device.sh (on-box STEP 17d: create the
# entry in the box's OWN NVRAM + lead BootOrder), and verify-device.sh (check (al): certify the
# entry exists AND leads BootOrder). Pure functions only -- NO `efibootmgr` calls, NO side effects:
# they parse an `efibootmgr` dump or a device path and decide, so they are Tier-0 testable by
# sourcing this file (the scripts/lib/ndi-provision.sh convention). The imperative shell (the real
# efibootmgr / findmnt / ssh calls) stays in the three scripts.

# The named entry's fixed identity -- ONE definition for all three scripts.
EFI_CAM_BOX_LABEL="cam-box"
# The loader path grub-install --removable (+ #344) wrote. The \EFI\ubuntu\grubx64.efi path does
# NOT exist with --removable, so the named entry MUST point at the removable BOOTX64.EFI core.
# shellcheck disable=SC2034  # consumed by the sourcing scripts (setup-device.sh STEP 17d), not here
EFI_CAM_BOX_LOADER='\EFI\BOOT\BOOTX64.EFI'

# efi_whole_disk_of <partition-or-disk> -> the parent WHOLE-disk device.
#   /dev/nvme0n1p2 -> /dev/nvme0n1 ; /dev/mmcblk0p1 -> /dev/mmcblk0 ; /dev/sda2 -> /dev/sda
# A bare whole-disk path (no partition suffix) is returned unchanged. nvme/mmcblk use a `pN`
# suffix; sdX/vdX use trailing digits.
efi_whole_disk_of() {
    local part="$1"
    case "$part" in
        *nvme*|*mmcblk*)
            case "$part" in
                *p[0-9]*) printf '%s\n' "${part%p[0-9]*}" ;;  # strip pN
                *)        printf '%s\n' "$part" ;;            # bare nvme0n1 / mmcblk0
            esac
            ;;
        *[0-9]) printf '%s\n' "${part%%[0-9]*}" ;;           # sdX / vdX: strip trailing digits
        *)      printf '%s\n' "$part" ;;
    esac
}

# efi_cam_box_bootnums <efibootmgr-output> -> the boot number(s) (e.g. 0003) whose label is exactly
# `cam-box`, one per line. Empty output = no such entry. (Mirrors the awk in create-usb's original
# create_efi_boot_entry: the label is the SECOND whitespace field of a `Boot<hex>[*]` line.)
efi_cam_box_bootnums() {
    # NB: [0-9A-Fa-f]+ (not {4}) -- portable across mawk (Ubuntu default) which lacks intervals.
    printf '%s\n' "$1" | awk -v lbl="$EFI_CAM_BOX_LABEL" \
        '$2==lbl && $1 ~ /^Boot[0-9A-Fa-f]+\*?$/ {n=$1; sub(/^Boot/,"",n); sub(/\*$/,"",n); print n}'
}

# efi_boot_order <efibootmgr-output> -> the BootOrder CSV (e.g. 0003,0001,0000), or empty if absent.
efi_boot_order() {
    printf '%s\n' "$1" | sed -n 's/^BootOrder:[[:space:]]*//p' | head -1
}

# _efi_upper <s> -> <s> with a-f uppercased (BootOrder + bootnums should already agree in case, but
# compare case-insensitively to be safe).
_efi_upper() { printf '%s' "$1" | tr 'a-f' 'A-F'; }

# efi_cam_box_leads <efibootmgr-output> -> exit 0 iff a `cam-box` entry exists AND is the FIRST
# element of BootOrder. Pure predicate (no output).
efi_cam_box_leads() {
    local out="$1" nums order first n
    nums="$(efi_cam_box_bootnums "$out")"
    [ -n "$nums" ] || return 1
    order="$(efi_boot_order "$out")"
    [ -n "$order" ] || return 1
    first="${order%%,*}"
    for n in $nums; do
        [ "$(_efi_upper "$n")" = "$(_efi_upper "$first")" ] && return 0
    done
    return 1
}

# efi_boot_order_lead <bootnum> <current-order-csv> -> the order CSV with <bootnum> moved to the
# FRONT (dropping any existing occurrence). Pure. Used to reorder a demoted pre-existing entry.
efi_boot_order_lead() {
    local num="$1" order="$2" item out="" IFS=,
    for item in $order; do
        [ "$(_efi_upper "$item")" = "$(_efi_upper "$num")" ] && continue
        out="${out:+$out,}$item"
    done
    printf '%s\n' "${num}${out:+,$out}"
}

# efi_entry_verdict <efibootmgr-output> -> exactly `ok` when a cam-box entry exists AND leads
# BootOrder; otherwise a single `FAIL: <reason>` line. Used by verify-device.sh (al). An
# empty/unreadable input is the CALLER's concern (an ssh failure is its own FAIL, test-strictness).
efi_entry_verdict() {
    local out="$1" nums order first
    nums="$(efi_cam_box_bootnums "$out")"
    if [ -z "$nums" ]; then
        printf 'FAIL: no `%s` UEFI boot entry present -- the box depends on the AMI USB auto-entry, which failed on cam2 after a warm reboot; re-run setup-device.sh to create it (#1066 D6)\n' "$EFI_CAM_BOX_LABEL"
        return 0
    fi
    order="$(efi_boot_order "$out")"
    if [ -z "$order" ]; then
        printf 'FAIL: BootOrder is unreadable/empty -- cannot certify `%s` leads the boot order (#1066 D6)\n' "$EFI_CAM_BOX_LABEL"
        return 0
    fi
    if efi_cam_box_leads "$out"; then
        printf 'ok\n'
    else
        first="${order%%,*}"
        printf 'FAIL: `%s` entry (Boot%s) exists but does NOT lead BootOrder (first=%s) -- firmware may boot a stale entry; re-run setup-device.sh to reorder (#1066 D6)\n' \
            "$EFI_CAM_BOX_LABEL" "$(printf '%s' "$nums" | tr '\n' ',' | sed 's/,$//')" "$first"
    fi
}
