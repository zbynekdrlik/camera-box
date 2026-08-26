#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-handoff.sh, cam2-painter-deadman.sh,
# rig-test-dropin.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so imposing strict mode here would leak into whichever caller
# sources it. scripts/rig-mode.sh (the only caller) already sets it.
#
# scripts/lib/cam2-fb0-blank.sh -- the EVENT-mode-stop framebuffer blank (#1176).
#
# WHY (#1176): rig-mode.sh event stops the cam2 painter with SIGTERM (kill $PID / pkill -x
# frame-probe). The issue-660 clean blank (src/probe/fb.rs::blank_fbdev) runs ONLY inside
# KmsPresenter's Drop (a clean --duration-secs self-exit), which SIGTERM bypasses -- so /dev/fb0
# memory keeps the last painted frame (e.g. a lipsync-test-mode.sh ffmpeg raw-fbdev write). On cam2
# (the #892 painter box) camera-box carries the permanent CAMERA_BOX_NO_DISPLAY=1 drop-in, so after
# the painter releases DRM master /dev/fb0 is left UNHELD and the kernel fbdev emulation
# (CONFIG_DRM_FBDEV_EMULATION) scans out the stale memory onto the HDMI monitor (owner-reported
# 2026-08-23: a frozen SyncNet lipsync face after `rig-mode.sh event`). The ledger clean-paint
# fallback (rig_test_ledger_clean_paint_fallback_cmds) only fires on KILL_NEEDED=1 (a SIGKILL); a
# clean SIGTERM stop leaves KILL_NEEDED=0, so nothing blanks fb0.
#
# This builder is the UNCONDITIONAL EVENT-stop blank: painter_stop_remote embeds it after the painter
# is stopped + fb0 released, so an EVENT-mode stop ALWAYS leaves the display surface clean. It mirrors
# the #660 fallback's mechanism (a raw-zero write of the fbdev memory) but with a DIFFERENT trigger --
# unconditional on every EVENT stop, not only after a SIGKILL -- and a different call site, so it is a
# separate one-line builder rather than a cross-purposed dependency into the anchor-fragile ledger lib.
# Best-effort (|| true): a blank failure must never abort the EVENT flow.
#
# Source-only: a pure string builder, no ssh, no side effects at source time.

# cam2_fb0_blank_cmds [FB_DEVICE] -> REMOTE bash (embed via `$(cam2_fb0_blank_cmds)` in a remote
# heredoc) that unconditionally zeroes FB_DEVICE (default /dev/fb0). count=8 MiB covers a 4K fbdev.
cam2_fb0_blank_cmds() {
  local fb_device="${1:-/dev/fb0}"
  cat <<CMDS
echo "[#1176] EVENT stop: blank cam2 ${fb_device} so a leftover frame (a lipsync raw-fbdev write, issue-660 class) is not revealed on the HDMI monitor by kernel fbdev emulation after the painter released DRM master (the issue-660 clean blank runs only in the KmsPresenter Drop, which SIGTERM bypasses)"
dd if=/dev/zero of="${fb_device}" bs=1M count=8 2>/dev/null || true
CMDS
}
