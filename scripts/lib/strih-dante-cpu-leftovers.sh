#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-cpu-leftovers.sh -- issue 1485, dstack v0.6.28 (owner ruling relayed by dstack
# issue 1, 4.11.2026): no CPU-core pinning for dlink apps, nobody can guard cores on every PC. dlink's
# default budget is 15 ms with a 6 ms pump margin instead, and dstack removed pump_cpu / io_cpu / rx_cpu.
# camera-box's whole Dante CPU reservation (the issue-1485 rx-pin and reserved-CPU libs of 3.-4.11.2026)
# is removed; this lib only takes what those libs wrote OFF a box, and grades it gone:
#
#   /etc/strih-dante-rx-cpu.conf                               the reserved CPUs (no header: always ours)
#   /usr/local/sbin/strih-dante-rx-pin                         the root helper
#   /etc/sudoers.d/strih-dante-rx-pin                          the operator's sudoers line for it
#   /etc/systemd/{system,user}.conf.d/50-strih-dante-reserved-cpu.conf  [Manager] CPUAffinity= (header-checked)
#   /etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf  ExecStartPost (header-checked)
#
#   strih_dante_cpu_leftovers_remove [USER]  setup-strih step 13 on EVERY box (strih_dante_input_install, root)
#   strih_dante_cpu_leftovers_grade          verify item 9's `(dante-cpu)` rows (strih_program_audio_grade)
#
# A removed manager drop-in re-executes PID 1 and USER's user manager (daemon-reexec restarts no unit; a
# failure is a WARNING, the next boot applies it); running processes keep the old mask until they restart
# (reboot the box, or restart OBS + the hub). A removed hub drop-in is followed by a daemon-reload (a
# failure fails the step). A file of a header-checked name without our header is left and named.
# Seam: STRIH_DANTE_RX_PIN_ROOT (a prefix for every path, the Dante libs' one seam, shared with
# strih-dante-ptp.sh).

STRIH_DANTE_CPU_MANAGER_HEADER="# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it."
STRIH_DANTE_CPU_HUB_HEADER="# issue 1485: written by camera-box setup-strih step 13 on a box whose hub Dante device has its own CPU; every run rewrites it."

# strih_dante_cpu_leftover_paths -> `<kind> <path>` per leftover, kind = plain | manager | hub.
strih_dante_cpu_leftover_paths() {
  local r="${STRIH_DANTE_RX_PIN_ROOT:-}"
  printf 'plain %s/etc/strih-dante-rx-cpu.conf\n' "$r"
  printf 'plain %s/usr/local/sbin/strih-dante-rx-pin\n' "$r"
  printf 'plain %s/etc/sudoers.d/strih-dante-rx-pin\n' "$r"
  printf 'manager %s/etc/systemd/system.conf.d/50-strih-dante-reserved-cpu.conf\n' "$r"
  printf 'manager %s/etc/systemd/user.conf.d/50-strih-dante-reserved-cpu.conf\n' "$r"
  printf 'hub %s/etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf\n' "$r"
}

# _strih_dante_cpu_ours KIND FILE -> rc 0 when FILE is ours to remove (a plain one always, the others by header).
_strih_dante_cpu_ours() {
  case "$1" in
    plain) return 0 ;;
    manager) head -n 1 "$2" 2>/dev/null | grep -qxF "$STRIH_DANTE_CPU_MANAGER_HEADER" ;;
    hub) head -n 1 "$2" 2>/dev/null | grep -qxF "$STRIH_DANTE_CPU_HUB_HEADER" ;;
    *) return 1 ;;
  esac
}

# strih_dante_cpu_leftovers_remove [USER] -> remove every leftover of ours (prints only when it touched
# something, so a clean box stays quiet). rc 1 when a file cannot be removed or the hub reload fails.
strih_dante_cpu_leftovers_remove() {
  local user="${1-}" kind f managers=0 hub=0 rc=0
  while read -r kind f; do
    [ -e "$f" ] || [ -L "$f" ] || continue
    if ! _strih_dante_cpu_ours "$kind" "$f"; then
      echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2
      continue
    fi
    if ! rm -f -- "$f"; then
      echo "dante-cpu: cannot remove ${f}" >&2
      rc=1
      continue
    fi
    echo "  dante-cpu: removed ${f} (no CPU pinning for Dante apps since dstack v0.6.28, issue 1485)"
    case "$kind" in manager) managers=1 ;; hub) hub=1 ;; esac
  done < <(strih_dante_cpu_leftover_paths)
  [ "$managers" = 0 ] || _strih_dante_cpu_leftovers_reexec "$user"
  if [ "$hub" = 1 ] && ! timeout 30 systemctl daemon-reload; then
    echo "dante-cpu: systemctl daemon-reload failed or timed out after the hub drop-in was removed" >&2
    rc=1
  fi
  return "$rc"
}

# _strih_dante_cpu_leftovers_reexec [USER] -> PID 1 and USER's user manager re-read system.conf.d /
# user.conf.d (daemon-reexec restarts no unit), each bounded by `timeout 30`; each failure is its own
# WARNING naming the manager (the next boot applies it). No USER = PID 1 only. rc 0.
_strih_dante_cpu_leftovers_reexec() {
  local user="${1-}" done=""
  if timeout 30 systemctl daemon-reexec; then
    done="PID 1"
  else
    echo "  WARNING: dante-cpu: PID 1 daemon-reexec failed or timed out -- the system manager drops the old CPUAffinity default at the next boot" >&2
  fi
  if [ -n "$user" ]; then
    if timeout 30 systemctl --user -M "${user}@" daemon-reexec; then
      done="${done:+${done} + }the user manager of ${user}"
    else
      echo "  WARNING: dante-cpu: the user manager of ${user}: daemon-reexec failed or timed out -- it drops the old default at the next boot" >&2
    fi
  fi
  [ -z "$done" ] || echo "  dante-cpu: re-read: ${done} re-executed (daemon-reexec); running processes keep their mask until they restart"
  return 0
}

# strih_dante_cpu_leftovers_grade -> one `(dante-cpu)` row per leftover (FAIL, a foreign file at the name
# too: it still sets a CPU default), or one OK row, through the caller's ok / bad. Read-only. rc 0.
strih_dante_cpu_leftovers_grade() {
  local kind f n=0
  while read -r kind f; do
    [ -e "$f" ] || [ -L "$f" ] || continue
    n=$((n + 1))
    if _strih_dante_cpu_ours "$kind" "$f"; then
      bad "(dante-cpu) leftover ${f} of the removed Dante CPU reservation -- re-run setup-strih.sh step 13 (it removes it), then reboot (issue 1485)"
    else
      bad "(dante-cpu) leftover ${f} (not camera-box's: no header) at a name of the removed Dante CPU reservation -- remove it by hand (issue 1485)"
    fi
  done < <(strih_dante_cpu_leftover_paths)
  [ "$n" != 0 ] || ok "(dante-cpu) no Dante CPU reservation on this box (no pinning since dstack v0.6.28)"
  return 0
}
