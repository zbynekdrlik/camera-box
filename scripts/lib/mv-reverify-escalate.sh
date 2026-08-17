#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + REMOTE-cmd/PowerShell builders + a few
# env-overridable LOCAL runners; no top-level statements) -- deliberately NO `set -euo pipefail`
# here: sourcing this file executes it in the CALLER's shell (scripts/recording-e2e.sh, which sets
# strict mode itself), so imposing `-e` here would leak into the caller. Mirrors every sibling
# scripts/lib/*.sh source-only lib (frozen-input-health.sh, cam2-painter-restore-verify.sh, ...).
#
# scripts/lib/mv-reverify-escalate.sh -- #1093: the ORDERING PROOF + RECEIVER-WEDGE ESCALATION
# around preflight_mv_reverify() (recording-e2e.sh's sender-bounce re-verify, #758). Two remaining
# items on the ticket (the ~52s reverify BUDGET recalibration already landed in full-path-e2e.yml):
#
#   (a) ORDERING -- cam1's picture IS cam2-painter's HDMI (one camera -> splitter -> every cambox),
#       so a cam2-painter that is mid-restart when the cam-pixel probe runs reads as a FALSE dead
#       leg (run 32008897833 attempt-2: the painter's KmsPresenter came back 11s AFTER the probe
#       gave up). mv_reverify_painter_up_* proves the painter is genuinely PAINTING before the
#       cam1 probe -- ordering, not a longer blind window (no-timeout-band-aids).
#
#   (b) ESCALATION -- when the reverify still exhausts its budget, distinguish a genuinely dead
#       SOURCE from the issue-1096 RECEIVER wedge (strih's DistroAV never re-locks after a sender
#       bounce). The DISCRIMINATOR is strih's `genlock-fifo audit '<src>': received=` counter: a
#       frozen SOURCE keeps SENDING 60fps of identical frames so `received=` keeps ADVANCING; a
#       wedged RECEIVER stops it (delta absent / no recv). ONLY for the wedge does mv_reverify_or_
#       escalate restart strih OBS ONCE per run and re-check once. The restart is headless-safe:
#       force-kill obs64 + clear .sentinel over ssh (session-agnostic, win-ssh-vs-mcp Context B) and
#       let strih's session-1 NL_STARTUP.ahk respawn one clean genlock obs64 -- NEVER an ssh GUI
#       launch (obs-ops "AHK on strih"; the launch-obs-genlock.sh --force PLANNER prints a
#       session-1 program the harness cannot run headlessly).
#
# Reuses (never reinvents): the presenter-aware painting signal of cam2-painter-restore-verify.sh
# (#863/#464); the `received=` flat-ssh OBS-log-tail read + the frozen/advancing decision of
# frozen-input-health.sh / frozen-input-alert-watchdog.sh; the launch-obs-genlock.sh --force
# kill+sentinel-clear (minus the session-1 Start-Process, which AHK owns); win-ssh-exec.sh's
# win_ssh_run (EncodedCommand) for the strih PowerShell; obs_burn_filter.py sweep-off for the
# post-restart burn reload (obs-ops: a force-kill reload can restore a saved genlock_burn=true).
#
# All pure/builder pieces are Tier-0 unit-tested with fakes on PATH (tests/harness_mv_reverify_
# escalate_1093.rs, the #833/#716 pattern); the LIVE strih-OBS restart itself is not exercisable at
# Tier-0 and is flagged UNVERIFIED for the supervisor's integration run.

# ---- (b) pure wedge verdict --------------------------------------------------------------------
# mv_reverify_wedge_verdict <prev_received> <curr_received> -> stdout: WEDGE | NO_WEDGE
#   NO_WEDGE: curr numeric AND (prev non-numeric OR curr != prev). A strictly-greater advance OR a
#             cumulative-counter RESET (curr<prev, an OBS restart between samples) both mean frames
#             are/again flowing. A first numeric reading with no prior sample cannot prove "stuck".
#   WEDGE:    curr non-numeric/empty (no `received=` line at all -> "no recv"), OR curr == prev
#             (both numeric -- the cumulative frame count did not move -> "delta absent").
mv_reverify_wedge_verdict() {
  local prev="${1:-}" curr="${2:-}"
  case "$curr" in '' | *[!0-9]*) printf 'WEDGE\n'; return 0 ;; esac
  case "$prev" in '' | *[!0-9]*) printf 'NO_WEDGE\n'; return 0 ;; esac
  if [ "$curr" = "$prev" ]; then printf 'WEDGE\n'; else printf 'NO_WEDGE\n'; fi
}

# ---- (a) painter-up proof: REMOTE cmd builder --------------------------------------------------
# mv_reverify_painter_up_cmds -> REMOTE bash (embed via `$(...)` into an ssh command string run
# against $PAINTER_IP). Bounded poll (MV_REVERIFY_PAINTER_UP_ITERS x ~2s) for a GENUINELY PAINTING
# painter: cam2-painter.service active + the presenter-aware signal (KMS DRM device held+vblank, or
# /dev/fb0 held -- #863/#464), with a process-based fallback (a live frame-probe holding /dev/fb0)
# for a transient painter with no service unit. Prints PAINTER_UP + exit 0 the moment it is
# confirmed; PAINTER_NOT_CONFIRMED + exit 1 after the budget. The CALLER (mv_reverify_painter_up_
# wait) treats non-UP as WARN-and-proceed, never a new hard gate.
mv_reverify_painter_up_cmds() {
  local iters="${MV_REVERIFY_PAINTER_UP_ITERS:-12}"
  cat <<CMDS
_pu=0
while [ \$_pu -lt $iters ]; do
  _puok=""
  if [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" = "active" ]; then
    _puj="\$(journalctl -u cam2-painter.service -n 100 --no-pager 2>/dev/null || true)"
    _pukms="\$(printf '%s\n' "\$_puj" | grep 'presenter: using DRM/KMS page-flip' | tail -n1 || true)"
    if [ -n "\$_pukms" ]; then
      _pudrm="\${_pukms#*(}"; _pudrm="\${_pudrm%)*}"
      if [ -n "\$_pudrm" ] && fuser -s "\$_pudrm" 2>/dev/null && printf '%s' "\$_puj" | grep -q 'vblank-locked'; then
        _puok=1
      fi
    elif fuser -s /dev/fb0 2>/dev/null; then
      _puok=1
    fi
  fi
  if [ -z "\$_puok" ] && pgrep -x frame-probe >/dev/null 2>&1 && fuser -s /dev/fb0 2>/dev/null; then
    _puok=1
  fi
  if [ -n "\$_puok" ]; then echo PAINTER_UP; exit 0; fi
  [ \$_pu -lt $((iters - 1)) ] && sleep 2
  _pu=\$((_pu + 1))
done
echo PAINTER_NOT_CONFIRMED
exit 1
CMDS
}

# mv_reverify_painter_up_wait <cam_pw> <painter_ip> -- LOCAL runner (dev1-side). ssh to the painter
# box, run the bounded painting poll, LOG the outcome. WARN-only: ALWAYS returns 0 (the reverify +
# the wedge escalation are the real gate; a genuinely dead painter still aborts via the reverify's
# own || exit 1). Called ONCE, before cam1's probe.
mv_reverify_painter_up_wait() {
  local cam_pw="$1" painter_ip="$2" out
  out="$(timeout "${MV_REVERIFY_PAINTER_UP_SSH_TIMEOUT:-45}" sshpass -p "$cam_pw" \
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$painter_ip" \
    "$(mv_reverify_painter_up_cmds)" 2>/dev/null || true)"
  case "$out" in
    *PAINTER_UP*)
      echo "    [#1093 painter-order] cam2-painter proven PAINTING before the cam-pixel probe" >&2 ;;
    *)
      echo "    WARNING #1093: cam2-painter NOT confirmed painting within budget before the cam-pixel probe -- proceeding (a false dead-leg is caught by the receiver-wedge escalation / the reverify itself)" >&2 ;;
  esac
  return 0
}

# ---- (b) received= reader (env-overridable) ----------------------------------------------------
# mv_reverify_probe_received <strih_ip> <source> -> stdout: newest cumulative `received=` for the
# named source, or empty. One flat ssh + single (non-nested) powershell OBS-log tail -- a session-
# agnostic FILE read (win-ssh-vs-mcp Context B), mirroring frozen-input-alert-watchdog.sh's
# probe_received. Override the WHOLE read with MV_REVERIFY_RECEIVED_CMD (run with "<ip> <source>",
# stdout = raw log text) for offline tests / a future alternate tap.
mv_reverify_probe_received() {
  local ip="$1" source="$2" raw
  if [ -n "${MV_REVERIFY_RECEIVED_CMD:-}" ]; then
    raw="$($MV_REVERIFY_RECEIVED_CMD "$ip" "$source" 2>/dev/null || true)"
  else
    raw="$(timeout "${MV_REVERIFY_RECEIVED_SSH_TIMEOUT:-20}" sshpass -p "${STRIH_PW:-newlevel}" \
      ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${STRIH_USER:-newlevel}@$ip" \
      "powershell -NoProfile -Command \"gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail ${MV_REVERIFY_RECEIVED_TAIL:-400}\"" \
      2>/dev/null || true)"
  fi
  printf '%s\n' "$raw" \
    | grep -F "genlock-fifo audit '$source': " \
    | tail -n1 \
    | sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p'
}

# ---- (b) headless strih-OBS restart ------------------------------------------------------------
# mv_reverify_obs_restart_ps -> PURE PowerShell string. Headless-safe strih OBS restart: force-kill
# obs64 + clear the crash sentinels ONLY. It deliberately does NOT Start-Process (a session-1 GUI
# launch, banned over ssh -- strih's NL_STARTUP.ahk respawns exactly one clean genlock obs64 since
# it keys on the obs64 WINDOW) and does NOT stop AutoHotkey64 (it IS the respawn watcher we rely
# on). Session-agnostic (a process kill + a file delete), per win-ssh-vs-mcp Context B. Mirrors
# launch-obs-genlock.sh --force's kill+sentinel-clear, minus the session-1 launch.
mv_reverify_obs_restart_ps() {
  cat <<'PS'
$ErrorActionPreference = 'SilentlyContinue'
# Force-kill the wedged obs64 (session-agnostic). strih's NL_STARTUP.ahk (session 1) then respawns
# ONE clean genlock obs64 within ~25s; this program never LAUNCHES obs64 itself (no ssh GUI launch,
# no double-launch) and never stops AutoHotkey64 (the respawn watcher we depend on).
Get-Process obs64 -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.Id -Force }
# Clear stale crash sentinels so the AHK respawn comes up clean (no "Crash Detected"/Safe-Mode modal,
# which disables DistroAV + genlock). Same clear as launch-obs-genlock.sh.
Remove-Item "$env:APPDATA\obs-studio\.sentinel\*" -Force -ErrorAction SilentlyContinue
Write-Host "MV_REVERIFY_OBS_RESTART: obs64 force-killed + .sentinel cleared; AutoHotkey64 respawns one clean genlock obs64"
PS
}

# mv_reverify_obs_restart_run <strih_ip> -- LOCAL runner. Runs the headless restart PowerShell on
# strih over ssh (win_ssh_run EncodedCommand). Override with MV_REVERIFY_OBS_RESTART_CMD (run with
# "<ip>") for offline tests. Best-effort (never returns non-zero to the caller).
mv_reverify_obs_restart_run() {
  local ip="$1"
  if [ -n "${MV_REVERIFY_OBS_RESTART_CMD:-}" ]; then
    $MV_REVERIFY_OBS_RESTART_CMD "$ip" 2>&1 | sed 's/^/    [#1093 obs-restart] /' || true
    return 0
  fi
  # win_ssh_run BLOCKS; bound it. timeout execvp()s directly so it cannot invoke a shell FUNCTION --
  # route through `bash -c` re-sourcing the lib, the SAME shape recording-e2e.sh's other win_ssh_run
  # call sites already use.
  timeout "${MV_REVERIFY_OBS_RESTART_SSH_TIMEOUT:-30}" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
    "$HERE/lib/win-ssh-exec.sh" "${STRIH_USER:-newlevel}" "${STRIH_PW:-newlevel}" "$ip" "$(mv_reverify_obs_restart_ps)" 2>&1 \
    | sed 's/^/    [#1093 obs-restart] /' || true
  return 0
}

# mv_reverify_wait_obs_ws <strih_ip> -- LOCAL runner. Poll strih OBS's WebSocket :4455 from dev1
# (a session-agnostic TCP connect) until it is back after the AHK respawn (~25s relaunch + init),
# bounded. WARN-and-return-0 on timeout (the single re-check decides whether the leg recovered).
mv_reverify_wait_obs_ws() {
  local ip="$1" iters="${MV_REVERIFY_OBS_WS_WAIT_ITERS:-40}" i=0
  echo "    [#1093 escalate] waiting for strih OBS WebSocket :4455 to return after the AHK respawn" >&2
  while [ "$i" -lt "$iters" ]; do
    if timeout 3 bash -c "exec 3<>/dev/tcp/$ip/4455" 2>/dev/null; then
      echo "    [#1093 escalate] strih OBS :4455 reachable again" >&2
      return 0
    fi
    sleep "${MV_REVERIFY_OBS_WS_WAIT_GAP_S:-3}"
    i=$((i + 1))
  done
  echo "    WARNING #1093: strih OBS :4455 did not return within budget after the restart -- the single re-check will decide" >&2
  return 0
}

# ---- the orchestrator --------------------------------------------------------------------------
# mv_reverify_or_escalate <box> <cam_n> -> 0 if the leg is live (immediately, or after the receiver-
# wedge escalation recovered it), 1 if it is genuinely dead. The DEPLOY-time drop-in for
# `preflight_mv_reverify <box> <cam_n> || exit 1` (never the cleanup wrapper -- an OBS restart is
# forbidden inside the EXIT trap). preflight_mv_reverify() already no-ops (returns 0) unless
# ALL_CAMBOX=1, so reaching the escalation means the budget genuinely exhausted.
mv_reverify_or_escalate() {
  local box="$1" cam_n="$2"
  preflight_mv_reverify "$box" "$cam_n" && return 0

  local src="NDI cam${cam_n}" r0 r1 verdict
  r0="$(mv_reverify_probe_received "$STRIH" "$src" 2>/dev/null || true)"
  sleep "${MV_REVERIFY_WEDGE_SAMPLE_GAP_S:-8}"
  r1="$(mv_reverify_probe_received "$STRIH" "$src" 2>/dev/null || true)"
  verdict="$(mv_reverify_wedge_verdict "$r0" "$r1")"
  echo "    [#1093 escalate] ${box} (${src}) reverify budget exhausted -- strih received= sample0='${r0:-none}' sample1='${r1:-none}' -> ${verdict}" >&2

  if [ "$verdict" != "WEDGE" ]; then
    echo "    [#1093 escalate] ${box}: received= is advancing -- NOT a receiver wedge; the source/leg is genuinely dead. Failing loud (no OBS restart)." >&2
    return 1
  fi
  if [ "${MV_REVERIFY_OBS_RESTARTED:-0}" = "1" ]; then
    echo "    [#1093 escalate] ${box}: still wedged AFTER a prior strih-OBS restart this run -- not restarting again (issue 1096: a fresh OBS can re-wedge on the next bounce). Failing loud." >&2
    return 1
  fi
  MV_REVERIFY_OBS_RESTARTED=1
  echo "    [#1093 escalate] ${box}: receiver WEDGE confirmed (issue 1096) -- restarting strih OBS once (force-kill+sentinel-clear; AutoHotkey64 respawns one clean genlock obs64), then re-checking once." >&2
  mv_reverify_obs_restart_run "$STRIH"
  mv_reverify_wait_obs_ws "$STRIH"
  # A force-kill reload can restore a SAVED genlock_burn=true (obs-ops) -- sweep it off before the
  # re-check + before the run proceeds. Best-effort dev1-side WS op (session-agnostic).
  python3 "$HERE/obs_burn_filter.py" sweep-off --host "$STRIH" >/dev/null 2>&1 || true
  if preflight_mv_reverify "$box" "$cam_n"; then
    echo "    [#1093 escalate] ${box}: recovered after the strih-OBS restart + re-check." >&2
    return 0
  fi
  echo "    [#1093 escalate] ${box}: STILL dead after the strih-OBS restart + single re-check. Failing loud." >&2
  return 1
}
