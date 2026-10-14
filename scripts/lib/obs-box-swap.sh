#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup script owns strict mode (set -euo pipefail)
# obs-box-swap.sh (issue 1428) -- the swap item of the shared OBS-box baseline, sourced by
# scripts/lib/obs-box-baseline.sh (callers source that ONE entry point; split out only because the
# baseline file is at the ~1000-line budget). Graded by the `swap` row of
# scripts/lib/obs-box-baseline-verify.sh. A cambox swaps to zram instead: scripts/lib/cambox-zram.sh.

# obs_box_swap -- issue 1428 (owner 14.10.2026, "To ze tam nieje swap je hrube a opakovane porusenie
# pravidiel pre linux boxy"): every Linux OBS box carries a swapfile the size of its RAM, rounded up to
# whole GiB, so a memory spike degrades the box instead of OOM-killing OBS or the restreamer (stream-pp
# 14.10.2026). Idempotent: an existing swapfile is reused, swapon only when it is not active, ONE fstab
# line. A root that is not ext4 (fallocate + swapfile rules differ) fails loud and writes nothing.
# Seams: OBS_BOX_SWAPFILE (/swapfile), OBS_BOX_FSTAB (/etc/fstab), OBS_BOX_MEMINFO (/proc/meminfo).
# rc 1 + a reason on stderr on any failure; the caller names the step.
obs_box_swap() {
    local sf="${OBS_BOX_SWAPFILE:-/swapfile}" fstab="${OBS_BOX_FSTAB:-/etc/fstab}" mi="${OBS_BOX_MEMINFO:-/proc/meminfo}"
    local fs kb gib
    fs="$(findmnt -no FSTYPE / 2>/dev/null || true)"
    if [ "$fs" != ext4 ]; then
        echo "obs-box-baseline: swap: the root file system is '${fs:-unreadable}', not ext4 -- refusing to create ${sf} (issue 1428)" >&2
        return 1
    fi
    kb="$(awk '/^MemTotal:/{print $2; exit}' "$mi" 2>/dev/null || true)"
    case "$kb" in '' | *[!0-9]*) echo "obs-box-baseline: swap: MemTotal unreadable in ${mi}" >&2; return 1 ;; esac
    gib=$(( (kb + 1048575) / 1048576 ))
    if [ ! -f "$sf" ]; then
        fallocate -l "${gib}G" "$sf" || { echo "obs-box-baseline: swap: fallocate ${gib}G ${sf} failed" >&2; return 1; }
        chmod 600 "$sf" || { echo "obs-box-baseline: swap: chmod 600 ${sf} failed" >&2; return 1; }
        mkswap "$sf" >/dev/null || { echo "obs-box-baseline: swap: mkswap ${sf} failed" >&2; return 1; }
        echo "  created ${sf} (${gib} GiB = RAM, issue 1428)"
    fi
    if ! swapon --show=NAME --noheadings 2>/dev/null | grep -qxF "$sf"; then
        swapon "$sf" || { echo "obs-box-baseline: swap: swapon ${sf} failed" >&2; return 1; }
        echo "  swapon ${sf}"
    fi
    if ! grep -qE "^${sf}[[:space:]]" "$fstab" 2>/dev/null; then
        printf '%s none swap sw 0 0\n' "$sf" >>"$fstab" || { echo "obs-box-baseline: swap: cannot append to ${fstab}" >&2; return 1; }
        echo "  ${fstab}: ${sf} none swap sw 0 0"
    fi
    return 0
}
