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
#          stderr. rc 1 + a named stderr line when HOST is not a Linux stream box, when LOCAL is missing,
#          NAME is not a plain file name or REMOTE_DIR holds whitespace / a quote (all three before any
#          contact), when REMOTE_DIR cannot be made, or when the copy fails. The caller ENDS the run on rc != 0: the stream decode
#          reads these files, so a missing marker log would cost the run its A/V measurement.
#   stream_lx_partial_pullback_note PARTIAL PIXELS
#       -> the pull-back lines for the run log (the decode already pulled both back over scp).
#
# The transport is the stream-box lib's (stream_box_exec + stream_box_upload: plain ssh/scp with
# STREAM_BOX_SSH_OPTS -- the address held the Windows box's host keys until 22.10.2026, so
# UserKnownHostsFile=/dev/null); this lib never opens its own ssh. NAME and REMOTE_DIR are
# E2E-generated (`<dir>/av-markers-<RUN_ID>.csv`); scp hands the destination to the remote shell, so
# whitespace or a quote in either is refused instead of being split there.

command -v stream_box_platform >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/stream-box.sh"
command -v shell_single_quote >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/shell-quote.sh"

stream_lx_push_file() {
  local user="${1-}" pw="${2-}" host="${3-}" local_path="${4-}" dir="${5-}" name="${6-}" dest
  dest="${dir%/}/${name}"
  if [ ! -f "$local_path" ]; then
    echo "ERROR: [8/8b-pre] ${local_path} is missing on dev1 -- nothing to push to the stream box ${host} as ${dest} (issue 1462)" >&2
    return 1
  fi
  if [ "$(stream_box_platform "$host")" != "linux" ]; then
    echo "ERROR: [8/8b-pre] ${host} is not a Linux stream box -- a Windows stream box takes win_ssh_upload (issue 1462)" >&2
    return 1
  fi
  case "$name" in
    "" | */* | *[[:space:]\'\"]*)
      echo "ERROR: [8/8b-pre] '${name}' is not a plain file name for the stream box ${host} (issue 1462)" >&2
      return 1 ;;
  esac
  case "$dir" in
    "" | *[[:space:]\'\"]*)
      echo "ERROR: [8/8b-pre] '${dir}' is no usable remote dir for ${name} on the stream box ${host} (empty, whitespace or a quote; issue 1462)" >&2
      return 1 ;;
  esac
  if ! stream_box_exec "$user" "$pw" "$host" "" "mkdir -p -- $(shell_single_quote "$dir")"; then
    echo "ERROR: [8/8b-pre] could not create ${dir} on the stream box ${host} for ${name} (issue 1462)" >&2
    return 1
  fi
  echo "      scp ${local_path} -> ${host}:${dest}" >&2
  if ! stream_box_upload "$user" "$pw" "$host" "$local_path" "" "$dest" >&2; then
    echo "ERROR: [8/8b-pre] could not copy ${local_path} to the stream box ${host} as ${dest} (issue 1462)" >&2
    return 1
  fi
  printf '%s\n' "$dest"
}

# stream_access_label HOST -> how the [8/8b] banner names the stream box's decode transport: the box's
# fleet name + plain ssh/scp on a Linux stream box, the win-* MCP name on a Windows one (the
# strih_access_label twin, scripts/lib/strih-platform.sh).
stream_access_label() {
  local name
  if [ "$(stream_box_platform "${1:-}")" = "linux" ]; then
    name="$(obs_fleet_name_for_host "${1:-}" 2>/dev/null || true)"
    printf '%s, plain ssh/scp' "${name:-${1:-}}"
  else
    printf 'win-stream-snv'
  fi
}

stream_lx_partial_pullback_note() {
  printf '    pull back to dev1: %s  AND the #186 pixel-proof dir %s\n' "${1:-}" "${2:-}"
  printf '      (the Linux stream box: already pulled back over scp by recording-verdict-on-stream-snv.sh;\n'
  printf '       the pixel-proof dir is absent on a clean run)\n'
}
