#!/usr/bin/env bash
set -euo pipefail
# scripts/lib/win-ssh-exec.sh -- shared ssh/scp helpers for EXECUTING recording-verdict
# directly on a Windows box (strih/stream), #703.
#
# WHY THIS EXISTS: the historical "scp/ssh to Windows is DENIED on this rig" premise (baked
# into recording-verdict-on-strih.sh / recording-verdict-on-stream.sh as pure PLANNERS that
# print a command for a human/MCP operator to paste) is STALE for strih (10.77.9.202) and
# stream (10.77.9.204) specifically -- #701 proved plain OpenSSH+password scp/ssh works
# cleanly with the targets.md creds (newlevel/newlevel, Win32-OpenSSH server already
# installed on both boxes). #703 needs the REQUIRED CI merge gate to actually EXECUTE the
# decode-in-place + merge, not just print a plan nobody runs -- this library is that
# execution path, mirroring recording-verdict-on-imag.sh's "ssh CAN reach this box, so DO
# the work" shape.
#
# Both boxes' ssh default shell is cmd.exe (Win32-OpenSSH default); PowerShell is invoked via
# `-EncodedCommand <base64 UTF-16LE>` to sidestep the THREE-LAYER quoting hazard (bash arg ->
# ssh remote-command string -> cmd.exe -> powershell parser) -- a naively double/triple-quoted
# PowerShell command string breaks on ANY path containing a space (both rig recording dirs
# do: `D:\_REC\2026-07-10 17-10-31.mkv`). Live-verified on strih (2026-07-11): a
# --extract-partial command referencing that exact space-bearing path round-tripped correctly
# and began decoding for real.

# win_ssh_ps_encoded_command <ps-command-text> -- base64-UTF16LE-encode a PowerShell command
# string for `-EncodedCommand`. Pure string function (no network) -- testable by sourcing.
win_ssh_ps_encoded_command() {
  printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE | base64 -w0
}

# win_ssh_run <user> <pw> <host> <ps-command-text> -- run a PowerShell command on a Windows
# box over ssh via -EncodedCommand. BLOCKS until the remote command exits; stdout/stderr
# stream through ssh in real time. A direct ssh exec has NO idle timeout (unlike the win-*
# MCP Shell tool's ~30-300s idle-timeout that forced the old detached Start-Process+poll
# dance) -- it just blocks for the command's real duration, so the CALLER must bound it with
# an outer `timeout` if a wedge must not hang forever.
win_ssh_run() {
  local user="$1" pw="$2" host="$3" ps_cmd="$4"
  local enc
  enc="$(win_ssh_ps_encoded_command "$ps_cmd")"
  sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
    -o ServerAliveInterval=30 -o ServerAliveCountMax=20 \
    "${user}@${host}" "powershell -NoProfile -NonInteractive -EncodedCommand $enc"
}

# win_ssh_upload <user> <pw> <host> <local-path> <remote-windows-path>
win_ssh_upload() {
  local user="$1" pw="$2" host="$3" local="$4" remote="$5"
  sshpass -p "$pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "$local" "${user}@${host}:${remote}"
}

# win_ssh_download <user> <pw> <host> <remote-windows-path> <local-path>
win_ssh_download() {
  local user="$1" pw="$2" host="$3" remote="$4" local="$5"
  sshpass -p "$pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${user}@${host}:${remote}" "$local"
}

# win_ssh_download_dir <user> <pw> <host> <remote-windows-dir> <local-dir-parent> -- scp -r a
# directory FROM a Windows box (the #186 pixel-proof dir). Caller checks existence first via
# win_ssh_path_exists (best-effort -- absent on a clean run).
win_ssh_download_dir() {
  local user="$1" pw="$2" host="$3" remote="$4" local_parent="$5"
  sshpass -p "$pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -r "${user}@${host}:${remote}" "$local_parent"
}

# win_ssh_path_exists <user> <pw> <host> <remote-windows-path> -- true (0) iff the path
# exists on the box (file or dir; cmd.exe `if exist` handles both).
win_ssh_path_exists() {
  local user="$1" pw="$2" host="$3" remote="$4"
  sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${user}@${host}" \
    "if exist \"$remote\" (exit 0) else (exit 1)"
}
