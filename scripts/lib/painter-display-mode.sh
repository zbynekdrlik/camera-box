#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the scripts/lib/cbox-burn-log-persist.sh / capture-rate-guard.sh
# convention (no top-level `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/painter-display-mode.sh -- opt-in painter display-mode override passthrough (#1179).
#
# The painter-side `--display-mode WxH@RR` flag is merged on frame-probe (src/bin/frame-probe.rs +
# src/painter_mode.rs, the 2560x1080@100 experiment from issue 881). This lib lets the E2E harness
# (scripts/recording-e2e.sh) run a full cam->strih->stream sweep at that override: when the operator
# sets PAINTER_DISPLAY_MODE, every painter recording-e2e.sh LAUNCHES gets `--display-mode <mode>`;
# when it is unset/empty the painter launch is BYTE-IDENTICAL to today (no flag at all -> frame-probe
# uses its CLI defaults). rig-mode.sh's own TEST painter is deliberately out of scope (pinned to
# fixed constants) -- the E2E harness is the sweep vehicle.
#
# The DECISION LOGIC (resolve the mode, validate it, print the flag args) lives here as a pure
# function so it is Tier-0 unit-testable (tests/harness_painter_display_mode_1179.rs); the
# recording-e2e.sh launch sites are thin callers that embed the result the same way the existing
# OPTIONAL `$_cam2_marker_flags` variable is embedded (the #675 sourced-helper pattern -- the static
# anchor tests read only recording-e2e.sh's own text, so this lib's body is invisible to them).
# Source-only: no side effects at source time.

# painter_display_mode_args [MODE] -> print the frame-probe painter display-mode flag args.
#   MODE defaults to $PAINTER_DISPLAY_MODE. When empty/unset: print NOTHING and return 0 (the
#   byte-identical-to-today path -- the caller embeds an empty string, no --display-mode token).
#   When set: validate the WxH@RR shape parse_display_mode accepts (integer W/H, integer-or-
#   fractional RR) -- this ALSO blocks shell-metacharacter injection into the remote ssh command --
#   then print `--display-mode <MODE>`. A malformed value is a FAIL-LOUD error (message to stderr,
#   return 1): captured via `VAR="$(painter_display_mode_args)"` under the caller's `set -euo
#   pipefail`, that aborts the run in seconds -- before any ssh/deploy -- instead of a painter dying
#   on the box and being caught by a downstream freshness gate minutes later.
painter_display_mode_args() {
  local mode="${1:-${PAINTER_DISPLAY_MODE:-}}"
  [ -n "$mode" ] || return 0
  if ! printf '%s' "$mode" | grep -qE '^[0-9]+x[0-9]+@[0-9]+(\.[0-9]+)?$'; then
    echo "ERROR: PAINTER_DISPLAY_MODE='$mode' is not a valid WxH@RR display mode (e.g. 2560x1080@100 or 1920x1080@59.94)" >&2
    return 1
  fi
  printf -- '--display-mode %s' "$mode"
}
