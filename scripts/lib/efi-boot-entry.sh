#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + two constants, no top-level side effects) --
# deliberately NOT `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it
# (the standing .claude/rules/ci-testing-gotchas.md rule); every caller owns its own strictness.
#
# scripts/lib/efi-boot-entry.sh -- #1066 D6: the ONE source of truth for the named `cam-box` UEFI
# NVRAM boot-entry decision logic, shared by create-usb-linux.sh (host-side guard: write the entry
# only when the target IS the builder's own boot disk), setup-device.sh (on-box STEP 17d: create the
# entry in the box's OWN NVRAM + lead BootOrder), and verify-device.sh (check (al): certify the
# entry exists AND leads BootOrder). Pure functions parse an `efibootmgr` dump or a device path
# and decide, so they are Tier-0 testable by sourcing this file (the scripts/lib/ndi-provision.sh
# convention). ONE imperative exception (issue 1311): `efi_cam_box_ensure` -- the create + READ-BACK
# + repair loop create-usb-linux.sh and setup-device.sh STEP 17d both run. It calls efibootmgr
# only through `${EFI_BOOTMGR_BIN:-efibootmgr}` (and blkid through `${EFI_BLKID_BIN:-blkid}`), so a
# test drives it against a fake NVRAM. The other imperative calls (findmnt / ssh) stay in the scripts.

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

# efi_entry_verdict <efibootmgr-output> [expected-esp-partuuid] -> exactly `ok` when a cam-box
# entry exists AND leads BootOrder; otherwise a single `FAIL: <reason>` line. Used by
# verify-device.sh (al). With a SECOND argument (even an empty one) the output is read as an
# `efibootmgr -v` dump and every cam-box entry's stored device path is graded too (issue 1311): a
# firmware-mangled VenHw() path, a non-HD() path or an entry on another ESP GUID FAILs. An
# empty/unreadable input is the CALLER's concern (an ssh failure is its own FAIL, test-strictness).
efi_entry_verdict() {
    local out="$1" nums order first n path
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
    if [ "$#" -ge 2 ]; then
        for n in $nums; do
            path="$(efi_cam_box_entry_path "$out" "$n")"
            if ! efi_device_path_healthy "$path" "$2"; then
                printf 'FAIL: `%s` entry Boot%s has a bad stored device path (%s) -- not an HD() path to %s on this disk'"'"'s ESP%s; re-run setup-device.sh to repair it (issue 1311)\n' \
                    "$EFI_CAM_BOX_LABEL" "$n" "${path:-<none>}" "$EFI_CAM_BOX_LOADER" "${2:+ (PARTUUID $2)}"
                return 0
            fi
        done
    fi
    if efi_cam_box_leads "$out"; then
        printf 'ok\n'
    else
        first="${order%%,*}"
        printf 'FAIL: `%s` entry (Boot%s) exists but does NOT lead BootOrder (first=%s) -- firmware may boot a stale entry; re-run setup-device.sh to reorder (#1066 D6)\n' \
            "$EFI_CAM_BOX_LABEL" "$(printf '%s' "$nums" | tr '\n' ',' | sed 's/,$//')" "$first"
    fi
}

# ---------------------------------------------------------------------------------------------
# Issue 1311: READ BACK the stored entry. On the 24.9.2026 M.2 migration `efibootmgr -c` reported
# success, yet cam1 ended up with NO entry and cam2 with a firmware-mangled `VenHw(...)` device
# path that was not first in BootOrder (verify-device (al) FAILED until it was repaired by hand).
# The parsers below read an `efibootmgr -v` dump (label and device path separated by whitespace).
# ---------------------------------------------------------------------------------------------

# efi_cam_box_entry_path <efibootmgr-v-output> <bootnum> -> the stored device path of that
# `cam-box` entry (everything after the label), or empty when <bootnum> is not a cam-box entry.
efi_cam_box_entry_path() {
    printf '%s\n' "$1" | awk -v lbl="$EFI_CAM_BOX_LABEL" -v want="$2" '
        !done && $1 ~ /^Boot[0-9A-Fa-f]+\*?$/ && $2 == lbl {
            n = $1; sub(/^Boot/, "", n); sub(/\*$/, "", n)
            if (toupper(n) == toupper(want)) {
                line = $0
                sub(/^[^ \t]+[ \t]+[^ \t]+[ \t]*/, "", line)
                print line
                done = 1
            }
        }'
}

# efi_esp_partition_of_disk <whole-disk> -> the ESP (partition 1) device: /dev/sda -> /dev/sda1,
# /dev/nvme0n1 -> /dev/nvme0n1p1, /dev/mmcblk0 -> /dev/mmcblk0p1 (both builders put the ESP first).
efi_esp_partition_of_disk() {
    case "$1" in
        *nvme* | *mmcblk*) printf '%sp1\n' "$1" ;;
        *) printf '%s1\n' "$1" ;;
    esac
}

# efi_device_path_healthy <device-path> [expected-esp-partuuid] -> exit 0 iff the path is a real
# `HD(...)` disk path (a firmware-expanded PciRoot()/.../HD(...) path is fine) with NO `VenHw(`
# node, that names the removable loader BOOTX64.EFI (the only core --removable writes), and --
# when an expected ESP PARTUUID is given -- whose HD() node names that partition GUID (all
# case-insensitive). A stale entry from a previous install points at a GUID that no longer exists,
# so it is NOT healthy. Pure predicate.
efi_device_path_healthy() {
    local path="$1" want="${2:-}" up
    up="$(printf '%s' "$path" | tr 'a-z' 'A-Z')"
    case "$up" in *VENHW\(*) return 1 ;; esac
    case "$up" in *HD\(*) ;; *) return 1 ;; esac
    case "$up" in *BOOTX64.EFI*) ;; *) return 1 ;; esac
    [ -n "$want" ] || return 0
    case "$up" in *"$(printf '%s' "$want" | tr 'a-z' 'A-Z')"*) return 0 ;; esac
    return 1
}

# efi_cam_box_repair_plan <efibootmgr-v-output> [expected-esp-partuuid] -> ONE line:
#   ok               a cam-box entry exists, every cam-box entry is healthy, and one leads BootOrder
#   create           no cam-box entry at all
#   recreate <nums>  at least one cam-box entry is mangled/stale: delete ALL cam-box entries
#                    (space-separated nums) and create one fresh
#   reorder <num>    healthy entries exist but none leads BootOrder: move <num> first
efi_cam_box_repair_plan() {
    local out="$1" want="${2:-}" nums n path bad=0 good="" all=""
    nums="$(efi_cam_box_bootnums "$out")"
    if [ -z "$nums" ]; then
        printf 'create\n'
        return 0
    fi
    for n in $nums; do
        all="${all:+$all }$n"
        path="$(efi_cam_box_entry_path "$out" "$n")"
        if efi_device_path_healthy "$path" "$want"; then
            good="${good:+$good }$n"
        else
            bad=1
        fi
    done
    if [ "$bad" -eq 1 ]; then
        printf 'recreate %s\n' "$all"
    elif efi_cam_box_leads "$out"; then
        printf 'ok\n'
    else
        printf 'reorder %s\n' "${good%% *}"
    fi
}

# _efi_read_v <efibootmgr-bin> -> the `-v` dump; non-zero when efibootmgr fails OR prints nothing.
_efi_read_v() {
    local dump
    dump="$("$1" -v 2>/dev/null)" || return 1
    [ -n "$dump" ] || return 1
    printf '%s\n' "$dump"
}

# efi_esp_partuuid_of_disk <whole-disk> -> the ESP's PARTUUID via blkid, or empty when unreadable
# (the health check then skips the GUID facet and still requires an HD() path).
efi_esp_partuuid_of_disk() {
    "${EFI_BLKID_BIN:-blkid}" -s PARTUUID -o value "$(efi_esp_partition_of_disk "$1")" 2>/dev/null || true
}

# efi_cam_box_ensure <whole-disk> <expected-esp-partuuid|""> [fresh] -> exit 0 once `efibootmgr -v`
# READS BACK exactly what we want: a `cam-box` entry with an HD() path (on the expected ESP when
# known) that leads BootOrder. Otherwise it repairs -- create a missing entry, delete a mangled or
# stale one and recreate it, move a demoted healthy one first -- and re-checks, at most three
# actions. `fresh` (create-usb: a brand-new install) first deletes every prior `cam-box` entry.
# Exit 1 with a final `FAIL: ...` line naming what the firmware stored when it still is not right.
# An UNREADABLE `efibootmgr -v` (non-zero exit or no output) is never read as "no entry": it FAILs
# at once, with no NVRAM write (three blind creates would only pile up duplicates).
# Progress lines go to stdout. Idempotent: a correct entry is never touched.
efi_cam_box_ensure() {
    local disk="$1" want="${2:-}" mode="${3:-}" bin="${EFI_BOOTMGR_BIN:-efibootmgr}"
    local out plan n order attempt stored
    if ! out="$(_efi_read_v "$bin")"; then
        printf 'FAIL: efibootmgr -v unreadable (non-zero exit or no output) -- not touching the NVRAM blind (issue 1311)\n'
        return 1
    fi
    if [ "$mode" = "fresh" ]; then
        for n in $(efi_cam_box_bootnums "$out"); do
            "$bin" -b "$n" -B >/dev/null 2>&1 || true
            printf '  removed prior %s entry Boot%s (fresh install)\n' "$EFI_CAM_BOX_LABEL" "$n"
        done
    fi
    for attempt in 1 2 3; do
        if ! out="$(_efi_read_v "$bin")"; then
            printf 'FAIL: efibootmgr -v became unreadable during the repair (attempt %s) -- stopping (issue 1311)\n' "$attempt"
            return 1
        fi
        plan="$(efi_cam_box_repair_plan "$out" "$want")"
        case "$plan" in
            ok)
                printf '  verified: %s entry has an HD() device path and leads BootOrder (%s)\n' \
                    "$EFI_CAM_BOX_LABEL" "$(efi_boot_order "$out")"
                return 0
                ;;
            recreate\ *)
                for n in ${plan#recreate }; do
                    printf '  %s entry Boot%s is mangled or stale (%s) -- deleting it\n' \
                        "$EFI_CAM_BOX_LABEL" "$n" "$(efi_cam_box_entry_path "$out" "$n")"
                    "$bin" -b "$n" -B >/dev/null 2>&1 || true
                done
                ;;
        esac
        case "$plan" in
            create | recreate\ *)
                printf '  creating %s -> %s partition 1 (%s), attempt %s\n' \
                    "$EFI_CAM_BOX_LABEL" "$disk" "$EFI_CAM_BOX_LOADER" "$attempt"
                "$bin" -c -d "$disk" -p 1 -L "$EFI_CAM_BOX_LABEL" -l "$EFI_CAM_BOX_LOADER" >/dev/null 2>&1 \
                    || printf '  efibootmgr -c failed (attempt %s)\n' "$attempt"
                ;;
            reorder\ *)
                n="${plan#reorder }"
                order="$(efi_boot_order_lead "$n" "$(efi_boot_order "$out")")"
                printf '  moving %s (Boot%s) to the front of BootOrder: %s\n' "$EFI_CAM_BOX_LABEL" "$n" "$order"
                "$bin" -o "$order" >/dev/null 2>&1 || printf '  efibootmgr -o failed (attempt %s)\n' "$attempt"
                ;;
        esac
    done
    if ! out="$(_efi_read_v "$bin")"; then
        printf 'FAIL: efibootmgr -v became unreadable after the repair -- cannot verify the %s entry (issue 1311)\n' "$EFI_CAM_BOX_LABEL"
        return 1
    fi
    plan="$(efi_cam_box_repair_plan "$out" "$want")"
    if [ "$plan" = "ok" ]; then
        printf '  verified: %s entry has an HD() device path and leads BootOrder (%s)\n' \
            "$EFI_CAM_BOX_LABEL" "$(efi_boot_order "$out")"
        return 0
    fi
    stored=""
    for n in $(efi_cam_box_bootnums "$out"); do
        stored="${stored:+$stored; }Boot$n=$(efi_cam_box_entry_path "$out" "$n")"
    done
    printf 'FAIL: the %s UEFI entry is still wrong after 3 repair attempts (plan: %s; BootOrder: %s; stored: %s)\n' \
        "$EFI_CAM_BOX_LABEL" "$plan" "$(efi_boot_order "$out")" "${stored:-none}"
    return 1
}
