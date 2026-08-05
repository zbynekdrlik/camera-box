#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/ndi-alive.sh convention which is also `set -euo pipefail`-free for the same reason.
#
# scripts/lib/capture-rate-guard.sh — shared "capture-delivery-rate defective" journal signal
# (#656 prevention item 2).
#
# The appliance's OWN capture loop (src/capture_rate_health.rs + its src/main.rs call site)
# already logs a WARN naming #656 once a camera's captured fps has sustained a deviation from
# its negotiated capture rate (>1% for most grabber models; #685 widens this to >10% for
# ShadowCast 2 specifically, since that model's own USB output clock free-runs even against its
# own HDMI input, producing a characteristic quantized-rate wobble that isn't a real defect) for
# CAPTURE_RATE_WARN_WINDOWS (6) consecutive 5s report windows — the exact #656 root cause (cam1's
# ShadowCast 2 silently delivering ~64fps instead of its negotiated 60.000fps, producing a
# persistent ~4Hz content-duplicate judder that was only caught after the fact via tick-pattern
# archaeology on a full recording). Rather than re-deriving the fps math a SECOND time in bash (a
# copy that could drift from the Rust decision, including the per-model tolerance), this preflight
# simply GREPS the source camera's recent journal for that WARN (the grep pattern below matches on
# the "#656 capture-delivery-rate DEFECTIVE" substring only — it does not care which tolerance
# fired it) before a doomed 30-minute E2E run gets kicked off.
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

# capture_rate_journalctl_cmd INVOCATION_ID [LINES] -> the REMOTE journalctl command text that
# reads ONLY the CURRENT camera-box.service process instance's log lines (scoped via
# _SYSTEMD_INVOCATION_ID), falling back to the OLD unscoped "-u camera-box -n LINES" form when
# INVOCATION_ID is empty (systemctl show failed/unavailable -- never silently skip the whole
# preflight just because the invocation id couldn't be read). LINES defaults to 200 (the original
# recording-e2e.sh call site passes only the invocation id and is unaffected by this default).
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
# #694: deploy-fleet.sh / verify-device.sh / upgrade-fleet-ndi.sh have the SAME exposure but read
# 200- or 300-line windows for their own emit-ok/FATAL/acceptance checks -- the optional LINES arg
# lets every caller reuse this ONE scoped builder instead of duplicating the scoping logic per
# script with a hardcoded line count.
#
# Pure string builder (no ssh, no I/O) -- the caller substitutes INVOCATION_ID (already resolved
# over ssh) locally, so this is directly unit-testable without a live rig.
capture_rate_journalctl_cmd() {
  local invocation_id="${1:-}" lines="${2:-200}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --no-pager -n %s 2>/dev/null' "$invocation_id" "$lines"
  else
    printf 'journalctl -u camera-box --no-pager -n %s 2>/dev/null' "$lines"
  fi
}

# capture_rate_window_journalctl_cmd INVOCATION_ID SINCE_EPOCH UNTIL_EPOCH -> the REMOTE
# journalctl command text that reads ONLY the CURRENT camera-box.service process instance's
# (#693 _SYSTEMD_INVOCATION_ID scoping) log lines whose OWN timestamp falls within
# [SINCE_EPOCH, UNTIL_EPOCH] (systemd's native absolute-time `--since=@N`/`--until=@N` form -- no
# bash-side timestamp parsing needed here, unlike scripts/clock-offset-guard.sh's
# freshest_offset_us, which has to parse `-o short-iso` lines itself because it grades an
# ALREADY-FETCHED window; this builder instead pushes the window bound down into journalctl
# itself, which is simpler and can't drift from what journalctl considers "the timestamp").
#
# #705: this is the mid-RECORDING sibling of capture_rate_journalctl_cmd (above, which reads the
# last N lines BEFORE a run even starts). A clean [0/8] preflight only proves the source camera
# was healthy at start -- the #656/#663 ShadowCast judder is confirmed to RECUR mid-session (PR
# #704's own real-verdict CI run: cam1's own recurrence_heal_count=30 at the time), so callers
# pass the recording's own StartRecord..StopRecord epoch seconds to prove (or disprove) a defect
# recurred DURING the recording, not just before it. Falls back to the unscoped `-u camera-box`
# form when INVOCATION_ID is empty, same fallback contract as capture_rate_journalctl_cmd.
capture_rate_window_journalctl_cmd() {
  local invocation_id="${1:-}" since_epoch="${2:-}" until_epoch="${3:-}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --since=@%s --until=@%s --no-pager 2>/dev/null' \
      "$invocation_id" "$since_epoch" "$until_epoch"
  else
    printf 'journalctl -u camera-box --since=@%s --until=@%s --no-pager 2>/dev/null' \
      "$since_epoch" "$until_epoch"
  fi
}

# capture_rate_recurrence_message CAMERA_NAME MATCHED_LINE -> the operator-facing diagnostic for
# a #656/#663-class capture-rate defect that RECURRED DURING the recording window (as opposed to
# only failing the pre-recording [0/8] preflight, capture_rate_preflight_message above) -- #705.
# Reuses the SAME captured/configured-fps extraction so the message carries real numbers, but is
# phrased DISTINCTLY ("RECURRED DURING") so a human/CI reader can immediately tell this apart
# from a genuine chain-loss/zero-loss regression surfaced elsewhere in the verdict, without
# manually correlating journalctl timestamps against the recording window by hand (exactly the
# manual-correlation pain #703's own PR #704 diagnosis required).
capture_rate_recurrence_message() {
  local cam="$1" line="$2" captured configured
  captured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps captured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  configured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps configured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  if [ -n "$captured" ] && [ -n "$configured" ]; then
    echo "${cam} capture-rate defect RECURRED DURING this recording (~${captured}fps, expected ${configured}fps) — see #656/#663/#705; this is a KNOWN grabber judder recurrence, not necessarily a NEW chain-loss regression"
  else
    echo "${cam} capture-rate defect RECURRED DURING this recording (see #656/#663/#705): ${line}"
  fi
}

# (#992) capture_rate_defect_grep_pattern_all -> the EXTENDED alternation the mid-recording [7b/8]
# #705 check greps for, covering every band that can indicate a capture-rate defect is active --
# not just the #656 jitter WARN the narrow capture_rate_defect_grep_pattern (above) matches. The
# rig-validated #889 failure mode (ShadowCast sustained ~62-64fps, INSIDE the widened #685 jitter
# tolerance) trips only the #717 SUSTAINED band (informational-only, no reset yet) or the #971
# CHRONIC band, never the #656 jitter WARN -- so the narrow pattern alone would still miss it even
# once the journald-blindness below is fixed. #663 is the shared self-heal-RESET event line
# (src/main.rs's SelfHealDecision::Heal arm, the SAME line self_heal_reset_grep_pattern in
# scripts/lib/self-heal-attribution.sh keys on) -- included here too since a reset having fired at
# all is itself proof a defect recurred, even if this check never sees the upstream band that
# triggered it. Deliberately NOT reusing/replacing capture_rate_defect_grep_pattern -- the [0/8]
# PRE-run preflight (which greps that narrow pattern) is unrelated to this mid-recording ticket and
# stays untouched.
capture_rate_defect_grep_pattern_all() {
  echo '#656 capture-delivery-rate DEFECTIVE|#717 capture-delivery-rate SUSTAINED band confirmed|#971 capture-delivery-rate CHRONIC sustained-band DEFECTIVE|#663 self-heal: USB reset attempt'
}

# (#992) capture_rate_burn_log_grep_cmd LOG_PATH -> the REMOTE command text that greps a capture
# burn instance's OWN log FILE (e.g. /tmp/cbox-burn.log) for capture_rate_defect_grep_pattern_all,
# the journald-blind sibling of capture_rate_window_journalctl_cmd. During an E2E recording the
# harness stops the camera-box.service unit and launches the SOURCE camera's capture as a
# transient systemd-run unit whose stdout/stderr are redirected DIRECTLY to this log file
# (--property=StandardOutput=append:... / StandardError=append:...) -- journald never sees a
# single line of it, so a #705 check scoped to journalctl alone reads an empty/stale window and
# always reports "ok" regardless of what the burn instance actually did (issue 992).
#
# No epoch time-window is needed here (unlike the journalctl-window sibling): the harness already
# does `rm -f LOG_PATH` immediately before systemd-run launches the burn for THIS run, so the
# file's entire lifetime is already 1:1 with this recording -- there is no cross-run staleness to
# guard against the way journalctl (which spans unit restarts) needs one.
#
# Pure string builder (no ssh, no I/O) -- directly unit-testable without a live rig, same
# convention as every other builder in this file.
capture_rate_burn_log_grep_cmd() {
  local log_path="$1"
  printf 'grep -E '\''%s'\'' "%s" 2>/dev/null | tail -1' "$(capture_rate_defect_grep_pattern_all)" "$log_path"
}

# (#992) capture_rate_burn_log_recurrence_message CAMERA_NAME MATCHED_LINE -> like
# capture_rate_recurrence_message, but names the burn-instance LOG FILE as the source (never
# "journal") so an operator/CI reader can immediately tell which of the two checks caught the
# defect, without guessing from the matched line's own text alone.
capture_rate_burn_log_recurrence_message() {
  local cam="$1" line="$2" captured configured
  captured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps captured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  configured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps configured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  if [ -n "$captured" ] && [ -n "$configured" ]; then
    echo "${cam} capture-rate defect RECURRED DURING this recording, per its OWN burn-instance log (~${captured}fps, expected ${configured}fps) — see #656/#663/#717/#971/#992; journald was blind to this (the burn instance logs to a file, not the journal)"
  else
    echo "${cam} capture-rate defect RECURRED DURING this recording, per its OWN burn-instance log (see #656/#663/#717/#971/#992): ${line}"
  fi
}
