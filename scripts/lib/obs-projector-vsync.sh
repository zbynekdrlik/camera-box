#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (no side effects at source time) — mirrors
# scripts/lib/imag-display-path.sh / scripts/lib/imag-cmdline-isolation.sh; a sourced lib must NOT
# impose `set -euo pipefail` on its caller.
# scripts/lib/obs-projector-vsync.sh — shared reader of the issue-1146 OBS-log observability marker
# `projector-vsync: present-vsync ARMED` (#1151).
#
# Root cause (#1151, follow-up to #1146): #1146 added a one-shot libobs marker
# `projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)`
# (vendor/obs-studio/libobs/obs-display.c, emitted from obs_display_set_vsync() ONLY when the
# fullscreen Program projector arms the issue-1107 EGL present-vsync tear-free present) but added NO
# consumer — a grep anchor is only trustworthy once the marker is deployed and its exact shape is
# confirmed live on imag (both true, STEP-0 2026-08-20). This lib is that consumer core, sourced by
# BOTH scripts/drift-guard.sh (--check-imag facet family) and scripts/recording-e2e.sh (the E2E
# [0/8] preflight) — the SAME split-lib pattern imag-display-path.sh / imag-cmdline-isolation.sh use,
# so the marker string lives in exactly ONE place.
#
# REPORT-ONLY (issue 781): the marker proves the tear-free present MECHANISM is ENGAGED on the
# Program projector, NEVER that scanout tearing is gone — objective scanout-tear proof needs the
# physical HDMI tap (issue 781, ops-wait hardware). Neither consumer GATES on this facet.
#
# #833 fail-closed: an unread / empty OBS log surfaces UNKNOWN, never a false OK. A read log with no
# ARMED marker is ALSO UNKNOWN — the Program projector was not (re)opened since OBS start (the marker
# is one-shot-on-change at projector open) or the build predates issue 1146 — never a DRIFT: a
# missing marker is a healthy ordering-dependent state, not a config drift.

# projector_vsync_armed_from_log TEXT -> "1" if the OBS log text carries the ARMED marker, "" (absent)
# otherwise. Mirrors drift-guard's genlock_capability_from_log (#119): a build-unique OBS-log
# capability read, drain-safe (grep|head, never grep -q — which flips a genuine match into SIGPIPE
# under the caller's set -euo pipefail). Matches ARMED specifically, never the `cleared` variant.
projector_vsync_armed_from_log() {
  local text="$1" line
  line="$(printf '%s\n' "$text" \
    | grep -iE 'projector-vsync: present-vsync ARMED' \
    | head -1 || true)"
  # Echo "1" when the ARMED marker is present; otherwise echo NOTHING (the absent signal). `return 0`
  # so the absent case is a clean exit (empty output, not a non-zero status), matching the sibling
  # genlock_capability_from_log.
  [ -n "$line" ] && echo 1
  return 0
}

# projector_vsync_verdict OBS_LOG_TEXT -> one `projector_vsync|<STATUS>|<detail>` line (STATUS is
# OK or UNKNOWN — REPORT-ONLY, never DRIFT). Both callers map this single line to their own report
# style. Three cases:
#   empty text          -> UNKNOWN (OBS log not read — SSH failed or OBS not launched; #833)
#   text + ARMED present -> OK      (Program projector present-vsync armed)
#   text + no ARMED      -> UNKNOWN (projector not (re)opened since OBS start, or a build predating
#                                    issue 1146)
projector_vsync_verdict() {
  local text="$1"
  if [ -z "$text" ]; then
    printf 'projector_vsync|UNKNOWN|OBS log not read (SSH failed or OBS not launched) — report-only, never gates\n'
  elif [ "$(projector_vsync_armed_from_log "$text")" = "1" ]; then
    printf 'projector_vsync|OK|Program projector present-vsync armed (issue-1107 EGL vsync; issue-1146 marker)\n'
  else
    printf 'projector_vsync|UNKNOWN|no present-vsync ARMED marker in the OBS log — Program projector not (re)opened since OBS start, or a build predating issue 1146; report-only, never gates\n'
  fi
}

# projector_vsync_report_line OBS_LOG_TEXT -> a single human "<STATUS>  (<detail>)" line built on the
# verdict (single judgment site). Used by the E2E [0/8] preflight for its one-liner. The verdict
# details carry no `|`, so the last-field split recovers the detail cleanly.
projector_vsync_report_line() {
  local v status detail
  v="$(projector_vsync_verdict "$1")"
  status="${v#projector_vsync|}"; status="${status%%|*}"
  detail="${v##*|}"
  printf '%s  (%s)\n' "$status" "$detail"
}

# projector_vsync_gather_remote_snippet -> the REMOTE shell command (a string) a caller runs over its
# own transport to cat the MOST-RECENT OBS log, whose text projector_vsync_verdict parses. OBS names
# its logs `YYYY-MM-DD HH-MM-SS.txt` (NOT .log) — the SAME `*.txt` glob every other imag OBS-log
# reader uses (verify-imag.sh, imag_scenes.py, imag-jitter-monitor.sh, rig-health-audit.py,
# mv-fps-*). Empty output = no readable log (the verdict then reads UNKNOWN, #833). Used by
# recording-e2e.sh's [0/8] preflight via $(fn) embedding (the issue-675 pattern).
projector_vsync_gather_remote_snippet() {
  printf '%s' 'f=$(ls -t "$HOME/.config/obs-studio/logs/"*.txt 2>/dev/null | head -1); [ -n "$f" ] && cat "$f" || true'
}
