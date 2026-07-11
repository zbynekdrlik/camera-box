#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) —
# matches the sibling scripts/lib/*.sh convention (rig-test-dropin.sh, audio-marker-check.sh,
# painter-csv-freshness.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this
# file executes it in the CALLER's shell, so imposing strict mode here would leak into whichever
# caller sources it. recording-e2e.sh (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/camera-box-restart-verify.sh — SINGLE SOURCE OF TRUTH for "restart camera-box, then
# VERIFY it genuinely came back active" remote command text. Sourced by scripts/recording-e2e.sh.
#
# WHY (#675): recording-e2e.sh's cleanup() restore of the SOURCE camera (cam1/cam3/cam4/cam5/cam6)
# and cam2's PRODUCTION `camera-box.service` used to be a bare `systemctl restart camera-box
# 2>/dev/null; true` (or `|| true`) — the trailing `; true`/`|| true` swallows EVERY failure
# silently, including an ssh-level hiccup/connection-contention (several concurrent MCP/ssh
# sessions are routinely open against these same boxes during a dispatch) or an earlier step in
# the same remote one-liner failing first. Live incident (2026-07-11): cam1, cam2, AND cam4's
# PRODUCTION camera-box.service were each left INACTIVE after a `recording-e2e.sh` cleanup
# completed normally (no error printed anywhere in the harness's own log) — hit 4 times in one
# session, twice breaking the REQUIRED "Full-path E2E (rig zero-loss gate)" CI check on an
# unrelated PR (#676) because the gate correctly detected the now-dark camera's NDI feed as
# FROZEN. A follow-up manual reproduction confirmed the restart command itself is NOT inherently
# broken (it succeeds instantly in isolation) — something about the busy harness-execution
# context intermittently drops it, so the fix is detect-and-retry, not a rewritten command.
#
# This restart+verify sequence polls `systemctl is-active camera-box` after the restart, RETRIES
# the restart exactly ONCE if it is not active yet (retried INSIDE the SAME already-open ssh
# session — no new connection, so it sidesteps the suspected connection-contention cause), and —
# only if it is STILL not active after the retry — prints a WARNING #675 line to stderr that is
# NEVER swallowed by a `2>/dev/null` (the outer `timeout ... ssh ...` call in recording-e2e.sh is
# not itself redirected, so this reaches the harness's own console/CI log). It never aborts the
# remote command chain or the caller's cleanup() (the trap must always run to completion — see
# #328/#649) — the loud warning IS the fail-loud signal, mirroring the existing #649 StopRecord
# WARNING blocks in cleanup() itself, which are also warn-only, never fatal.
#
# Source-only: defines a PURE string builder, runs nothing on its own (no ssh) — safe to source
# from recording-e2e.sh and from unit tests. Same "one source, many callers" model as
# rig_test_dropin_clear_cmds in scripts/lib/rig-test-dropin.sh.

# camera_box_verify_active_cmds LABEL -> the REMOTE bash text that polls `systemctl is-active
# camera-box` (~8s), retries `systemctl restart camera-box` ONCE if it is not active yet, re-polls
# (~8s), and — only if STILL not active — echoes a loud WARNING naming LABEL. Meant to be spliced
# in via command substitution immediately AFTER an existing `systemctl restart camera-box` call in
# the SAME remote ssh command string, e.g.:
#     "... systemctl restart camera-box 2>/dev/null; true
# $(camera_box_verify_active_cmds "$CAMERA_NAME (source, $CAM1_IP)")"
#
# PURE string builder: every remote-side `$`/`$(...)` in the heredoc body is backslash-escaped so
# THIS function's own local evaluation (on dev1, where recording-e2e.sh calls it) never expands
# them — only $label (a real local bash parameter) is meant to be substituted locally, into the
# warning/ok text. The unescaped result is literal shell text that the REMOTE host evaluates when
# it receives the full spliced-together command string over ssh.
camera_box_verify_active_cmds() {
  local label="$1"
  cat <<VERIFY
_cbv=0
while [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ] && [ \$_cbv -lt 8 ]; do sleep 1; _cbv=\$((_cbv+1)); done
if [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ]; then
  echo "WARNING #675: camera-box not active on $label after restart -- retrying restart once" >&2
  systemctl restart camera-box 2>/dev/null || true
  _cbv=0
  while [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ] && [ \$_cbv -lt 8 ]; do sleep 1; _cbv=\$((_cbv+1)); done
fi
if [ "\$(systemctl is-active camera-box 2>/dev/null)" != "active" ]; then
  echo "WARNING #675: camera-box FAILED to come back active on $label after restart+retry -- rig may be degraded, verify manually (systemctl status camera-box)" >&2
else
  echo "[cleanup] camera-box active on $label"
fi
VERIFY
}
