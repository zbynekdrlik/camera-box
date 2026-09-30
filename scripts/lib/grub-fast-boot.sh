#!/usr/bin/env bash
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell
# (pure functions + three constants, no top-level side effects; every caller owns its own strictness,
# the standing .claude/rules/ci-testing-gotchas.md rule).
#
# scripts/lib/grub-fast-boot.sh -- #1394: the ONE declaration of the cambox fast-boot GRUB settings.
# A cambox boots straight to Linux: no GRUB menu and no countdown, ever (owner ROZHODNUTÉ
# issuecomment-5913502328, 30.9.2026 -- cam6 sat in the menu with a 30 s recordfail countdown).
#
#   GRUB_TIMEOUT=0             no menu wait on a normal boot
#   GRUB_TIMEOUT_STYLE=hidden  the menu is never drawn
#   GRUB_RECORDFAIL_TIMEOUT=0  the one that bites: Ubuntu's grub.cfg sets `recordfail` on every boot
#                              and only a completed boot clears it (grub-common, which the appliance
#                              masks). After a cut boot or a hard power-off the recordfail branch runs,
#                              and its timeout defaults to 30 s with the menu shown.
#
# Consumers (all through this lib, so the settings cannot drift again):
#   create-usb-linux.sh  the base image (reflash stick / fresh M.2): copied into the chroot, applied
#                        to the /etc/default/grub it writes, and the generated grub.cfg is graded
#   build-image.sh       the ro-root+overlay image: applied to the rootfs /etc/default/grub
#   setup-device.sh      STEP 10: applied to the box's own /etc/default/grub before update-grub
#   verify-device.sh     check (aq): grades the box's generated /boot/grub/grub.cfg

GRUB_FAST_BOOT_TIMEOUT=0
GRUB_FAST_BOOT_STYLE=hidden
GRUB_FAST_BOOT_RECORDFAIL_TIMEOUT=0
# The /etc/default/grub keys, in the order they are appended to a file that lacks them.
GRUB_FAST_BOOT_KEYS="GRUB_TIMEOUT GRUB_TIMEOUT_STYLE GRUB_RECORDFAIL_TIMEOUT"

# _grub_fast_boot_wanted <key> -> the wanted `KEY=VALUE` line for one of the three keys (no newline);
# rc 1 for any other key.
_grub_fast_boot_wanted() {
    case "${1:-}" in
        GRUB_TIMEOUT)            printf 'GRUB_TIMEOUT=%s' "$GRUB_FAST_BOOT_TIMEOUT" ;;
        GRUB_TIMEOUT_STYLE)      printf 'GRUB_TIMEOUT_STYLE=%s' "$GRUB_FAST_BOOT_STYLE" ;;
        GRUB_RECORDFAIL_TIMEOUT) printf 'GRUB_RECORDFAIL_TIMEOUT=%s' "$GRUB_FAST_BOOT_RECORDFAIL_TIMEOUT" ;;
        *) return 1 ;;
    esac
}

# grub_fast_boot_default_lines -> the three `KEY=VALUE` lines, one per line.
grub_fast_boot_default_lines() {
    local k
    for k in $GRUB_FAST_BOOT_KEYS; do
        _grub_fast_boot_wanted "$k"
        printf '\n'
    done
}

# _grub_fast_boot_active_key <line> -> the variable an ACTIVE shell assignment line sets
# (`KEY=...`, leading blanks and `export ` allowed), nothing for a comment / blank / other line.
# /etc/default/grub is sourced by grub-mkconfig, so every such line counts, not only column 0.
_grub_fast_boot_active_key() {
    local l="${1:-}"
    l="${l#"${l%%[![:space:]]*}"}"
    case "$l" in
        export[[:space:]]*)
            l="${l#export}"
            l="${l#"${l%%[![:space:]]*}"}"
            ;;
    esac
    case "$l" in
        [A-Za-z_]*=*) printf '%s' "${l%%=*}" ;;
    esac
}

# grub_fast_boot_is_applied <file> -> rc 0 when each of the three keys has at least one active line
# and every active line of each key is exactly the wanted `KEY=VALUE`; rc 1 otherwise (also for an
# unreadable file).
grub_fast_boot_is_applied() {
    local file="${1:-}" line key wanted k seen=" "
    [ -n "$file" ] && [ -r "$file" ] || return 1
    while IFS= read -r line || [ -n "$line" ]; do
        key="$(_grub_fast_boot_active_key "$line")"
        [ -n "$key" ] || continue
        wanted="$(_grub_fast_boot_wanted "$key")" || continue
        [ "$line" = "$wanted" ] || return 1
        seen="${seen}${key} "
    done < "$file"
    for k in $GRUB_FAST_BOOT_KEYS; do
        case "$seen" in
            *" $k "*) ;;
            *) return 1 ;;
        esac
    done
    return 0
}

# grub_fast_boot_apply <file> -> make <file> (an /etc/default/grub) carry the three settings:
# every active line of a key is rewritten to the wanted value in place, a missing key is appended.
# Every other line is kept as is. An already-correct file is NOT rewritten (byte-identical, same
# mtime). The file is rewritten in place (`>`), so its inode and mode stay. rc 1 + a stderr line
# when <file> is missing or cannot be written.
grub_fast_boot_apply() {
    local file="${1:-}" line key wanted k out="" seen=" "
    if [ -z "$file" ] || [ ! -f "$file" ]; then
        printf 'grub_fast_boot_apply: not a file: %s (#1394)\n' "${file:-<empty>}" >&2
        return 1
    fi
    grub_fast_boot_is_applied "$file" && return 0
    while IFS= read -r line || [ -n "$line" ]; do
        key="$(_grub_fast_boot_active_key "$line")"
        if [ -n "$key" ] && wanted="$(_grub_fast_boot_wanted "$key")"; then
            out="${out}${wanted}"$'\n'
            seen="${seen}${key} "
        else
            out="${out}${line}"$'\n'
        fi
    done < "$file"
    for k in $GRUB_FAST_BOOT_KEYS; do
        case "$seen" in
            *" $k "*) ;;
            *) out="${out}$(_grub_fast_boot_wanted "$k")"$'\n' ;;
        esac
    done
    if ! printf '%s' "$out" > "$file"; then
        printf 'grub_fast_boot_apply: could not write %s (#1394)\n' "$file" >&2
        return 1
    fi
}

# grub_fast_boot_cfg_verdict <grub.cfg text> -> `ok`, or `FAIL: <reasons>` (always rc 0).
# Grades a GENERATED grub.cfg (what the box really boots), not /etc/default/grub:
#   - the recordfail branch (`if [ "${recordfail}" = 1 ] ; then` from Ubuntu's 00_header) must exist
#     and set timeout to GRUB_FAST_BOOT_RECORDFAIL_TIMEOUT -- a missing branch is a FAIL, the box's
#     behaviour after a cut boot cannot be proven;
#   - every other `set timeout=` must be GRUB_FAST_BOOT_TIMEOUT (this also catches the
#     recordfail_broken EFI block and a countdown);
#   - every `set timeout_style=` must be GRUB_FAST_BOOT_STYLE, and there must be at least one.
# An empty (unreadable) cfg is a FAIL. Comment lines are ignored.
grub_fast_boot_cfg_verdict() {
    local cfg="${1:-}"
    if [ -z "$(printf '%s' "$cfg" | tr -d '[:space:]')" ]; then
        printf 'FAIL: grub.cfg is empty or unreadable -- cannot certify the box boots with no GRUB menu or countdown (#1394)\n'
        return 0
    fi
    printf '%s\n' "$cfg" | awk \
        -v want_to="$GRUB_FAST_BOOT_TIMEOUT" \
        -v want_style="$GRUB_FAST_BOOT_STYLE" \
        -v want_rf="$GRUB_FAST_BOOT_RECORDFAIL_TIMEOUT" '
        function value(s) { sub(/^[^=]*=/, "", s); gsub(/"/, "", s); sub(/[[:space:]].*$/, "", s); return s }
        function add(r, s) { return (r == "") ? s : r "; " s }
        { line = $0; sub(/^[[:space:]]+/, "", line) }
        line ~ /^#/ { next }
        index(line, "if [ \"${recordfail}\" = 1 ]") == 1 { inrf = 1; next }
        inrf && (line ~ /^else([[:space:]]|$)/ || line ~ /^fi([[:space:]]|;|$)/) { inrf = 0 }
        line ~ /^set timeout=/ {
            v = value(line)
            if (inrf && !rf_seen) { rf = v; rf_seen = 1 }
            else if (v != want_to) { bad_to = add(bad_to, v) }
            next
        }
        line ~ /^set timeout_style=/ {
            s = value(line)
            if (s == want_style) { hidden = 1 } else { bad_style = add(bad_style, s) }
            next
        }
        END {
            r = ""
            if (!rf_seen) {
                r = add(r, "no recordfail branch (if [ \"${recordfail}\" = 1 ]) found -- the boot after a cut or failed boot cannot be proven menu-free")
            } else if (rf != want_rf) {
                r = add(r, "recordfail timeout is " rf " s (want " want_rf ") -- after a cut boot or a power-off GRUB shows its menu with that countdown")
            }
            if (bad_to != "") {
                r = add(r, "menu timeout " bad_to " s (want " want_to ")")
            }
            if (bad_style != "") {
                r = add(r, "timeout_style " bad_style " (want " want_style ")")
            } else if (!hidden) {
                r = add(r, "no timeout_style=" want_style " -- the menu is not hidden")
            }
            if (r == "") {
                print "ok"
            } else {
                print "FAIL: " r " -- re-run setup-device.sh (or reflash via create-usb-linux.sh) so /etc/default/grub carries the scripts/lib/grub-fast-boot.sh settings (#1394)"
            }
        }'
}
