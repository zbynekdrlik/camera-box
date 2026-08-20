#!/usr/bin/env bash
# scripts/lib/presenter-liveness-check.sh — SINGLE SOURCE OF TRUTH for the #464 presenter-aware
# painter-liveness check: is the just-launched QR painter ALIVE AND ACTUALLY PAINTING?
#
# WHY (#464, confirmed live on cam2 2026-07-04 — a FALSE FAIL, the painter was fine): rig-mode.sh
# launches the painter with the default `--presenter auto` (no `--presenter` flag). On cam2's
# Intel i915 with a connected HDMI, `auto` acquires DRM master and runs the KMS page-flip
# presenter (crate::probe::kms::KmsPresenter, via src/probe/presenter.rs::open_presenter) — which
# BY DESIGN never opens /dev/fb0 (only the fbdev fallback, crate::probe::fb::VsyncFb, does; see
# src/presenter_kind.rs::resolve_presenter_kind, the pure decision this check mirrors). The OLD
# liveness gate was a bare `fuser -s /dev/fb0`, so a healthy, correctly-painting KMS run was
# reported "FAIL: painter PID <pid> alive but NOT writing /dev/fb0".
#
# Live evidence reproduced exactly (PID 51673):
#   - "presenter: using DRM/KMS page-flip (/dev/dri/card1)"
#   - "KmsPresenter: 1920x1080@60.000Hz, double-buffered DRM page-flip, vblank-locked 1:1"
#   - `fuser /dev/dri/card1` held by frame-probe; `fuser /dev/fb0` held by nobody
#   - /run/rig-qpsk-markers.csv growing (genuinely painting)
#
# The KMS path is the IDEAL path (vblank-locked, tear-free, 1:1 60Hz) — nothing was broken except
# the gate. This check reads LOG_FILE (the painter's own stdout/stderr — `open_presenter` logs
# exactly ONE of "presenter: using DRM/KMS page-flip (<device>)" (success) or "DRM/KMS unavailable
# (...), falling back to fbdev" (failure)) and asserts the signal that actually matches the
# presenter in use:
#   KMS line found   -> the parsed DRM device is HELD (fuser -s <cardN>) AND the "vblank-locked"
#                        confirmation line is present.
#   no KMS line found (fbdev fallback, or an older painter build with no presenter log line at
#   all) -> the ORIGINAL fuser -s FB_DEVICE check, unchanged (#247/#68 behavior preserved — this
#            fix never weakens the fbdev-path gate).
# FAIL LOUD either way (never a silent pass) and `tail` LOG_FILE on failure — the old FAIL branch
# didn't even do that, so an operator saw only "not writing /dev/fb0" with no clue which presenter
# had actually been selected.
#
# Sourced by scripts/rig-mode.sh (painter_launch_remote's step 5). Source-only: pure string
# builder, no ssh, no side effects at source time — mirrors scripts/lib/audio-marker-check.sh's
# shape exactly (a derivation helper + a pure remote-command builder).

# painter_liveness_check_cmds LOG_FILE [FB_DEVICE] -> the REMOTE bash snippet (embed via
# `$(painter_liveness_check_cmds ...)` inside a heredoc) that polls for up to ~10s for the
# presenter-appropriate liveness signal, then asserts it once more and FAILS LOUD if it never
# came up. LOG_FILE and FB_DEVICE are baked in as literal values at generation time (mirrors every
# other _cmds builder in this codebase); the `\$`-escaped names below (KMS_LINE, DRM_DEV, i) are
# runtime bash evaluated on the REMOTE shell.

# #1148: the presenter-appropriate PASS/FAIL predicate is now the shared `_cb_paint_signal`
# (scripts/lib/cam2-paint-signal.sh), which echoes a reason token (KMS_OK / KMS_NODRM /
# KMS_NOVBLANK / FBDEV_OK / FBDEV_DEAD + the parsed DRM device). This site keeps its granular,
# operator-facing FAIL/PASS messages by MAPPING that token, so the SIGNAL lives in one place while
# the diagnostics stay as detailed as before. Lazy-source it; emit its definition before the poll.
command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"

painter_liveness_check_cmds() {
  local log_file="$1"
  local fb_device="${2:-/dev/fb0}"
  cam2_paint_signal_remote_fn
  cat <<PCHECK
i=0
while [ \$i -lt 20 ]; do
  if cat "$log_file" 2>/dev/null | _cb_paint_signal "$fb_device" >/dev/null 2>&1; then break; fi
  sleep 0.5; i=\$((i+1))
done
# NOTE the trailing '|| true': the production caller embeds this snippet under set -e (rig-mode.sh
# painter_launch_remote). Without it a FAIL token (non-zero return) aborts THIS assignment before
# the case runs, silently swallowing the granular FAIL message + log tail (a silent exit 1 - the
# 464 operator-blindness this check exists to prevent). The token is still captured (it is echoed
# before the function returns non-zero), so the case runs and the FAIL arm exits 1 with diagnostics.
_reason="\$(cat "$log_file" 2>/dev/null | _cb_paint_signal "$fb_device" || true)"
_tok="\${_reason%% *}"; _dev="\${_reason#* }"
case "\$_tok" in
  KMS_OK)
    echo "PASS: #464 KMS presenter (\$_dev) alive + vblank-locked (tear-free page-flip; /dev/fb0 is NOT expected to be held)"
    ;;
  KMS_NODRM)
    echo "FAIL: #464 KMS presenter selected (\$_dev) but that DRM device is not held (see $log_file):" >&2
    tail -n 20 "$log_file" >&2 2>/dev/null || true
    exit 1
    ;;
  KMS_NOVBLANK)
    echo "FAIL: #464 KMS presenter (\$_dev) held but no vblank-locked confirmation in $log_file:" >&2
    tail -n 20 "$log_file" >&2 2>/dev/null || true
    exit 1
    ;;
  FBDEV_OK)
    echo "PASS: fbdev presenter alive + painting $fb_device"
    ;;
  *)
    echo "FAIL: painter alive but NOT writing $fb_device (see $log_file):" >&2
    tail -n 20 "$log_file" >&2 2>/dev/null || true
    exit 1
    ;;
esac
PCHECK
}
