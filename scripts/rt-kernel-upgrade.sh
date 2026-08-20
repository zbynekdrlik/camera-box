#!/usr/bin/env bash
# scripts/rt-kernel-upgrade.sh -- issue 899: DRY-RUN planner for the PREEMPT_RT kernel upgrade of a
# single cam box. See the extended header below.
set -euo pipefail
#
# ISSUE 899 (lane 2). The cam fleet runs a stock PREEMPT_DYNAMIC kernel, not PREEMPT_RT (defect 1);
# the chosen kernel is linux-image-realtime. Installing it is a REBOOT-CLASS per-box change on a
# `ro`-root appliance with issue-295/547 brick-hardening + the single-kernel invariant
# (verify-device.sh check (k)) -- the SUPERVISOR's coordinated deploy step, ONE box at a time.
#
# This tool NEVER mutates a box. It reads a box's state READ-ONLY over ssh and prints the exact,
# drift-aware, atomic upgrade PLAN (from scripts/lib/rt-kernel-plan.sh, the pure + unit-tested
# decision logic). `--commands` additionally prints the concrete shell the supervisor runs per step.
# The apply/reboot is the operator's reviewed step per docs/runbooks/899-realtime-isolation.md --
# there is deliberately NO apply mode here (enable-only doctrine: the code plans, the supervisor
# applies). Mirrors the DRY-RUN-default shape of scripts/strih-recordings-retention.ps1.
#
# Usage:
#   scripts/rt-kernel-upgrade.sh --box <ip> [--commands]      # read the box read-only, print its plan
#   scripts/rt-kernel-upgrade.sh --facts "RUN INST PRO GEN GRUB" [--commands]  # offline, no ssh
#   scripts/rt-kernel-upgrade.sh --help
# SSH password override: RT_KERNEL_SSH_PASS (default `newlevel`, the fleet-wide committed default).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/rt-kernel-plan.sh
. "$HERE/lib/rt-kernel-plan.sh"

SSH_PW="${RT_KERNEL_SSH_PASS:-newlevel}"
SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=8 -o BatchMode=no"
SSH_TIMEOUT="${RT_KERNEL_SSH_TIMEOUT:-20}"   # overall session bound (ConnectTimeout bounds connect only)

usage() {
  cat <<'USAGE'
scripts/rt-kernel-upgrade.sh -- issue 899 DRY-RUN PREEMPT_RT kernel upgrade planner (one cam box).
  --box <ip>                    read the box READ-ONLY over ssh, print its atomic upgrade plan
  --box <ip> --commands         also print the concrete shell the supervisor runs per step
  --facts "RUN INST PRO GEN GRUB [CAND]"   offline (no ssh); each 1/0, GRUB is `saved` or a number
  --help
NEVER mutates a box. Apply/reboot is the supervisor's reviewed step (docs/runbooks/899-realtime-isolation.md).
SSH password override: RT_KERNEL_SSH_PASS (default `newlevel`, the fleet-wide committed default).
USAGE
}

require_tools() {
  local miss=()
  for t in sshpass ssh timeout; do command -v "$t" >/dev/null 2>&1 || miss+=("$t"); done
  if [ "${#miss[@]}" -gt 0 ]; then
    echo "FATAL: missing required tool(s): ${miss[*]} (apt-get install -y sshpass openssh-client)" >&2
    exit 3
  fi
}

# gather_facts IP -> echoes the 5 planner args "RUN INST PRO GEN GRUBDEF" from READ-ONLY box reads.
gather_facts() {
  local ip="$1" raw
  require_tools
  # ONE ssh round-trip, all reads non-mutating, bounded by an overall session timeout. The remote
  # collapses GRUB_DEFAULT to its FIRST whitespace-delimited token (${gd%% *}) so a titled value
  # (e.g. "Advanced options...") can never shift the space-delimited 6-field split below.
  raw="$(timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "root@$ip" '
    rt=0; { grep -q PREEMPT_RT /proc/version || [ "$(cat /sys/kernel/realtime 2>/dev/null)" = "1" ]; } && rt=1
    inst=0; dpkg-query -W -f="\${Status}" linux-image-realtime 2>/dev/null | grep -q "install ok installed" && inst=1
    pro=0; command -v pro >/dev/null 2>&1 && pro status --format json 2>/dev/null | grep -q "\"attached\": *true" && pro=1
    gen=0; dpkg-query -W -f="\${Status}" linux-image-generic 2>/dev/null | grep -q "install ok installed" && gen=1
    cand=0; apt-cache policy linux-image-realtime 2>/dev/null | grep -qE "Candidate: [0-9]" && cand=1
    gd="$(sed -nE "s/^GRUB_DEFAULT=(.*)/\1/p" /etc/default/grub 2>/dev/null | tr -d "\"" | head -1)"
    gd="${gd:-0}"; gd="${gd%% *}"
    echo "$rt $inst $pro ${gen} ${gd} ${cand}"
  ' 2>/dev/null)" || { echo "FATAL: could not read box $ip over ssh (rc=$?)" >&2; exit 4; }
  raw="$(printf '%s' "$raw" | tr -s ' ' | tail -1)"
  [ -n "$raw" ] || { echo "FATAL: empty facts from box $ip" >&2; exit 4; }
  printf '%s' "$raw"
}

emit_plan() {
  local facts="$1" commands="$2"
  # shellcheck disable=SC2086  # facts is a deliberate 5-word split
  set -- $facts
  local run="${1:-}" inst="${2:-}" pro="${3:-}" gen="${4:-}" gd="${5:-0}" cand="${6:-1}"
  echo "# facts: running_rt=$run rt_installed=$inst pro_attached=$pro generic_present=$gen grub_default=$gd rt_candidate=$cand"
  echo "# readiness: $(rt_kernel_readiness_verdict "$run" "$cand" "$pro")"
  echo "# kernel choice: $(rt_kernel_flavour)"
  echo "# ---- atomic per-box plan (SUPERVISOR applies; reboot-class, one box at a time) ----"
  local plan tok
  plan="$(rt_kernel_upgrade_plan "$run" "$inst" "$pro" "$gen" "$gd" "$cand")"
  while IFS= read -r tok; do
    [ -n "$tok" ] || continue
    if [ "$commands" = "1" ]; then
      printf '%-26s %s\n' "$tok" "$(rt_kernel_step_command "$tok")"
    else
      printf '%s\n' "$tok"
    fi
  done <<< "$plan"
}

main() {
  local mode="" arg="" commands=0
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)      mode="box";   arg="${2:-}"; shift 2 ;;
      --facts)    mode="facts"; arg="${2:-}"; shift 2 ;;
      --commands) commands=1;   shift ;;
      -h|--help)  usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
    esac
  done
  local facts=""
  case "$mode" in
    box)   [ -n "$arg" ] || { echo "FATAL: --box needs an IP" >&2; exit 2; }; facts="$(gather_facts "$arg")" ;;
    facts) [ -n "$arg" ] || { echo "FATAL: --facts needs 5 values" >&2; exit 2; }; facts="$arg" ;;
    *)     usage; exit 2 ;;
  esac
  emit_plan "$facts" "$commands"
}

# Only run main when executed, not when sourced by the test harness.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
