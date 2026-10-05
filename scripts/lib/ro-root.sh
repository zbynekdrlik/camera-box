#!/usr/bin/env bash
# scripts/lib/ro-root.sh — the ONE read-only-root canon of the provisioning scripts (issue 808).
#
# A cambox (scripts/setup-device.sh STEP 18) and a handheld SBC (scripts/bkshading-provision-sbc.sh
# --install) both run with a READ-ONLY root plus tmpfs for the directories the system must write.
# An abrupt power-off (a cambox switched off with the rig, a handheld unplugged after each use) then
# cannot corrupt the root filesystem. Owner ruling 5948648089 (2.10.2026): the SBC root goes
# read-only "the same as the camboxes". So the tmpfs set, the root line and the "is the root ro?"
# reading live HERE, once:
#   - setup-device.sh STEP 18 calls ro_root_root_line + ro_root_tmpfs_line per path inside its own
#     heredoc: the cambox fstab also carries the EFI line (from fstab.bak) and the issue-1309 journal
#     partition line BETWEEN /var/log and /var/tmp, so it shares the LINES, not a whole-file builder.
#     Its written fstab is byte-identical to before (golden: tests/fixtures/ro_root_fstab_808/).
#   - bkshading-provision-sbc.sh writes ro_root_fstab_text: the same root line + every other mount
#     of the original fstab kept + the same tmpfs block.
#   - ro_root_mount_mode is the one first-token reading: setup-device.sh's root_mount_is_readonly
#     calls it, verify-device.sh keeps its own copy (a python parity test pins both), the SBC
#     --check and bkshading-deploy-relay.sh decide from it, and cam2-painter-ro-persist.sh emits its
#     definition into cam2's remote text to verify the root went back to ro (issue 1405).
# NOT covered: the image builders write their own fstab. create-usb-linux.sh writes the first-boot
# (rw) fstab that setup-device STEP 18 later replaces with this canon; build-image.sh's read-only
# overlay image still carries its own, different tmpfs set (no /var/spool, /var/log 64M).
#
# Source-only: pure functions, NO side effects, and deliberately no `set -euo pipefail` (it would
# leak into the sourcing shell — .claude/rules/ci-testing-gotchas.md). No grep/awk/sed either: the
# issue-1311 heredoc test runs STEP 18 with `grep` stubbed out.
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# The writable tmpfs mount points of a read-only-root box, in fstab order.
ro_root_tmpfs_paths() {
  printf '%s\n' /tmp /var/log /var/tmp /var/cache /var/spool
}

# ro_root_tmpfs_line PATH -> the fstab line mounting PATH as tmpfs. An unknown PATH prints nothing
# and returns 1. Sizes: /var/cache is >= 512M fleet-wide so apt can never ENOSPC and leave a kernel
# without its initrd (issue 295); /var/log is bounded by logrotate on a cambox (issue 679).
ro_root_tmpfs_line() {
  case "${1:-}" in
    /tmp) printf '%s\n' 'tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0' ;;
    /var/log) printf '%s\n' 'tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0' ;;
    /var/tmp) printf '%s\n' 'tmpfs /var/tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=50M 0 0' ;;
    /var/cache) printf '%s\n' 'tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0' ;;
    /var/spool) printf '%s\n' 'tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0' ;;
    *) return 1 ;;
  esac
}

# ro_root_tmpfs_lines -> every tmpfs line, in ro_root_tmpfs_paths order.
ro_root_tmpfs_lines() {
  local p
  for p in $(ro_root_tmpfs_paths); do
    ro_root_tmpfs_line "$p"
  done
}

# ro_root_root_line UUID FSTYPE -> the read-only root line `UUID=<uuid> / <fstype> ro 0 1`.
# An empty UUID or FSTYPE prints nothing and returns 1 (never an unbootable `UUID= /` line).
ro_root_root_line() {
  local uuid="${1:-}" fstype="${2:-}"
  [ -n "$uuid" ] && [ -n "$fstype" ] || return 1
  printf 'UUID=%s / %s ro 0 1\n' "$uuid" "$fstype"
}

# ro_root_mount_mode OPTS -> `ro` | `rw` | `unknown`, from the FIRST comma-token of a mount-options
# string (`findmnt -no OPTIONS /`; the kernel always emits ro/rw first). Substring-safe: an rw mount
# carrying `errors=remount-ro` is `rw`. An empty or unexpected read is `unknown`: a caller must treat
# it as unreadable, never as rw or ro.
ro_root_mount_mode() {
  case "${1:-}" in
    ro | ro,*) printf '%s\n' ro ;;
    rw | rw,*) printf '%s\n' rw ;;
    *) printf '%s\n' unknown ;;
  esac
}

# ro_root_is_tmpfs_path PATH -> 0 when PATH is one of the tmpfs mount points.
ro_root_is_tmpfs_path() {
  local p
  for p in $(ro_root_tmpfs_paths); do
    [ "$p" = "${1:-}" ] && return 0
  done
  return 1
}

# ro_root_kept_mounts ORIGINAL_FSTAB_TEXT -> the original fstab's mount lines that the read-only
# fstab keeps VERBATIM: every entry except the root (`/`, replaced by the ro root line) and the
# tmpfs mount points (replaced by the canonical set). Comments and blank lines are dropped. This is
# what keeps a board's own extra mount (a Raspberry Pi OS `/boot/firmware`, a swap entry) intact.
ro_root_kept_mounts() {
  local line first mnt
  while IFS= read -r line || [ -n "$line" ]; do
    read -r first mnt _ <<<"$line"
    [ -n "$first" ] || continue
    case "$first" in \#*) continue ;; esac
    [ "$mnt" = "/" ] && continue
    ro_root_is_tmpfs_path "$mnt" && continue
    printf '%s\n' "$line"
  done <<<"${1:-}"
}

# ro_root_fstab_text UUID FSTYPE ORIGINAL_FSTAB_TEXT -> the whole read-only fstab for a box with a
# single root filesystem (the handheld SBC): the ro root line, the kept mounts, the tmpfs set.
# Idempotent: fed its own output it returns the same text. Returns 1 (and prints nothing) on an
# empty UUID or FSTYPE.
ro_root_fstab_text() {
  local root kept
  root="$(ro_root_root_line "${1:-}" "${2:-}")" || return 1
  kept="$(ro_root_kept_mounts "${3:-}")"
  printf '%s\n' "# Root filesystem - read-only (scripts/lib/ro-root.sh, the cambox canon; issue 808)."
  printf '%s\n' "# Update it with: mount -o remount,rw /  ->  change  ->  mount -o remount,ro /"
  printf '%s\n' "$root"
  printf '\n%s\n' "# Other mounts kept from the original fstab"
  if [ -n "$kept" ]; then
    printf '%s\n' "$kept"
  else
    printf '%s\n' "# (none)"
  fi
  printf '\n%s\n' "# tmpfs mounts for writable directories"
  ro_root_tmpfs_lines
}
