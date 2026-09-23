#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure-ish functions + a few sibling-lib sources, no top-level
# statements that act) -- matches the scripts/lib/*.sh convention (camera-box-restart-verify.sh /
# rig-mode-state.sh / obs-watchdog-decision.sh) of deliberately NOT setting `set -euo pipefail`
# here: sourcing this file executes it in the CALLER's shell, so strict mode here would leak into
# whichever caller sources it. The caller (the E2E cleanup(), deferred wiring) already sets its own
# `set -euo pipefail`; every orchestrator step below is guarded so it can NEVER abort that caller.
#
# scripts/lib/ndi-cadence-heal.sh -- #1203 item (a)+(c) helper: after a MASS cambox restart, 2-3
# strih NDI receivers can come back at HALF the sender's cadence (recv-timing #797 cap_avg ~33ms at
# a 60fps sender = 30fps; genlock FIFO starving) even while the camboxes emit 60/60 and locked=1.
# The single idle->restore RECEIVER reattach heals SOME cases but not all -- what heals every case
# is a SENDER restart. This lib drives the two-arm cure from the pure cure_plan escalation
# (idle-restore -> sender-restart -> escalate) and is REPORT-ONLY in outcome (it ALWAYS exits 0,
# never fails the caller -- the #1133 class), so the E2E cleanup() can call it to hand the rig back
# with verified 60fps receivers, a machine reading in place of today's human "read all seven cap_avg".
#
# THREE public functions:
#   ndi_cadence_read <strih_host> [only_input]
#       -> stdout: one TSV line per configured input `name<TAB>verdict<TAB>fps<TAB>cap_avg<TAB>exp`,
#          reusing ndi_halving_decision.py `analyze` (the #797 tap parser -- NEVER a 2nd parser) over
#          the ONE strih OBS-log read from NDI_CADENCE_FETCH_CMD (seam; default = ssh log tail).
#          `only_input` restricts the read to that one input (a bounded per-input re-check).
#   ndi_cadence_heal_input <input> <arm> [strih_host]
#       -> idle-restore: NDI_CADENCE_IDLE_CMD (default obs_phase2 idle-receiver -> restore);
#          sender-restart: NDI_CADENCE_SENDER_CMD (default: ssh the sender cambox, restart
#          camera-box + verify via camera_box_verify_active_cmds); return 0 = attempted.
#   ndi_cadence_verify_and_heal <strih_host>
#       -> the orchestrator: read all inputs; for each HALVED one, in PARALLEL and BOUNDED
#          (<= NDI_CADENCE_MAX_ARMS arms x <= NDI_CADENCE_SETTLE_S settle each), apply cure_plan,
#          re-read, log `[cleanup] ndi-cadence: <input> HALVED -> <arm> -> <result>` per arm, write
#          `ndi-cadence-<RUN>.json` into NDI_CADENCE_RUN_DIR. ALWAYS returns 0.
#
# Tier-0: pytest covers the pure cure_plan/cadence_report; tests/harness_ndi_cadence_heal_1203.rs
# drives this orchestrator with fake FETCH/IDLE/SENDER seams (settle 0, run dir a tempdir). The live
# read/heal (strih WS, cambox ssh) is verified by the SUPERVISOR on the rig -- UNVERIFIED from a lane.

_NDI_CADENCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/camera-box-restart-verify.sh
. "$_NDI_CADENCE_DIR/camera-box-restart-verify.sh"
# issue 1360: the strih OBS-log read goes through the ONE platform-resolved reader (Windows STRIH-SNV
# or Linux strih-lx).
# shellcheck source=scripts/lib/strih-log-read.sh
if [ -f "$_NDI_CADENCE_DIR/strih-log-read.sh" ]; then
  . "$_NDI_CADENCE_DIR/strih-log-read.sh"
fi

# -- config (all env-overridable) ---------------------------------------------------------------
# Watched strih inputs as `<name>|<expected_fps>`, ';'-separated (names contain spaces). Default =
# the seven cambox receivers strih pulls at 60fps (the design's live-observed `NDI camN` names). The
# EXACT OBS input names + fps are pinned when this is wired into the E2E cleanup() (item b, deferred).
NDI_CADENCE_INPUTS="${NDI_CADENCE_INPUTS:-NDI cam1|60;NDI cam2|60;NDI cam3|60;NDI cam4|60;NDI cam5|60;NDI cam6|60;NDI cam7|60}"
NDI_CADENCE_DEFAULT_FPS="${NDI_CADENCE_DEFAULT_FPS:-60}"
NDI_CADENCE_SETTLE_S="${NDI_CADENCE_SETTLE_S:-45}"
NDI_CADENCE_MAX_ARMS="${NDI_CADENCE_MAX_ARMS:-2}"
NDI_CADENCE_RUN_ID="${NDI_CADENCE_RUN_ID:-adhoc}"
NDI_CADENCE_RUN_DIR="${NDI_CADENCE_RUN_DIR:-.}"
NDI_CADENCE_DECIDE="${NDI_CADENCE_DECIDE:-$_NDI_CADENCE_DIR/../ndi_halving_decision.py}"
NDI_CADENCE_OBS_PHASE2="${NDI_CADENCE_OBS_PHASE2:-$_NDI_CADENCE_DIR/../obs_phase2.py}"
NDI_CADENCE_OBS_WS_PW="${NDI_CADENCE_OBS_WS_PW:-${OBS_PASSWORD:-}}"
NDI_CADENCE_OBS_WS_CALL_TIMEOUT_S="${NDI_CADENCE_OBS_WS_CALL_TIMEOUT_S:-40}"
NDI_CADENCE_OBS_LOG_TAIL="${NDI_CADENCE_OBS_LOG_TAIL:-800}"
# receiver (strih) ssh -- for the default OBS-log fetch.
NDI_CADENCE_RECV_SSH_USER="${NDI_CADENCE_RECV_SSH_USER:-newlevel}"
NDI_CADENCE_RECV_SSH_PW="${NDI_CADENCE_RECV_SSH_PW:-newlevel}"
# sender (cambox) ssh -- for the default sender-restart arm.
NDI_CADENCE_SENDER_SSH_USER="${NDI_CADENCE_SENDER_SSH_USER:-root}"
NDI_CADENCE_SENDER_SSH_PW="${NDI_CADENCE_SENDER_SSH_PW:-newlevel}"
NDI_CADENCE_SSH_TIMEOUT="${NDI_CADENCE_SSH_TIMEOUT:-30}"
NDI_CADENCE_SSH_OPTS="${NDI_CADENCE_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"

# -- helpers ------------------------------------------------------------------------------------

# _ndi_cadence_interval <expected_fps> -> the frame interval in ms (1000/fps), "0" for a bad fps.
_ndi_cadence_interval() {
  LC_ALL=C awk -v e="${1:-0}" 'BEGIN{ e=e+0; if(e<=0){print "0"} else {printf "%.4f", 1000.0/e} }'
}

# _ndi_cadence_fetch <host> -> stdout: the RAW strih OBS-log tail (all inputs' recv-timing lines),
# or EMPTY on a failed/absent read. Overridable via NDI_CADENCE_FETCH_CMD (<host>, stdout=raw log).
_ndi_cadence_fetch() {
  local host="$1"
  if [ -n "${NDI_CADENCE_FETCH_CMD:-}" ]; then
    $NDI_CADENCE_FETCH_CMD "$host" 2>/dev/null || true
    return 0
  fi
  # default: a flat ssh OBS-log tail (session-agnostic file read, per win-ssh-vs-mcp) through the
  # shared platform-resolved reader (issue 1360): plain `tail` on a Linux strih, the cmd.exe-proof
  # -EncodedCommand PowerShell `-Tail` on a Windows one (the #1259 root cause). Any failure -> empty.
  command -v strih_log_tail >/dev/null 2>&1 || { return 0; }
  local tail
  tail="$(ps_clamp_numeric "$NDI_CADENCE_OBS_LOG_TAIL" 800 2>/dev/null || echo 800)"
  strih_log_tail "$host" "$NDI_CADENCE_RECV_SSH_USER" "$NDI_CADENCE_RECV_SSH_PW" "$tail" \
    "$NDI_CADENCE_SSH_TIMEOUT" 2>/dev/null || true
}

# ndi_cadence_read <host> [only_input] -> per-input TSV verdicts (see the header).
ndi_cadence_read() {
  local host="${1:-}" only="${2:-}"
  local raw
  raw="$(_ndi_cadence_fetch "$host")" || raw=""
  local old_ifs="$IFS" spec name exp out verdict fps cap had_noglob=0
  # Snapshot the caller's noglob state so `set -f`/`set +f` here can NEVER silently re-enable
  # globbing in a caller that had it OFF (#1203 review 🔵3) -- we only restore +f if the caller
  # did not already have -f set.
  case $- in *f*) had_noglob=1 ;; esac
  local -a specs=()
  set -f
  IFS=';'
  for spec in $NDI_CADENCE_INPUTS; do specs+=("$spec"); done
  IFS="$old_ifs"
  [ "$had_noglob" -eq 1 ] || set +f
  for spec in "${specs[@]}"; do
    if [ "${spec%%|*}" = "$spec" ]; then
      name="$spec"
      exp="$NDI_CADENCE_DEFAULT_FPS"
    else
      name="${spec%|*}"
      exp="${spec##*|}"
    fi
    # trim leading/trailing whitespace off the name.
    name="${name#"${name%%[![:space:]]*}"}"
    name="${name%"${name##*[![:space:]]}"}"
    [ -n "$name" ] || continue
    if [ -n "$only" ] && [ "$name" != "$only" ]; then
      continue
    fi
    out="$(printf '%s' "$raw" | python3 "$NDI_CADENCE_DECIDE" analyze \
      --source "$name" --expected-fps "$exp" --box-reachable 1 --expected-live 1 2>/dev/null)" || out=""
    verdict="$(printf '%s\n' "$out" | sed -n 's/^verdict=//p' | tail -1)"
    fps="$(printf '%s\n' "$out" | sed -n 's/^fps=//p' | tail -1)"
    cap="$(printf '%s\n' "$out" | sed -n 's/^cap_avg=//p' | tail -1)"
    [ -n "$verdict" ] || verdict="UNKNOWN"
    printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$verdict" "$fps" "$cap" "$exp"
  done
  return 0
}

# _ndi_cadence_default_idle <host> <input> -- the idle-receiver -> restore RECEIVER reattach.
_ndi_cadence_default_idle() {
  local host="$1" input="$2"
  local idle_out prev
  idle_out="$(timeout "$NDI_CADENCE_OBS_WS_CALL_TIMEOUT_S" python3 "$NDI_CADENCE_OBS_PHASE2" \
    idle-receiver --host "$host" --password "$NDI_CADENCE_OBS_WS_PW" --input "$input" 2>&1)" || idle_out=""
  prev="$(printf '%s\n' "$idle_out" | sed -n 's/^PREV_NDI_NAME=//p' | head -1)"
  [ -n "$prev" ] || return 1
  timeout "$NDI_CADENCE_OBS_WS_CALL_TIMEOUT_S" python3 "$NDI_CADENCE_OBS_PHASE2" \
    idle-receiver --host "$host" --password "$NDI_CADENCE_OBS_WS_PW" --input "$input" --restore "$prev" >/dev/null 2>&1
}

# _ndi_cadence_default_sender <input> [host] -- restart the SENDER cambox + verify it came back.
_ndi_cadence_default_sender() {
  local input="$1"
  local cam
  cam="$(printf '%s' "$input" | grep -oE 'cam[0-9]+' | head -1)" || cam=""
  if [ -z "$cam" ]; then
    printf '[cleanup] ndi-cadence: sender-restart target for %s not resolvable (no camN token)\n' "$input" >&2
    return 1
  fi
  local verify
  verify="$(camera_box_verify_active_cmds "$cam (sender)")" || verify=""
  # shellcheck disable=SC2086
  timeout "$NDI_CADENCE_SSH_TIMEOUT" sshpass -p "$NDI_CADENCE_SENDER_SSH_PW" ssh $NDI_CADENCE_SSH_OPTS \
    "$NDI_CADENCE_SENDER_SSH_USER@$cam" "systemctl restart camera-box 2>/dev/null; true
$verify" >&2 2>&1 || return 1
  return 0
}

# ndi_cadence_heal_input <input> <arm> [host] -- dispatch one arm; return 0 = attempted.
ndi_cadence_heal_input() {
  local input="$1" arm="$2" host="${3:-}"
  case "$arm" in
    idle-restore)
      if [ -n "${NDI_CADENCE_IDLE_CMD:-}" ]; then
        $NDI_CADENCE_IDLE_CMD "$host" "$input"
        return $?
      fi
      _ndi_cadence_default_idle "$host" "$input"
      return $?
      ;;
    sender-restart)
      if [ -n "${NDI_CADENCE_SENDER_CMD:-}" ]; then
        $NDI_CADENCE_SENDER_CMD "$input" "$host"
        return $?
      fi
      _ndi_cadence_default_sender "$input" "$host"
      return $?
      ;;
    *)
      return 0
      ;;
  esac
}

# _ndi_cadence_heal_one <host> <input> <fps> <cap> <exp> <settle> <max_arms>
#   Runs the two-arm escalation for ONE halved input; logs one line per arm to stderr; prints the
#   final per-input TSV record to stdout. Runs in a subshell (backgrounded by the orchestrator).
_ndi_cadence_heal_one() {
  local host="$1" input="$2" fps="$3" cap="$4" exp="$5" settle="$6" max_arms="$7"
  local interval attempt plan last_arm="escalate" result="still-halved" rr rverdict
  interval="$(_ndi_cadence_interval "$exp")" || interval="0"
  for attempt in $(seq 1 "$max_arms"); do
    plan="$(python3 "$NDI_CADENCE_DECIDE" cure-plan --attempt "$attempt" --cap-avg-ms "${cap:-0}" --expected-ms "$interval" 2>/dev/null | sed -n 's/^plan=//p' | tail -1)" || plan=""
    [ -n "$plan" ] || plan="escalate"
    if [ "$plan" = "escalate" ]; then
      last_arm="escalate"
      result="escalated"
      break
    fi
    last_arm="$plan"
    ndi_cadence_heal_input "$input" "$plan" "$host" || true
    if [ "${settle:-0}" != "0" ]; then
      sleep "$settle" || true
    fi
    rr="$(ndi_cadence_read "$host" "$input")" || rr=""
    rverdict="$(printf '%s' "$rr" | awk -F'\t' 'NF>=2{print $2}' | tail -1)" || rverdict=""
    if [ "$rverdict" = "HEALTHY" ]; then
      result="healed"
      printf '[cleanup] ndi-cadence: %s HALVED -> %s -> healed\n' "$input" "$plan" >&2
      break
    fi
    printf '[cleanup] ndi-cadence: %s HALVED -> %s -> still-halved\n' "$input" "$plan" >&2
  done
  if [ "$result" != "healed" ]; then
    last_arm="escalate"
    result="escalated"
    printf '[cleanup] ndi-cadence: %s HALVED -> escalate -> needs manual\n' "$input" >&2
  fi
  printf '%s\tHALVED\t%s\t%s\t%s\t%s\t%s\n' "$input" "$fps" "$cap" "$exp" "$last_arm" "$result"
}

# ndi_cadence_verify_and_heal <strih_host> -- the orchestrator. ALWAYS returns 0.
ndi_cadence_verify_and_heal() {
  local host="${1:-}"
  local run_id="${NDI_CADENCE_RUN_ID:-adhoc}"
  local run_dir="${NDI_CADENCE_RUN_DIR:-.}"
  local settle="${NDI_CADENCE_SETTLE_S:-45}"
  local max_arms="${NDI_CADENCE_MAX_ARMS:-2}"
  mkdir -p "$run_dir" 2>/dev/null || true
  local workdir
  workdir="$(mktemp -d "${TMPDIR:-/tmp}/ndi-cadence.XXXXXX" 2>/dev/null || true)"
  [ -n "$workdir" ] || workdir="$run_dir/.ndi-cadence-work.$$"
  mkdir -p "$workdir" 2>/dev/null || true

  local reads
  reads="$(ndi_cadence_read "$host")" || reads=""

  local name verdict fps cap exp idx=0 recfile
  while IFS=$'\t' read -r name verdict fps cap exp; do
    [ -n "$name" ] || continue
    idx=$((idx + 1))
    recfile="$workdir/rec.$idx"
    if [ "$verdict" = "HALVED" ]; then
      (_ndi_cadence_heal_one "$host" "$name" "$fps" "$cap" "$exp" "$settle" "$max_arms" >"$recfile" 2>>"$workdir/log") &
    else
      # A non-HALVED read (HEALTHY/UNKNOWN/BORDERLINE/SKIP) took no action -- record its OWN verdict
      # as the result rather than a blanket "ok" that would overstate an UNKNOWN/BORDERLINE input's
      # health in the telemetry (#1203 review 🔵4). It carries no arm; counts stay verdict-driven.
      printf '%s\t%s\t%s\t%s\t%s\t%s\tno-action:%s\n' "$name" "$verdict" "$fps" "$cap" "$exp" "" "$verdict" >"$recfile" 2>/dev/null || true
    fi
  done <<<"$reads"
  wait || true

  # replay the per-arm log lines (in launch order is not guaranteed across parallel workers, but
  # every line is self-identifying by input name), then assemble the telemetry TSV in launch order.
  [ -f "$workdir/log" ] && cat "$workdir/log" >&2 || true
  local tsv="$workdir/records.tsv" i
  : >"$tsv" 2>/dev/null || true
  for i in $(seq 1 "$idx"); do
    [ -f "$workdir/rec.$i" ] && cat "$workdir/rec.$i" >>"$tsv" 2>/dev/null || true
  done
  python3 "$NDI_CADENCE_DECIDE" cadence-report --run-id "$run_id" --host "$host" \
    <"$tsv" >"$run_dir/ndi-cadence-$run_id.json" 2>/dev/null || true

  rm -rf "$workdir" 2>/dev/null || true
  return 0
}
