#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time) -- mirrors
# scripts/lib/capture-rate-guard.sh / scripts/lib/udev-camera-box.sh convention of deliberately NOT
# setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so
# imposing strict mode here would leak into whichever caller sources it (recording-e2e.sh already
# sets its own).
#
# scripts/lib/self-heal-attribution.sh -- the #895 mid-recording self-heal-RESET scan: did
# capture_rate_selfheal (#663) USB-reset a capture device on ANY active camera during this
# recording window, so recording-verdict.rs can attribute the resulting stale/duplicate frames to
# self_heal_reset instead of misreporting them as frozen_leg (a camera fault)?
#
# WHY THIS IS A SEPARATE CHECK FROM capture-rate-guard.sh's existing [7b/8] mid-recording recheck
# (#705, already shipped): that check greps ONLY the literal "#656 capture-delivery-rate
# DEFECTIVE" text -- the WARN src/main.rs logs on its JITTER-band trigger branch. The self-heal
# trigger has a SECOND, independent branch, the SUSTAINED band (#717, a narrower, longer-window
# tolerance for a chronic deviation that stays inside the wide jitter envelope), which logs a
# DIFFERENT literal: "#717 capture-delivery-rate SUSTAINED defect: ...". A self-heal reset
# triggered via the sustained branch alone is invisible to that existing grep, sails through
# unflagged, reaches recording-verdict.rs, and its resulting duplicate/stale frames get classified
# frozen_leg on the camera -- exactly the misdiagnosis #895 exists to fix. That existing check is
# also scoped to the single "source" camera only, never swept across every active box, even though
# capture_rate_selfheal runs identically on every one of them.
#
# THE FIX: grep for the RESET event ITSELF (see self_heal_reset_grep_pattern below) rather than
# either upstream detection band's WARN text -- this is the ONE line both trigger branches share
# (src/main.rs's shared SelfHealDecision::Heal match arm), so a hypothetical future THIRD detection
# band is automatically covered too, with no further harness change needed. The design decision is
# ALLOW, not SUPPRESS: self-heal keeps firing during a measurement (the underlying rate defect is
# real, tracked separately -- disabling the safety net would trade today's misdiagnosis for a
# worse, silent one). Every detected reset is (1) printed loudly right here, at the point it is
# discovered (never only in post-hoc forensics), and (2) threaded into recording-verdict.rs via a
# repeatable --self-heal-reset CAMBOX:EPOCH_NS flag, so the pure src/self_heal_attribution.rs
# module can correlate it against the classified per-window frozen_leg data and re-attribute
# honestly -- while STILL gating the run (the reset itself is a real run-integrity concern,
# whether or not it also produced a classified-Frozen window; #895 acceptance criterion 4).

# self_heal_reset_grep_pattern -> the journalctl grep proving capture_rate_selfheal ITSELF
# actually performed a USB reset (src/capture_rate_selfheal.rs's perform_usb_reset success path,
# logged verbatim from src/main.rs's SelfHealDecision::Heal match arm) -- the ONE signal shared by
# BOTH the #656 jitter-band and #717 sustained-band trigger paths.
self_heal_reset_grep_pattern() { echo '#663 self-heal: USB reset attempt #[0-9]+ succeeded'; }

# self_heal_reset_window_journalctl_cmd INVOCATION_ID SINCE_EPOCH UNTIL_EPOCH -> the REMOTE
# journalctl command text that reads ONLY the CURRENT camera-box.service process instance's
# (#693 _SYSTEMD_INVOCATION_ID scoping, same discipline as capture_rate_window_journalctl_cmd) log
# lines whose OWN timestamp falls within [SINCE_EPOCH, UNTIL_EPOCH] -- `-o short-unix` so each
# matched line's own timestamp is directly extractable as EPOCH.MICROSEC with no bash-side date
# parsing or timezone assumptions needed. Falls back to the unscoped "-u camera-box" form when
# INVOCATION_ID is empty, same fallback contract as its capture-rate-guard.sh sibling.
#
# Pure string builder (no ssh, no I/O) -- directly unit-testable without a live rig.
self_heal_reset_window_journalctl_cmd() {
  local invocation_id="${1:-}" since_epoch="${2:-}" until_epoch="${3:-}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --since=@%s --until=@%s -o short-unix --no-pager 2>/dev/null' \
      "$invocation_id" "$since_epoch" "$until_epoch"
  else
    printf 'journalctl -u camera-box --since=@%s --until=@%s -o short-unix --no-pager 2>/dev/null' \
      "$since_epoch" "$until_epoch"
  fi
}

# self_heal_reset_events_from_output SSH_OUTPUT_TEXT -> zero or more epoch-NANOSECOND timestamps,
# one per line, parsed from each matched line's leading `-o short-unix` "SEC.USEC " field. Pure
# parse (no I/O). short-unix's fractional part is exactly 6 digits (microseconds); ns = sec*1e9 +
# usec*1000, built via string concatenation (not arithmetic) so it never overflows awk's
# double-precision float mantissa the way a direct multiply of two large numbers could.
self_heal_reset_events_from_output() {
  printf '%s\n' "$1" \
    | { grep -E "$(self_heal_reset_grep_pattern)" || true; } \
    | awk -F'[ .]' '{ printf "%d%06d000\n", $1, $2 }'
}

# self_heal_reset_scan_message CAMBOX AT_NS -> the operator-facing line printed the moment a reset
# is discovered (pure formatting, no I/O) -- distinctly labeled so a human/CI reader never mistakes
# this for a frozen-camera accusation.
self_heal_reset_scan_message() {
  local cambox="${1:-?}" at_ns="${2:-?}"
  printf '%s self-heal RESET detected at %s ns (epoch) during this recording -- capture_rate_selfheal (#663) USB-reset the capture device; recording-verdict.rs will attribute any resulting stale/duplicate frames to self_heal_reset, NOT frozen_leg\n' \
    "$cambox" "$at_ns"
}
