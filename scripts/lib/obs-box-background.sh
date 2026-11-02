#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup/verify script owns strict mode and fail()
# obs-box-background.sh (issue 1387) -- background work on a Linux OBS box stays off the OBS cores.
#
# Owner ruling 2.11.2026: OBS on a broadcast box must be immune to background load; backups are not
# disabled or time-boxed. On strih-snv the fleet backup (restic under fbagent@main, nice 19, ionice idle,
# no CPU fence) raised program-render-audit lagged from 0 to 57-213 per 10 min and the on-box recording
# decode did the same. nice only lowers a weight; it never keeps a thread off a core or off an SMT sibling.
#
# The fence is one systemd slice, rig-background.slice, on the CPUs OBS does not use: the complement of
# the OBS mask /etc/<BOX>-isolated-cpus.conf (obs_box_cpu_affinity), never a literal. Members:
#   - fbagent@.service (the fleet-backup project's unit) through a drop-in only;
#   - the on-box decode through /usr/local/sbin/obs-box-background-run, a root helper the desktop user
#     may run by a narrow sudoers line; it starts a transient scope in the slice and drops back to the
#     caller's uid, so the process stays in the ssh session (exit status, stdout, pkill all unchanged).
#
# Pure, sourceable under `set -euo pipefail`, no root: obs_box_cpulist_expand, obs_box_cpulist_compress,
# obs_box_background_cpus, obs_box_background_smt_shared, the *_text renderers, obs_box_background_grade.
# Needs root and the caller's fail(): obs_box_background_slice BOX DESKTOP_USER. It never stops or
# restarts OBS or the backup. Full story: .claude/rules/obs-box-baseline.md.

obs_box_background_slice_name() { printf 'rig-background.slice\n'; }
obs_box_background_slice_path() { printf '/etc/systemd/system/rig-background.slice\n'; }
obs_box_background_dropin_path() { printf '/etc/systemd/system/fbagent@.service.d/50-rig-background.conf\n'; }
obs_box_background_run_path() { printf '/usr/local/sbin/obs-box-background-run\n'; }
obs_box_background_sudoers_path() { printf '/etc/sudoers.d/obs-box-background\n'; }

# The memory.high share of the box's RAM. A soft bound: above it the kernel reclaims and throttles only
# this cgroup, it never OOM-kills, so a full backup's page cache cannot evict OBS and the backup still
# finishes. 30 % = 4.2 GB on the 14 GB strih-snv: the decode's two workers plus a restic index fit.
obs_box_background_memory_high() { printf '30%%\n'; }

# obs_box_cpulist_expand LIST -> the sorted unique CPU numbers, space separated. rc 1 on an empty or
# malformed kernel cpulist (never a partial answer).
obs_box_cpulist_expand() {
    local list="${1-}" item a b i
    local -a items=() cpu_set=()
    case "$list" in '' | *[!0123456789,-]*) return 1 ;; esac
    IFS=, read -ra items <<<"$list"
    for item in "${items[@]}"; do
        case "$item" in
            '' | -* | *- | *-*-*) return 1 ;;
            *-*) a="${item%-*}"; b="${item#*-}" ;;
            *) a="$item"; b="$item" ;;
        esac
        a=$((10#$a)); b=$((10#$b))
        [ "$a" -le "$b" ] || return 1
        for ((i = a; i <= b; i++)); do cpu_set[i]=1; done
    done
    printf '%s\n' "${!cpu_set[*]}"
}

# obs_box_cpulist_compress "0 1 12 13" -> 0-1,12-13 (the numbers must be sorted, as expand prints them).
obs_box_cpulist_compress() {
    local n start="" prev="" out=""
    for n in ${1-}; do
        if [ -z "$start" ]; then start="$n"
        elif [ "$n" -ne "$((prev + 1))" ]; then
            if [ "$start" = "$prev" ]; then out="${out}${out:+,}${start}"; else out="${out}${out:+,}${start}-${prev}"; fi
            start="$n"
        fi
        prev="$n"
    done
    [ -n "$start" ] || return 0
    if [ "$start" = "$prev" ]; then out="${out}${out:+,}${start}"; else out="${out}${out:+,}${start}-${prev}"; fi
    printf '%s\n' "$out"
}

# obs_box_background_cpus ONLINE OBS -> the CPUs OBS does not use, compressed. rc 1 when either list is
# unreadable or nothing is left: a box with no free CPU has no fence to build, never a guessed one.
obs_box_background_cpus() {
    local online obs c out=""
    online="$(obs_box_cpulist_expand "${1-}")" || return 1
    obs="$(obs_box_cpulist_expand "${2-}")" || return 1
    for c in $online; do
        case " $obs " in *" $c "*) ;; *) out="${out}${out:+ }${c}" ;; esac
    done
    [ -n "$out" ] || return 1
    obs_box_cpulist_compress "$out"
}

# obs_box_background_smt_shared BG OBS (stdin: `<cpu> <thread_siblings_list>` lines) -> the background
# CPUs whose SMT sibling is an OBS CPU, space separated (empty = none). Such a pair shares one physical
# core: the fence would hold on paper and the load would still slow the OBS thread beside it. Prints
# `unread` when a sibling list is malformed or no background CPU has one: unread is never "no sharing".
obs_box_background_smt_shared() {
    local bg obs cpu sibs s list out="" read_any=0
    bg="$(obs_box_cpulist_expand "${1-}")" || return 1
    obs="$(obs_box_cpulist_expand "${2-}")" || return 1
    while read -r cpu sibs; do
        [ -n "$cpu" ] || continue
        case " $bg " in *" $cpu "*) ;; *) continue ;; esac
        if ! list="$(obs_box_cpulist_expand "$sibs")"; then
            printf 'unread\n'
            return 0
        fi
        read_any=1
        for s in $list; do
            case " $obs " in
                *" $s "*) out="${out}${out:+ }${cpu}"; break ;;
            esac
        done
    done
    if [ "$read_any" = 0 ]; then out=unread; fi
    printf '%s\n' "$out"
}

# obs_box_background_slice_text CPUS -> the slice unit.
obs_box_background_slice_text() {
    cat <<EOF
# Background work on an OBS box, off the OBS cores (issue 1387). Written by setup (obs_box_background_slice).
[Unit]
Description=Background work off the OBS cores

[Slice]
AllowedCPUs=${1:?obs_box_background_slice_text: CPUS required}
CPUWeight=1
IOWeight=1
MemoryHigh=$(obs_box_background_memory_high)

[Install]
WantedBy=multi-user.target
EOF
}

# obs_box_background_dropin_text -> the fbagent@.service drop-in. Slice= only: the unit is the
# fleet-backup project's, and the slice carries every limit.
obs_box_background_dropin_text() {
    cat <<EOF
# OBS must be immune to the backup (issue 1387). The unit belongs to the fleet-backup project.
[Service]
Slice=$(obs_box_background_slice_name)
EOF
}

# obs_box_background_run_text -> /usr/local/sbin/obs-box-background-run.
obs_box_background_run_text() {
    cat <<'EOF'
#!/bin/bash
# obs-box-background-run COMMAND [ARG...] -- run COMMAND in rig-background.slice as the sudo caller (issue
# 1387). A transient scope: the process stays a child of the caller's session, so its exit status, its
# output and a pkill by the same user behave as without this helper. setpriv drops to the caller's uid,
# gid and own groups inside the scope: `systemd-run --uid --gid` alone keeps root's supplementary group 0.
set -euo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
if [ "$#" -lt 1 ]; then
    echo "usage: obs-box-background-run COMMAND [ARG...]" >&2
    exit 2
fi
if [ "$(id -u)" != 0 ]; then
    echo "obs-box-background-run: run it through sudo" >&2
    exit 2
fi
case "${SUDO_UID:-}:${SUDO_GID:-}" in
    0:* | :* | *: | *[!0123456789:]*)
        echo "obs-box-background-run: no non-root sudo caller (SUDO_UID='${SUDO_UID:-}')" >&2
        exit 2
        ;;
esac
HOME_DIR="$(getent passwd "$SUDO_UID" | cut -d: -f6 || true)"
[ -n "$HOME_DIR" ] || { echo "obs-box-background-run: uid $SUDO_UID has no passwd entry" >&2; exit 2; }
exec systemd-run --quiet --scope --collect --slice=rig-background.slice -- \
    setpriv --reuid="$SUDO_UID" --regid="$SUDO_GID" --init-groups \
    nice -n 19 ionice -c 3 env HOME="$HOME_DIR" "$@"
EOF
}

# obs_box_background_sudoers_text USER -> the sudoers line for the helper alone.
obs_box_background_sudoers_text() {
    printf '# issue 1387: the on-box decode runs in rig-background.slice under its caller'"'"'s own uid.\n'
    printf '%s ALL=(root) NOPASSWD: %s\n' "${1:?obs_box_background_sudoers_text: USER required}" "$(obs_box_background_run_path)"
}

# obs_box_background_slice BOX DESKTOP_USER -- the baseline item. Compared, rewritten only on a
# difference, daemon-reload only then, so the genlock deploy's setup re-run stays quiet. A running
# fbagent@ instance takes the limits live through a runtime property (Slice= itself applies at its next
# start); nothing is ever stopped or restarted, and OBS is not touched. The slice is enabled and started
# here so the cpuset controller comes up at setup or boot, never when a backup starts during a broadcast
# (the genlock deploy runs setup with OBS down). The drop-in is written whether or
# not fbagent is installed: without the template it is inert. OBS_BOX_BACKGROUND_ROOT,
# OBS_BOX_BACKGROUND_OWNER and OBS_BOX_ONLINE_CPUS are the test seams.
obs_box_background_slice() {
    local box="${1:?obs_box_background_slice: BOX required}" user="${2:?obs_box_background_slice: DESKTOP_USER required}"
    local root="${OBS_BOX_BACKGROUND_ROOT:-}" owner="${OBS_BOX_BACKGROUND_OWNER:-root:root}"
    local online_file="${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}"
    local conf="${root}/etc/${box}-isolated-cpus.conf" online obs bg changed=0 tmp unit
    online="$(head -n1 "$online_file" 2>/dev/null || true)"
    obs="$(head -n1 "$conf" 2>/dev/null || true)"
    bg="$(obs_box_background_cpus "$online" "$obs")" \
        || fail "issue 1387: no CPU left for background work (online '${online}', OBS mask '${obs}' from ${conf})"
    _bg_put() {  # PATH MODE LABEL (content on stdin)
        local out
        out="$(obs_box_write_if_changed "$1" "$2" "$owner" "$3")" || fail "issue 1387: could not write $1"
        echo "$out"
        case "$out" in *" written") changed=1 ;; esac
    }
    tmp="$(mktemp)" || fail "issue 1387: no temp file for the sudoers check"
    obs_box_background_sudoers_text "$user" > "$tmp"
    if ! visudo -cf "$tmp" >/dev/null; then
        rm -f "$tmp"
        fail "issue 1387: the rendered sudoers line does not parse"
    fi
    rm -f "$tmp"
    install -d -m 0755 "${root}$(dirname "$(obs_box_background_dropin_path)")" "${root}$(dirname "$(obs_box_background_run_path)")" \
        "${root}$(dirname "$(obs_box_background_slice_path)")" || fail "issue 1387: could not create the unit directories"
    _bg_put "${root}$(obs_box_background_slice_path)" 0644 "background slice" < <(obs_box_background_slice_text "$bg")
    _bg_put "${root}$(obs_box_background_dropin_path)" 0644 "fbagent drop-in" < <(obs_box_background_dropin_text)
    _bg_put "${root}$(obs_box_background_run_path)" 0755 "background runner" < <(obs_box_background_run_text)
    _bg_put "${root}$(obs_box_background_sudoers_path)" 0440 "background sudoers" < <(obs_box_background_sudoers_text "$user")
    unset -f _bg_put
    if [ "$changed" = 1 ]; then systemctl daemon-reload || fail "issue 1387: systemctl daemon-reload failed"; fi
    systemctl enable --now "$(obs_box_background_slice_name)" >/dev/null || fail "issue 1387: could not enable the background slice"
    while read -r unit _; do
        [ -n "$unit" ] || continue
        # ControlGroup is where it RUNS: the Slice property reads the drop-in as soon as it is loaded.
        case "$(systemctl show -p ControlGroup --value "$unit" 2>/dev/null || true)" in
            */"$(obs_box_background_slice_name)"/*) continue ;;
        esac
        if systemctl set-property --runtime "$unit" "AllowedCPUs=${bg}" CPUWeight=1 IOWeight=1 "MemoryHigh=$(obs_box_background_memory_high)"; then
            echo "  background slice: running ${unit} fenced live (it joins the slice at its next start)"
        else
            echo -e "  ${YELLOW:-}issue 1387: could not fence the running ${unit}; verify's background row FAILs until it restarts${NC:-}"
        fi
    done < <(systemctl list-units --type=service --state=active,activating --no-legend --plain 'fbagent@*' 2>/dev/null || true)
    echo "  background slice: $(obs_box_background_slice_name) on CPUs ${bg} (OBS mask ${obs})"
}

# obs_box_background_grade ONLINE OBS LOADED ALLOWED FILE RUN SUDO FBAGENT DROPIN PGREP PROCS OBS_RUNNING
#   OBS_MASK SMT CGROUP_CPUS IOSCHED ENABLED -> the `background` row's detail line, rc 0 iff the fence holds.
# Pure over the gather's facts, fail-closed: an unread fact FAILs, never a pass. PROCS = space separated
# `<comm>:<pid>:<Cpus_allowed_list>` of every running restic / fbagent. IOSCHED is reported, never graded.
obs_box_background_grade() {
    local online="${1-}" obs="${2-}" loaded="${3-}" allowed="${4-}" file="${5-}" run="${6-}" sudo_ok="${7-}"
    local fbagent="${8-}" dropin="${9-}" pgrep_ok="${10-}" procs="${11-}" obs_running="${12-}" obs_mask="${13-}"
    local smt="${14-}" cgroup_cpus="${15-}" iosched="${16-}" enabled="${17-}"
    local bad="" want want_list p c m name
    if ! want="$(obs_box_background_cpus "$online" "$obs")"; then
        printf 'FENCE UNREADABLE: online=[%s] OBS mask=[%s] leave no background CPU\n' "$online" "$obs"
        return 1
    fi
    want_list="$(obs_box_cpulist_expand "$want")"
    [ "$loaded" = loaded ] || bad="${bad} slice-not-loaded(${loaded:-unread})"
    [ "$(obs_box_cpulist_expand "$allowed" 2>/dev/null || true)" = "$want_list" ] || bad="${bad} slice-AllowedCPUs(${allowed:-unread})"
    [ "$enabled" = enabled ] || bad="${bad} slice-not-enabled(${enabled:-unread})"
    [ "$file" = 1 ] || bad="${bad} slice-file-differs"
    [ "$run" = 1 ] || bad="${bad} runner-missing-or-differs"
    [ "$sudo_ok" = 1 ] || bad="${bad} runner-not-allowed-by-sudo"
    case "$fbagent" in
        0) ;;
        1) [ "$dropin" = 1 ] || bad="${bad} fbagent-drop-in-missing" ;;
        *) bad="${bad} fbagent-unread" ;;
    esac
    [ "$pgrep_ok" = 1 ] || bad="${bad} pgrep-missing"
    for p in $procs; do
        name="${p%%:*}"
        if ! m="$(obs_box_cpulist_expand "${p##*:}" 2>/dev/null)"; then bad="${bad} ${name}-mask-unread"; continue; fi
        for c in $m; do
            case " $want_list " in *" $c "*) ;; *) bad="${bad} ${p}-outside-the-slice"; break ;; esac
        done
    done
    case "$obs_running" in
        0) ;;
        1)
            if ! m="$(obs_box_cpulist_expand "$obs_mask" 2>/dev/null)"; then bad="${bad} obs-mask-unread"; else
                for c in $m; do
                    case " $want_list " in *" $c "*) bad="${bad} OBS-runs-on-background-cpu-${c}"; break ;; esac
                done
            fi
            ;;
        *) bad="${bad} obs-state-unread" ;;
    esac
    [ -z "$smt" ] || bad="${bad} SMT-shared-with-OBS(${smt})"
    # systemd keeps no cgroup directory for an empty slice, so it reads only while a member runs.
    if [ -n "${procs}${cgroup_cpus}" ] &&
        [ "$(obs_box_cpulist_expand "$cgroup_cpus" 2>/dev/null || true)" != "$want_list" ]; then
        bad="${bad} cgroup-cpus(${cgroup_cpus:-unread})"
    fi
    printf '%s CPUs=%s fbagent=%s running=[%s] OBS live mask=[%s] io-scheduler=[%s]%s\n' \
        "$(obs_box_background_slice_name)" "$want" "${fbagent:-unread}" "$procs" "${obs_mask:-not running}" "${iosched:-unread}" \
        "${bad:+ BAD:${bad}}"
    [ -z "$bad" ]
}
