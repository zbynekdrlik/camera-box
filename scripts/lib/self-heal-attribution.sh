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

# NOTE (issue 946 / issue 910): the self-heal-ONLY parser (self_heal_reset_events_from_output) and
# operator message (self_heal_reset_scan_message) were superseded by the UNIFIED recognised-event
# table below (restart_events_from_journal_output / restart_events_from_burn_log_output /
# restart_event_scan_message), which tags each event with its KIND and also reads the burn-instance
# log. self_heal_reset_grep_pattern (above) and self_heal_reset_window_journalctl_cmd (below) are
# still used verbatim by that table + the harness journal-window read.

# ---------------------------------------------------------------------------------------------
# issue 946 + issue 910 -- the UNIFIED recognised-event table (one table, never three parallel
# greps). The #663 self-heal reset joins the issue-945 capture-wedge (exit 79) and issue-944
# emit-freeze (exit 81) watchdog CRITICAL lines as attributable run-integrity restart events. All
# three are read from BOTH journald AND -- during an E2E burn, when camera-box.service is stopped
# and each camera runs as a transient systemd-run burn unit logging to /tmp/cbox-burn*.log
# (issue 910, mirroring the issue-992 capture-rate burn-log read) -- the burn instance's own log.
# ---------------------------------------------------------------------------------------------

# restart_event_kind_patterns -> the recognised-event TABLE, one `LABEL<TAB>GREP_PATTERN` row per
# kind. The label is the SAME string src/self_heal_attribution.rs's RestartEventKind::label()
# emits (the tagged CLI token `--restart-event LABEL:CAMBOX:EPOCH_NS`), so the bash scan and the
# Rust correlator can never drift. Each pattern keys on the ONE distinct line that kind logs (the
# shared #663 reset line, or each watchdog's uniquely-worded CRITICAL line) -- never an upstream
# detection band's WARN wording (the generalisable lesson in
# .claude/rules/self-heal-frozen-leg-attribution.md).
restart_event_kind_patterns() {
  printf '%s\t%s\n' 'self_heal_reset' "$(self_heal_reset_grep_pattern)"
  printf '%s\t%s\n' 'capture_wedge' 'CRITICAL #945: capture/emit thread WEDGED'
  printf '%s\t%s\n' 'emit_freeze' 'CRITICAL #944: NDI output FROZEN'
}

# restart_event_grep_pattern -> the combined `grep -E` alternation of every recognised kind's
# pattern (one grep pass finds any recognised event). Pure string builder.
restart_event_grep_pattern() {
  restart_event_kind_patterns | awk -F'\t' 'NR>1{printf "|"} {printf "%s", $2}'
  printf '\n'
}

# restart_event_kind_for_line LINE -> the kind LABEL for a single matched log line (empty if the
# line matches no recognised pattern). Tests the table rows in order (self-heal first) and returns
# the first match -- pure classification, no I/O beyond the grep it runs on the passed-in text.
restart_event_kind_for_line() {
  local _line="$1" _label _pat
  while IFS="$(printf '\t')" read -r _label _pat; do
    [ -z "$_label" ] && continue
    if grep -qE "$_pat" <<<"$_line"; then
      printf '%s\n' "$_label"
      return 0
    fi
  done <<EOF
$(restart_event_kind_patterns)
EOF
}

# restart_events_from_journal_output SSH_OUTPUT_TEXT -> zero or more `LABEL:EPOCH_NS` lines, one
# per recognised event, parsed from a `-o short-unix` journalctl dump (each matched line's leading
# "SEC.USEC " field). Pure parse. Same ns-from-short-unix string-concat math as the #895 self-heal
# path (never a large multiply that could overflow awk's double mantissa), now tagged with the
# event KIND from the recognised-event table. The grep stage is `|| true`-guarded so a ZERO-match
# input (the common case) never trips `set -o pipefail` (the gotcha in the governing rule).
restart_events_from_journal_output() {
  printf '%s\n' "$1" \
    | { grep -E "$(restart_event_grep_pattern)" || true; } \
    | while IFS= read -r _line; do
        [ -z "$_line" ] && continue
        _kind="$(restart_event_kind_for_line "$_line")"
        [ -z "$_kind" ] && continue
        _ns="$(printf '%s\n' "$_line" | awk -F'[ .]' '{ printf "%d%06d000", $1, $2 }')"
        case "$_ns" in '' | *[!0-9]*) continue ;; esac
        printf '%s:%s\n' "$_kind" "$_ns"
      done
}

# restart_events_from_burn_log_output BURN_LOG_TEXT -> zero or more `LABEL:EPOCH_NS` lines parsed
# from a camera-box BURN-instance log (issue 910). Unlike journald `-o short-unix`, the burn log is
# camera-box's own tracing_subscriber stdout: each line is ANSI-colour-wrapped and carries a
# microsecond RFC3339-Z timestamp as its first field (live-verified 2026-08-14: e.g.
# `\x1b[2m2026-08-14T10:17:56.523683Z\x1b[0m \x1b[33m WARN\x1b[0m ... message`). So: strip the
# ANSI escapes, take the first field, and convert the RFC3339-Z timestamp to epoch-ns with
# `date -u -d ... +%s%N` (deterministic; runs locally on dev1 where the harness parses, never on
# the remote). A line whose timestamp will not parse is skipped, never emitted malformed. Pure
# parse (the grep stage is `|| true`-guarded, same pipefail-safe contract as the journal sibling).
restart_events_from_burn_log_output() {
  printf '%s\n' "$1" \
    | { grep -E "$(restart_event_grep_pattern)" || true; } \
    | while IFS= read -r _line; do
        [ -z "$_line" ] && continue
        _stripped="$(printf '%s' "$_line" | sed -E 's/\x1b\[[0-9;]*m//g')"
        _kind="$(restart_event_kind_for_line "$_stripped")"
        [ -z "$_kind" ] && continue
        _ts="$(printf '%s' "$_stripped" | awk '{print $1}')"
        _ns="$(date -u -d "$_ts" +%s%N 2>/dev/null)"
        case "$_ns" in '' | *[!0-9]*) continue ;; esac
        printf '%s:%s\n' "$_kind" "$_ns"
      done
}

# restart_event_burn_log_grep_cmd LOG_PATH -> the REMOTE command text that greps a burn instance's
# OWN log FILE for ANY recognised restart-event line (the journald-blind sibling of
# self_heal_reset_window_journalctl_cmd; mirrors capture_rate_burn_log_grep_cmd, issue 992). No
# epoch window needed: the deploy step `rm -f`s the log immediately before systemd-run launches
# THIS run's burn, so the file's whole content is already scoped to this recording. NO `tail -1`
# here (unlike the capture-rate check) -- every event in the window must be threaded through, so
# the Rust correlator can attribute each to its own window. Pure string builder (no ssh, no I/O).
restart_event_burn_log_grep_cmd() {
  local _log="$1"
  printf 'grep -E '\''%s'\'' "%s" 2>/dev/null' "$(restart_event_grep_pattern)" "$_log"
}

# restart_event_scan_message KIND CAMBOX AT_NS -> the operator-facing line printed the moment a
# restart event is discovered (pure formatting, no I/O). Distinctly KIND-labelled and explicitly
# disclaiming a frozen-camera accusation, so a human/CI reader never mistakes a run-integrity
# restart for a camera fault -- the same reassurance shape as the #895 self_heal_reset_scan_message.
restart_event_scan_message() {
  local _kind="${1:-?}" _cambox="${2:-?}" _at_ns="${3:-?}"
  printf '%s %s event detected at %s ns (epoch) during this recording -- a run-integrity restart (%s); recording-verdict.rs will attribute any resulting stale/duplicate frames to this event, NOT frozen_leg\n' \
    "$_cambox" "$_kind" "$_at_ns" "$_kind"
}
