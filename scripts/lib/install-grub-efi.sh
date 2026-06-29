#!/bin/bash
# Shared GRUB EFI core installer for the USB/image builders (#344).
#
# build_grub_efi_core <efi_dir> <root_fs_uuid> [grub_modules_dir]
#   Writes the removable EFI boot stub (EFI/BOOT/BOOTX64.EFI) under <efi_dir>
#   (the mounted ESP, e.g. /boot/efi) that boots the system whose root
#   filesystem has UUID <root_fs_uuid>. The caller must already have generated
#   <root>/boot/grub/grub.cfg (update-grub) and installed the grub modules under
#   <root>/boot/grub/x86_64-efi (grub-install does this). [grub_modules_dir] is
#   where grub-mkimage reads modules from at build time (default the host/chroot
#   /usr/lib/grub/x86_64-efi).
#
# WHY NOT `grub-install --removable` (#344): on Ubuntu 24.04 it embeds a
# signed-grub memdisk core whose live-media probe (`search /.disk/info` …) fails
# on a normal installed root, so the core never chains to the real grub.cfg and
# drops to the `grub>` rescue prompt — the USB never boots. We instead build a
# clean core with grub-mkimage whose embedded config chains straight to
# /boot/grub/grub.cfg by root filesystem UUID (topology-independent: works no
# matter which disk slot the USB ends up in). Proven via scripts/test-usb-grub-boot.sh.
#
# SECURE BOOT (#344): the grub-mkimage core is UNSIGNED, so the target firmware must have UEFI
# Secure Boot DISABLED. This is NOT a regression — the prior `grub-install --removable` core was
# not shim-chained either, so these sticks already required Secure Boot off; the cam-box appliances
# run with it disabled. If a future target needs Secure Boot, that's a separate shim+signed-grub
# task, not this boot fix.
build_grub_efi_core() {
    local efi_dir="$1"
    local root_uuid="$2"
    local modules_dir="${3:-/usr/lib/grub/x86_64-efi}"

    [ -n "$efi_dir" ]   || { echo "build_grub_efi_core: efi_dir required" >&2; return 1; }
    [ -n "$root_uuid" ] || { echo "build_grub_efi_core: root_uuid required" >&2; return 1; }
    [ -d "$modules_dir" ] || { echo "build_grub_efi_core: grub modules dir not found: $modules_dir" >&2; return 1; }

    mkdir -p "$efi_dir/EFI/BOOT"

    local embed
    embed="$(mktemp)"
    # Minimal embedded config: find the root by UUID, then hand off to the real
    # grub.cfg. No live-media probing, no hard-coded disk hints.
    cat > "$embed" <<EOF
search.fs_uuid $root_uuid root
set prefix=(\$root)/boot/grub
configfile \$prefix/grub.cfg
EOF

    grub-mkimage \
        -O x86_64-efi \
        -d "$modules_dir" \
        -o "$efi_dir/EFI/BOOT/BOOTX64.EFI" \
        -p '(memdisk)/boot/grub' \
        -c "$embed" \
        part_gpt part_msdos fat ext2 normal configfile search search_fs_uuid search_label \
        linux echo all_video gzio test true loadenv efi_gop efi_uga gfxterm \
        serial terminal boot reboot halt sleep
    local rc=$?
    rm -f "$embed"

    [ "$rc" -eq 0 ] && [ -s "$efi_dir/EFI/BOOT/BOOTX64.EFI" ]
}
