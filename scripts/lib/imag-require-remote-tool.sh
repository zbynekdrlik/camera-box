#!/bin/bash
# #833 -- a MISSING tool on imag-nb must never be read as a MEASURED zero/empty result.
set -euo pipefail
#
# The #756 projector-count preflight shells out to `wmctrl -l` on imag-nb over SSH; when wmctrl
# is ABSENT (a freshly (re)provisioned box, #791), the remote command emits nothing, the local
# `grep -c` on that empty output reads 0, and recording-e2e.sh reported "Multiview=0 Program=0 --
# stray projector windows are accumulating" while BOTH projectors were in fact open and correct
# -- three wasted hardware-gate re-runs before the missing tool was suspected. The same shape
# hits the [1/8] MV render-divisor capability check (`nm -D -u /usr/bin/obs | grep -c ...`): a
# missing `nm` (binutils) reads as "MV divisor capability MISSING -- #756 regression" and
# hard-fails by default.
#
# This is the SAME class `imag_require_tools()` already fixed in scripts/setup-imag.sh (#822) --
# but that one runs LOCALLY on the imag box during provisioning (plain `command -v`). This helper
# is its REMOTE counterpart: recording-e2e.sh runs on dev1 and must check tool presence ON THE
# REMOTE box over SSH, so the check itself has to travel there and back, not just run in-process.
#
# Usage (mirrors scripts/lib/imag-projector-heal.sh's $(...)-embedding pattern):
#   probe="$(sshpass -p "$PW" ssh ... "$HOST" "$(imag_require_remote_tool_cmd wmctrl)" 2>/dev/null || true)"
#   missing="$(imag_remote_tool_probe_missing "$probe")"
#   [ -z "$missing" ] || { echo "ERROR: ... missing: $missing ..." >&2; exit 1; }

# imag_require_remote_tool_cmd TOOL... -> prints a REMOTE bash snippet (to be embedded via $(...)
# into an ssh command string) that checks EACH named tool is on the box's PATH and prints one
# "TOOL_MISSING:<name>" line per absent tool -- nothing at all when every tool is present. The
# remote snippet itself always exits 0 (the CALLER decides what a non-empty probe means), so a
# `|| true` wrapped around the ssh call can never mask THIS check's own verdict.
# shellcheck disable=SC2016  # the snippet expands $t/$missing REMOTELY, not here
imag_require_remote_tool_cmd() {
  local t
  echo 'missing=""'
  for t in "$@"; do
    printf 'command -v %s >/dev/null 2>&1 || missing="$missing %s"\n' "$t" "$t"
  done
  cat <<'EOF'
for _rrt_m in $missing; do echo "TOOL_MISSING:$_rrt_m"; done
EOF
}

# imag_remote_tool_probe_missing PROBE_OUTPUT -> prints the space-joined list of tool names the
# probe (imag_require_remote_tool_cmd's remote stdout) reported missing; empty string when none.
# Pure text parsing, no ssh -- testable with a synthetic PROBE_OUTPUT fixture, no rig required.
imag_remote_tool_probe_missing() {
  printf '%s\n' "$1" | sed -n 's/^TOOL_MISSING://p' | tr '\n' ' ' | sed 's/ *$//'
}
