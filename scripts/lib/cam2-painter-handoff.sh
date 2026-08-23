#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, cam2-painter-
# deadman.sh, audio-marker-check.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak
# into whichever caller sources it. scripts/rig-mode.sh (the only caller) already sets it.
#
# scripts/lib/cam2-painter-handoff.sh -- SINGLE SOURCE OF TRUTH for handing the TEST-mode
# STEADY-STATE painter to the PERMANENT supervised cam2-painter.service (#1008/#937).
#
# WHY (#1008/#937): rig-mode.sh test made the steady-state painter a TRANSIENT
# `nohup frame-probe --duration-secs 7200` (2h). It expires silently and unsupervised -- no
# restart-on-crash, no survive-reboot -- and rig-mode.sh test first STOPS the permanent unit
# (#440, so the two never race /dev/fb0) without ever re-enabling it. So two hours after
# `rig-mode.sh test`, or after an event->test cycle (EVENT disables the unit, #892), the cam2
# monitor goes black, the QPSK marker stops and /run/rig-qpsk-markers.csv stops growing -- with
# nothing saying so. The permanent cam2-painter.service (#863: Restart=always, RestartSec=2,
# WantedBy=multi-user.target; marker default-ON under --paint-only since #984) is exactly the
# durable, supervised mechanism the "must-stay-alive" rule needs.
#
# This builder is the HANDOFF: rig-mode.sh test keeps its transient painter ONLY for the
# at-mode-set chain verification, then calls this to (1) stop the transient painter (free
# fb0/DRM so the unit does not race it, #440), (2) `systemctl enable --now cam2-painter.service`
# (enable -> survive reboot + re-arm after any EVENT #892 disable; --now -> start immediately),
# (3) verify it is active + GENUINELY PAINTING (presenter-aware #464) + the marker CSV GROWING
# (#431), FAILING LOUD (exit 1) on any miss -- a durable steady state that is claimed must be
# proven, never a silent 2h nohup.
#
# Source-only: pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase. REUSES audio_marker_emission_check_cmds (scripts/lib/audio-
# marker-check.sh, already sourced by rig-mode.sh) for the marker-growth assert, so the two call
# sites can never drift on what "the marker is actually emitting" means.

# cam2_painter_steady_state_handoff_cmds PIDFILE [MARKER_LOG] -> REMOTE bash (embed via
# `cam_ssh "$(cam2_painter_steady_state_handoff_cmds "$PAINTER_PIDFILE" "$AUDIO_MARKER_LOG")"`
# at the END of do_test, after the whole chain has verified). MARKER_LOG defaults to the path the
# permanent unit's --marker-log writes (/run/rig-qpsk-markers.csv), which is also rig-mode.sh's
# AUDIO_MARKER_LOG default. FAIL LOUD (exit 1) makes the enclosing `cam_ssh` return non-zero, so
# rig-mode.sh's own `set -euo pipefail` aborts TEST mode -- a durable painter that did not come up
# is never reported as achieved.
# #1148: (H5)'s paint check now sources the shared `_cb_paint_signal` (scripts/lib/cam2-paint-
# signal.sh) instead of an inline copy; lazy-source it and emit its definition before the heredoc.
command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"
# #1175: the enable step must be remount-rw-window-safe on cam2's read-only root. Lazy-source the
# shared persist-state builder (rig-mode.sh already sources it; this covers a test sourcing the
# handoff lib alone) -- same lazy-source pattern as cam2-paint-signal.sh above.
command -v cam2_painter_persist_state_cmds >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-painter-ro-persist.sh"

cam2_painter_steady_state_handoff_cmds() {
  local pidfile="$1"
  local marker_log="${2:-/run/rig-qpsk-markers.csv}"
  cam2_paint_signal_remote_fn
  cat <<HANDOFF
set -e
# (H1) the durable steady-state painter is the PERMANENT cam2-painter.service (#863). It MUST be
#      installed on cam2 -- a missing unit means TEST mode cannot hand steady-state to a durable
#      supervised painter, so FAIL LOUD rather than silently leave a disposable 2h nohup.
if ! systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  echo "FAIL: [#1008] cam2-painter.service is NOT installed on this box -- TEST mode cannot hand steady-state to a durable supervised painter. Provision it (scripts/setup-device.sh installs+enables the #863 unit), then re-run rig-mode.sh test." >&2
  exit 1
fi
# (H2) stop the TRANSIENT verification painter first (free /dev/fb0 + DRM master) so the permanent
#      unit does not race it (#440). Stop via the PID FILE only -- never a pkill matching
#      frame-probe (self-kill footgun + would hit the permanent unit's own frame-probe) -- then
#      wait for it to actually exit before starting the unit.
if [ -f "$pidfile" ]; then
  TPID=\$(cat "$pidfile" 2>/dev/null || true)
  if [ -n "\$TPID" ] && kill -0 "\$TPID" 2>/dev/null; then
    kill "\$TPID" 2>/dev/null || true
    i=0; while kill -0 "\$TPID" 2>/dev/null && [ \$i -lt 20 ]; do sleep 0.5; i=\$((i+1)); done
  fi
  rm -f "$pidfile" 2>/dev/null || true
fi
# (H3) hand STEADY STATE to the permanent unit: ENABLE (survive reboot; re-arm after any EVENT #892
#      disable) + START NOW. reset-failed first so a prior failed state never blocks the start.
#      #1175: the enable runs inside a remount-rw window and FAILS LOUD + verifies is-enabled=enabled
#      (cam2's read-only root would otherwise silently swallow the symlink write, leaving the unit
#      unenabled -> dead at the next reboot while this handoff claimed "survives reboot").
systemctl reset-failed cam2-painter.service 2>/dev/null || true
$(cam2_painter_persist_state_cmds enable-now)
echo "[#1008] handed TEST-mode steady state to the permanent cam2-painter.service (enabled + started -- Restart=always, survives reboot)"
# (H4) verify ACTIVE -- FAIL LOUD.
_h=0; while [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ] && [ \$_h -lt 16 ]; do sleep 0.5; _h=\$((_h+1)); done
if [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ]; then
  echo "FAIL: [#1008] cam2-painter.service did NOT become active after 'enable --now' -- the TEST-mode steady-state painter is DOWN." >&2
  systemctl status cam2-painter.service --no-pager >&2 2>/dev/null || true
  exit 1
fi
# (H5) verify it is GENUINELY PAINTING -- presenter-aware (#464), via the shared _cb_paint_signal
#      (#1148): the default --presenter auto lands on KMS page-flip (holds a DRM card, never
#      /dev/fb0); the fbdev fallback holds /dev/fb0. FAIL LOUD -- an "active" unit painting nothing
#      still leaves the monitor black.
_hp=0; _hok=""
while [ \$_hp -lt 16 ]; do
  _hj="\$(journalctl -u cam2-painter.service -n 120 --no-pager 2>/dev/null || true)"
  if printf '%s\n' "\$_hj" | _cb_paint_signal >/dev/null 2>&1; then _hok=1; break; fi
  sleep 0.5; _hp=\$((_hp+1))
done
if [ -z "\$_hok" ]; then
  echo "FAIL: [#1008] cam2-painter.service active but NOT genuinely painting (no KMS DRM device held+vblank-locked, and /dev/fb0 not held) -- monitor may be black. See: journalctl -u cam2-painter.service" >&2
  exit 1
fi
echo "PASS: [#1008] cam2-painter.service active + genuinely painting (durable steady-state painter up)"
$(audio_marker_emission_check_cmds "$marker_log" "true" "permanent cam2-painter.service steady-state")
echo "PASS: [#1008/#937] durable TEST-mode painter handoff complete -- QR + QPSK marker now supervised (Restart=always), survive crash + reboot; no more silent 2h expiry."
HANDOFF
}
