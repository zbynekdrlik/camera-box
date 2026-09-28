#!/usr/bin/env bash
# Issue 1389: RUN scripts/ndi-discovery-laptop.ps1 against scratch %ProgramData% dirs (see below).
set -euo pipefail

# The Windows writer of the NDI extra-IP list (stream, resolume, laptops) MERGES into an existing
# config and keeps the machine's own entries. Issue 1389 made it also REMOVE every cambox IP (a
# remote finder with a cambox on its list holds a discovery connection into that cambox, and libndi
# 6.3.2 aborts camera-box when it closes). This runner executes the real script, not its text:
#   1. an issue-1342 config (cambox IPs + an own entry + another key) -> cambox IPs gone, own entry
#      and other key kept, the fleet senders added, a backup left, no BOM;
#   2. a -Ips list naming a cambox -> refused, the file untouched;
#   3. -DryRun -> nothing written, the removal announced;
#   4. no config yet -> exactly the fleet senders.
# The expected lists come from the ONE generator (scripts/lib/ndi-discovery.sh --ips pinned /
# --cambox-ips), never retyped here.
#
# Needs pwsh: $PWSH, else `pwsh` on PATH. dev1 has none; unpack the powershell-7.x-linux-x64
# release tarball into a scratch dir and point PWSH at it. A missing pwsh FAILS, never skips.
# Exit 0 = every case ok, 1 = a case failed, 2 = no pwsh. Not in CI (no pwsh on the runner).
NDI_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NDI_TEST_ROOT="$(cd "$NDI_TEST_DIR/../.." && pwd)"
PS1="$NDI_TEST_ROOT/scripts/ndi-discovery-laptop.ps1"
LIB="$NDI_TEST_ROOT/scripts/lib/ndi-discovery.sh"
PWSH="${PWSH:-$(command -v pwsh || true)}"
if [ -z "$PWSH" ] || [ ! -x "$PWSH" ]; then
  echo "FAIL: no pwsh (set PWSH=/path/to/pwsh; dev1 has none -- unpack the powershell-7.x-linux-x64 release tarball)" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FLEET="$(bash "$LIB" --ips pinned)"
CAMBOXES="$(bash "$LIB" --cambox-ips)"
fails=0
check() { # check NAME ACTUAL EXPECTED
  if [ "$2" = "$3" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1: got '$2', want '$3'"
    fails=$((fails + 1))
  fi
}
ips_of() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["ndi"]["networks"]["ips"])' "$1"; }
run_ps1() { # run_ps1 PROGRAMDATA ARGS... -> the script's exit code, output in $WORK/out
  local rc=0
  ProgramData="$1" "$PWSH" -NoProfile -NonInteractive -File "$PS1" "${@:2}" >"$WORK/out" 2>&1 || rc=$?
  return "$rc"
}
seed() { # seed PROGRAMDATA IPS
  mkdir -p "$1/NDI"
  printf '{"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": "%s"}}}\n' "$2" >"$1/NDI/ndi-config.v1.json"
}

# 1. an issue-1342 config on a machine with one own entry
seed "$WORK/c1" "10.1.2.3,$CAMBOXES,$FLEET"
rc=0; run_ps1 "$WORK/c1" || rc=$?
check "1: exit code" "$rc" 0
check "1: networks.ips" "$(ips_of "$WORK/c1/NDI/ndi-config.v1.json")" "10.1.2.3,$FLEET"
check "1: other key kept" "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ndi"]["groups"]["recv"])' "$WORK/c1/NDI/ndi-config.v1.json")" Public
bom="$(head -c 3 "$WORK/c1/NDI/ndi-config.v1.json" | od -An -tx1 | tr -d ' \n')"
check "1: no BOM" "$([ "$bom" != efbbbf ] && echo none || echo BOM)" none
check "1: one backup" "$(find "$WORK/c1/NDI" -name 'ndi-config.v1.json.bak-*' | wc -l | tr -d ' ')" 1
check "1: removal named" "$(grep -c 'removed the cambox' "$WORK/out" || true)" 1

# 2. a -Ips list naming a cambox is refused and the file is left alone
seed "$WORK/c2" "$FLEET"
before="$(cat "$WORK/c2/NDI/ndi-config.v1.json")"
rc=0; run_ps1 "$WORK/c2" -Ips "$FLEET,${CAMBOXES%%,*}" || rc=$?
check "2: refused" "$([ "$rc" -ne 0 ] && echo yes || echo no)" yes
check "2: file untouched" "$(cat "$WORK/c2/NDI/ndi-config.v1.json")" "$before"
check "2: names issue 1389" "$(grep -c 'issue 1389' "$WORK/out" || true)" 1

# 3. -DryRun writes nothing and announces the removal
seed "$WORK/c3" "$CAMBOXES,$FLEET"
before="$(cat "$WORK/c3/NDI/ndi-config.v1.json")"
rc=0; run_ps1 "$WORK/c3" -DryRun || rc=$?
check "3: exit code" "$rc" 0
check "3: file untouched" "$(cat "$WORK/c3/NDI/ndi-config.v1.json")" "$before"
check "3: would remove" "$(grep -c 'would remove the cambox' "$WORK/out" || true)" 1

# 4. no config yet
mkdir -p "$WORK/c4"
rc=0; run_ps1 "$WORK/c4" || rc=$?
check "4: exit code" "$rc" 0
check "4: networks.ips" "$(ips_of "$WORK/c4/NDI/ndi-config.v1.json")" "$FLEET"

if [ "$fails" -ne 0 ]; then
  echo "--- last script output ---"
  cat "$WORK/out"
  exit 1
fi
echo "all cases ok"
