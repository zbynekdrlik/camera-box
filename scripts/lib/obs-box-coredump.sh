#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup/verify script owns strict mode (set -euo pipefail)
# obs-box-coredump.sh (issue 1441) -- the coredump bound of the shared OBS-box baseline, sourced by
# scripts/lib/obs-box-baseline.sh (callers source that ONE entry point; split out only because the
# baseline file is at its line budget). Graded by the `coredump` row of
# scripts/lib/obs-box-baseline-verify.sh.
#
# stream-pp, 18.11.2026 17:16: OBS aborted (SIGABRT after `amdgpu: Not enough memory for command
# submission`) and systemd-coredump -- installed on every OBS box by obs_box_crash_popup_off -- read,
# wrote and compressed the multi-GB core under its compiled defaults (ProcessSizeMax=32G,
# ExternalSizeMax=32G). The notebook froze for minutes (no ssh, no MCP, the operator's mouse stalled).
#
# systemd's own source (v255 + v258 src/coredump/coredump.c, save_external_coredump) streams at most
# MIN(RLIMIT_CORE, MAX(ProcessSizeMax, <Storage>SizeMax)) bytes to /var/lib/systemd/coredump, logs
# `Core file was truncated to N bytes` beyond it and exits, which closes the kernel's pipe and ends the
# dump. So capping BOTH sizes bounds the whole cost of a crash. The journal still gets the crash record
# (`Process N (obs) ... dumped core.`, signal, exe, cmdline); the stack trace needs the core on disk, so
# an OBS core bigger than the cap gets no full stack (coredump.conf(5)). Storage stays the default
# `external`: every small process (Xorg, dsync, the hub, the session apps) keeps a full core + stack in
# coredumpctl, and so does an imag OBS core of the issue-882 size (88 MB live, 30.7.2026: imag's unit sets
# LimitCORE=infinity on purpose). systemd-coredump parses coredump.conf.d at every invocation (one per crash), so the
# drop-in needs no reload and no restart.

# obs_box_coredump_size_max -> the ONE bound for ProcessSizeMax and ExternalSizeMax: above a helper
# core (tens of MB; an Xorg core is ~100-200 MB), far below an OBS core (GBs), and small enough that
# the write cannot freeze a notebook under memory pressure.
obs_box_coredump_size_max() { printf '256M\n'; }

# obs_box_coredump_dropin_path -> where the drop-in lives (the grader reads the same path). 90-: sorted
# after any vendor/distro drop-in, so it wins over them.
obs_box_coredump_dropin_path() { printf '%s' /etc/systemd/coredump.conf.d/90-obs-box-coredump.conf; }

# obs_box_coredump_dropin_text -> the drop-in.
obs_box_coredump_dropin_text() {
    local max
    max="$(obs_box_coredump_size_max)" || return 1
    cat <<EOF
# camera-box shared OBS-box baseline (issue 1441): a crashing OBS must never freeze its box. Cap the
# core systemd-coredump reads and writes; small cores keep their stack trace (Storage stays external).
[Coredump]
ProcessSizeMax=${max}
ExternalSizeMax=${max}
EOF
}

# obs_box_coredump_effective (stdin: `systemd-analyze cat-config systemd/coredump.conf`) ->
# `<Storage>,<ProcessSizeMax>,<ExternalSizeMax>` as systemd will use them: the LAST assignment of each
# key inside a [Coredump] section (cat-config prints the main file, then every drop-in in parse order).
# An unset Storage is `external` (the compiled default), an unset size `unset`. Input without a
# [Coredump] section (empty: cat-config failed) prints nothing, so an unreadable config never reads as
# the bound. Pure, always rc 0.
obs_box_coredump_effective() {
    local line key val sect="" seen=0 storage=external psm=unset esm=unset
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        case "$line" in
            '' | '#'* | ';'*) continue ;;
            '['*']') sect="$line"; [ "$sect" != "[Coredump]" ] || seen=1; continue ;;
            *=*) ;;
            *) continue ;;
        esac
        [ "$sect" = "[Coredump]" ] || continue
        key="${line%%=*}"; key="${key%"${key##*[![:space:]]}"}"
        val="${line#*=}"; val="${val#"${val%%[![:space:]]*}"}"
        case "$key" in
            Storage) storage="$val" ;;
            ProcessSizeMax) psm="$val" ;;
            ExternalSizeMax) esm="$val" ;;
        esac
    done
    [ "$seen" = 1 ] || return 0
    printf '%s,%s,%s\n' "$storage" "$psm" "$esm"
}

# obs_box_coredump_bound [DEST [OWNER]] -- the item: the drop-in (0644) through
# obs_box_write_if_changed_rendered. No daemon reload or restart (read per crash). The arguments are
# test seams; setup passes none. rc 1 + a reason on stderr on any failure; the caller names the step.
# shellcheck disable=SC2120  # the destination args are passed only by the tests (temp paths)
obs_box_coredump_bound() {
    local dest="${1:-$(obs_box_coredump_dropin_path)}" owner="${2:-root:root}"
    mkdir -p "${dest%/*}" 2>/dev/null \
        || { echo "obs-box-baseline: coredump (issue 1441): could not create ${dest%/*}" >&2; return 1; }
    obs_box_write_if_changed_rendered "$dest" 0644 "$owner" "coredump bound (issue 1441)" obs_box_coredump_dropin_text \
        || { echo "obs-box-baseline: coredump (issue 1441): the drop-in ${dest} could not be installed (above)" >&2; return 1; }
}
