#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing verify script owns strict mode; the gather is a read-only fact dump that must never abort mid-way
# obs-box-baseline-verify.sh (issue 1357) -- the ONE grader for the shared OBS-box appliance baseline.
#
# scripts/lib/obs-box-baseline.sh provisions every baseline item on every Linux OBS box; this lib grades
# every one of them, and BOTH acceptance gates call it: verify-imag.sh (runs on dev1, reads the box over
# ssh) and verify-strih.sh (runs on the box). So it is split the way scripts/lib/imag-power-envelope.sh
# is: a GATHER snippet (read-only bash text the caller runs on the box -- locally or over ssh -- that
# prints `key=value` facts) and a PURE verdict over those facts. A box missing ANY item FAILS its verify.
#
#   obs_box_baseline_gather_snippet BOX DESKTOP_USER OBS_UNIT -> the gather bash text
#   obs_box_baseline_gather_cmd     BOX DESKTOP_USER OBS_UNIT -> `bash -c <quoted snippet>` (ssh-ready)
#   obs_box_baseline_verdict        (stdin: the facts)        -> `item|OK|detail` / `item|FAIL|detail`
#                                                                lines, rc 1 when any item FAILs
#
# Items (the obs-box-baseline.sh functions they grade): net (network tuning), perf (governor +
# <BOX>-maxperf persistence), nosleep, boot (boot safety net), kernel (preempt=full low-latency),
# affinity (AFFINITY-ONLY core reservation, no kernel isolcpus/nohz_full), gpu (PRIME nvidia-primary on
# a dGPU box, the iGPU max-frequency pin otherwise), dejitter (oomd + off-hours + OBS ProcessPriority=High
# + the user-unit masks), crash (no operator crash popup), kiosk (lightdm + openbox installed, autologin
# -> openbox, no GNOME),
# autostart (the openbox autostart contract), power (thermald purged + PL1 envelope units), touchpad.
# Fail-closed: a fact the gather could not read grades FAIL ("unreadable"), never a silent pass.

# (the directory is a pure parameter expansion -- no dirname/cd -- like obs-box-baseline.sh's own)
# shellcheck source=scripts/lib/obs-box-baseline.sh
if [ "${BASH_SOURCE[0]%/*}" != "${BASH_SOURCE[0]}" ]; then . "${BASH_SOURCE[0]%/*}/obs-box-baseline.sh"; else . ./obs-box-baseline.sh; fi

# obs_box_crash_popup_unit_ok ENABLED_STATE ACTIVE_STATE -> 0 iff one crash unit is quiet: its
# `systemctl is-enabled` FIRST line is masked / masked-runtime / disabled / not-found / empty (unit file
# absent) AND its `systemctl is-active` first line is inactive / failed. Anything else (enabled, static,
# an unreadable active state, an unknown token) returns 1 -- fail-closed.
obs_box_crash_popup_unit_ok() {
    local en="${1-}" act="${2-}"
    en="${en%%$'\n'*}"
    act="${act%%$'\n'*}"
    case "$en" in
        masked|masked-runtime|disabled|not-found|'') ;;
        *) return 1 ;;
    esac
    case "$act" in
        inactive|failed) return 0 ;;
        *) return 1 ;;
    esac
}

# obs_box_crash_popup_template_ok ENABLED_STATE -> 0 iff a TEMPLATE crash unit (`name@.service`) is
# blocked: its `systemctl is-enabled` first line is masked / masked-runtime, or not-found / empty (the
# package is absent). `static` (the 26.04 default -- pulled by systemd-coredump's OnSuccess=),
# `disabled` (disable does not stop an OnSuccess= pull) or anything else returns 1. The active state of
# a template name is not meaningful, so it is never graded.
obs_box_crash_popup_template_ok() {
    local en="${1-}"
    en="${en%%$'\n'*}"
    case "$en" in
        masked|masked-runtime|not-found|'') return 0 ;;
        *) return 1 ;;
    esac
}

# obs_box_crash_popup_member_ok UNIT ENABLED_STATE ACTIVE_STATE -> grade one obs_box_crash_popup_units
# member: a template (`*@.service`) by obs_box_crash_popup_template_ok (is-enabled only), any other unit
# by obs_box_crash_popup_unit_ok (is-enabled AND is-active).
obs_box_crash_popup_member_ok() {
    local unit="${1-}"
    case "$unit" in
        *@.service) obs_box_crash_popup_template_ok "${2-}" ;;
        *)          obs_box_crash_popup_unit_ok "${2-}" "${3-}" ;;
    esac
}

# obs_box_crash_reports_count [DIR] -> the number of regular `*.crash` files in DIR (default /var/crash).
# update-notifier re-raises the popup at login for reports ALREADY there, so the verify callers REPORT
# them; deleting them is a supervisor data action, never provisioning. A missing/unreadable DIR prints
# 0. Always rc 0 (safe as a bare assignment under set -e).
obs_box_crash_reports_count() {
    local dir="${1:-/var/crash}" n=0 f
    for f in "$dir"/*.crash; do
        if [ -f "$f" ]; then n=$((n + 1)); fi
    done
    printf '%s' "$n"
}

# obs_box_governor_ok GOVERNORS -> 0 iff the space/newline-separated per-core scaling_governor values
# are ALL `performance` and there is at least one (an empty/unreadable read is a FAIL, never a pass).
obs_box_governor_ok() {
    local g seen=0
    for g in ${1-}; do
        seen=1
        [ "$g" = performance ] || return 1
    done
    [ "$seen" = 1 ]
}

# obs_box_cmdline_has_word CMDLINE WORD -> 0 iff WORD is a whole whitespace-separated token of CMDLINE
# (`preempt=full` must never match `preempt=full_debug`, `isolcpus` matches `isolcpus=...`).
obs_box_cmdline_has_word() {
    local tok
    for tok in ${1-}; do
        case "$tok" in
            "$2"|"$2="*) return 0 ;;
        esac
    done
    return 1
}

# obs_box_holds_generic_kernel HOLDS -> 0 iff the space-separated `apt-mark showhold` list HOLDS carries
# at least one generic kernel package (linux-image-* / linux-headers-* / linux-generic-hwe-*) -- the
# boot-safety-net pin. An empty list (nothing held, or an unreadable read) returns 1.
obs_box_holds_generic_kernel() {
    local h
    for h in ${1-}; do
        case "$h" in
            linux-image-*|linux-headers-*|linux-generic-hwe-*) return 0 ;;
        esac
    done
    return 1
}

# obs_box_baseline_gather_snippet BOX DESKTOP_USER OBS_UNIT -> read-only bash text that, run ON the box
# as any user (no root needed), prints one `key=value` fact per line for obs_box_baseline_verdict. The
# detectors it needs (obs_box_has_discrete_nvidia, obs_box_crash_popup_units) are embedded from THIS lib
# via `declare -f` -- one source of truth on the provisioning AND the grading side. Every read is `|| true`
# guarded: a missing tool/file yields an empty value, which the verdict grades FAIL (unreadable).
obs_box_baseline_gather_snippet() {
    local box="${1-}" user="${2-}" unit="${3-}"
    if [ -z "$box" ] || [ -z "$user" ] || [ -z "$unit" ]; then
        echo "obs_box_baseline_gather_snippet: BOX DESKTOP_USER OBS_UNIT required" >&2
        return 1
    fi
    printf 'BOX=%q\nU=%q\nUNIT=%q\n' "$box" "$user" "$unit"
    declare -f obs_box_has_discrete_nvidia obs_box_crash_popup_units obs_box_dejitter_user_units
    cat <<'GATHER_EOF'
set +e
HOMEDIR="$(getent passwd "$U" 2>/dev/null | cut -d: -f6)"; [ -n "$HOMEDIR" ] || HOMEDIR="/home/$U"
fexists() { if [ -e "$1" ]; then echo 1; else echo 0; fi; }
first() { head -n1 2>/dev/null | tr -d '\r'; }
pkg() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null; }
echo "sysctl_conf=$(fexists /etc/sysctl.d/99-network-performance.conf)"
echo "rmem_max=$(sysctl -n net.core.rmem_max 2>/dev/null | first)"
echo "nic_hook=$(if [ -x /etc/networkd-dispatcher/routable.d/optimize-nic ]; then echo 1; else echo 0; fi)"
echo "cpu_perf_unit=$(systemctl is-enabled cpu-performance.service 2>/dev/null | first)"
echo "rc_local_eee=$(if [ -x /etc/rc.local ] && grep -qF 'ethtool --set-eee' /etc/rc.local 2>/dev/null; then echo 1; else echo 0; fi)"
echo "rc_local_active=$(systemctl is-active rc-local.service 2>/dev/null | first)"
echo "governors=$(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | sort -u | tr '\n' ' ' | sed 's/ $//')"
echo "maxperf_active=$(systemctl is-active "${BOX}-maxperf.service" 2>/dev/null | first)"
echo "maxperf_udev=$(fexists "/etc/udev/rules.d/99-${BOX}-maxperf-pm.rules")"
echo "sleep_target=$(systemctl is-enabled sleep.target 2>/dev/null | first)"
echo "logind_nosleep=$(fexists "/etc/systemd/logind.conf.d/99-${BOX}-no-sleep.conf")"
echo "logind_powerkey=$(fexists /etc/systemd/logind.conf.d/99-production-no-powerkey.conf)"
echo "kernel_lockdown=$(fexists "/etc/apt/apt.conf.d/51${BOX}-kernel-lockdown")"
echo "initrd_hook=$(if [ -x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee ]; then echo 1; else echo 0; fi)"
echo "holds=$(apt-mark showhold 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"
echo "cmdline=$( { first < /proc/cmdline; } 2>/dev/null)"
echo "lowlatency_cfg=$(if grep -qw 'preempt=full' /etc/default/grub.d/99-lowlatency.cfg 2>/dev/null; then echo 1; else echo 0; fi)"
echo "isolated_cpus=$( { first < "/etc/${BOX}-isolated-cpus.conf"; } 2>/dev/null)"
echo "dgpu=$(if lspci -nn 2>/dev/null | obs_box_has_discrete_nvidia; then echo 1; else echo 0; fi)"
echo "prime=$(prime-select query 2>/dev/null | first)"
echo "igpu_unit=$(systemctl is-enabled "${BOX}-igpu-maxperf.service" 2>/dev/null | first)"
echo "oomd=$(systemctl is-enabled systemd-oomd.service 2>/dev/null | first)"
echo "offhours=$(fexists "/etc/systemd/system/apt-daily-upgrade.timer.d/${BOX}-offhours.conf")"
echo "process_priority=$(if grep -qx 'ProcessPriority=High' "${HOMEDIR}/.config/obs-studio/global.ini" 2>/dev/null; then echo 1; else echo 0; fi)"
_um=0; _ut=0
while IFS= read -r _uu; do
    [ -n "$_uu" ] || continue
    _ut=$((_ut + 1))
    if [ "$(readlink "${HOMEDIR}/.config/systemd/user/${_uu}" 2>/dev/null)" = /dev/null ]; then _um=$((_um + 1)); fi
done < <(obs_box_dejitter_user_units)
echo "user_masked=${_um}/${_ut}"
while IFS= read -r _cu; do
    [ -n "$_cu" ] || continue
    echo "crash_unit=${_cu}|$(systemctl is-enabled "$_cu" 2>/dev/null | first)|$(systemctl is-active "$_cu" 2>/dev/null | first)"
done < <(obs_box_crash_popup_units)
echo "coredump=$(pkg systemd-coredump)"
echo "lightdm=$(pkg lightdm)"
echo "openbox=$(pkg openbox)"
echo "dm=$(readlink -f /etc/systemd/system/display-manager.service 2>/dev/null)"
echo "dm_lightdm=$(readlink -f /lib/systemd/system/lightdm.service 2>/dev/null)"
_al="/etc/lightdm/lightdm.conf.d/50-${BOX}-autologin.conf"
echo "autologin=$(if grep -qxF "autologin-user=${U}" "$_al" 2>/dev/null && grep -qxF 'autologin-session=openbox' "$_al" 2>/dev/null; then echo 1; else echo 0; fi)"
echo "gdm3=$(pkg gdm3)"
echo "gnome_shell=$(pkg gnome-shell)"
_as="${HOMEDIR}/.config/openbox/autostart"
echo "autostart_exec=$(if [ -x "$_as" ]; then echo 1; else echo 0; fi)"
echo "autostart_xset=$(if grep -qF 'xset s off -dpms s noblank' "$_as" 2>/dev/null; then echo 1; else echo 0; fi)"
echo "autostart_sentinel=$(if grep -qF '.config/obs-studio/.sentinel' "$_as" 2>/dev/null; then echo 1; else echo 0; fi)"
echo "autostart_unit=$(if grep -qF "systemctl --user start ${UNIT}" "$_as" 2>/dev/null; then echo 1; else echo 0; fi)"
echo "thermald=$(pkg thermald)"
echo "pe_unit=$(systemctl is-enabled imag-power-envelope.service 2>/dev/null | first)"
echo "pe_guard=$(systemctl is-enabled imag-power-envelope-guard.timer 2>/dev/null | first)"
echo "touchpad=$(if grep -qF 'Option "Tapping" "on"' /etc/X11/xorg.conf.d/30-touchpad-tap.conf 2>/dev/null; then echo 1; else echo 0; fi)"
echo "gather_done=1"
GATHER_EOF
}

# obs_box_baseline_gather_cmd BOX DESKTOP_USER OBS_UNIT -> `bash -c <%q-quoted snippet>`: one command
# string a caller hands to ssh (verify-imag.sh) or runs locally (verify-strih.sh), so both run the
# identical gather under bash regardless of the remote login shell.
obs_box_baseline_gather_cmd() {
    local snippet
    snippet="$(obs_box_baseline_gather_snippet "$@")" || return 1
    printf 'bash -c %q\n' "$snippet"
}

# _obs_box_fact KEY FACTS -> the value of the LAST `KEY=` line in FACTS (empty when absent).
_obs_box_fact() {
    local key="$1" line val=""
    while IFS= read -r line; do
        case "$line" in "${key}="*) val="${line#"${key}="}" ;; esac
    done <<<"${2-}"
    printf '%s' "$val"
}

# obs_box_baseline_verdict (stdin: the gather facts) -> one `item|OK|detail` or `item|FAIL|detail` line
# per baseline item, in provisioning order; rc 0 iff every item is OK. A gather that never completed
# (no `gather_done=1`: ssh died, snippet aborted) FAILS every item as unreadable -- never a pass.
obs_box_baseline_verdict() {
    local facts fails=0 v
    facts="$(cat)"
    _obs_box_item() {  # _obs_box_item NAME OK(0|1) DETAIL
        if [ "$2" = 1 ]; then printf '%s|OK|%s\n' "$1" "$3"; else printf '%s|FAIL|%s\n' "$1" "$3"; fails=$((fails + 1)); fi
    }
    _obs_box_f() { _obs_box_fact "$1" "$facts"; }
    if [ "$(_obs_box_f gather_done)" != 1 ]; then
        for v in net perf nosleep boot kernel affinity gpu dejitter crash kiosk autostart power touchpad; do
            _obs_box_item "$v" 0 "unreadable (the baseline gather did not complete)"
        done
        return 1
    fi
    local ok
    # net -- sysctl drop-in present AND live, EEE/flow-control dispatcher hook installed
    ok=0; [ "$(_obs_box_f sysctl_conf)" = 1 ] && [ "$(_obs_box_f rmem_max)" = 134217728 ] && [ "$(_obs_box_f nic_hook)" = 1 ] && ok=1
    _obs_box_item net "$ok" "sysctl_conf=$(_obs_box_f sysctl_conf) rmem_max=$(_obs_box_f rmem_max) nic_hook=$(_obs_box_f nic_hook)"
    # perf -- cpu-performance.service enabled, every core performance, the rc.local boot hook (NIC EEE off),
    # maxperf unit active + udev rule. Whether rc-local.service ran it this boot is REPORTED (a release
    # without the rc-local generator never runs it; the networkd-dispatcher hook still covers EEE).
    ok=0; [ "$(_obs_box_f cpu_perf_unit)" = enabled ] && obs_box_governor_ok "$(_obs_box_f governors)" \
        && [ "$(_obs_box_f rc_local_eee)" = 1 ] \
        && [ "$(_obs_box_f maxperf_active)" = active ] && [ "$(_obs_box_f maxperf_udev)" = 1 ] && ok=1
    _obs_box_item perf "$ok" "cpu-performance=$(_obs_box_f cpu_perf_unit) governors=[$(_obs_box_f governors)] rc.local-eee=$(_obs_box_f rc_local_eee) rc-local.service=$(_obs_box_f rc_local_active) maxperf=$(_obs_box_f maxperf_active) udev=$(_obs_box_f maxperf_udev)"
    # nosleep -- sleep.target masked + both logind drop-ins
    ok=0; [ "$(_obs_box_f sleep_target)" = masked ] && [ "$(_obs_box_f logind_nosleep)" = 1 ] && [ "$(_obs_box_f logind_powerkey)" = 1 ] && ok=1
    _obs_box_item nosleep "$ok" "sleep.target=$(_obs_box_f sleep_target) no-sleep.conf=$(_obs_box_f logind_nosleep) no-powerkey.conf=$(_obs_box_f logind_powerkey)"
    # boot -- kernel lockdown + initrd-guarantee hook + at least one generic kernel package apt-held
    ok=0; [ "$(_obs_box_f kernel_lockdown)" = 1 ] && [ "$(_obs_box_f initrd_hook)" = 1 ] \
        && obs_box_holds_generic_kernel "$(_obs_box_f holds)" && ok=1
    _obs_box_item boot "$ok" "kernel-lockdown=$(_obs_box_f kernel_lockdown) initrd-hook=$(_obs_box_f initrd_hook) kernel-held=$(obs_box_holds_generic_kernel "$(_obs_box_f holds)" && echo yes || echo no)"
    # kernel -- preempt=full on the RUNNING cmdline + the lowlatency config drop-in + its package held
    ok=0; obs_box_cmdline_has_word "$(_obs_box_f cmdline)" preempt=full && [ "$(_obs_box_f lowlatency_cfg)" = 1 ] \
        && obs_box_cmdline_has_word "$(_obs_box_f holds)" lowlatency-kernel && ok=1
    _obs_box_item kernel "$ok" "preempt=full running=$(obs_box_cmdline_has_word "$(_obs_box_f cmdline)" preempt=full && echo yes || echo no) lowlatency.cfg=$(_obs_box_f lowlatency_cfg) lowlatency-kernel held=$(obs_box_cmdline_has_word "$(_obs_box_f holds)" lowlatency-kernel && echo yes || echo no)"
    # affinity -- a persisted OBS cpulist AND no kernel isolation (#842: isolcpus/nohz_full is the regression)
    ok=0; [[ "$(_obs_box_f isolated_cpus)" =~ ^[0-9][0-9,-]*$ ]] && ! obs_box_cmdline_has_word "$(_obs_box_f cmdline)" isolcpus \
        && ! obs_box_cmdline_has_word "$(_obs_box_f cmdline)" nohz_full && ok=1
    _obs_box_item affinity "$ok" "isolated-cpus=[$(_obs_box_f isolated_cpus)] kernel-isolation=$( { obs_box_cmdline_has_word "$(_obs_box_f cmdline)" isolcpus || obs_box_cmdline_has_word "$(_obs_box_f cmdline)" nohz_full; } && echo PRESENT || echo none)"
    # gpu -- a dGPU box renders NVIDIA-primary (PRIME); an iGPU-only box has nothing to select
    if [ "$(_obs_box_f dgpu)" = 1 ]; then
        ok=0; [ "$(_obs_box_f prime)" = nvidia ] && ok=1
        _obs_box_item gpu "$ok" "discrete NVIDIA present, prime-select=$(_obs_box_f prime)"
    else
        ok=0; [ "$(_obs_box_f igpu_unit)" = enabled ] && ok=1
        _obs_box_item gpu "$ok" "no discrete NVIDIA GPU: the iGPU max-frequency pin unit=$(_obs_box_f igpu_unit)"
    fi
    # dejitter -- systemd-oomd masked, the apt-daily-upgrade off-hours pin, OBS ProcessPriority=High and
    # every obs_box_dejitter_user_units member masked for the desktop user
    local um
    um="$(_obs_box_f user_masked)"
    ok=0; [ "$(_obs_box_f oomd)" = masked ] && [ "$(_obs_box_f offhours)" = 1 ] && [ "$(_obs_box_f process_priority)" = 1 ] \
        && [[ "$um" =~ ^([1-9][0-9]*)/([0-9]+)$ ]] && [ "${BASH_REMATCH[1]}" = "${BASH_REMATCH[2]}" ] && ok=1
    _obs_box_item dejitter "$ok" "systemd-oomd=$(_obs_box_f oomd) apt-daily off-hours=$(_obs_box_f offhours) ProcessPriority=High:$(_obs_box_f process_priority) user units masked=${um:-?}"
    # crash -- every crash-popup unit quiet + systemd-coredump installed
    local line unit en act bad="" seen=0
    while IFS= read -r line; do
        case "$line" in crash_unit=*) ;; *) continue ;; esac
        seen=$((seen + 1))
        line="${line#crash_unit=}"
        IFS='|' read -r unit en act <<<"$line"
        obs_box_crash_popup_member_ok "$unit" "$en" "$act" || bad="${bad}${bad:+ }${unit}(${en:-?}/${act:-?})"
    done <<<"$facts"
    ok=0; [ "$seen" -ge 3 ] && [ -z "$bad" ] && [ "$(_obs_box_f coredump)" = "install ok installed" ] && ok=1
    _obs_box_item crash "$ok" "live crash units: ${bad:-none} (graded ${seen}); systemd-coredump='$(_obs_box_f coredump)'"
    # kiosk -- lightdm + openbox installed, DM is lightdm, autologin -> openbox for the desktop user,
    # no gdm3 / gnome-shell
    ok=0; [ "$(_obs_box_f lightdm)" = "install ok installed" ] && [ "$(_obs_box_f openbox)" = "install ok installed" ] \
        && [ -n "$(_obs_box_f dm)" ] && [ "$(_obs_box_f dm)" = "$(_obs_box_f dm_lightdm)" ] && [ "$(_obs_box_f autologin)" = 1 ] \
        && [ "$(_obs_box_f gdm3)" != "install ok installed" ] && [ "$(_obs_box_f gnome_shell)" != "install ok installed" ] && ok=1
    _obs_box_item kiosk "$ok" "lightdm='$(_obs_box_f lightdm)' openbox='$(_obs_box_f openbox)' display-manager=$(_obs_box_f dm) autologin->openbox=$(_obs_box_f autologin) gdm3='$(_obs_box_f gdm3)' gnome-shell='$(_obs_box_f gnome_shell)'"
    # autostart -- the openbox autostart contract: executable, never-blank, sentinel clear, starts the OBS unit
    ok=0; [ "$(_obs_box_f autostart_exec)" = 1 ] && [ "$(_obs_box_f autostart_xset)" = 1 ] && [ "$(_obs_box_f autostart_sentinel)" = 1 ] \
        && [ "$(_obs_box_f autostart_unit)" = 1 ] && ok=1
    _obs_box_item autostart "$ok" "exec=$(_obs_box_f autostart_exec) xset-noblank=$(_obs_box_f autostart_xset) sentinel-clear=$(_obs_box_f autostart_sentinel) starts-obs-unit=$(_obs_box_f autostart_unit)"
    # power -- thermald purged + the PL1 envelope oneshot + its guard timer enabled
    ok=0; [ "$(_obs_box_f thermald)" != "install ok installed" ] && [ "$(_obs_box_f pe_unit)" = enabled ] && [ "$(_obs_box_f pe_guard)" = enabled ] && ok=1
    _obs_box_item power "$ok" "thermald='$(_obs_box_f thermald)' imag-power-envelope.service=$(_obs_box_f pe_unit) guard.timer=$(_obs_box_f pe_guard)"
    # touchpad -- the libinput tap-to-click InputClass
    ok=0; [ "$(_obs_box_f touchpad)" = 1 ] && ok=1
    _obs_box_item touchpad "$ok" "30-touchpad-tap.conf tapping=$(_obs_box_f touchpad)"
    [ "$fails" -eq 0 ]
}
