#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time) — matches the
# sibling scripts/lib/*.sh convention (cambox-offline-ack.sh, camera-box-restart-verify.sh) of
# deliberately NOT setting `set -euo pipefail`: sourcing this file executes it in the CALLER's
# shell, so strict mode here would leak into whichever caller sources it. deploy-fleet.sh (the only
# caller today) already sets -euo pipefail itself.
#
# scripts/lib/frame-probe-deploy.sh — the ONE genuinely-pure decision for the #1138 frame-probe
# (cam2 painter) fleet deploy: how to RESTORE the cam2-painter.service enabled-state after swapping
# /usr/local/bin/frame-probe. The ssh/scp/byte-verify orchestration lives inline in deploy-fleet.sh
# (mirroring its camera-box loop); only this state-preserving decision is factored out so it is
# Tier-0 unit-testable without a rig (tests/frame_probe_deploy_1138.rs).

# frame_probe_restore_enable_decision IS_ENABLED_STATE -> "enable-now" | "leave"
#
# #892 lifecycle discipline (.claude/rules/cam2-painter-lifecycle.md): the cam2-painter.service
# enabled-state IS the mode discriminator — `enabled` = devel/TEST mode (the standing QR painter),
# `disabled` = EVENT mode (rig-mode.sh event deliberately DISABLES it so a QR can never return onto
# a LIVE broadcast via a restart/reboot). A binary swap must PRESERVE that intent: re-arm ONLY a
# persistently-enabled unit (`enable --now` with the new binary); LEAVE everything else untouched —
# `disabled` (event mode stays dark; the next `rig-mode.sh test` re-arms it with the new binary),
# and also `static` / `masked` / `enabled-runtime` (transient) / "" (unreadable) / not-installed,
# none of which is a persistent devel-mode painter this deploy should light up. Blindly running
# `enable --now` on every deploy is exactly the "naive restart fights the unit" hazard #1138 warns
# against — a merge landing mid-event would paint a QR onto air.
frame_probe_restore_enable_decision() {
  case "${1:-}" in
    enabled) printf 'enable-now' ;;
    *) printf 'leave' ;;
  esac
}
