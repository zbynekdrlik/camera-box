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

# #1148: the presenter-aware painting SIGNAL is now the shared `_cb_paint_signal`
# (scripts/lib/cam2-paint-signal.sh); lazy-source it so mv_reverify_painter_up_cmds can emit its
# definition and pipe the journal into it (the frame-probe/fb0 fallback below stays site-local).
command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"
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
  cam2_paint_signal_remote_fn
  cat <<CMDS
_pu=0
while [ \$_pu -lt $iters ]; do
  _puok=""
  if [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" = "active" ]; then
    _puj="\$(journalctl -u cam2-painter.service -n 100 --no-pager 2>/dev/null || true)"
    if printf '%s\n' "\$_puj" | _cb_paint_signal >/dev/null 2>&1; then
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
# mv_reverify_probe_raw <strih_ip> <source> -> stdout: the RAW newest-tail of strih's OBS log. One
# flat ssh + single (non-nested) powershell OBS-log tail -- a session-agnostic FILE read (win-ssh-
# vs-mcp Context B), mirroring frozen-input-alert-watchdog.sh's probe_received. Override the WHOLE
# read with MV_REVERIFY_RECEIVED_CMD (run with "<ip> <source>", stdout = raw log text) for offline
# tests / a future alternate tap. EMPTY output => the READ ITSELF failed (a healthy tail is never
# empty) -- the orchestrator treats that as READ_FAIL, NOT as "no recv" (#1093 review finding 3, the
# frozen_input_classify UNKNOWN discipline: never act on absence-of-evidence).
mv_reverify_probe_raw() {
  local ip="$1" source="$2"
  if [ -n "${MV_REVERIFY_RECEIVED_CMD:-}" ]; then
    $MV_REVERIFY_RECEIVED_CMD "$ip" "$source" 2>/dev/null || true
  else
    # #1258: invoke PowerShell via -EncodedCommand (base64 UTF-16LE), NEVER the naive
    # `-Command "gc (gci ... | sort ... | select ...)..."` string. Win32-OpenSSH's default cmd.exe
    # shell MANGLES the naive triple-quoted form (the bash -> ssh -> cmd.exe -> powershell three-layer
    # quoting hazard win-ssh-exec.sh documents + live-verified): the unescaped `|` pipes leak to
    # cmd.exe, so the read returned non-tail noise and EVERY source read `received=none` on EVERY
    # [4c/8] frozen-camera-gate attempt of EVERY run since #1233 (run 33513175938 + the 4 prior green
    # runs all 4/4 INCONCLUSIVE) -> the abort gate silently never bit; only the QR sweep protected.
    # The base64 blob is pure ASCII with no shell-special chars, so cmd.exe cannot mangle it and
    # PowerShell decodes it back to the exact command -- the same mechanism win_ssh_run already uses.
    # Inlined (rather than sourcing win-ssh-exec.sh) so this source-only lib never imports that
    # helper's own top-level `set -euo pipefail`, which would leak strict mode into non-strict
    # callers (the frozen-input watchdog + the Tier-0 harness). iconv + base64 are required (present
    # fleet-wide -- win_ssh_ps_encoded_command uses the same pair); if either is somehow absent the
    # encode yields "" -> an empty -EncodedCommand -> an empty read -> INCONCLUSIVE, NEVER an abort
    # (the `|| _enc=""` guard keeps this line self-contained under a future set -e caller, the #266
    # never-abort discipline this lib documents).
    local _ps _enc _tail
    # numeric-only tail -> the override can never inject shell/PS metachars into the encoded payload.
    _tail="${MV_REVERIFY_RECEIVED_TAIL:-400}"
    case "$_tail" in '' | *[!0-9]*) _tail=400 ;; esac
    _ps="gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $_tail"
    _enc="$(printf '%s' "$_ps" | iconv -f UTF-8 -t UTF-16LE | base64 -w0 2>/dev/null)" || _enc=""
    timeout "${MV_REVERIFY_RECEIVED_SSH_TIMEOUT:-20}" sshpass -p "${STRIH_PW:-newlevel}" \
      ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${STRIH_USER:-newlevel}@$ip" \
      "powershell -NoProfile -NonInteractive -EncodedCommand $_enc" \
      2>/dev/null || true
  fi
}

# mv_reverify_extract_received <source> -- stdin: raw OBS-log text; stdout: newest cumulative
# `received=` for the named source, or empty (raw read but no audit line = genuine "no recv").
mv_reverify_extract_received() {
  grep -F "genlock-fifo audit '$1': " | tail -n1 | sed -n 's/.*received=\([0-9][0-9]*\).*/\1/p'
}

# mv_reverify_probe_received <strih_ip> <source> -> the newest `received=` (raw read + extract in
# one step). Kept for callers/tests that only need the value; the orchestrator uses the raw form
# above so it can tell READ_FAIL (empty raw) from genuine no-recv (raw present, no audit line).
mv_reverify_probe_received() {
  mv_reverify_probe_raw "$1" "$2" | mv_reverify_extract_received "$2"
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
# GUARD (#1093 review finding 2): NEVER kill obs64 unless the AutoHotkey64 respawn watcher is alive
# -- otherwise a force-kill leaves strih OBS DOWN with nothing to relaunch it (strictly worse than
# the old exit 1). We cannot detect NL_STARTUP.ahk's SafeLoop=0 "No"-latch (#774) over ssh, so that
# stays an accepted residual; process-presence is the detectable half. No AHK -> report + exit 2
# WITHOUT killing; the orchestrator treats exit 2 / MV_REVERIFY_NO_AHK as "restart impossible -> fail
# loud, strih untouched".
if (-not (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue)) {
  Write-Host "MV_REVERIFY_NO_AHK: AutoHotkey64 respawn watcher not running on strih -- NOT killing obs64 (a force-kill would leave OBS down with no relaunch)"
  exit 2
}
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
# "<ip>") for offline tests. Returns 2 (restart NOT performed -- AHK respawn watcher absent, obs64
# untouched, #1093 review finding 2) when the program reported MV_REVERIFY_NO_AHK; 0 otherwise.
mv_reverify_obs_restart_run() {
  local ip="$1" out
  if [ -n "${MV_REVERIFY_OBS_RESTART_CMD:-}" ]; then
    out="$($MV_REVERIFY_OBS_RESTART_CMD "$ip" 2>&1 || true)"
  else
    # win_ssh_run BLOCKS; bound it. timeout execvp()s directly so it cannot invoke a shell FUNCTION --
    # route through `bash -c` re-sourcing the lib, the SAME shape recording-e2e.sh's other win_ssh_run
    # call sites already use.
    out="$(timeout "${MV_REVERIFY_OBS_RESTART_SSH_TIMEOUT:-30}" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
      "$HERE/lib/win-ssh-exec.sh" "${STRIH_USER:-newlevel}" "${STRIH_PW:-newlevel}" "$ip" "$(mv_reverify_obs_restart_ps)" 2>&1 || true)"
  fi
  printf '%s\n' "$out" | sed 's/^/    [#1093 obs-restart] /' >&2
  case "$out" in *MV_REVERIFY_NO_AHK*) return 2 ;; esac
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

# mv_reverify_reopen_multiview_run <strih_ip> -- #1098: (re)open strih's operator FULLSCREEN
# Multiview projector after the force-kill restart. The AHK respawn only re-launches obs64 (no
# projector), and strih's SaveProjectors=true has an EMPTY SavedProjectors that a force-kill never
# repopulates, so OBS restores nothing -- the operator is left without their standing multiview
# until it is re-opened. obs_phase2.py open-multiview issues OpenVideoMixProjector(MULTIVIEW) on
# strih's DERIVED single monitor (dev1-side, session-agnostic WS op -- the SAME class as the
# sweep-off; strih's WS accepts an empty password, like the sweep-off's own obs_burn_filter.py
# call). WARN-only and ALWAYS returns 0: the leg recovery already succeeded projector-independently
# (the positive warm-settle activates the input itself), so this operator-facing nicety must NEVER
# turn a recovered run into a failure. Override with MV_REVERIFY_REOPEN_MV_CMD ("<ip>") for offline
# tests; timeout-bound like every other OBS-touching call (#328).
mv_reverify_reopen_multiview_run() {
  local ip="$1"
  if [ -n "${MV_REVERIFY_REOPEN_MV_CMD:-}" ]; then
    $MV_REVERIFY_REOPEN_MV_CMD "$ip" >/dev/null 2>&1 || true
  else
    timeout "${MV_REVERIFY_REOPEN_MV_TIMEOUT:-30}" python3 "$HERE/obs_phase2.py" open-multiview --host "$ip" >/dev/null 2>&1 || true
  fi
  echo "    [#1098] re-opened strih's operator Multiview projector after the restart (best-effort)" >&2
  return 0
}

# ---- (issue 1197) bounded COLD-finder discovery-wait + re-enforce ------------------------------
# mv_reverify_finder_heal_wait <host> <active_spec> <deadline_s> -- ride out a COLD DistroAV finder
# (right after a strih OBS boot OR the #1093 escalation force-kill restart, where a genuinely-live
# sender is not-yet-discovered) and re-enforce the #399 baseline of each active input the INSTANT its
# sender re-appears in the finder. Delegates to set-ndi-mapping.py --heal-wait (the shared
# obs_phase2.reenforce_ndi_name policy: discoverable -> set -> read-back-verify; NEVER blind-sets a
# name absent from the finder, the #795 mangle ban). It RETURNS EARLY the instant every input is
# discoverable+bound, so a warm finder pays ~one WS round-trip. WARN-only: ALWAYS returns 0 -- the
# pixel re-verify / the next camera's own reverify is the real gate; a leg still absent after the
# bound is logged LOUD (the python's own #1197 lines) but never aborts the run. Override the WHOLE
# call with MV_REVERIFY_HEAL_WAIT_CMD (run with "<host> <active> <deadline>") for offline tests.
mv_reverify_finder_heal_wait() {
  local host="$1" active_spec="$2" deadline_s="$3" out
  if [ -n "${MV_REVERIFY_HEAL_WAIT_CMD:-}" ]; then
    out="$($MV_REVERIFY_HEAL_WAIT_CMD "$host" "$active_spec" "$deadline_s" 2>&1 || true)"
  else
    # --heal-wait bounds the python's OWN poll loop to deadline_s; the outer timeout is the #328
    # belt-and-suspenders bound on the WS connect/init. strih's WS accepts an empty password, like
    # preflight_mv_reverify's own frozen-camera-gate calls.
    # #1197 review 🔵-2: coerce to an INTEGER for bash arithmetic -- deadline_s is documented
    # integer-seconds, but a stray float override (e.g. 90.5) would make $((...)) throw, timeout get
    # an empty duration, and `|| true` silently swallow the whole heal-wait. `${deadline_s%.*}` drops
    # any fractional part; python's --heal-wait below accepts the raw value (float-tolerant) fine.
    out="$(timeout "${MV_REVERIFY_HEAL_WAIT_SSH_TIMEOUT:-$(( ${deadline_s%.*} + 30 ))}" \
      python3 "$HERE/set-ndi-mapping.py" --host "$host" --password "" \
      --active "$active_spec" --heal-wait "$deadline_s" \
      --heal-wait-interval "${MV_REVERIFY_HEAL_WAIT_INTERVAL_S:-4}" 2>&1 || true)"
  fi
  # #1197 review 🟡-1: `|| true` so the WARN-only guarantee holds at the HELPER regardless of the
  # caller's set-e state (the #1133 discipline: never let a report-only probe's own pipeline abort the
  # run). Both current call sites disable set -e (`… || exit 1` / `if …; then`), but harden here so a
  # future bare-statement caller under `set -euo pipefail` can never be aborted by this line.
  printf '%s\n' "$out" | sed 's/^/    [#1197 finder-warm] /' >&2 || true
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

  # Read strih's received= RAW twice (default gap >= 2x the ~5s audit emit cadence, #1093 review
  # finding 6, so healthy emit jitter/flush can't read the same newest line twice = false WEDGE).
  local src="NDI cam${cam_n}" raw0 raw1 r0 r1 verdict
  raw0="$(mv_reverify_probe_raw "$STRIH" "$src" 2>/dev/null || true)"
  sleep "${MV_REVERIFY_WEDGE_SAMPLE_GAP_S:-12}"
  raw1="$(mv_reverify_probe_raw "$STRIH" "$src" 2>/dev/null || true)"
  # READ_FAIL (#1093 review finding 3): a healthy 400-line tail is NEVER empty, so both reads empty
  # means the LOG READ failed (ssh blip / log absent), not "no recv". Never force-kill strih on
  # absence-of-evidence (the frozen_input_classify UNKNOWN discipline) -- fail loud, no restart.
  if [ -z "$raw0" ] && [ -z "$raw1" ]; then
    echo "    [#1093 escalate] ${box} (${src}): could NOT read strih's OBS log for received= (both samples empty) -- READ_FAIL, not a proven wedge. Failing loud WITHOUT restarting strih OBS." >&2
    return 1
  fi
  r0="$(printf '%s\n' "$raw0" | mv_reverify_extract_received "$src")"
  r1="$(printf '%s\n' "$raw1" | mv_reverify_extract_received "$src")"
  verdict="$(mv_reverify_wedge_verdict "$r0" "$r1")"
  echo "    [#1093 escalate] ${box} (${src}) reverify budget exhausted -- strih received= sample0='${r0:-none}' sample1='${r1:-none}' -> ${verdict}" >&2

  if [ "$verdict" != "WEDGE" ]; then
    echo "    [#1093 escalate] ${box}: received= is advancing -- NOT a receiver wedge; the source/leg is genuinely dead. Failing loud (no OBS restart)." >&2
    return 1
  fi
  # ACCEPTED RESIDUAL (#1093 review finding 4): a DEAD SENDER (its burn unit never came up) ALSO
  # freezes strih's received= and is misclassified WEDGE here, costing ONE needless strih restart +
  # the wait budget before the same exit 1. This is bounded (once-per-run guard below), rare (a dead
  # sender right after its own deploy is a run-ending failure regardless), and the OUTCOME is still
  # correct (exit 1). A sender-side pre-check would need the box IP + burn-unit name threaded through
  # both call sites; deferred as not worth the coupling for a bounded, self-healing waste.
  # Restart BUDGET (issue 1096 live rate, 2026-08-17): a COUNTER capped by
  # MV_REVERIFY_OBS_RESTART_MAX (default 3) -- one restart per run could not carry a run whose
  # deploy bounces 3+ senders when each bounce coin-flips a fresh wedge (run 32031076988: cam1
  # cured by the single restart, cam2's next bounce re-wedged the fresh OBS and failed the run).
  # Each restart costs ~60s of wait budget, so the cap is self-limiting; the legacy
  # MV_REVERIFY_OBS_RESTARTED=1 kill-switch still blocks outright (kept for operators + tests).
  if [ "${MV_REVERIFY_OBS_RESTARTED:-0}" = "1" ] \
    || [ "${MV_REVERIFY_OBS_RESTARTS:-0}" -ge "${MV_REVERIFY_OBS_RESTART_MAX:-3}" ]; then
    echo "    [#1093 escalate] ${box}: still wedged AFTER a prior strih-OBS restart this run and the restart budget (${MV_REVERIFY_OBS_RESTARTS:-0}/${MV_REVERIFY_OBS_RESTART_MAX:-3}) is spent -- not restarting again (issue 1096: a fresh OBS can re-wedge on the next bounce). Failing loud." >&2
    return 1
  fi
  echo "    [#1093 escalate] ${box}: receiver WEDGE confirmed (issue 1096) -- restarting strih OBS once (force-kill+sentinel-clear; AutoHotkey64 respawns one clean genlock obs64), then re-checking once." >&2
  local _rr=0
  mv_reverify_obs_restart_run "$STRIH" || _rr=$?
  if [ "$_rr" = "2" ]; then
    # AHK respawn watcher absent -> the restart was NOT performed (obs64 untouched). We cannot cure
    # the wedge from the harness without risking a dead strih; fail loud instead of leaving OBS down.
    echo "    [#1093 escalate] ${box}: strih's AutoHotkey64 respawn watcher is ABSENT -- restart skipped (obs64 left running). Cannot recover this wedge safely from the harness; failing loud." >&2
    return 1
  fi
  MV_REVERIFY_OBS_RESTARTS=$((${MV_REVERIFY_OBS_RESTARTS:-0} + 1))
  mv_reverify_wait_obs_ws "$STRIH"
  # A force-kill reload can restore a SAVED genlock_burn=true (obs-ops) -- sweep it off before the
  # re-check + before the run proceeds. Best-effort dev1-side WS op (session-agnostic), timeout-bound
  # like every other OBS-touching call (#328). Override with MV_REVERIFY_SWEEP_CMD for offline tests.
  if [ -n "${MV_REVERIFY_SWEEP_CMD:-}" ]; then
    $MV_REVERIFY_SWEEP_CMD "$STRIH" >/dev/null 2>&1 || true
  else
    timeout "${MV_REVERIFY_SWEEP_SSH_TIMEOUT:-30}" python3 "$HERE/obs_burn_filter.py" sweep-off --host "$STRIH" >/dev/null 2>&1 || true
  fi
  # #1098: restore the operator's own standing FULLSCREEN Multiview projector, which the force-kill
  # restart dropped (SaveProjectors=true but an EMPTY SavedProjectors + the force-kill bypassing the
  # graceful save -> OBS restores nothing; the AHK respawn only re-launches obs64). WARN-only, after
  # the sweep-off (so the fresh OBS's burn is cleared first). This is purely the operator-facing view
  # -- the re-check below is projector-INDEPENDENT (it PREVIEW-activates the input via the positive
  # warm-settle), so a failed re-open never affects the run outcome.
  mv_reverify_reopen_multiview_run "$STRIH"
  # issue 1197: the fresh OBS's DistroAV finder is COLD after the force-kill restart. Before the run
  # proceeds to the NEXT camera's deploy bounce (whose reattach would otherwise hit the cold finder,
  # empty a correct ndi_source_name and leave a stopped-thread wedge -- gh run 32743557703), warm the
  # finder + re-enforce the #399 baseline of EVERY active input. Bounded; WARN-only (the re-check
  # below + the next camera's own reverify are the real gates). Runs AFTER the burn sweep-off so the
  # fresh OBS's burn is cleared first.
  mv_reverify_finder_heal_wait "$STRIH" "${CAMERA_ACTIVE_SET:-}" "${MV_REVERIFY_RESTART_HEAL_WAIT_S:-120}"
  # #1093 review finding 1 (CRITICAL): the fresh OBS's built-in Multiview projector may NOT reopen
  # (SaveProjectors), so the "NDI camN" inputs the reverify relies on can be INACTIVE. Run the single
  # re-check with a POSITIVE warm-settle so frozen-camera-gate PREVIEW-activates the input itself
  # (#747, Studio Mode; it restores the operator's preview afterwards) -- recovery no longer depends
  # on the projector. (The operator's own strih multiview is restored ABOVE by
  # mv_reverify_reopen_multiview_run, #1098 -- independently of this projector-free re-check.)
  if PREFLIGHT_MV_REVERIFY_WARM_SETTLE="${MV_REVERIFY_RECHECK_WARM_SETTLE:-3}" preflight_mv_reverify "$box" "$cam_n"; then
    echo "    [#1093 escalate] ${box}: recovered after the strih-OBS restart + re-check." >&2
    return 0
  fi
  echo "    [#1093 escalate] ${box}: STILL dead after the strih-OBS restart + single re-check. Failing loud." >&2
  return 1
}

# mv_reverify_resolve_wait BOX CAM_N [CALL_TIMEOUT] -> issue 1114 REZÍDUUM (harness side). After the
# merged WS-side CLEAR-then-SET reattach() (strih_mv_scenes.py) TEARS DOWN + rebuilds strih's NDI
# receiver, its fresh DistroAV finder must RE-RESOLVE the live post-bounce burn sender by URL before
# any pixel can change again. That re-resolve was MEASURED at up to ~2 min on the live rig (issue
# 1114 owner comments, 2026-08-19: two cameras per run read "no pixel change" through the whole ~52s
# [2/8] attempt budget, then recovered), FAR longer than a single per-attempt settle. Give the fresh
# finder its OWN one-time bounded re-resolve window right after the kick: poll the SAME pixel-change
# gate (frozen-camera-gate.py, identical flags to preflight_mv_reverify) at RESOLVE_CADENCE_S until
# the leg delivers a changing frame OR the RESOLVE_SETTLE_S deadline. This is a REAL bounded poll --
# it RETURNS 0 the instant a pixel changes, so a fast re-lock costs ~0 extra time, and only a
# genuinely slow re-resolve spends the full window -- NOT a blind sleep and NOT a blind workflow
# budget bump (no-timeout-band-aids: a MEASURED, documented window for a confirmed-slower op, issue
# 1114). Reads the same $HERE / $STRIH / $PROBE_BIN_DIR globals preflight_mv_reverify uses. Returns 0
# on recovery, 1 on deadline; never exits (caller falls back into its own attempt loop / escalation).
mv_reverify_resolve_wait() {
  local box="$1" cam_n="$2" call_timeout="${3:-30}"
  # #1114 review 🔵-3: coerce to an INTEGER before the $((...)) deadline below (the #1197 finder-heal
  # precedent, lines ~254-258). RESOLVE_SETTLE_S is documented integer-seconds, but a stray float
  # override (e.g. 90.5) would make `$((SECONDS + resolve_s))` throw a FATAL arithmetic error that
  # aborts a non-interactive shell REGARDLESS of the caller's `|| true` (an expansion error is not a
  # command failure the `||` list can catch) -- exactly the WARN-only hole this + the proactive-reset
  # doc now claim closed. `${resolve_s%.*}` drops any fractional part; the loop uses only the integer.
  local resolve_s="${PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S:-120}"; resolve_s="${resolve_s%.*}"
  local cadence="${PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S:-6}"
  # #1114 review 🔵-2: bound the WALL CLOCK, not just the accumulated sleeps. Each iteration also
  # spends one frozen-camera-gate.py probe (~a few s, up to call_timeout), so a sleep-only counter
  # would run ~2x past the documented measured window. A SECONDS-based deadline makes RESOLVE_SETTLE_S
  # a truthful wall-clock bound on how long the fresh finder is given to re-resolve.
  # issue 1197: the attempt-1 reattach kick may have EMPTIED this camera's ndi_source_name on a cold
  # finder (its sender is absent from the finder DURING its own deploy bounce -> a stopped-thread
  # wedge the pixel poll below can never clear). Before polling, ride out the cold finder for THIS
  # camera and re-enforce its #399 baseline the instant the sender re-appears (never blind-setting an
  # absent name, the #795 mangle ban). Bounded; WARN-only -- the pixel poll then confirms recovery.
  mv_reverify_finder_heal_wait "$STRIH" "$box" "${MV_REVERIFY_FINDER_HEAL_WAIT_S:-90}"
  local start="$SECONDS" deadline=$((SECONDS + resolve_s)) waited
  while [ "$SECONDS" -lt "$deadline" ]; do
    sleep "$cadence"
    if timeout "$call_timeout" python3 "$HERE/frozen-camera-gate.py" --host "$STRIH" --password "" \
        --sources "NDI cam${cam_n}" --samples 2 --cadence 3.5 --threshold 1 --warm-settle "${PREFLIGHT_MV_REVERIFY_WARM_SETTLE:-0}" \
        --verdict-bin "$PROBE_BIN_DIR/frozen-camera-gate" >/dev/null 2>&1; then
      waited=$((SECONDS - start))
      echo "    [sender-bounce] ${box} recovered ${waited}s after the receiver reset — fresh finder re-resolved the live burn sender (issue 1114)" >&2
      return 0
    fi
  done
  echo "    [sender-bounce] ${box} still no pixel change ${resolve_s}s (wall clock) after the receiver reset — fresh finder did not re-resolve within the measured window (issue 1114)" >&2
  return 1
}

# mv_reverify_proactive_reset BOX CAM_N [CALL_TIMEOUT] -> issue 1114 ROOT FIX (deploy-context
# proactive receiver reset). At a burn-deploy site the strih receiver for this leg is KNOWN-STALE the
# instant its production sender was stopped (systemctl stop camera-box) and the burn sender came up at
# a NEW URL under the SAME NDI name -- the DistroAV finder still holds the dead pre-bounce URL, so a
# pixel poll run FIRST is GUARANTEED to read "no pixel change" until something resets the receiver.
# preflight_mv_reverify's attempt-1 therefore always fails on a bounced leg (logging the alarming
# "no pixel change right after its deploy" / "camera leg is dead" line) and only THEN kicks reactively.
# Fire the CLEAR-then-SET reattach + ride out the fresh finder's bounded re-resolve BEFORE the pixel
# poll starts counting (owner directive, issuecomment-5335833149: "sequence the burn deploy so the
# receiver is kicked BEFORE the pixel-change poll starts counting"), so the guarded reverify that
# follows (mv_reverify_or_escalate -> preflight_mv_reverify) passes attempt-1 CLEANLY -- no guaranteed
# attempt-1 failure log, no reliance on the reactive escalation path. Reuses the merged CLEAR-then-SET
# reattach (strih_mv_scenes.py) + mv_reverify_resolve_wait (its own #1197 finder-heal-wait + #795
# mangle guard). WARN-only: ALWAYS returns 0 -- the guarded reverify that follows is the real gate; a
# genuinely-dead leg that never re-resolves here still fails there and escalates exactly as before (the
# only cost is ~1 extra bounded re-resolve window before that already-rare destructive #1093 escalation,
# an acceptable trade for more recovery chance before a strih-OBS force-kill). DEPLOY context only
# (PREFLIGHT_MV_REVERIFY_CONTEXT != cleanup): the cleanup trap must stay fast enough never to outlast a
# GH-Actions cancellation grace window. Opt-out via PREFLIGHT_MV_REVERIFY_PROACTIVE=0. Reads the same
# $HERE / $STRIH / $PROBE_BIN_DIR globals as preflight_mv_reverify / mv_reverify_resolve_wait. Safe
# against a fast-recovery regression via the SILENT pre-probe below: a leg already delivering (a later
# ALL_CAMBOX-loop camera that re-resolved on its own during the preceding cameras' serial reverifies)
# is left UNTOUCHED, so this reset never tears down an already-delivering receiver. All guards + the
# pre-probe `if` use the || return / case / if-condition idioms proven set-e-safe in
# preflight_mv_reverify (the #1133 discipline); the two work calls are `|| true`-hardened and the
# function ends in an explicit `return 0`, so a caller under `set -euo pipefail` can never be aborted.
mv_reverify_proactive_reset() {
  local box="$1" cam_n="$2" call_timeout="${3:-${PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT:-30}}"
  [ "${ALL_CAMBOX:-0}" = "1" ] || return 0
  [ "${PREFLIGHT_MV_REVERIFY_CONTEXT:-preflight}" != "cleanup" ] || return 0
  [ "${PREFLIGHT_MV_REVERIFY_PROACTIVE:-1}" = "1" ] || return 0
  case " ${PREFLIGHT_EXCLUDED_CAMS:-} " in *" $box "*) return 0 ;; esac
  # SILENT, UNCOUNTED pre-probe (issue 1114 review 🟡-1): the "known-stale" premise holds at the cam1
  # site (kick ~4s after its bounce) and for the FIRST ALL_CAMBOX-loop camera, but a LATER loop
  # camera's receiver may have re-resolved on its own during the preceding cameras' SERIAL reverifies
  # (each spends its own ~20s-2min window) — the exact ~20s-2min stale-finder timescale of this
  # ticket. So check FIRST: if this leg is already delivering, do NOT tear down a working receiver —
  # return without kicking. This probe logs NOTHING as a failure and counts toward NO attempt budget,
  # so the owner's "kick BEFORE the pixel-change poll starts counting" still holds for a genuinely
  # stale leg (it is kicked here, before the guarded reverify's counted poll). Same gate flags as
  # preflight_mv_reverify / mv_reverify_resolve_wait; the `if` condition is set-e-exempt on both arms.
  if timeout "$call_timeout" python3 "$HERE/frozen-camera-gate.py" --host "$STRIH" --password "" \
      --sources "NDI cam${cam_n}" --samples 2 --cadence 3.5 --threshold 1 --warm-settle "${PREFLIGHT_MV_REVERIFY_WARM_SETTLE:-0}" \
      --verdict-bin "$PROBE_BIN_DIR/frozen-camera-gate" >/dev/null 2>&1; then
    return 0
  fi
  echo "    [sender-bounce] ${box} (NDI cam${cam_n}) proactive receiver reset right after its deploy bounce — resetting the stale receiver + riding out the fresh finder BEFORE the pixel poll starts counting (issue 1114 root fix)" >&2
  timeout "$call_timeout" python3 "$HERE/strih_mv_scenes.py" --host "$STRIH" --password "" --reattach "$cam_n" >&2 || true
  mv_reverify_resolve_wait "$box" "$cam_n" "$call_timeout" || true
  return 0
}
