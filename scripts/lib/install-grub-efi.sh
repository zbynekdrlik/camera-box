#!/bin/bash
# Shared GRUB EFI core installer for the USB/image builders (#344).
#
# build_grub_efi_core <efi_dir> <root_fs_uuid> [grub_modules_dir]
#   Installs the removable EFI boot stub (EFI/BOOT/BOOTX64.EFI) that boots the
#   system whose root filesystem has UUID <root_fs_uuid>. <efi_dir> is the
#   mounted ESP (e.g. /boot/efi). Caller is responsible for having already
#   generated /boot/grub/grub.cfg (update-grub) and installed the grub modules
#   under <root>/boot/grub/x86_64-efi.
#
# NOTE (#344): this is the BROKEN baseline — `grub-install --removable` on
# Ubuntu 24.04 embeds a signed-grub memdisk core whose live-media probe
# (search /.disk/info) fails on an installed root, so it never chains to the
# real grub.cfg and drops to the `grub>` rescue prompt. The regression test
# scripts/test-usb-grub-boot.sh proves this fails to boot. The fix replaces
# the body with a clean grub-mkimage core.
build_grub_efi_core() {
    local efi_dir="$1"
    local root_uuid="$2"   # unused in the broken baseline
    local modules_dir="${3:-/usr/lib/grub/x86_64-efi}"  # unused in the broken baseline
    grub-install --target=x86_64-efi --efi-directory="$efi_dir" \
        --bootloader-id=ubuntu --removable
}
