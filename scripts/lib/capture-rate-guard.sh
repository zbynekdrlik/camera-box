#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/ndi-alive.sh convention which is also `set -euo pipefail`-free for the same reason.
#
# scripts/lib/capture-rate-guard.sh — shared "capture-delivery-rate defective" journal signal
# (#656 prevention item 2).
#
# The appliance's OWN capture loop (src/capture_rate_health.rs + its src/main.rs call site)
# already logs a WARN naming #656 once a camera's captured fps has sustained a deviation from
# its negotiated capture rate (>1% for most grabber models; #685 widens this to >9% for
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

# (#992 ROZHODNUTÉ -- supervisor, gate rerun 31028767542 evidence, see issue 992 comment
# https://github.com/zbynekdrlik/camera-box/issues/992#issuecomment-5195254731)
#
# capture_rate_defect_grep_pattern_hard -> the alternation for the bands that are genuine defect
# DECLARATIONS or EVENTS, and must still abort the run (exit 1): #656 (the original jitter WARN,
# src/capture_rate_health.rs), #971 (the CHRONIC sustained-band escalation -- the appliance's own
# escalation policy already decided this is a defect), and #663 (the shared self-heal-RESET event
# line, src/main.rs's SelfHealDecision::Heal arm, the SAME line self_heal_reset_grep_pattern in
# scripts/lib/self-heal-attribution.sh keys on -- a reset having fired at all is itself proof a
# defect recurred and the window may be invalid, e.g. device renumbering).
#
# Deliberately EXCLUDES #717 SUSTAINED (see capture_rate_sustained_band_grep_pattern below): the
# rig-validated issue 889 failure mode (ShadowCast sustained ~62-64fps, INSIDE the widened #685
# jitter tolerance) trips ONLY the #717 band, which is informational by design (issue 909: the
# genlock decimation gate absorbs the over-rate into exact NDI output before it ever reaches a
# real defect) -- hard-failing this gate on that band recreates the issue-909 mistake one layer
# up, since the over-rate is CHRONIC on cam1's ShadowCast 2 (redevelops ~2min after any fresh
# device open, issue 889) and would permanently red this gate before any verdict is computed.
#
# Deliberately NOT reusing/replacing capture_rate_defect_grep_pattern (above, the narrow #656-only
# pattern) -- the [0/8] PRE-run preflight is unrelated to this mid-recording ticket and stays
# untouched.
capture_rate_defect_grep_pattern_hard() {
  echo '#656 capture-delivery-rate DEFECTIVE|#971 capture-delivery-rate CHRONIC sustained-band DEFECTIVE|#663 self-heal: USB reset attempt'
}

# (#992 ROZHODNUTÉ) capture_rate_sustained_band_grep_pattern -> the #717 SUSTAINED-band signal,
# ALONE. Matching this band is measured and reported (a run's log must still carry the over-rate
# evidence -- issue 889's own close conditions key on it) but must NEVER abort the run: see
# capture_rate_defect_grep_pattern_hard's doc comment above for why, and
# capture_rate_sustained_band_warn_message / capture_rate_burn_log_sustained_band_warn_message
# below for the report-only WARN text.
capture_rate_sustained_band_grep_pattern() {
  echo '#717 capture-delivery-rate SUSTAINED band confirmed'
}

# (#992) capture_rate_burn_log_grep_cmd LOG_PATH PATTERN -> the REMOTE command text that greps a
# capture burn instance's OWN log FILE (e.g. /tmp/cbox-burn.log) for the caller-supplied PATTERN,
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
# (#992 ROZHODNUTÉ) PATTERN is now an explicit argument (was hardcoded to the old union pattern,
# since removed) -- the caller greps this SAME log path twice, once with
# capture_rate_defect_grep_pattern_hard (exit 1 on match) and once with
# capture_rate_sustained_band_grep_pattern (WARN only), never sharing one `tail -1` so a reset
# line can never be masked by a sustained line.
#
# Pure string builder (no ssh, no I/O) -- directly unit-testable without a live rig, same
# convention as every other builder in this file.
capture_rate_burn_log_grep_cmd() {
  local log_path="$1" pattern="$2"
  printf 'grep -E '\''%s'\'' "%s" 2>/dev/null | tail -1' "$pattern" "$log_path"
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

# (#992 ROZHODNUTÉ) capture_rate_sustained_band_warn_message CAMERA_NAME MATCHED_LINE -> the
# REPORT-ONLY diagnostic for a #717 SUSTAINED-band match found in the journald window. Unlike
# capture_rate_recurrence_message (the HARD-fail sibling), this NEVER precedes an exit 1 -- the
# band is informational by design (issue 909: the genlock decimation gate absorbs the over-rate
# into exact NDI output before it ever becomes a real defect). Still prints the matched line
# verbatim so every run's log carries the over-rate evidence (issue 889's own close conditions
# key on it), and points at issue 909 so a reader lands on the rationale, not just the fact of
# the match. "WARNING #992:" prefix makes this loud and greppable without being mistaken for one
# of the ERROR/exit-1 lines above.
capture_rate_sustained_band_warn_message() {
  local cam="$1" line="$2"
  echo "WARNING #992: ${cam} capture-delivery-rate SUSTAINED band confirmed in its journal during this recording -- informational by design (issue 909: absorbed by the genlock decimation gate), does NOT fail this gate: ${line}"
}

# (#992 ROZHODNUTÉ) capture_rate_burn_log_sustained_band_warn_message CAMERA_NAME MATCHED_LINE ->
# the burn-instance-log sibling of capture_rate_sustained_band_warn_message above -- same
# report-only contract, but names the burn-instance LOG as the source (never "journal"), mirroring
# the same journal/burn-log discriminator capture_rate_recurrence_message /
# capture_rate_burn_log_recurrence_message already keep distinct for the HARD-fail messages.
capture_rate_burn_log_sustained_band_warn_message() {
  local cam="$1" line="$2"
  echo "WARNING #992: ${cam} capture-delivery-rate SUSTAINED band confirmed in its burn-instance log during this recording -- informational by design (issue 909: absorbed by the genlock decimation gate), does NOT fail this gate: ${line}"
}

# (#994) capture_rate_secondary_recurrence_warn_message CAMERA_NAME MATCHED_LINE -> the REPORT-ONLY
# diagnostic for a HARD capture-rate defect band (#656 jitter / #971 chronic / #663 self-heal
# reset) that matched on a SECONDARY camera's journald window during an ALL_CAMBOX recording.
# Unlike capture_rate_recurrence_message (the SOURCE-camera HARD-fail sibling, which precedes an
# abort), this capture-RATE sweep NEVER aborts the run: hard-failing THIS rate band on a chronic
# secondary quirk (cam2 IS a secondary), which the genlock decimation gate absorbs into exact NDI
# output, would recreate the issue-909 permanently-red-gate mistake -- so a secondary capture-rate
# defect is surfaced loudly for diagnostics, never gated. (A secondary's RESET events are a
# distinct signal that DOES gate via self_heal_reset since issue 905; issue 914 had decoupled it
# while cam1's grabber was unresolved.) "WARNING #994:" prefix makes it greppable without being mistaken for an
# ERROR line. Reuses the same captured/configured-fps extraction as the source-camera messages.
capture_rate_secondary_recurrence_warn_message() {
  local cam="$1" line="$2" captured configured
  captured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps captured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  configured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps configured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  if [ -n "$captured" ] && [ -n "$configured" ]; then
    echo "WARNING #994: ${cam} (secondary camera) capture-rate HARD defect during this recording, per its journal (~${captured}fps, expected ${configured}fps) -- see #656/#663/#971/#994; report-only for a SECONDARY camera (the source camera above is the hard gate; this capture-RATE band stays report-only so a chronic rate wobble the decimation gate absorbs never aborts every run -- a secondary's RESET events, by contrast, now gate via self_heal_reset since issue 905), does NOT fail this gate"
  else
    echo "WARNING #994: ${cam} (secondary camera) capture-rate HARD defect during this recording, per its journal (see #656/#663/#971/#994); report-only for a SECONDARY camera, does NOT fail this gate: ${line}"
  fi
}

# (#994) capture_rate_secondary_burn_log_recurrence_warn_message CAMERA_NAME MATCHED_LINE -> the
# burn-instance-log sibling of capture_rate_secondary_recurrence_warn_message above -- same
# report-only contract, but names the burn-instance LOG as the source (never "journal"), mirroring
# the journal/burn-log discriminator capture_rate_recurrence_message /
# capture_rate_burn_log_recurrence_message already keep distinct. During an ALL_CAMBOX burn each
# secondary logs to its OWN /tmp/cbox-burn-<camname>.log (journald is blind to it -- the burn runs
# as a transient systemd-run unit redirecting straight to that file), so this is the journald-blind
# sibling read, same as the source camera's #992 burn-log read.
capture_rate_secondary_burn_log_recurrence_warn_message() {
  local cam="$1" line="$2" captured configured
  captured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps captured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  configured="$(printf '%s' "$line" | grep -oE '[0-9]+\.[0-9]+ fps configured' | head -1 | grep -oE '[0-9]+\.[0-9]+')"
  if [ -n "$captured" ] && [ -n "$configured" ]; then
    echo "WARNING #994: ${cam} (secondary camera) capture-rate HARD defect during this recording, per its OWN burn-instance log (~${captured}fps, expected ${configured}fps) -- see #656/#663/#971/#992/#994; journald was blind to this (the burn instance logs to a file, not the journal); report-only for a SECONDARY camera, does NOT fail this gate"
  else
    echo "WARNING #994: ${cam} (secondary camera) capture-rate HARD defect during this recording, per its OWN burn-instance log (see #656/#663/#971/#992/#994); report-only for a SECONDARY camera, does NOT fail this gate: ${line}"
  fi
}
