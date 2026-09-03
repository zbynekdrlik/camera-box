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

# onbox_decode_priority_class -- issue 1260. Pure/testable (no network): resolve+validate the
# on-box `recording-verdict.exe --extract-partial` decode's Windows PriorityClass from
# $E2E_ONBOX_DECODE_PRIORITY, one shared place both scripts/recording-verdict-on-strih.sh and
# -on-stream.sh source (they already source this whole file unconditionally at file scope).
#
# WHY (issue 1260): the on-strih/on-stream post-E2E decode runs for ~20 minutes, competing 1:1 for
# CPU with the LIVE `obs64` process on the SAME box -- correlated in the strih OBS
# `multiview-audit` log with a rendered_fps collapse (supervisor comment 5518052846: 0-3
# dips/10min idle vs 89-117 dips/10min during exactly the decode windows, program `lagged=0`
# throughout -- Multiview is the victim, program is not). issue 767 already solved the SAME class
# for imag-nb (`nice -n 19` in build_onimag_command); this brings the two Windows planners up to
# that shape via a PowerShell host-process PriorityClass (the &-invoked recording-verdict.exe
# child inherits it -- Win32 CreateProcess semantics).
#
# Default BelowNormal, with NO opt-out -- owner rule: a needed property is default-on, never a
# forgettable toggle. `${VAR:-BelowNormal}` defaults on BOTH unset and empty (an empty override is
# silently treated as "not set", never a warning -- only a genuinely non-matching value warns
# below). An invalid value falls back to BelowNormal too, printing a WARNING naming the rejected
# value on stderr (fail-safe: never silently swallow a typo'd override, and never let an
# unrecognized string reach the emitted PowerShell text -- PowerShell would reject an unknown
# PriorityClass at parse time on the box, aborting the whole decode).
onbox_decode_priority_class() {
  local v="${E2E_ONBOX_DECODE_PRIORITY:-BelowNormal}"
  case "$v" in
    Idle|BelowNormal|Normal) printf '%s' "$v" ;;
    *)
      echo "WARNING: E2E_ONBOX_DECODE_PRIORITY=\"$v\" is not one of Idle|BelowNormal|Normal -- using BelowNormal" >&2
      printf '%s' "BelowNormal"
      ;;
  esac
}

# win_ssh_basename <windows-path> -- the filename component of a WINDOWS path (backslash- or
# forward-slash-separated). Plain bash `basename` splits ONLY on `/` -- fed a backslash Windows
# path (e.g. `C:\camera-box\verdict-out\strih-partial-12345.json`), it finds no `/` and returns
# the WHOLE STRING unchanged (live-verified live-CI bug, #703, 2026-07-11: the pulled-back local
# destination became the literal nonsense path `$LOCAL_OUT_DIR/C:\camera-box\verdict-out\...`).
# Pure string function, no network.
win_ssh_basename() {
  local p="${1//\\//}" # normalize backslashes to / first (matches win_ssh_scp_source_path)
  printf '%s' "${p##*/}"
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

# win_ssh_scp_source_path <windows-path> -- live-verified fix (2026-07-11, #703 real-CI-run
# incident): a scp SOURCE spec (the box-to-dev1 DOWNLOAD direction) with a BACKSLASH Windows
# path fails "No such file or directory" even though the file genuinely exists on the box
# (confirmed: `scp user@host:'C:\camera-box\...\x.json' local` -> error; the IDENTICAL file via
# `scp user@host:'C:/camera-box/.../x.json' local` -> succeeds, byte-identical). The reverse
# (scp UPLOAD to a backslash DESTINATION) is UNAFFECTED (also live-verified) -- this conversion
# is deliberately applied ONLY on the download side, never win_ssh_upload. Win32-OpenSSH's
# sftp-server (what scp uses since OpenSSH 8.7+) accepts forward slashes transparently for a
# Windows path (recording-verdict.exe itself already reads/writes fine with either separator,
# per its own decode logs using forward-slash StopRecord-returned paths) -- only the scp CLIENT
# side's remote-source parsing chokes on the backslash. Pure string function, no network.
win_ssh_scp_source_path() {
  printf '%s' "${1//\\//}"
}

# win_ssh_download <user> <pw> <host> <remote-windows-path> <local-path>
win_ssh_download() {
  local user="$1" pw="$2" host="$3" remote="$4" local="$5"
  local scp_remote
  scp_remote="$(win_ssh_scp_source_path "$remote")"
  sshpass -p "$pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${user}@${host}:${scp_remote}" "$local"
}

# win_ssh_download_dir <user> <pw> <host> <remote-windows-dir> <local-dir-parent> -- scp -r a
# directory FROM a Windows box (the #186 pixel-proof dir). Caller checks existence first via
# win_ssh_path_exists (best-effort -- absent on a clean run).
win_ssh_download_dir() {
  local user="$1" pw="$2" host="$3" remote="$4" local_parent="$5"
  local scp_remote
  scp_remote="$(win_ssh_scp_source_path "$remote")"
  sshpass -p "$pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -r "${user}@${host}:${scp_remote}" "$local_parent"
}

# win_ssh_path_exists <user> <pw> <host> <remote-windows-path> -- true (0) iff the path
# exists on the box (file or dir; cmd.exe `if exist` handles both).
win_ssh_path_exists() {
  local user="$1" pw="$2" host="$3" remote="$4"
  sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${user}@${host}" \
    "if exist \"$remote\" (exit 0) else (exit 1)"
}
