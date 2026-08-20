#!/usr/bin/env bash
# scripts/rt-kernel-upgrade.sh -- issue 899: DRY-RUN planner for the low-latency kernel upgrade of a
# single cam box. See the extended header below.
set -euo pipefail
#
# ISSUE 899. The cam fleet runs a stock PREEMPT_DYNAMIC kernel with no full preemption (defect 1).
# OWNER DECISION (2026-08-20): Ubuntu Pro is REJECTED; STEP 1 is the FREE official-archive
# `linux-lowlatency-hwe-24.04` (a config meta that drops preempt=full via the `lowlatency-kernel`
# package -- the imag-nb precedent). Installing it is a REBOOT-CLASS per-box change on a `ro`-root
# appliance with issue-295/547 brick-hardening + the single-kernel invariant (verify-device.sh
# check (k)) -- the SUPERVISOR's coordinated deploy step, ONE box at a time, in a window with no
# live E2E. STEP 2 (custom PREEMPT_RT via CI) is a documented escalation gated on a live measurement
# (docs/runbooks/899-realtime-isolation.md); it is NOT this tool.
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
#   scripts/rt-kernel-upgrade.sh --facts "RUN INST GEN GRUB" [--commands]  # offline, no ssh
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
scripts/rt-kernel-upgrade.sh -- issue 899 DRY-RUN low-latency kernel upgrade planner (one cam box).
  --box <ip>                    read the box READ-ONLY over ssh, print its atomic upgrade plan
  --box <ip> --commands         also print the concrete shell the supervisor runs per step
  --facts "RUN INST GEN GRUB [CAND]"   offline (no ssh); each 1/0, GRUB is `saved` or a number
  --help
NEVER mutates a box. Apply/reboot is the supervisor's reviewed step (docs/runbooks/899-realtime-isolation.md).
Kernel: linux-lowlatency-hwe-24.04 (free Ubuntu main archive, no Pro -- owner decision 2026-08-20).
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

# gather_facts IP -> echoes the 4 planner args "RUN INST GEN GRUBDEF" from READ-ONLY box reads.
#   RUN  : preempt=full is the ACTIVE boot mode (the low-latency profile already running)
#   INST : the `lowlatency-kernel` config package is installed (provisioned, maybe not rebooted)
#   GEN  : the HWE generic meta (linux-image-generic-hwe-24.04) is NOT installed -> the lowlatency
#          install will pull a NEW HWE image and supersede the running one -> a purge is needed to
#          restore the single-kernel invariant. (1 = superseded-generic-will-remain, needs purge.)
#   GRUBDEF : GRUB_DEFAULT, collapsed to its first whitespace token so a titled value cannot shift
#             the space-delimited split.
gather_facts() {
  local ip="$1" raw
  require_tools
  # ONE ssh round-trip, all reads non-mutating, bounded by an overall session timeout.
  raw="$(timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "root@$ip" '
    run=0; grep -qw "preempt=full" /proc/cmdline && run=1
    inst=0; dpkg-query -W -f="\${Status}" lowlatency-kernel 2>/dev/null | grep -q "install ok installed" && inst=1
    # GEN=1 iff the HWE generic meta is NOT installed -> installing lowlatency-hwe pulls a new HWE
    # image, superseding the running one (today`s cam fleet). If the HWE meta IS installed the
    # install is config-only (no new image) -> GEN=0, no purge.
    gen=1; dpkg-query -W -f="\${Status}" linux-image-generic-hwe-24.04 2>/dev/null | grep -q "install ok installed" && gen=0
    cand=0; apt-cache policy linux-lowlatency-hwe-24.04 2>/dev/null | grep -qE "Candidate: [0-9]" && cand=1
    gd="$(sed -nE "s/^GRUB_DEFAULT=(.*)/\1/p" /etc/default/grub 2>/dev/null | tr -d "\"" | head -1)"
    gd="${gd:-0}"; gd="${gd%% *}"
    echo "$run $inst ${gen} ${gd} ${cand}"
  ' 2>/dev/null)" || { echo "FATAL: could not read box $ip over ssh (rc=$?)" >&2; exit 4; }
  raw="$(printf '%s' "$raw" | tr -s ' ' | tail -1)"
  [ -n "$raw" ] || { echo "FATAL: empty facts from box $ip" >&2; exit 4; }
  printf '%s' "$raw"
}

emit_plan() {
  local facts="$1" commands="$2"
  # shellcheck disable=SC2086  # facts is a deliberate word split
  set -- $facts
  local run="${1:-}" inst="${2:-}" gen="${3:-}" gd="${4:-0}" cand="${5:-1}"
  echo "# facts: running_lowlat=$run lowlat_installed=$inst superseded_generic=$gen grub_default=$gd rt_candidate=$cand"
  echo "# readiness: $(rt_kernel_readiness_verdict "$run" "$inst" "$cand")"
  echo "# kernel choice: $(rt_kernel_flavour)  (free Ubuntu main archive, no Pro -- STEP 1)"
  echo "# ---- atomic per-box plan (SUPERVISOR applies; reboot-class, one box at a time) ----"
  local plan tok
  plan="$(rt_kernel_upgrade_plan "$run" "$inst" "$gen" "$gd" "$cand")"
  while IFS= read -r tok; do
    [ -n "$tok" ] || continue
    if [ "$commands" = "1" ]; then
      printf '%-28s %s\n' "$tok" "$(rt_kernel_step_command "$tok")"
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
    facts) [ -n "$arg" ] || { echo "FATAL: --facts needs 4 values" >&2; exit 2; }; facts="$arg" ;;
    *)     usage; exit 2 ;;
  esac
  emit_plan "$facts" "$commands"
}

# Only run main when executed, not when sourced by the test harness.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
