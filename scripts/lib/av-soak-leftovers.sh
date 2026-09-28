#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs this in
# the CALLER's shell (scripts/av-soak.sh, which sets its own strict mode). The function is safe
# under the caller's `set -euo pipefail`.
#
# scripts/lib/av-soak-leftovers.sh -- issue 1367: the soak's --stop-leftovers mode.
#
# `scripts/av-soak.sh --stop-leftovers <run-dir>` is the unit's ExecStopPost safety net: after the
# soak itself is gone (a SIGKILL, a cleanup that could not stop a recording, a broadcast that kept
# it from stopping one) it stops what the soak provably left and releases the soak's lease.
#
# The decision is the pure `scripts/av_soak_rig_state.py leftovers` plan over one (settled)
# `obs_phase2.py rig-busy-check` read and the run's own <run-dir>/recording.state:
#   - nothing is touched while ANY box streams (strih never streams, so strih "recording, not
#     streaming" can be the broadcast's own recording) or while a box is unreadable;
#   - a flagged box's recording is stopped only when its own age puts its start at the soak's flag
#     time (the window written into recording.state by the run);
#   - everything else is kept, with the reason.
# An unreadable rig-busy read is retried (AV_SOAK_BROADCAST_READS, default 3, AV_SOAK_BROADCAST_RETRY_S,
# default 20 s apart) before it counts.
# Exit: 0 nothing left (the soak's lease released, holder-checked -- a lease another run holds is
# never touched), 5 something kept (the lease stays held), 4 the soak's own process still runs,
# 3 the plan could not be made.
#
# Needs scripts/lib/rig-lease.sh (rig_lease_release) and scripts/lib/av-soak.sh
# (av_soak_rig_busy_settled) sourced by the caller. Source-only: sourcing
# defines the function and runs nothing.

_av_soak_leftovers_log() { printf '%s [av-soak] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

# av_soak_stop_leftovers RUN_DIR OBS_DIR STRIH_HOST STREAM_HOST OBS_TIMEOUT_S RIG_STATE_PY
av_soak_stop_leftovers() {
  local run_dir="$1" obs_dir="$2" strih_host="$3" stream_host="$4" obs_timeout="$5" rig_state="$6"
  local state="$run_dir/recording.state" pid busy plan rc=0 box action reason host lease
  local reads="${AV_SOAK_BROADCAST_READS:-3}" gap="${AV_SOAK_BROADCAST_RETRY_S:-20}"
  if [ ! -f "$state" ]; then
    _av_soak_leftovers_log "stop-leftovers: no $state -- nothing to do"
    return 0
  fi
  pid="$(head -n 1 "$run_dir/pid" 2>/dev/null || true)"
  case "$pid" in
    '' | *[!0-9]*) ;;
    *)
      if kill -0 "$pid" 2>/dev/null && grep -qa 'av-soak\.sh' "/proc/$pid/cmdline" 2>/dev/null; then
        echo "av-soak: ERROR: the soak (pid $pid) is still running -- stop it first (touch $run_dir/STOP); --stop-leftovers only cleans up after it" >&2
        return 4
      fi
      ;;
  esac
  # an unreadable read is retried (AV_SOAK_BROADCAST_READS x AV_SOAK_BROADCAST_RETRY_S) before the
  # plan keeps everything for it -- a stream OBS restart must not leave the leftover running
  busy="$(av_soak_rig_busy_settled "$obs_dir" "$strih_host" "$stream_host" "$obs_timeout" "$rig_state" \
    "$reads" "$gap")"
  if ! plan="$(printf '%s' "$busy" | python3 "$rig_state" leftovers --state "$state" --now "$(date +%s)" \
      --start-window-s "$((obs_timeout + 30))")"; then
    echo "av-soak: ERROR: the stop-leftovers plan could not be made from $state" >&2
    return 3
  fi
  while IFS=$'\t' read -r box action reason; do
    [ -n "$box" ] || continue
    if [ "$box" = strih ]; then host="$strih_host"; else host="$stream_host"; fi
    case "$action" in
      stop)
        timeout "$obs_timeout" python3 "$obs_dir/obs_phase2.py" record --host "$host" --action stop >/dev/null 2>&1 || true
        if timeout "$obs_timeout" python3 "$obs_dir/obs_phase2.py" record --host "$host" --action status 2>/dev/null \
            | grep -q '^active=False'; then
          sed -i "s/^${box}=1\$/${box}=0/" "$state"
          _av_soak_leftovers_log "stop-leftovers: stopped $reason on $box"
        else
          _av_soak_leftovers_log "WARNING: stop-leftovers: the $box recording did not stop -- stop it by hand"
          rc=5
        fi
        ;;
      clear)
        sed -i "s/^${box}=1\$/${box}=0/" "$state"
        _av_soak_leftovers_log "stop-leftovers: $box is not recording"
        ;;
      *)
        _av_soak_leftovers_log "stop-leftovers: NOT stopping $box -- $reason"
        rc=5
        ;;
    esac
  done <<<"$plan"
  lease="$(sed -n '/^lease=/{s/^lease=//p;q;}' "$state")"
  if [ "$rc" = 0 ] && ! grep -q '^\(strih\|stream\)=1$' "$state"; then
    if [ -n "$lease" ]; then
      rig_lease_release "$lease" >/dev/null 2>&1 || true
      _av_soak_leftovers_log "stop-leftovers: rig lease released ($lease; a lease another run holds is never touched)"
    fi
  elif [ -n "$lease" ]; then
    _av_soak_leftovers_log "stop-leftovers: rig lease KEPT ($lease) -- a recording the soak may have left is still there"
  fi
  return "$rc"
}
