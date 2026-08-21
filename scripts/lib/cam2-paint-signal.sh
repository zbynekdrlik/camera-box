#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, presenter-
# liveness-check.sh, audio-marker-check.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak
# into whichever caller sources it. Every caller (recording-e2e.sh, rig-mode.sh, the watchdogs)
# already sets its own strict mode.
#
# scripts/lib/cam2-paint-signal.sh -- #1148: the SINGLE SOURCE OF TRUTH for the presenter-aware
# "is cam2-painter GENUINELY PAINTING (not merely process-alive)?" signal (#863/#860/#464).
#
# WHY (#1148): this exact predicate -- parse the OBS "presenter: using DRM/KMS page-flip (<device>)"
# selection line, then assert either (KMS path) the parsed DRM device is HELD by a process AND a
# 'vblank-locked' confirmation line is present, OR (fbdev fallback) the /dev/fb0 device is held --
# was copy-pasted into FIVE separate remote-bash builders (cam2_painter_restore_verify_cmds,
# cam2_painter_steady_state_handoff_cmds, painter_liveness_check_cmds, mv_reverify_painter_up_cmds,
# cam2_painter_genuine_paint_check_cmd), each re-tuning only the poll count / exit semantics AROUND
# the identical signal. It had already begun to drift (the #1126 copy polled 6x where the original
# polled 8x). This is black-monitor-safety code under the "never mask a black monitor" discipline
# (#863/#860): a future correction to the signal -- an OBS presenter-log rename, a new presenter
# backend -- fixed in ONE copy would silently leave the others able to false-PASS a dead monitor.
# Consolidating the SIGNAL here means one edit corrects all five.
#
# The five call sites keep their OWN poll counts + exit semantics (WARN-only vs FAIL-LOUD vs
# exit-0/1 prune vs PAINTER_UP vs the file-reading granular-message check); only the SIGNAL itself
# is sourced from here. Each site lazy-sources this lib (the bundle-state-selfheal.sh idiom):
#   command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"
# and emits `cam2_paint_signal_remote_fn` (the `_cb_paint_signal` definition) at the top of its
# remote snippet, then pipes its own log source (journalctl output or a painter log FILE) into
# `_cb_paint_signal`.
#
# Source-only: pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase.

# cam2_paint_signal_remote_fn -> REMOTE bash text that DEFINES the function `_cb_paint_signal
# [FB_DEVICE]`. `_cb_paint_signal` reads the painter log text on STDIN, echoes ONE reason token to
# stdout, and RETURNS (never exits) 0 iff the presenter-appropriate painting signal is present:
#   KMS_OK <dev>       (return 0) -- a KMS page-flip presenter line, its parsed DRM device HELD
#                                    (fuser -s), and a 'vblank-locked' confirmation line present.
#   KMS_NODRM <dev>    (return 1) -- a KMS line present but its parsed DRM device is NOT held.
#   KMS_NOVBLANK <dev> (return 1) -- the KMS DRM device is held but no 'vblank-locked' line.
#   FBDEV_OK           (return 0) -- no KMS line; the fb device (default /dev/fb0) is held.
#   FBDEV_DEAD         (return 1) -- no KMS line; the fb device is not held.
# Callers that only need the boolean pipe into `_cb_paint_signal >/dev/null 2>&1`; the granular
# presenter-liveness site captures the token and maps it to its per-failure operator messages.
# `return`, never `exit`, so it is safe both inside a `set -e` remote (the handoff) AND inside
# recording-e2e.sh cleanup()'s WARN-only EXIT trap (a bare `exit` there would abort the trap).
# Single-quoted heredoc -> every $ below is LITERAL remote bash (evaluated on the cam box), so the
# emitted definition embeds identically whether the calling site's own heredoc is single-quoted or
# $-escaped (each site emits this OUTSIDE its own heredoc).
cam2_paint_signal_remote_fn() {
  cat <<'PAINTSIG'
_cb_paint_signal() {
  local _fbdev="${1:-/dev/fb0}"
  local _log _kms _drm
  _log="$(cat)"
  _kms="$(printf '%s\n' "$_log" | grep 'presenter: using DRM/KMS page-flip' | tail -n1 || true)"
  if [ -n "$_kms" ]; then
    _drm="${_kms#*(}"; _drm="${_drm%)*}"
    if [ -z "$_drm" ] || ! fuser -s "$_drm" 2>/dev/null; then
      echo "KMS_NODRM $_drm"; return 1
    fi
    if ! grep -q 'vblank-locked' <<<"$_log"; then
      echo "KMS_NOVBLANK $_drm"; return 1
    fi
    echo "KMS_OK $_drm"; return 0
  fi
  if fuser -s "$_fbdev" 2>/dev/null; then
    echo "FBDEV_OK"; return 0
  fi
  echo "FBDEV_DEAD"; return 1
}
PAINTSIG
}
