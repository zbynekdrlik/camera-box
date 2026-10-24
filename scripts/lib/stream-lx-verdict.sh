#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements but a guarded
# source of stream-box.sh) -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell
# (recording-e2e.sh sets `set -euo pipefail` itself).
#
# scripts/lib/stream-lx-verdict.sh -- issue 1462: the recording-e2e.sh `[8/8b]` steps that move files
# to and from the LINUX stream box (stream-snv, obs-fleet class linux-genlock) around its on-box
# stream decode (scripts/recording-verdict-on-stream-snv.sh):
#
#   stream_lx_push_file USER PW HOST LOCAL REMOTE_DIR NAME
#       -> `[8/8b-pre]`: put LOCAL (the cam2 A/V marker log, the #312 switch schedule) on the box as
#          REMOTE_DIR/NAME and print that path (stdout, the ONE line the caller captures); progress on
#          stderr. rc 1 + a named stderr line when LOCAL is missing (nothing contacted), when REMOTE_DIR
#          cannot be made, or when the copy fails. The caller ENDS the run on rc != 0: the stream decode
#          reads these files, so a missing marker log would cost the run its A/V measurement.
#   stream_lx_partial_pullback_note PARTIAL PIXELS
#       -> the pull-back lines for the run log (the decode already pulled both back over scp).
#
# The transport is the stream-box lib's (plain ssh/scp with STREAM_BOX_SSH_OPTS: the address held the
# Windows box's host keys until 22.10.2026, so UserKnownHostsFile=/dev/null). NAME must be a plain
# file name and REMOTE_DIR a path without whitespace or quotes: both are E2E-generated
# (`<dir>/av-markers-<RUN_ID>.csv`), and the remote shell parses the mkdir path once (single-quoted).

command -v stream_box_platform >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/stream-box.sh"
command -v shell_single_quote >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/shell-quote.sh"

stream_lx_push_file() {
  local user="${1-}" pw="${2-}" host="${3-}" local_path="${4-}" dir="${5-}" name="${6-}" dest
  dest="${dir%/}/${name}"
  if [ ! -f "$local_path" ]; then
    echo "ERROR: [8/8b-pre] ${local_path} is missing on dev1 -- nothing to push to the stream box ${host} as ${dest} (issue 1462)" >&2
    return 1
  fi
  case "$name" in
    "" | */* | *[[:space:]\'\"]*)
      echo "ERROR: [8/8b-pre] '${name}' is not a plain file name for the stream box ${host} (issue 1462)" >&2
      return 1 ;;
  esac
  if ! sshpass -p "$pw" ssh -n "${STREAM_BOX_SSH_OPTS[@]}" "${user}@${host}" "mkdir -p -- $(shell_single_quote "$dir")"; then
    echo "ERROR: [8/8b-pre] could not create ${dir} on the stream box ${host} for ${name} (issue 1462)" >&2
    return 1
  fi
  echo "      scp ${local_path} -> ${host}:${dest}" >&2
  if ! sshpass -p "$pw" scp "${STREAM_BOX_SSH_OPTS[@]}" "$local_path" "${user}@${host}:${dest}" >&2; then
    echo "ERROR: [8/8b-pre] could not copy ${local_path} to the stream box ${host} as ${dest} (issue 1462)" >&2
    return 1
  fi
  printf '%s\n' "$dest"
}

stream_lx_partial_pullback_note() {
  printf '    pull back to dev1: %s  AND the #186 pixel-proof dir %s\n' "${1:-}" "${2:-}"
  printf '      (the Linux stream box: already pulled back over scp by recording-verdict-on-stream-snv.sh;\n'
  printf '       the pixel-proof dir is absent on a clean run)\n'
}
