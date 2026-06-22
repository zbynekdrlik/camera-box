#!/usr/bin/env bash
# recording-fetch-windows.sh — pull an OBS recording off a Windows box (strih/stream)
# to dev1, for the recording-based 4-node E2E (#105/#7).
#
# scp/ssh TO the Windows boxes is DENIED on this rig (CLAUDE.md). The established,
# CI-safe mechanism (proven this session) is a standing `python -m http.server` on each
# Windows box serving its OBS record DIRECTORY on port 8899; dev1 curls the file by name.
# OBS StopRecord gives the host's ABSOLUTE path; we take its basename and fetch
# http://<box>:<HTTP_PORT>/<basename> → the dev1 destination.
#
# Usage:
#   recording-fetch-windows.sh <strihHost> <strihHostPath> <strihDest> \
#                              <streamHost> <streamHostPath> <streamDest>
# Env:
#   WIN_HTTP_PORT  (default 8899) — the http.server port on the Windows boxes.
#
# The agent-driven path (when an operator/agent has the win-* MCP) can instead
# FileDownload the host path directly; this script is the curl path the CI runner uses.
set -euo pipefail
WIN_HTTP_PORT="${WIN_HTTP_PORT:-8899}"

fetch_one() {
  local host="$1" host_path="$2" dest="$3"
  if [ -z "$host_path" ]; then
    echo "WARNING: no recorded path for $host (StopRecord returned empty) — skipping fetch" >&2
    return 1
  fi
  # Windows paths use backslashes; take the trailing component as the filename.
  local name="${host_path//\\//}"; name="${name##*/}"
  local url="http://${host}:${WIN_HTTP_PORT}/${name}"
  echo "    fetch $host: $url → $dest"
  if curl -fsS --max-time 600 -o "$dest" "$url"; then
    local sz; sz=$(stat -c%s "$dest" 2>/dev/null || echo 0)
    echo "    ok: $dest ($sz bytes)"
    [ "$sz" -gt 0 ] || { echo "ERROR: fetched $dest is empty" >&2; return 1; }
  else
    echo "ERROR: could not fetch $url — is the http.server (port $WIN_HTTP_PORT) running on $host," >&2
    echo "       serving the OBS record directory that contains $name?" >&2
    return 1
  fi
}

[ "$#" -eq 6 ] || { echo "usage: $0 <strihHost> <strihPath> <strihDest> <streamHost> <streamPath> <streamDest>" >&2; exit 2; }
rc=0
fetch_one "$1" "$2" "$3" || rc=1
fetch_one "$4" "$5" "$6" || rc=1
exit "$rc"
