#!/bin/bash
# #769 — imag projector windowed-stray heal (sourced by scripts/recording-e2e.sh).
#
# OBS's launch-restore can recreate a projector as a WINDOWED window (internal monitor = -1).
# OBSBasic::OpenProjector()'s CloseExistingProjectors replace loop closes only projectors whose
# GetMonitor() == the target monitor, so a windowed stray is INVISIBLE to it — every
# `obs_phase2.py open-projectors` call then stacks one more window while the stray survives
# (live ping-pong: 3 gate refusals on `Multiview=2..3` in one afternoon, 2026-07-15). A WM-side
# `wmctrl -b add,fullscreen` does NOT fix it: it changes the X window state, not OBS's internal
# monitor index, so the replace loop still cannot see the window.
#
# imag_projector_heal_cmds prints the REMOTE bash snippet (run over ssh on imag, same pattern as
# scripts/lib/rig-test-dropin.sh): for each projector kind, enumerate matching windows, KEEP the
# NEWEST (highest X window id — the one the ensure-open step JUST opened on the proper monitor)
# and close the older ones. The [0/8] count check that runs right after stays the LOUD backstop:
# if the heal does not converge to exactly 1+1 (a genuinely regressed CloseExistingProjectors
# config, #756), the gate still refuses — this heal only removes the mechanical restore artifact.
# shellcheck disable=SC2016  # the snippet must expand $kind/$ids REMOTELY, not here
imag_projector_heal_cmds() {
  cat <<'HEAL_EOF'
export DISPLAY=:0 XAUTHORITY="$HOME/.Xauthority"
for kind in Multiview Program; do
  ids="$(wmctrl -l 2>/dev/null | grep -F "Projector - $kind" | cut -d' ' -f1 | sort)"
  n="$(printf '%s\n' "$ids" | grep -c . || true)"
  if [ "${n:-0}" -gt 1 ]; then
    keep="$(printf '%s\n' "$ids" | tail -1)"
    for id in $ids; do
      [ "$id" = "$keep" ] && continue
      wmctrl -i -c "$id" && echo "healed: closed stray $kind projector $id (kept newest $keep)"
    done
  fi
done
sleep 1
HEAL_EOF
}
