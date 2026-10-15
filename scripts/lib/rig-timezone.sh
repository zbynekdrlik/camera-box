#!/usr/bin/env bash
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell
# (one constant + functions, no top-level side effects; every caller owns its own strictness,
# the standing .claude/rules/ci-testing-gotchas.md rule).
#
# scripts/lib/rig-timezone.sh -- issue 1415: the ONE declaration of the rig time zone.
#
# Every cambox and Linux OBS box (strih, stream, imag) runs local time, Europe/Bratislava (both venues
# are in Slovakia). Before this lib no provisioning flow set a zone, so every freshly provisioned
# cambox, strih and stream box came up on the image default Etc/UTC: the operator's browser-source
# clocks and countdowns ran 2 h behind (owner 12.10.2026: "rovnaku chybu si mal vzdy"). The handheld
# SBC flow (bkshading-provision-sbc.sh) shows no operator clock and does not set it.
#
# A RUNNING process mostly keeps the zone it loaded (glibc's localtime_r, and so Python's time module,
# read it once; OBS's CEF browser sources cache it), so a box whose zone changed needs its services
# restarted or a reboot before every clock agrees. obs_box_timezone says so when it changed the zone.
#
# The writer writes the /etc/localtime symlink + /etc/timezone DIRECTLY, never `timedatectl
# set-timezone`: on a read-only-root cambox, systemd-timedated keeps its own mount namespace from the
# time / was ro, so set-timezone fails with "Read-only file system" even inside a correctly opened rw
# window, and afterwards `timedatectl show -p Timezone` reports the new zone while /etc/localtime
# still points at UTC (live 12.10.2026). So every reader here reads `readlink /etc/localtime`.
#
#   RIG_TIMEZONE                 the zone (default Europe/Bratislava; an env override for a box
#                                outside Slovakia)
#   rig_timezone_zoneinfo [ROOT] the zoneinfo file the zone needs, under ROOT
#   rig_timezone_apply [ROOT]    write ROOT/etc/localtime -> /usr/share/zoneinfo/<zone> + ROOT/etc/timezone;
#                                ROOT is empty on a live box, the image root for create-usb-linux.sh
#   rig_timezone_read [ROOT]     print the zone ROOT/etc/localtime points at (empty when it is no
#                                symlink; the raw link target when it is not a zoneinfo path)
#   rig_timezone_gather_cmd      self-contained bash text (rig_timezone_read embedded via declare -f)
#                                that prints the box's zone, for an ssh gather
#   rig_timezone_verdict ZONE    `OK` (rc 0) or `FAIL: <why>` (rc 1)
#
# Consumers: setup-device.sh STEP 1 + create-usb-linux.sh (camboxes), obs_box_timezone in the shared
# OBS-box baseline (setup-strih.sh / setup-imag.sh step 1), verify-device.sh (as), and the baseline
# grader's `timezone` row (verify-strih item 32, verify-imag (bb)).

RIG_TIMEZONE="${RIG_TIMEZONE:-Europe/Bratislava}"

# _rig_timezone_name_ok ZONE -> rc 0 iff ZONE is a plain zoneinfo name: not empty, not absolute, no
# empty component, only the characters zoneinfo names use. `.` is not in the set, so a `..` component
# can never pass. The set is spelled out, never a bracket range (a range follows the locale, the
# issue-1302 rule).
_rig_timezone_name_ok() {
    local z="${1-}"
    case "$z" in
        ''|/*|*/|*//*) return 1 ;;
        *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/_+-]*) return 1 ;;
    esac
    return 0
}

# rig_timezone_zoneinfo [ROOT] -> the zoneinfo file of RIG_TIMEZONE under ROOT.
rig_timezone_zoneinfo() {
    printf '%s/usr/share/zoneinfo/%s\n' "${1:-}" "$RIG_TIMEZONE"
}

# rig_timezone_apply [ROOT] -- point ROOT/etc/localtime at /usr/share/zoneinfo/<RIG_TIMEZONE> (an
# absolute link, so it is right inside an image root too) and write ROOT/etc/timezone, then read the
# link back. Fails loud (rc 1, nothing written) on a bad zone name or a missing zoneinfo file
# (install tzdata first). Idempotent: a box already on the zone is left as it is. Every write is
# chained with `&&`, because a caller that runs it as `rig_timezone_apply || fail` runs the whole body
# with errexit off.
rig_timezone_apply() {
    local root="${1:-}" zone="$RIG_TIMEZONE" target link
    if ! _rig_timezone_name_ok "$zone"; then
        echo "rig_timezone_apply: '${zone}' is not a zoneinfo name (issue 1415) -- nothing written" >&2
        return 1
    fi
    target="/usr/share/zoneinfo/${zone}"
    if [ ! -f "${root}${target}" ]; then
        echo "rig_timezone_apply: ${root}${target} is missing (install tzdata) -- nothing written (issue 1415)" >&2
        return 1
    fi
    link="$(readlink "${root}/etc/localtime" 2>/dev/null || true)"
    if [ "$link" = "$target" ] && [ "$(cat "${root}/etc/timezone" 2>/dev/null || true)" = "$zone" ]; then
        echo "  issue 1415: time zone already ${zone} (/etc/localtime -> ${target})"
        return 0
    fi
    # Each file is built beside its final name and renamed over it (tzdata's own localtime.dpkg-new
    # shape): a reader on a live box never sees a missing /etc/localtime, which glibc reads as UTC.
    # A directory where the link goes makes `mv -T` refuse, and the step fails by name.
    rm -f "${root}/etc/localtime.rig-new" "${root}/etc/timezone.rig-new" 2>/dev/null
    if ! { mkdir -p "${root}/etc" \
        && ln -sfn "$target" "${root}/etc/localtime.rig-new" \
        && mv -Tf "${root}/etc/localtime.rig-new" "${root}/etc/localtime" \
        && printf '%s\n' "$zone" > "${root}/etc/timezone.rig-new" \
        && mv -Tf "${root}/etc/timezone.rig-new" "${root}/etc/timezone"; }; then
        rm -f "${root}/etc/localtime.rig-new" "${root}/etc/timezone.rig-new" 2>/dev/null
        echo "rig_timezone_apply: could not write ${root}/etc/localtime or ${root}/etc/timezone (issue 1415)" >&2
        return 1
    fi
    link="$(readlink "${root}/etc/localtime" 2>/dev/null || true)"
    if [ "$link" != "$target" ]; then
        echo "rig_timezone_apply: ${root}/etc/localtime reads '${link}' after the write, want '${target}' (issue 1415)" >&2
        return 1
    fi
    echo "  issue 1415: time zone set to ${zone} (/etc/localtime -> ${target}, /etc/timezone)"
}

# rig_timezone_read [ROOT] -> the zone ROOT/etc/localtime points at: the part after `zoneinfo/` of an
# absolute or relative zoneinfo link, the raw link target for any other link, empty when it is not a
# symlink or missing. Always rc 0 (safe as a bare assignment under set -e). Never timedatectl.
rig_timezone_read() {
    local l
    l="$(readlink "${1:-}/etc/localtime" 2>/dev/null)" || l=""
    case "$l" in
        */zoneinfo/*) printf '%s\n' "${l#*/zoneinfo/}" ;;
        *) printf '%s\n' "$l" ;;
    esac
    return 0
}

# rig_timezone_gather_cmd -> bash text, self-contained (no lib on the box), that prints the box's zone.
rig_timezone_gather_cmd() {
    declare -f rig_timezone_read
    printf 'rig_timezone_read;\n'
}

# rig_timezone_verdict ZONE -> `OK` (rc 0) when ZONE is RIG_TIMEZONE, else one `FAIL: ...` line (rc 1).
rig_timezone_verdict() {
    local z="${1-}"
    if [ "$z" = "$RIG_TIMEZONE" ]; then
        printf 'OK\n'
        return 0
    fi
    if [ -z "$z" ]; then
        printf 'FAIL: /etc/localtime is no zoneinfo symlink (unreadable, or a copied file) -- want %s (issue 1415)\n' "$RIG_TIMEZONE"
    else
        printf "FAIL: the box runs time zone '%s', want %s (issue 1415)\n" "$z" "$RIG_TIMEZONE"
    fi
    return 1
}
