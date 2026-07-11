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

# capture_rate_journalctl_cmd INVOCATION_ID -> the REMOTE journalctl command text that reads
# ONLY the CURRENT camera-box.service process instance's log lines (scoped via
# _SYSTEMD_INVOCATION_ID), falling back to the OLD unscoped "-u camera-box -n 200" form when
# INVOCATION_ID is empty (systemctl show failed/unavailable -- never silently skip the whole
# preflight just because the invocation id couldn't be read).
#
# WHY (#693): `journalctl -u <unit>` spans ACROSS service restarts -- it is NOT scoped to the
# CURRENTLY RUNNING process. Live-diagnosed 2026-07-11: cam1's camera-box.service was bounced by
# an earlier gate run's routine cleanup() restart; the OLD process instance's #656 DEFECTIVE WARN
# (logged 2s BEFORE the restart) was still inside the NEW instance's "-n 200" lookback window and
# false-failed the preflight even though the new process's own captured rate was already healthy
# (59.8-59.9fps, confirmed live). Same journal-freshness bug class already fixed for DanteSync
# journal reads (#550/#591/#595/#607, and #686 today) -- a journal-window check with no
# freshness/process-instance boundary can read a stale line and false-fail a currently-healthy
# node. `_SYSTEMD_INVOCATION_ID=<uuid>` (the running unit's own InvocationID, from `systemctl show
# -p InvocationID --value camera-box`) restricts journalctl to ONLY that exact process instance's
# lines -- a WARN from a killed prior instance can never leak into the lookback window again.
#
# Pure string builder (no ssh, no I/O) -- the caller substitutes INVOCATION_ID (already resolved
# over ssh) locally, so this is directly unit-testable without a live rig.
capture_rate_journalctl_cmd() {
  local invocation_id="${1:-}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --no-pager -n 200 2>/dev/null' "$invocation_id"
  else
    printf 'journalctl -u camera-box --no-pager -n 200 2>/dev/null'
  fi
}
