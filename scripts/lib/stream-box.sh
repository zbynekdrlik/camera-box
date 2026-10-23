#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + one constant, no other top-level
# statements) -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode
# here would leak into it (rig-mode.sh, recording-e2e.sh and their `timeout bash -c` children).
#
# scripts/lib/stream-box.sh -- issue 1462: how the dev1 tools reach the STREAM box.
#
# The stream box (10.77.9.204) was a Windows PC until 22.10.2026, when it was reinstalled as the Linux
# stream-snv (the stream role of setup-strih.sh, the same setup as stream-pp). Every dev1 step that
# runs a command ON it used to send PowerShell through win_ssh_run. This lib is the ONE place that
# chooses the transport, by the box's obs-fleet CLASS (scripts/lib/obs-fleet.sh -- the same alias-aware
# authority strih_platform uses), never by a literal address:
#
#   stream_box_platform HOST      -> linux | windows
#   stream_box_exec ...           -> run the platform's command text on the box
#   stream_box_download ...       -> scp one file from the box
#   stream_box_visibility_gate_linux ...  -> the [0/8] operator-visibility gate on a Linux stream box
#   stream_mbc_vban_expect HOST   -> `<stream> <port>` the Linux box's mbc VBAN input must carry
#
# Callers hand BOTH texts (the PowerShell one and the bash one); the Windows text is the same text as
# before, byte for byte. The Linux transport is plain ssh with UserKnownHostsFile=/dev/null: the
# address belonged to the Windows box until 22.10.2026, and a stale known_hosts key with
# StrictHostKeyChecking=no makes OpenSSH disable password authentication (the strih-snv log reader's
# rule, .claude/rules/strih-log-read.md).
#
# win_ssh_run / win_ssh_download come from scripts/lib/win-ssh-exec.sh, which the CALLER sources: that
# lib sets `set -euo pipefail` at its top, so sourcing it from here would change the caller's shell.

command -v obs_fleet_class_for_host >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/obs-fleet.sh"

# The ssh/scp options of the Linux transport (connect bounded; the remote command is the caller's to
# bound, like win_ssh_run).
STREAM_BOX_SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10)

# stream_box_platform HOST -> "linux" when the obs-fleet row HOST addresses is class linux-genlock,
# else "windows" (a Windows-class row, an address the fleet list does not know, an empty HOST). Pure
# for an IP literal; a name goes through the fleet lib's time-bounded resolver.
stream_box_platform() {
  local host="${1:-}"
  if [ -n "$host" ] && [ "$(obs_fleet_class_for_host "$host" 2>/dev/null || true)" = "linux-genlock" ]; then
    printf 'linux'
  else
    printf 'windows'
  fi
}

# stream_box_exec USER PW HOST WIN_PS LINUX_SH -> run the command text for HOST's platform on HOST:
# LINUX_SH over plain ssh (ONE remote argument, parsed once by the box's shell), WIN_PS through
# win_ssh_run (-EncodedCommand). stdout/stderr pass through; the rc is the remote command's (ssh 255
# / sshpass 5-6 on a transport failure). Blocks for the command's duration: the caller bounds it.
stream_box_exec() {
  local user="${1-}" pw="${2-}" host="${3-}" win_ps="${4-}" linux_sh="${5-}"
  if [ "$(stream_box_platform "$host")" = "linux" ]; then
    sshpass -p "$pw" ssh "${STREAM_BOX_SSH_OPTS[@]}" -o ServerAliveInterval=30 -o ServerAliveCountMax=20 \
      "${user}@${host}" "$linux_sh"
    return
  fi
  if ! declare -F win_ssh_run >/dev/null; then
    echo "stream-box: ${host} is a Windows stream box but win_ssh_run is not defined -- source scripts/lib/win-ssh-exec.sh first (issue 1462)" >&2
    return 127
  fi
  win_ssh_run "$user" "$pw" "$host" "$win_ps"
}

# stream_box_download USER PW HOST WIN_REMOTE LINUX_REMOTE LOCAL -> copy one file from HOST to LOCAL:
# LINUX_REMOTE over plain scp on Linux, WIN_REMOTE through win_ssh_download on Windows. rc = scp's.
stream_box_download() {
  local user="${1-}" pw="${2-}" host="${3-}" win_remote="${4-}" linux_remote="${5-}" local_path="${6-}"
  if [ "$(stream_box_platform "$host")" = "linux" ]; then
    sshpass -p "$pw" scp "${STREAM_BOX_SSH_OPTS[@]}" "${user}@${host}:${linux_remote}" "$local_path"
    return
  fi
  if ! declare -F win_ssh_download >/dev/null; then
    echo "stream-box: ${host} is a Windows stream box but win_ssh_download is not defined -- source scripts/lib/win-ssh-exec.sh first (issue 1462)" >&2
    return 127
  fi
  win_ssh_download "$user" "$pw" "$host" "$win_remote" "$local_path"
}

# stream_box_visibility_gate_linux HOST USER PW TIMEOUT HERE -> the recording-e2e [0/8] operator-
# visibility gate for a Linux stream box. The box is the same OBS appliance as strih-snv (setup-strih,
# stream role): OBS runs under the operator's supervised `strih-obs.service`, so the check IS
# strih_linux_visibility_check (the unit over plain ssh + a bounded obs-websocket round-trip). Prints
# the ok line and returns 0, or the ERROR + the recovery command on stderr and returns 1 (the caller
# exits). Never the Windows obs64/AHK probe.
stream_box_visibility_gate_linux() {
  local host="${1-}" user="${2-}" pw="${3-}" tmo="${4-}" here="${5-}" msg
  command -v strih_linux_visibility_check >/dev/null 2>&1 \
    || . "${BASH_SOURCE[0]%/*}/strih-platform.sh"
  msg="$(strih_linux_visibility_check "$host" "$user" "$pw" "$tmo" "$here")"
  if [ -n "$msg" ]; then
    echo "ERROR: [0/8] stream INVISIBLE: $msg" >&2
    echo "       Recovery: ssh ${user}@${host} 'systemctl --user restart strih-obs.service'   # the Linux stream box's supervised user unit (issue 1462)" >&2
    return 1
  fi
  echo "    ok: stream strih-obs.service active + obs-websocket answers on :4455 (the Linux stream box, issue 1462)"
}

# stream_mbc_vban_expect HOST -> on a Linux stream box, print `<stream> <port>`: the VBAN stream name
# and UDP port its OBS input `mbc` (the vendored obs-vban receive source, issue 1439) must carry, from
# the box's FACT file (scripts/strih-boxes/<fleet name>.env, STRIH_PROGRAM_VBAN_STREAMS / _PORT,
# loaded by scripts/lib/strih-box-facts.sh in a SUBSHELL, so the caller's shell never gets a loaded
# box). On a Windows stream box print nothing and return 0 (the Dante device check applies there).
# A Linux box whose fleet name has no fact file, or whose facts declare no program VBAN stream,
# refuses: rc 1 with the reason on stderr -- the mbc check cannot be graded without the facts.
stream_mbc_vban_expect() {
  local host="${1-}" dir="${BASH_SOURCE[0]%/*}" name out
  [ "$(stream_box_platform "$host")" = "linux" ] || return 0
  name="$(obs_fleet_name_for_host "$host" 2>/dev/null)" || name=""
  if [ -z "$name" ]; then
    echo "stream-box: ${host} is a linux-genlock box but no obs-fleet row names it -- cannot read its box facts (issue 1462)" >&2
    return 1
  fi
  if ! out="$(
    command -v strih_box_load >/dev/null 2>&1 || . "${dir}/strih-box-facts.sh"
    strih_box_load "$name" >/dev/null || exit 1
    s="$(strih_lx_program_vban_streams)" && p="$(strih_lx_program_vban_port)" || exit 1
    [ "$s" != "none" ] && [ "$p" != "none" ] || exit 2
    printf '%s %s' "$s" "$p"
  )"; then
    echo "stream-box: ${host} is the Linux stream box '${name}' but its box facts give no program VBAN stream (scripts/strih-boxes/${name}.env STRIH_PROGRAM_VBAN_STREAMS / STRIH_PROGRAM_VBAN_PORT) -- the mbc VBAN transport cannot be checked (issue 1462)" >&2
    return 1
  fi
  printf '%s\n' "$out"
}
