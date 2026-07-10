#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/ndi-alive.sh convention which is also `set -euo pipefail`-free for the same reason.
#
# scripts/lib/capture-rate-guard.sh — shared "capture-delivery-rate defective" journal signal
# (#656 prevention item 2).
#
# The appliance's OWN capture loop (src/capture_rate_health.rs + its src/main.rs call site)
# already logs a WARN naming #656 once a camera's captured fps has sustained a >1% deviation
# from its negotiated capture rate for CAPTURE_RATE_WARN_WINDOWS (6) consecutive 5s report
# windows — the exact #656 root cause (cam1's ShadowCast 2 silently delivering ~64fps instead
# of its negotiated 60.000fps, producing a persistent ~4Hz content-duplicate judder that was
# only caught after the fact via tick-pattern archaeology on a full recording). Rather than
# re-deriving the fps math a SECOND time in bash (a copy that could drift from the Rust
# decision), `scripts/recording-e2e.sh`'s preflight simply GREPS the source camera's recent
# journal for that WARN before a doomed 30-minute E2E run gets kicked off.
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# capture_rate_defect_grep_pattern -> the journalctl grep proving the appliance ITSELF already
# detected a sustained capture-rate defect (see src/capture_rate_health.rs's WARN message,
# emitted verbatim from src/main.rs's capture-loop report block).
capture_rate_defect_grep_pattern() { echo '#656 capture-delivery-rate DEFECTIVE'; }

# capture_rate_preflight_message CAMERA_NAME MATCHED_LINE -> the operator-facing fail message, a
# pure string formatter (no I/O) so it is directly unit-testable. Extracts the captured/
# configured fps values straight out of the matched WARN line
# ("... N.NN fps captured vs M.MM fps configured/negotiated (...) ...", src/main.rs) when the
# shape matches; falls back to echoing the raw matched line otherwise (never silently swallows
# the signal just because the message format drifted).
capture_rate_preflight_message() {
  local cam="$1" line="$2" captured configured
  captured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps captured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  configured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps configured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  if [ -n "$captured" ] && [ -n "$configured" ]; then
    echo "${cam} capture rate defective (~${captured}fps, expected ${configured}fps) — USB-reset the grabber (see #656)"
  else
    echo "${cam} capture rate defective (see #656): ${line}"
  fi
}
