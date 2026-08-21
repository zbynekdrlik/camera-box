#!/usr/bin/env bash
#
# clock-offset-guard.sh — cluster clock-offset regression guard (#8).
#
# The software genlock in src/ndi.rs aligns every camera's NDI send timecode to ABSOLUTE
# wall-clock frame boundaries (wait_for_next_boundary_100ns), which only yields a COMMON boundary
# across nodes if their wall clocks are synchronized (src/ndi.rs:62-65 states this verbatim). The
# cluster is disciplined by DanteSync (strih = master; NTP anchor + PTP fine servo — see SETUP.md
# "Cluster clock synchronization"). If a node's clock silently drifts past a fraction of the
# 16.7 ms (60 fps) frame period, multi-camera genlock degrades with NO error and cross-node
# latency becomes meaningless. This guard is the regression check #8 requires: it queries each
# REACHABLE node's DanteSync-reported absolute clock offset and FAILS LOUDLY (exit non-zero) if
# any node exceeds the documented bound, so drift cannot silently re-break genlock.
#
# Architecture mirrors scripts/drift-guard.sh: PURE functions (parse the offset from the two real
# DanteSync status formats, compare |offset| against the bound — unit-tested from
# tests/clock_offset_guard.rs by sourcing this file) and a flow that runs only when executed
# directly. The source-guard below (BASH_SOURCE != $0) lets the tests exercise the pure functions
# in isolation. Per the project Script Failure Policy + test-strictness: an UNREACHABLE node or an
# UNREADABLE offset is an explicit failure (DRIFT-status incomplete), NEVER a silent pass.
#
# The Linux cameras log DanteSync to journald:
#   Jun 15 09:11:53 CAM2 dantesync[3649]: [NTP] offset:+300us (threshold:520us, adaptive)
# The Windows OBS boxes (strih/stream) expose the same signal as JSON on the \\.\pipe\dantesync
# status pipe ("ntp_offset_us":1249). This script gathers the LINUX nodes over SSH (the part that
# runs unattended from dev1); the Windows offsets are gathered read-only via the win-* MCP tools
# (the offset_us_from_pipe_json parser here is the shared, unit-tested comparator for that path).
#
# Usage:
#   scripts/clock-offset-guard.sh [--bound-us N] [--stability-us N] [--targets "cam1=10.77.9.61 ..."]
#   scripts/clock-offset-guard.sh --help
#
# Exit codes: 0 = all reachable nodes within bound + stable, 20 = DRIFT or UNSTABLE (a node
# exceeds the bound or its samples scatter past --stability-us),
# 11 = at least one node UNREACHABLE / offset UNKNOWN (status incomplete — NOT clean),
# 1 = usage/IO error (e.g. no targets configured).

set -euo pipefail

# --- the documented bound -------------------------------------------------------------------
#
# DEFAULT_BOUND_US — the maximum tolerated ABSOLUTE clock offset, in microseconds.
#
# Rationale (tied to the measured baseline, per #8's acceptance criteria):
#   * The 60 fps frame period is 16.7 ms = 16667 µs. A clock offset of that magnitude would put a
#     camera one whole frame off the common genlock boundary; offsets of "tens-to-hundreds of ms"
#     (an unsynced clock — the exact failure mode #8 names) are 1-2 orders of magnitude past it.
#   * Observed steady-state offsets on the live DanteSync cluster (2026-06-15, read-only):
#       cam2  +281..+380 µs,  stream  +302 µs,  strih (master→GM)  +1249 µs.
#     The DanteSync daemon's own adaptive spike threshold sits ~520 µs on the cameras.
#   * 2000 µs (2 ms) is ~8x under the frame period (so genlock boundary divergence stays well
#     within a frame) yet comfortably above strih's legitimate ~1249 µs master-to-grandmaster
#     offset, so the guard does NOT false-positive on the healthy cluster while still catching the
#     tens-of-ms unsynced failure mode. Documented in SETUP.md alongside the baseline.
DEFAULT_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"

# #837: max tolerated SPREAD (max-min) of the FRESH journal offset samples, in us. The
# journal-fallback twin of dantesync-gate.sh's GATE_STABILITY_US -- same 2000us default and
# same DANTESYNC_STABILITY_US env knob, so one var tunes the stability bound across every
# caller of dantesync_offset_verdict. A scattered-but-in-bound-median node grades UNSTABLE.
DEFAULT_STABILITY_US="${DANTESYNC_STABILITY_US:-2000}"

# CLOCK_GUARD_TARGETS — space-separated "name=ip" pairs of the LINUX nodes to query over SSH.
# Defaults to the four cameras (targets.md / CLAUDE.md IP table). strih + stream are Windows and
# are checked via the win-* MCP path (this guard's pipe-JSON parser), not over SSH.
CLOCK_GUARD_TARGETS="${CLOCK_GUARD_TARGETS-cam1=10.77.9.61 cam2=10.77.9.62 cam3=10.77.9.63 cam4=10.77.9.64}"

# SSH params for the read-only camera query (root/newlevel per CLAUDE.md / targets.md).
CLOCK_GUARD_SSH_USER="${CLOCK_GUARD_SSH_USER:-root}"
CLOCK_GUARD_SSH_PASS="${CLOCK_GUARD_SSH_PASS:-newlevel}"
CLOCK_GUARD_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"

# #550/#591/#595/#607: a node's freshest "[NTP] offset:" journal line must be no older than this
# many seconds behind its own newest journal line, or the reading is STALE and must never be
# graded as the current offset -- see dantesync_offset_verdict() below. Mirrors the SAME knob
# (DANTESYNC_OFFSET_FRESHNESS_S) dantesync-gate.sh (#7) and clock-offset-painter-gate.sh (#326)
# already read, so one env var tunes the freshness window across all three callers of this file.
CLOCK_GUARD_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"

# --- PURE functions (no network, no MCP — unit-tested) -------------------------------------

# offset_us_from_journal TEXT -> the SIGNED microsecond offset from the LAST DanteSync
# "[NTP] offset:+Nus" journald line ("" if none). The many "[PTP] ... Drift: Nns/s" lines are
# the fine servo's drift RATE, NOT the absolute offset, so they must be ignored. `tail -1` picks
# the most recent sample. A leading '+' is dropped (a negative '-' is kept) so the value is a
# plain signed integer offset_check can compare numerically. `|| true` keeps a no-match from
# tripping the caller's set -e.
offset_us_from_journal() {
  printf '%s\n' "$1" \
    | grep -oE '\[NTP\] offset:[+-]?[0-9]+us' \
    | sed -n 's/.*offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us/\1/p' \
    | tail -1 || true
}

# offset_us_from_pipe_json TEXT -> the SIGNED integer value of the JSON "ntp_offset_us" field
# ("" if absent). This is the Windows DanteSync status-pipe signal. It deliberately reads
# ntp_offset_us (the absolute NTP offset the master/boxes report), NOT offset_ns or
# accumulated_phase_us. The real status blob carries exactly one such field; if it ever carries
# more than one, the WORST (largest |offset|) is returned, never the first — a guard whose
# contract is "never silently pass an out-of-bound offset" must not let a later drifted value be
# masked by an earlier in-bound one. `|| true` survives a no-match under set -e.
offset_us_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_offset_us"[[:space:]]*:[[:space:]]*-?[0-9]+' \
    | sed -n 's/.*:[[:space:]]*\(-\{0,1\}[0-9][0-9]*\).*/\1/p' \
    | awk 'NR==1 || ($1<0?-$1:$1) > (m<0?-m:m) { m=$1 } END { if (NR) print m }' \
    || true
}

# abs_int N -> |N| (strips a leading '+' or '-'). Empty stays empty.
abs_int() {
  local n="${1#+}"
  printf '%s' "${n#-}"
}

# --- Windows HTTP status freshness (#648) ---------------------------------------------------
#
# dantesync#47 gave every managed box a network status endpoint (http://<box>:8898/status)
# serving the SAME JSON the status pipe already emits — including "updated_ts" (unix epoch
# seconds of the daemon's last self-report). Before #648 this field went unused here: the
# status-pipe JSON path was documented (see dantesync-gate.sh's own #595-scope note) as
# AGE-BLIND because comparing it to "now" needs a real wall-clock reference, unlike the Linux
# journal path's freshness check (which compares two lines from the SAME journal, no wall clock
# needed). #648's --win-http gate flow supplies that reference explicitly (via `date +%s` at the
# call site), keeping these two functions pure and deterministically testable.

# updated_ts_from_pipe_json TEXT -> the unsigned integer value of the JSON "updated_ts" field
# (unix epoch seconds), "" if absent/malformed. `|| true` survives a no-match under set -e.
updated_ts_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"updated_ts"[[:space:]]*:[[:space:]]*[0-9]+' \
    | sed -n 's/.*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    | tail -1 || true
}

# pipe_json_freshness_verdict TEXT NOW_EPOCH FRESHNESS_S -> "fresh" | "stale" | "absent".
#   absent -- no "updated_ts" field in TEXT, OR NOW_EPOCH/FRESHNESS_S is not a plain non-negative
#             integer. A malformed input must never be graded as fresh (test-strictness: no
#             silent pass on a value we cannot prove) -- the caller maps "absent" to UNKNOWN,
#             same as a genuinely missing field.
#   stale  -- |NOW_EPOCH - updated_ts| exceeds FRESHNESS_S (the box's HTTP server is serving a
#             cached/stuck snapshot -- e.g. the dantesync daemon died but the server kept running
#             -- OR, oddly, a clock in the future; either way not trustworthy as "current").
#   fresh  -- updated_ts is within FRESHNESS_S of NOW_EPOCH.
pipe_json_freshness_verdict() {
  local text="$1" now="$2" fresh="$3" ts delta
  if ! grep -qE '^[0-9]+$' <<<"$now" || ! grep -qE '^[0-9]+$' <<<"$fresh"; then
    printf 'absent\n'
    return 0
  fi
  ts="$(updated_ts_from_pipe_json "$text")"
  if [ -z "$ts" ]; then
    printf 'absent\n'
    return 0
  fi
  delta=$(( now - ts ))
  delta="$(abs_int "$delta")"
  if [ "$delta" -le "$fresh" ]; then
    printf 'fresh\n'
  else
    printf 'stale\n'
  fi
}

# --- NTP-measurement freshness (#1014, dantesync v1.8.30 / dantesync issue 68 and issue 71) ---
#
# updated_ts (above) is driven by the PTP loop and stays fresh even when the NTP subsystem itself
# is dead, OR -- dantesync issue 68 -- intentionally free-running after a one-time sync at
# startup (ntp_server_mode disables periodic upstream queries "by design" on the NTP master, so
# its ntp_offset_us free-runs at the box's own oscillator error for as long as the service stays
# up). #1014 found this live: strih's ntp_offset_us frozen at a ~30x-stale value while
# updated_ts kept advancing every ~30s from the healthy PTP servo, and the gate graded the frozen
# value as a live DRIFT. dantesync v1.8.30 added two fields specifically so the NTP MEASUREMENT's
# own freshness can be proven independently of updated_ts:
#   ntp_updated_ts -- unix epoch of the last successful NTP measurement (0 = never).
#   ntp_age_s      -- seconds since that measurement (a plain integer), OR JSON null when the
#                      daemon has NEVER completed one (deliberately NOT 0 -- 0 would mean "just
#                      measured").
# Grading via ntp_age_s directly (rather than epoch-diffing ntp_updated_ts against the gate's own
# "now") avoids any dependency on clock skew between the gate host and the target box -- the box
# has already computed its own age.

# ntp_age_s_raw_from_pipe_json TEXT -> the RAW text of the "ntp_age_s" JSON value: a plain
# non-negative integer string, the literal "null", or "" if the field is absent entirely (an
# older, pre-1.8.30 payload). Kept as a raw/unparsed accessor (rather than folding null into "")
# so callers can distinguish all THREE states -- collapsing null and absent into the same value
# would lose the "never measured" signal ntp_freshness_verdict (below) depends on.
ntp_age_s_raw_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_age_s"[[:space:]]*:[[:space:]]*(null|[0-9]+)' \
    | sed -n 's/.*:[[:space:]]*\(null\|[0-9][0-9]*\).*/\1/p' \
    | tail -1 || true
}

# ntp_updated_ts_from_pipe_json TEXT -> the unsigned integer value of "ntp_updated_ts" (unix
# epoch of the daemon's last successful NTP measurement; 0 = never), "" if absent. Not itself
# used for grading (ntp_age_s already gives an age with no clock-skew dependency) -- exposed for
# parity with updated_ts_from_pipe_json and for any future caller that wants the raw timestamp.
ntp_updated_ts_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_updated_ts"[[:space:]]*:[[:space:]]*[0-9]+' \
    | sed -n 's/.*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    | tail -1 || true
}

# ntp_failed_from_pipe_json TEXT -> "true" | "false" | "" (absent/unparseable). #1014: v1.8.30
# widened this field's meaning to ALSO cover "no fresh measurement within window", not only an
# outright measurement error -- ntp_freshness_verdict treats true as an INDEPENDENT stale signal,
# not merely OR'd into the age check, so a payload reporting one without the other still refuses.
ntp_failed_from_pipe_json() {
  printf '%s\n' "$1" | grep -oE '"ntp_failed"[[:space:]]*:[[:space:]]*(true|false)' \
    | sed -n 's/.*:[[:space:]]*\(true\|false\).*/\1/p' | tail -1 || true
}

# ntp_freshness_verdict TEXT FRESHNESS_S -> "fresh" | "stale" | "never" | "absent".
#   absent -- TEXT has no "ntp_age_s" field at all -- a pre-1.8.30 payload. The caller must fall
#             back to the pre-#1014 heuristic (dantesync-gate.sh's frozen_sample_verdict, below);
#             this function itself never guesses at a payload shape it cannot see.
#   never  -- "ntp_age_s":null -- the daemon has NEVER completed an NTP measurement.
#   stale  -- ntp_age_s is a valid integer > FRESHNESS_S, OR ntp_failed is true (dantesync issue
#             68's widened meaning) -- either signal alone is sufficient, checked independently.
#   fresh  -- ntp_age_s is a valid integer <= FRESHNESS_S AND ntp_failed is not true.
# A malformed FRESHNESS_S is treated like "absent" -- never grade a value we cannot bound.
ntp_freshness_verdict() {
  local text="$1" fresh="$2" age failed
  age="$(ntp_age_s_raw_from_pipe_json "$text")"
  if [ -z "$age" ]; then
    printf 'absent\n'
    return 0
  fi
  if [ "$age" = "null" ]; then
    printf 'never\n'
    return 0
  fi
  if ! grep -qE '^[0-9]+$' <<<"$fresh"; then
    printf 'absent\n'
    return 0
  fi
  failed="$(ntp_failed_from_pipe_json "$text")"
  if [ "$failed" = "true" ]; then
    printf 'stale\n'
    return 0
  fi
  if [ "$age" -gt "$fresh" ]; then
    printf 'stale\n'
  else
    printf 'fresh\n'
  fi
}

# --- FRESHNESS-aware offset reading (#550/#591/#595) ----------------------------------------
#
# offset_us_from_journal (above) is AGE-BLIND: it `tail -1`s the LAST "[NTP] offset:" line
# regardless of how old it is. `journalctl -n N` is COUNT-bounded, not TIME-bounded, so a
# died/hung dantesync (the journal simply stops advancing) or a long gap between the
# adaptive-cadence offset samples can leave only a stale multi-hour-old boot-STEP line in the
# window -- grading THAT value as "current" is the exact #550 bug (a false-fail on a healthy box,
# or a false-pass masking a real desync). These two helpers were originally added to
# scripts/verify-device.sh for #591/#600 and are MOVED here for #595 so every caller (this file's
# own callers: dantesync-gate.sh's #7 precondition, clock-offset-painter-gate.sh's #326 sweep
# comparator, AND verify-device.sh's own #591 fleet-acceptance check) shares ONE implementation
# instead of three copies drifting apart. Both compare the offset line's OWN `-o short-iso`
# timestamp against the newest journal line's timestamp -- both timestamps come from the SAME
# box, so neither the verifier host's nor another box's clock ever enters the comparison.

# _short_iso_epoch ISO -> epoch seconds for a `journalctl -o short-iso` timestamp
# (e.g. 2026-07-07T18:36:44+02:00), "" if unparseable/empty. Uses `date -d` (deterministic given
# the input -- no network/state; the explicit numeric TZ offset makes the result independent of the
# host's local timezone). The `T` is normalised to a space for maximal date(1) portability.
_short_iso_epoch() {
  local iso="$1" e
  [ -n "$iso" ] || { printf ''; return 0; }
  e="$(date -d "${iso/T/ }" +%s 2>/dev/null)" || true
  printf '%s' "${e:-}"
}

# _freshest_ntp_offset_line JOURNAL -> the LAST "[NTP] offset:" line in JOURNAL, "" if none.
# Private helper shared by freshest_offset_us and dantesync_offset_verdict so "which line IS the
# offset reading" is defined in exactly ONE place -- otherwise the two would each carry their own
# copy of this grep+tail, and a future edit to the offset-line pattern could update one copy and
# silently miss the other (the "three copies drifting apart" failure #595 exists to eliminate).
_freshest_ntp_offset_line() {
  printf '%s\n' "$1" | grep -E '\[NTP\] offset:' | tail -1 || true
}

# freshest_offset_us JOURNAL FRESHNESS_S -> the SIGNED microsecond VALUE of the freshest
# "[NTP] offset:" line in JOURNAL, or "" if that line is ABSENT, STALE (older than FRESHNESS_S
# behind the newest journal line), or malformed. This is the value-returning sibling of
# dantesync_offset_verdict (below), for a caller that must compare TWO boxes' fresh offsets
# against EACH OTHER (the #326 painter-gate RELATIVE comparator, #595) rather than grade one
# offset against a single absolute bound. JOURNAL must be gathered with `-o short-iso`.
#
# FRESHNESS_S itself is validated here (unlike BOUND_US, which dantesync-gate.sh and
# clock-offset-painter-gate.sh both validate via their own --bound-us/--guard-us CLI parsing before
# calling in -- verify-device.sh's own DEVICE_CLOCK_BOUND_US is a separately-unvalidated env var too,
# a pre-existing, unrelated gap): FRESHNESS_S is caller-configurable ONLY via an unchecked env var
# (DANTESYNC_OFFSET_FRESHNESS_S / GATE_OFFSET_FRESHNESS_S / PAINTER_GATE_FRESHNESS_S), never a
# validated flag. A malformed value (e.g. a typo'd env var) would otherwise make the `-gt "$fresh"`
# arithmetic comparison below throw a bash "integer expression expected" error, which evaluates as a
# FAILED test -- silently defeating the staleness OR-chain and making every reading look "fresh"
# regardless of its true age (a real desync would then silently pass). So a non-numeric FRESHNESS_S
# is treated exactly like a stale reading: refuse to certify it.
freshest_offset_us() {
  local journal="$1" fresh="$2"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local off_line off_iso off_us now_iso now_e off_e
  if ! grep -qE '^[0-9]+$' <<<"$fresh"; then
    printf ''
    return 0
  fi
  off_line="$(_freshest_ntp_offset_line "$journal")"
  [ -n "$off_line" ] || { printf ''; return 0; }
  off_iso="$(printf '%s' "$off_line" | grep -oE "^$iso_re" | head -1 || true)"
  now_iso="$(printf '%s\n' "$journal" | grep -oE "^$iso_re" | tail -1 || true)"
  now_e="$(_short_iso_epoch "$now_iso")"
  off_e="$(_short_iso_epoch "$off_iso")"
  if [ -z "$now_e" ] || [ -z "$off_e" ] || [ "$((now_e - off_e))" -gt "$fresh" ]; then
    printf ''
    return 0
  fi
  off_us="$(printf '%s' "$off_line" | sed -n 's/.*\[NTP\] offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us.*/\1/p' | head -1 || true)"
  if [ -z "$off_us" ] || ! grep -qE '^-?[0-9]+$' <<<"$off_us"; then
    printf ''
    return 0
  fi
  printf '%s' "$off_us"
}

# _fresh_offset_samples_us JOURNAL FRESHNESS_S [K] -> the signed us VALUES of the individually-FRESH
# "[NTP] offset:" samples among the K (default 5) most recent such lines, one per line; NOTHING when
# none is fresh and parseable. This is the single source that BOTH the median (_fresh_offset_median_us)
# and the spread (_fresh_offset_spread_us, #837) grade, so the "[NTP] offset:" parse + per-sample
# freshness lives in ONE place (the #595 "three copies drift apart" discipline -- the same reason
# _freshest_ntp_offset_line exists). Freshness is checked PER SAMPLE against the newest journal line;
# a non-numeric FRESHNESS_S yields NOTHING (fail-closed, the #595 numeric-guard gotcha: an unvalidated
# value in the `-le` arithmetic below would throw "integer expression expected" and could silently
# defeat the staleness check). JOURNAL must be gathered with `-o short-iso`.
_fresh_offset_samples_us() {
  local journal="$1" fresh="$2" k="${3:-5}"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local now_iso now_e lines line off_iso off_e off_us
  grep -qE '^[0-9]+$' <<<"$fresh" || return 0
  now_iso="$(printf '%s\n' "$journal" | grep -oE "^$iso_re" | tail -1 || true)"
  now_e="$(_short_iso_epoch "$now_iso")"
  [ -n "$now_e" ] || return 0
  lines="$(printf '%s\n' "$journal" | grep -E '\[NTP\] offset:' | tail -n "$k" || true)"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    off_iso="$(printf '%s' "$line" | grep -oE "^$iso_re" | head -1 || true)"
    off_e="$(_short_iso_epoch "$off_iso")"
    [ -n "$off_e" ] || continue
    [ "$((now_e - off_e))" -le "$fresh" ] || continue
    off_us="$(printf '%s' "$line" | sed -n 's/.*\[NTP\] offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us.*/\1/p' | head -1 || true)"
    grep -qE '^-?[0-9]+$' <<<"$off_us" || continue
    printf '%s\n' "$off_us"
  done <<< "$lines"
  # A conditionally-skipped `printf` as the loop's last statement can leave a non-zero exit under
  # set -e when the final iteration was `continue`d -- mirror slew_excluded_survivors_us's own
  # explicit terminator so command-substitution callers never abort on it.
  return 0
}

# _fresh_offset_median_us JOURNAL FRESHNESS_S [K] -> the lower MEDIAN of the individually-FRESH
# samples among the K (default 5) most recent "[NTP] offset:" lines; "" when none is fresh and
# parseable. #767-era measurement-noise rejection (live, 2026-07-15): under E2E network load a
# SINGLE [NTP] offset sample spikes to ~2-3ms (cam5 -2787us, cam7 -2316us) while PTP stays
# NANO-locked and the surrounding samples read tens of us -- the CLOCK is fine, the one
# measurement is noisy, and a verdict graded on the single freshest sample flakes exactly when
# an E2E run loads the LAN. The median across the recent fresh samples rejects a lone spike
# while a SUSTAINED out-of-bound offset (the real cam5/6 5.28s class) still lands every sample
# out of bound -> median out of bound -> drift. Now a thin wrapper over _fresh_offset_samples_us
# (which owns the parse + freshness) + median_of_ints (the #836 estimator the HTTP path shares) --
# same result as before, one parse. (freshest_offset_us is left single-sample: its #326
# painter-gate caller compares two boxes' raw values relatively and owns its own tolerance.)
_fresh_offset_median_us() {
  median_of_ints "$(_fresh_offset_samples_us "$1" "$2" "${3:-5}")"
}

# _fresh_offset_spread_us JOURNAL FRESHNESS_S [K] -> the SPREAD (max-min) of the SAME individually-
# FRESH sample set _fresh_offset_median_us grades, via the #836 spread_of_ints; "" for fewer than 2
# fresh samples (scatter is undefined from a single point). The journal-path sibling of
# sampled_offset_report's spread column (#837).
_fresh_offset_spread_us() {
  spread_of_ints "$(_fresh_offset_samples_us "$1" "$2" "${3:-5}")"
}

# dantesync_offset_verdict JOURNAL FRESHNESS_S BOUND_US [STABILITY_US] ->
#   "ok" | "drift" | "unstable" | "drift_unstable" | "stale" | "absent".
# Supersedes the age-blind dantesync_offset_ok (which read the LAST "[NTP] offset:" line via tail -1
# regardless of age -- on cam5/6 that graded on a STALE boot-STEP line, the #550 bug). It finds the
# FRESHEST "[NTP] offset:" line (via freshest_offset_us, above) and only then grades |offset|
# against BOUND_US.
#   absent -- no "[NTP] offset:" line at all in JOURNAL.
#   stale  -- the freshest "[NTP] offset:" line is older than FRESHNESS_S behind the newest journal
#             line, OR either timestamp is unparseable, OR the offset value is malformed (we cannot
#             prove a FRESH in-bound reading -- never a silent pass on a possibly-stale value).
#   drift  -- a FRESH offset line whose |offset| exceeds BOUND_US (a real desync -- the cam5/6 5.28s
#             case; a bare "[NTP] offset:-5280959us" fallback line lands here by magnitude).
#   ok     -- a FRESH in-bound median AND (STABILITY_US omitted, fewer than 2 fresh samples, or
#             spread in-bound).
# STABILITY_US (#837, the journal-fallback twin of #836's HTTP spread check): OPTIONAL 4th arg.
# OMITTED/empty keeps the pre-#837 median-only contract byte-for-byte (every 3-arg caller
# unchanged). Present -> the SPREAD (max-min, spread_of_ints) of the SAME K=11 fresh sample set the
# median grades is checked against STABILITY_US, adding two verdicts (same words the HTTP path's
# sampled_offset_verdict uses):
#   unstable       -- median in-bound but spread exceeds STABILITY_US (>=2 samples) -- scattered/
#                     unusable, a NEW failure the median-only path could never detect.
#   drift_unstable -- both the median AND the spread fail.
# A NON-numeric STABILITY_US fails closed to unstable (the #595 numeric-guard gotcha: an unvalidated
# value in `-gt` would throw and silently defeat the check -- a broken knob must fail loud). The
# location bound (BOUND_US) is never relaxed; the spread check only ever ADDS a failure.
# JOURNAL must be gathered with `-o short-iso` (ISO-timestamped lines).
dantesync_offset_verdict() {
  local journal="$1" fresh="$2" bound="$3" stability="${4:-}"
  local off_line off_us mag spread drift=0 unstable=0
  off_line="$(_freshest_ntp_offset_line "$journal")"
  if [ -z "$off_line" ]; then
    printf 'absent\n'
    return 0
  fi
  # #767: grade the MEDIAN of the recent fresh samples, not the single freshest one -- see
  # _fresh_offset_median_us above for the measurement-noise rationale. Contract unchanged.
  # Window K=11 (~5min at the ~30s sample cadence): a same-sign 2-3-sample burst under E2E
  # LAN load majority-covers a 5-window (live cam5 +2624/+2508/+2865 consecutive; cam7 median
  # 2113 in run 29420477560) but stays a minority of 11; a genuine clock step shifts ALL
  # samples and still drifts. Callers gather journalctl -n 400, so the depth exists.
  off_us="$(_fresh_offset_median_us "$journal" "$fresh" 11)"
  if [ -z "$off_us" ]; then
    printf 'stale\n'
    return 0
  fi
  mag="$(abs_int "$off_us")"
  [ "$mag" -gt "$bound" ] && drift=1
  # #837: spread/stability over the SAME K=11 fresh set (mirrors the HTTP sampled_offset_verdict).
  # Only when the caller passes STABILITY_US; omitted/empty keeps the median-only contract. A
  # present-but-non-numeric bound fails closed to unstable (never a silent pass on a broken knob,
  # the #595 numeric-guard gotcha). >=2 samples required, since spread_of_ints returns "" for one.
  if [ -n "$stability" ]; then
    if ! grep -qE '^[0-9]+$' <<<"$stability"; then
      unstable=1
    else
      spread="$(_fresh_offset_spread_us "$journal" "$fresh" 11)"
      [ -n "$spread" ] && [ "$spread" -gt "$stability" ] && unstable=1
    fi
  fi
  if [ "$drift" = 1 ] && [ "$unstable" = 1 ]; then
    printf 'drift_unstable\n'
  elif [ "$drift" = 1 ]; then
    printf 'drift\n'
  elif [ "$unstable" = 1 ]; then
    printf 'unstable\n'
  else
    printf 'ok\n'
  fi
}

# --- PTP-LOCK signal (the #7 precondition the recording-E2E gate requires) -----------------
#
# NTP offset alone is NOT enough for the recording E2E: cross-node per-hop latency/timestamps
# are only meaningful when the cluster's FINE servo is the µs-grade PTP, not the ±1 ms NTP
# sawtooth fallback (GM 10.77.9.184 down). DanteSync exposes the PTP servo state two ways:
#
#  * Linux cams (journald): while PTP is LOCKED the daemon emits a continuous stream of
#      `[PTP] NANO  Drift: …ns/s`  (ultra-precise servo) or `[PTP] LOCK  Drift: …ns/s` (locked).
#    When PTP degrades to NTP-only those `[PTP] (NANO|LOCK)` servo lines STOP and only
#    `[NTP] offset:` lines remain. So "a recent `[PTP] (NANO|LOCK)` servo line exists" = PTP up.
#  * Windows OBS boxes (status pipe JSON): `"is_locked":true` AND `"mode":"NANO"|"LOCK"` =
#    PTP servo locked; `"is_locked":false` (or a non-NANO/LOCK mode) = degraded/NTP-only.

# --- #864: false-DEGRADED grace (timestamp-aware servo-liveness) ------------------------------
# When an `[NTP] offset:` line is POSITIONALLY newer than the last servo line, the pre-#864 parser
# reported DEGRADED unconditionally. But `[NTP] offset:` lines emit on a FASTER cadence (~15s live)
# than the `[PTP] (NANO|LOCK) Drift:` servo (~30s live, cam2 2026-08-14), so in a genuinely LOCKED
# steady state the window's last line is routinely an NTP line that arrived after the last servo
# tick with the next tick not yet due -> a false DEGRADED. The grace below requires the NTP line to
# trail the last servo line by MORE than one servo interval before DEGRADED is reported.
# PTP_LOCK_SERVO_GRACE_FACTOR: how many servo intervals of trailing gap are tolerated (2 means
# the servo missed at least one full extra due tick). PTP_LOCK_SERVO_GRACE_FLOOR_S: absolute minimum
# grace in seconds, used when the cadence can't be measured (fewer than two ISO-timestamped servo
# lines) and as a lower bound against a degenerate tiny measured cadence (75s = ~2.5x the observed
# ~30s cadence, comfortably above any healthy inter-tick gap). Both env-overridable for a future
# recalibration without a code change.
PTP_LOCK_SERVO_GRACE_FACTOR="${PTP_LOCK_SERVO_GRACE_FACTOR:-2}"
PTP_LOCK_SERVO_GRACE_FLOOR_S="${PTP_LOCK_SERVO_GRACE_FLOOR_S:-75}"

# _dante_line_iso_ts LINE -> the `-o short-iso` timestamp (first whitespace-delimited field) of a
# dantesync journal LINE, but ONLY when it is a genuine ISO timestamp (YYYY-MM-DDThh:mm:ss...); ""
# for a non-ISO line (the older `Jun 22 ...` journalctl default the unit fixtures use, or an empty
# line). Lets the grace path degrade gracefully to the position verdict on any non-`-o short-iso`
# journal. `|| true` keeps a no-match from tripping the caller's set -e/pipefail.
_dante_line_iso_ts() {
  printf '%s\n' "$1" | awk '{print $1}' \
    | { grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' || true; }
}

# _servo_cadence_s JOURNAL -> the MEDIAN gap in SECONDS between consecutive `[PTP] (NANO|LOCK)
# Drift:` servo-line timestamps in JOURNAL (median rejects a single dropped tick), "" when fewer
# than two ISO-timestamped servo lines exist. JOURNAL must be `-o short-iso`. Sizes the #864 grace
# to the node's OWN observed cadence so a future dantesync report-cadence change (already happened
# once, #679) can't rot a hard-coded value. Reuses median_of_ints (defined below; bash resolves it
# at call time) and _short_iso_epoch (above).
_servo_cadence_s() {
  local text="$1" epochs ts e prev="" diffs="" d
  epochs="$(printf '%s\n' "$text" \
    | { grep -E '\[PTP\] +(NANO|LOCK) +Drift:' || true; } | { grep -v 'MODE ===' || true; } \
    | awk '{print $1}' | { grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}T' || true; })"
  [ -n "$epochs" ] || { printf ''; return 0; }
  while IFS= read -r ts; do
    [ -n "$ts" ] || continue
    e="$(_short_iso_epoch "$ts")"
    [ -n "$e" ] || continue
    if [ -n "$prev" ]; then
      d=$(( e - prev ))
      [ "$d" -gt 0 ] && diffs="${diffs}${d}"$'\n'
    fi
    prev="$e"
  done <<< "$epochs"
  median_of_ints "$diffs"
}

# ptp_locked_from_journal TEXT -> "LOCKED" if the journal's MOST RECENT DanteSync clock event
# is a `[PTP] (NANO|LOCK)  Drift:` servo line (servo CURRENTLY running); "DEGRADED" if servo
# line(s) exist in the buffer but the LATEST clock event is an `[NTP] offset:` (the servo has
# STOPPED — degraded to the NTP-only sawtooth, GM down); "" (UNKNOWN) if NO `[PTP]` servo line
# is present at all (PTP never observed).
#
# The freshness check is ESSENTIAL: `journalctl -n N` is count-bounded, NOT time-bounded, so when
# PTP degrades the servo lines stop but stale ones linger in the last-N window. Returning LOCKED
# on any stale servo line anywhere would pass a freshly-degraded node. So we compare the POSITION
# of the last servo line against the last NTP-offset line: a servo line AFTER the most recent NTP
# line = servo still ticking = LOCKED; an NTP line after the last servo line = servo stopped =
# DEGRADED. The `[PTP] === … MODE ===` transition banners are excluded (events, not the steady
# servo signal). `|| true`/`echo 0` keep set -e happy on a no-match.
ptp_locked_from_journal() {
  local text="$1" last_servo_n last_ntp_n
  # 1-based line number of the LAST steady servo line (NANO/LOCK Drift), 0 if none. Exclude the
  # `=== … MODE ===` banners (they contain "MODE"; the Drift lines do not). The `|| true` on each
  # grep keeps a NO-MATCH (grep exit 1) from tripping the caller's `set -e`/`pipefail`.
  last_servo_n="$(printf '%s\n' "$text" \
    | { grep -nE '\[PTP\] +(NANO|LOCK) +Drift:' || true; } | { grep -v 'MODE ===' || true; } \
    | tail -1 | cut -d: -f1)"
  last_servo_n="${last_servo_n:-0}"
  if [ "$last_servo_n" = 0 ]; then
    printf ''        # no servo line at all -> PTP never observed (UNKNOWN)
    return 0
  fi
  # 1-based line number of the LAST `[NTP] offset:` line, 0 if none.
  last_ntp_n="$(printf '%s\n' "$text" | { grep -nE '\[NTP\] offset:' || true; } | tail -1 | cut -d: -f1)"
  last_ntp_n="${last_ntp_n:-0}"
  # Servo line is the more-recent of the two -> servo currently ticking -> LOCKED.
  if [ "$last_servo_n" -ge "$last_ntp_n" ]; then
    printf 'LOCKED'
    return 0
  fi
  # An `[NTP] offset:` line is POSITIONALLY newer than the last servo line. Do NOT declare DEGRADED
  # on that alone (#864): in a genuinely LOCKED steady state this is NORMAL, because NTP lines emit
  # faster than the servo cadence. Grade by the `-o short-iso` TIMESTAMPS instead — DEGRADED only
  # when the NTP line trails the last servo line by MORE than one servo interval (a stopped servo
  # misses every subsequent tick, so the gap grows without bound; a healthy inter-tick gap is at
  # most ~one interval). Self-calibrate the interval from the servo lines' own timestamps.
  local last_servo_line last_ntp_line servo_epoch ntp_epoch cadence grace gap
  last_servo_line="$(printf '%s\n' "$text" \
    | { grep -E '\[PTP\] +(NANO|LOCK) +Drift:' || true; } | { grep -v 'MODE ===' || true; } | tail -1)"
  last_ntp_line="$(printf '%s\n' "$text" | { grep -E '\[NTP\] offset:' || true; } | tail -1)"
  servo_epoch="$(_short_iso_epoch "$(_dante_line_iso_ts "$last_servo_line")")"
  ntp_epoch="$(_short_iso_epoch "$(_dante_line_iso_ts "$last_ntp_line")")"
  if [ -z "$servo_epoch" ] || [ -z "$ntp_epoch" ]; then
    printf 'DEGRADED'   # no ISO timestamps to grade -> fall back to the position verdict
    return 0
  fi
  cadence="$(_servo_cadence_s "$text")"
  if [ -n "$cadence" ] && [ "$cadence" -gt 0 ]; then
    grace=$(( cadence * PTP_LOCK_SERVO_GRACE_FACTOR ))
    [ "$grace" -lt "$PTP_LOCK_SERVO_GRACE_FLOOR_S" ] && grace="$PTP_LOCK_SERVO_GRACE_FLOOR_S"
  else
    grace="$PTP_LOCK_SERVO_GRACE_FLOOR_S"   # <2 servo lines -> cadence unmeasurable
  fi
  gap=$(( ntp_epoch - servo_epoch ))
  if [ "$gap" -gt "$grace" ]; then
    printf 'DEGRADED'
  else
    printf 'LOCKED'
  fi
}

# ptp_locked_from_pipe_json TEXT -> "LOCKED" iff the status blob reports `"is_locked":true` AND
# a `"mode"` of NANO or LOCK; "DEGRADED" if is_locked is present but false / mode is not a lock
# mode; "" (UNKNOWN) if neither field is present (unreadable status). Reads is_locked + mode only
# (NOT offset_ns, which is the raw pre-anchor PTP phase and is legitimately large). `|| true`.
ptp_locked_from_pipe_json() {
  local text="$1" locked mode
  locked="$(printf '%s' "$text" | grep -oE '"is_locked"[[:space:]]*:[[:space:]]*(true|false)' \
    | sed -n 's/.*:[[:space:]]*\(true\|false\).*/\1/p' | tail -1 || true)"
  mode="$(printf '%s' "$text" | grep -oE '"mode"[[:space:]]*:[[:space:]]*"[A-Za-z]+"' \
    | sed -n 's/.*"mode"[[:space:]]*:[[:space:]]*"\([A-Za-z][A-Za-z]*\)".*/\1/p' | tail -1 || true)"
  if [ -z "$locked" ] && [ -z "$mode" ]; then
    printf ''   # neither field readable -> UNKNOWN
    return 0
  fi
  if [ "$locked" = "true" ] && { [ "$mode" = "NANO" ] || [ "$mode" = "LOCK" ]; }; then
    printf 'LOCKED'
    return 0
  fi
  printf 'DEGRADED'
}

# ptp_check LABEL STATE -> prints a status line; returns 0 LOCKED / 2 DEGRADED / 3 UNKNOWN.
# STATE is the output of one of the two ptp_locked_from_* parsers ("LOCKED" / "DEGRADED" / "").
# Empty or any non-LOCKED value is NEVER treated as OK (test-strictness: an unread/degraded PTP
# servo must fail the gate, never silently pass — meaningless cross-node latency otherwise).
ptp_check() {
  local label="$1" state="$2"
  case "$state" in
    LOCKED)
      printf '  %-14s PTP LOCKED   (fine servo µs-grade — cross-node latency is meaningful)\n' "$label"
      return 0 ;;
    DEGRADED)
      printf '  %-14s PTP DEGRADED (NTP-only sawtooth — GM 10.77.9.184 down? latency meaningless)\n' "$label"
      return 2 ;;
    *)
      printf '  %-14s PTP UNKNOWN  (no servo signal read — status incomplete)\n' "$label"
      return 3 ;;
  esac
}

# --- GRANDMASTER identity (#834) -------------------------------------------------------------
#
# #834 (stream box, 2026-07-28): a node reported `is_locked:true`/`settled:true` while sitting
# 14.7 ms off the rig -- it had PTP-locked to a FOREIGN grandmaster (`gm_source_ip:10.77.7.109`)
# instead of the rig's own `10.77.9.184`. `is_locked` alone only proves the servo is disciplined
# to SOME clock; it says nothing about WHICH one. A node faithfully locked to the wrong
# grandmaster reads every local health indicator green while being genuinely 15 ms out -- the same
# false-green shape as the #591 competing-timesync-daemon incident, but the mechanism here is
# grandmaster ELECTION, not a second daemon. Every node in the rig must agree on the SAME
# `gm_source_ip`; this is a gated FACT distinct from (and in addition to) the offset bound.

# gm_source_ip_from_pipe_json TEXT -> the `"gm_source_ip"` string field value in TEXT (a DanteSync
# status-pipe/HTTP JSON blob -- the SAME blob offset_us_from_pipe_json/ptp_locked_from_pipe_json
# read), "" if the field is absent/unparseable. `|| true` survives a no-match under set -e/
# pipefail (same convention as every other *_from_pipe_json parser in this file).
gm_source_ip_from_pipe_json() {
  printf '%s' "$1" | grep -oE '"gm_source_ip"[[:space:]]*:[[:space:]]*"[^"]*"' \
    | sed -n 's/.*"gm_source_ip"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | tail -1 || true
}

# gm_matches_expected ACTUAL EXPECTED -> 0 iff both are non-empty and IDENTICAL. An empty ACTUAL
# (the field was unreadable) is never treated as a match -- test-strictness: a grandmaster we
# could not read must never look correct, exactly the "unreachable = FAIL" contract every other
# check in this file follows.
gm_matches_expected() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# gm_check LABEL ACTUAL EXPECTED -> prints a status line; returns 0 OK / 2 FOREIGN-GM / 3 UNKNOWN.
# A foreign or unreadable grandmaster is ALWAYS a hard fail regardless of how small the node's own
# offset looks at that instant (#834: "is_locked:true" was true while 15 ms out) -- callers must
# gate on this in addition to, never instead of, the offset/PTP-lock checks above.
gm_check() {
  local label="$1" actual="$2" expected="$3"
  if [ -z "$actual" ]; then
    printf '  %-14s GM UNKNOWN   (gm_source_ip unread; expected %s)\n' "$label" "$expected"
    return 3
  fi
  if gm_matches_expected "$actual" "$expected"; then
    printf '  %-14s GM OK        (locked to the rig grandmaster %s)\n' "$label" "$actual"
    return 0
  fi
  printf '  %-14s GM FOREIGN   (locked to %s, rig grandmaster is %s -- #834: is_locked can be true while badly out)\n' \
    "$label" "$actual" "$expected"
  return 2
}

# offset_check LABEL OFFSET_US BOUND_US -> prints a status line; returns 0 OK / 2 DRIFT /
# 3 UNKNOWN. OK iff |OFFSET_US| <= BOUND_US (NUMERIC compare). An empty OFFSET_US is UNKNOWN,
# never OK — an offset we could not read must never look in-bound (test-strictness: no silent
# pass). A non-numeric offset is also UNKNOWN (a malformed read is not a clean read).
offset_check() {
  local label="$1" offset="$2" bound="$3" mag
  if [ -z "$offset" ]; then
    printf '  %-14s UNKNOWN  (offset unread; bound %s us)\n' "$label" "$bound"
    return 3
  fi
  if ! grep -qE '^[+-]?[0-9]+$' <<<"$offset"; then
    printf '  %-14s UNKNOWN  (malformed offset %s; bound %s us)\n' "$label" "$offset" "$bound"
    return 3
  fi
  mag="$(abs_int "$offset")"
  if [ "$mag" -le "$bound" ]; then
    printf '  %-14s OK       (offset %s us, |%s| <= %s)\n' "$label" "$offset" "$mag" "$bound"
    return 0
  fi
  printf '  %-14s DRIFT    (offset %s us, |%s| > %s us bound)\n' "$label" "$offset" "$mag" "$bound"
  return 2
}

# --- Multi-sample offset + stability grading (#836) -----------------------------------------
#
# offset_check (above) grades a SINGLE read of "ntp_offset_us" against the bound -- for the
# Windows/HTTP status-pipe signal that is close to a coin flip on a noisy node: live data (#836,
# stream box, 22 reads 25s apart) shows only 2/22 individual reads landing inside the existing
# 2000us bound, so the SAME unchanged node passes or fails almost at random depending purely on
# which instant it happened to be sampled. There was also no check at all for how much the
# readings SCATTER between samples -- a node whose offset bounces wildly is invisible to a
# single-read gate as long as one lucky read is in-bound. And the daemon's HTTP endpoint only
# refreshes on its own cadence, so back-to-back reads can return the byte-identical "updated_ts"
# (observed live: 2014,2014 / 7482,7482 6-25s apart) -- these are NOT independent measurements.
#
# These functions turn a SEQUENCE of raw status-JSON reads of the SAME node (gathered by the
# caller -- dantesync-gate.sh's gather_http_samples, the only IMPURE piece of this feature) into
# a graded verdict: the MEDIAN of the DISTINCT samples against the EXISTING, UNCHANGED bound
# (better estimator, same bound -- a lucky/unlucky single sample can no longer decide the node),
# PLUS a NEW check a single-sample gate could never make at all: the SPREAD (max-min) of those
# same distinct samples against a stability threshold, so a node whose readings scatter widely
# FAILS even when its median looks perfect (its timestamps are equally unusable either way). Too
# few distinct samples (below MIN_DISTINCT) is itself a hard failure -- never a silent pass on
# one lucky read. Net effect vs the single-read gate: strictly MORE ways to fail, never fewer;
# the bound itself never moves.

# distinct_offset_samples_us PAYLOADS_NEWLINE -> newline list of the DISTINCT ntp_offset_us
# values in PAYLOADS_NEWLINE (one raw status-JSON blob per line, in the order they were read).
# ASSUMES each individual read's payload is itself a SINGLE line -- true of every real capture in
# this file (dantesync#47's HTTP endpoint always serves compact, non-pretty-printed JSON; see the
# fixtures throughout this file and tests/clock_offset_guard.rs) and of gather_http_samples in
# dantesync-gate.sh (which newline-joins the reads it gathers). If a future endpoint ever emitted
# multi-line JSON, a read would fail safe here (its "updated_ts"/"ntp_offset_us" wouldn't be found
# on any single line -> skipped, never miscounted or misread), never a false pass.
#
# A read is counted as a NEW independent sample only when its "updated_ts" differs from the last
# ACCEPTED sample's "updated_ts" -- a read whose updated_ts repeats the last accepted one is the
# daemon re-serving its own cached value between refreshes and is skipped, never double-counted
# (#836 point 5). A read with an unparseable/missing updated_ts OR ntp_offset_us is also skipped
# entirely (it neither counts as a new sample nor resets the "last accepted" tracker) -- the same
# "cannot prove it -> do not count it" discipline as every other parser in this file.
distinct_offset_samples_us() {
  local payloads="$1" line ts off prev_ts=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    ts="$(updated_ts_from_pipe_json "$line")"
    [ -n "$ts" ] || continue
    if [ -n "$prev_ts" ] && [ "$ts" = "$prev_ts" ]; then
      continue
    fi
    off="$(offset_us_from_pipe_json "$line")"
    [ -n "$off" ] || continue
    printf '%s\n' "$off"
    prev_ts="$ts"
  done <<< "$payloads"
}

# median_of_ints LIST_NEWLINE -> the LOWER median of the integers in LIST_NEWLINE (one per line;
# non-integer lines are ignored), "" if none. Same convention as the journal path's
# _fresh_offset_median_us (sort -n, pick position int((n+1)/2)) so both estimators round the same
# way on an even count.
median_of_ints() {
  local list="$1" n
  n="$(printf '%s\n' "$list" | grep -cE '^-?[0-9]+$' || true)"
  [ "${n:-0}" -gt 0 ] || { printf ''; return 0; }
  printf '%s\n' "$list" | grep -E '^-?[0-9]+$' | sort -n \
    | awk -v n="$n" 'NR == int((n+1)/2) { print; exit }'
}

# spread_of_ints LIST_NEWLINE -> max(LIST) - min(LIST) over the integers in LIST_NEWLINE (one per
# line; non-integer lines are ignored), "" if fewer than 2 valid values (scatter is undefined from
# a single point -- the caller's own MIN_DISTINCT gate normally keeps this from mattering, but the
# function stays honest standalone).
spread_of_ints() {
  local list="$1" n v max min first=1
  n="$(printf '%s\n' "$list" | grep -cE '^-?[0-9]+$' || true)"
  [ "${n:-0}" -ge 2 ] || { printf ''; return 0; }
  while IFS= read -r v; do
    grep -qE '^-?[0-9]+$' <<<"$v" || continue
    if [ "$first" = 1 ]; then
      max="$v"; min="$v"; first=0
    else
      [ "$v" -gt "$max" ] && max="$v"
      [ "$v" -lt "$min" ] && min="$v"
    fi
  done <<< "$list"
  printf '%s' "$((max - min))"
}

# frozen_sample_verdict PAYLOADS_NEWLINE MIN_DISTINCT -> "frozen" | "live" | "insufficient".
# #1014 backward-compat fallback for a PRE-1.8.30 payload (no "ntp_age_s" field to grade
# freshness directly via ntp_freshness_verdict above): a byte-identical ntp_offset_us across
# EVERY distinct-by-updated_ts sample in PAYLOADS_NEWLINE is the signature a dead/free-running
# NTP measurement leaves in the OLD payload shape -- #1014's original strih incident was exactly
# this: six polls, six identical readings, spread 0us, while the general updated_ts kept
# advancing from the healthy PTP loop. Reuses distinct_offset_samples_us/spread_of_ints (the same
# samples the gate already collects for sampled_offset_verdict), never a second, independently-
# gathered sample set. Requires at least MIN_DISTINCT distinct samples before calling anything
# "frozen" -- with fewer, the spread is undefined/unprovable and this must never guess.
#   insufficient -- fewer than MIN_DISTINCT distinct samples, or MIN_DISTINCT is not a plain
#                   non-negative integer.
#   frozen       -- >= MIN_DISTINCT distinct samples, all reporting the SAME ntp_offset_us
#                   (spread == 0).
#   live         -- >= MIN_DISTINCT distinct samples with any variation (spread != 0) -- a
#                   genuinely live (if possibly noisy) NTP measurement; the caller falls through
#                   to the unchanged legacy sampled_offset_check grading.
frozen_sample_verdict() {
  local payloads="$1" min_distinct="$2" samples n spread
  if ! grep -qE '^[0-9]+$' <<<"$min_distinct"; then
    printf 'insufficient\n'
    return 0
  fi
  samples="$(distinct_offset_samples_us "$payloads")"
  n="$(printf '%s\n' "$samples" | grep -cE '^-?[0-9]+$' || true)"
  n="${n:-0}"
  if [ "$n" -lt "$min_distinct" ]; then
    printf 'insufficient\n'
    return 0
  fi
  spread="$(spread_of_ints "$samples")"
  if [ "$spread" = "0" ]; then
    printf 'frozen\n'
  else
    printf 'live\n'
  fi
}

# sampled_offset_verdict PAYLOADS_NEWLINE BOUND_US STABILITY_US MIN_DISTINCT [MODE] ->
#   "insufficient" | "drift" | "unstable" | "drift_unstable" | "ok"
#   insufficient -- fewer than MIN_DISTINCT distinct samples were obtained (#836 point 5, second
#                   half) OR BOUND_US/STABILITY_US/MIN_DISTINCT is not a plain non-negative
#                   integer -- never grade on data we cannot prove is enough.
#   drift          -- the MEDIAN of the distinct samples exceeds BOUND_US (spread in bound).
#   unstable       -- the median is in-bound but the SPREAD exceeds STABILITY_US (#836 point 3 --
#                     a NEW failure mode the single-read gate could never detect at all).
#   drift_unstable -- both the median AND the spread fail.
#   ok             -- median in-bound AND (fewer than 2 samples, so spread is undefined, OR
#                     spread in-bound).
#
# MODE (#1014, default "full" when omitted -- every pre-existing 4-arg call site is therefore
# byte-for-byte unchanged): "median-only" skips the spread/stability check ENTIRELY, so the
# verdict can only ever be "insufficient" | "drift" | "ok" -- never "unstable"/"drift_unstable".
# This is for the NTP MASTER node, whose spread is a by-design correction-lag sawtooth (dantesync
# issue 71), not a fleet-coherence signal -- see dantesync-gate.sh's GATE_NTP_MASTER_NAME. The
# location bound (BOUND_US) is never skipped in either mode; a genuinely drifted master still
# fails on its median exactly like any other node.
sampled_offset_verdict() {
  local payloads="$1" bound="$2" stability="$3" min_distinct="$4" mode="${5:-full}"
  local samples n median spread drift=0 unstable=0
  if ! grep -qE '^[0-9]+$' <<<"$bound" \
     || ! grep -qE '^[0-9]+$' <<<"$stability" \
     || ! grep -qE '^[0-9]+$' <<<"$min_distinct"; then
    printf 'insufficient\n'
    return 0
  fi
  samples="$(distinct_offset_samples_us "$payloads")"
  n="$(printf '%s\n' "$samples" | grep -cE '^-?[0-9]+$' || true)"
  n="${n:-0}"
  if [ "$n" -lt "$min_distinct" ]; then
    printf 'insufficient\n'
    return 0
  fi
  median="$(median_of_ints "$samples")"
  [ -n "$median" ] || { printf 'insufficient\n'; return 0; }
  [ "$(abs_int "$median")" -gt "$bound" ] && drift=1
  if [ "$mode" != "median-only" ] && [ "$n" -ge 2 ]; then
    spread="$(spread_of_ints "$samples")"
    [ -n "$spread" ] && [ "$spread" -gt "$stability" ] && unstable=1
  fi
  if [ "$drift" = 1 ] && [ "$unstable" = 1 ]; then
    printf 'drift_unstable\n'
  elif [ "$drift" = 1 ]; then
    printf 'drift\n'
  elif [ "$unstable" = 1 ]; then
    printf 'unstable\n'
  else
    printf 'ok\n'
  fi
}

# sampled_offset_report PAYLOADS_NEWLINE -> "DISTINCT MEDIAN SPREAD" (space-separated; MEDIAN/
# SPREAD print as "NA" when undefined -- zero samples, or fewer than 2 for SPREAD). Lets a caller
# print BOTH numbers on every status line regardless of pass/fail (#836 point 4: "a red says which
# kind of bad it is").
sampled_offset_report() {
  local payloads="$1" samples n median spread
  samples="$(distinct_offset_samples_us "$payloads")"
  n="$(printf '%s\n' "$samples" | grep -cE '^-?[0-9]+$' || true)"
  n="${n:-0}"
  median="NA"
  spread="NA"
  [ "$n" -ge 1 ] && median="$(median_of_ints "$samples")"
  if [ "$n" -ge 2 ]; then
    spread="$(spread_of_ints "$samples")"
  fi
  printf '%s %s %s\n' "$n" "${median:-NA}" "${spread:-NA}"
}

# sampled_offset_check LABEL PAYLOADS_NEWLINE BOUND_US STABILITY_US MIN_DISTINCT [MODE] [NOTE] ->
# prints ONE status line (median + spread + distinct count, always -- #836 point 4) and returns
# 0 OK / 2 DRIFT-or-UNSTABLE / 3 UNKNOWN (insufficient distinct samples or malformed input). Same
# rc contract as offset_check (0/2/3) so node_verdict's OK/BAD/UNKNOWN combiner (dantesync-gate.sh)
# is unchanged -- a stability failure is exactly as hard a failure as a location (drift) failure.
#
# MODE (#1014, default "full") is passed straight through to sampled_offset_verdict -- see its
# own doc comment. When MODE is "median-only" this function appends an inline note explaining WHY
# no stability verdict is possible for this node, on the SAME line as the OK/DRIFT verdict rather
# than a separate line -- several dantesync-gate.sh callers locate "the" node's own report via
# `.lines().find(|l| l.starts_with(label))`, so printing a second line ahead of the real verdict
# would silently break those. NOTE (optional, appended after any MODE note) lets a caller add its
# own free-form annotation (e.g. dantesync-gate.sh's pre-1.8.30 backward-compat marker) without
# this function needing to know about every possible reason.
sampled_offset_check() {
  local label="$1" payloads="$2" bound="$3" stability="$4" min_distinct="$5"
  local mode="${6:-full}" extra_note="${7:-}"
  local verdict report n median spread note=""
  verdict="$(sampled_offset_verdict "$payloads" "$bound" "$stability" "$min_distinct" "$mode")"
  report="$(sampled_offset_report "$payloads")"
  n="$(printf '%s' "$report" | awk '{print $1}')"
  median="$(printf '%s' "$report" | awk '{print $2}')"
  spread="$(printf '%s' "$report" | awk '{print $3}')"
  if [ "$mode" = "median-only" ]; then
    note=" -- NTP MASTER: spread reported for reference only, not gated (correction-lag sawtooth by design, dantesync issue 71, #1014)"
  fi
  note="${note}${extra_note}"
  case "$verdict" in
    ok)
      printf '  %-14s OK       (median %sus <= %sus bound; spread %sus, stability %sus; %s distinct samples)%s\n' \
        "$label" "$median" "$bound" "$spread" "$stability" "$n" "$note"
      return 0 ;;
    drift)
      printf '  %-14s DRIFT    (median %sus > %sus bound; spread %sus, stability %sus; %s distinct samples)%s\n' \
        "$label" "$median" "$bound" "$spread" "$stability" "$n" "$note"
      return 2 ;;
    unstable)
      printf '  %-14s UNSTABLE (median %sus <= %sus bound; spread %sus > %sus stability -- #836 scattered/unusable; %s distinct samples)%s\n' \
        "$label" "$median" "$bound" "$spread" "$stability" "$n" "$note"
      return 2 ;;
    drift_unstable)
      printf '  %-14s DRIFT+UNSTABLE (median %sus > %sus bound; spread %sus > %sus stability; %s distinct samples)%s\n' \
        "$label" "$median" "$bound" "$spread" "$stability" "$n" "$note"
      return 2 ;;
    *)
      printf '  %-14s UNKNOWN  (only %s distinct sample(s) [median %sus, spread %sus], need >= %s -- refresh-interval duplicates, #836)%s\n' \
        "$label" "$n" "$median" "$spread" "$min_distinct" "$note"
      return 3 ;;
  esac
}

# --- Client-row spread-side chase completion (#1022 spread-side completion) ------------------
#
# client_chase_bound_us (below) only ever widens a CLIENT row's MEDIAN check -- its own doc
# comment says so explicitly, and the spread/stability check stays fully active for client rows
# by design (so genuine scatter, the #836 measurement-noise class, is never masked). Live
# evidence (a merged round's real E2E rerun) showed the SAME master step that the median fix
# already handles ALSO inflates a client's SPREAD: one master step lands inside an otherwise-
# baseline sampling window as one (or a few) elevated samples, which can push spread past the
# fixed 2000us stability bound even while the median stays correctly in-bound. Because the step
# is on ONE clock shared by the whole fleet, the SAME step can trip MULTIPLE clients in the SAME
# run (observed live: cam1, cam2, AND stream all UNSTABLE simultaneously).
#
# should_resample_for_chase is the DECISION only (pure, directly testable) -- whether a node's
# "unstable" verdict is worth a one-time fresh resample before failing. It never re-samples
# itself; the caller (dantesync-gate.sh's grade_http_node) does that impure step when this
# function says "yes".

# max_abs_of_ints LIST_NEWLINE -> the largest |value| among the integers in LIST_NEWLINE (one per
# line; non-integer lines ignored), "" if none. Distinguishes "the worst sample in this window
# still fits inside a plausible single master step-chase" (worth a resample) from "something in
# here is far bigger than any legitimate step could produce" (fail immediately -- #836's genuine-
# scatter class, or a real clock fault, is never given a second chance by a resample).
max_abs_of_ints() {
  local list="$1" v mag max=""
  while IFS= read -r v; do
    grep -qE '^-?[0-9]+$' <<<"$v" || continue
    mag="$(abs_int "$v")"
    if [ -z "$max" ] || [ "$mag" -gt "$max" ]; then
      max="$mag"
    fi
  done <<< "$list"
  printf '%s' "$max"
}

# should_resample_for_chase PAYLOADS_NEWLINE BOUND_US STABILITY_US MIN_DISTINCT MODE ->
# "yes" | "no". "yes" ONLY when ALL of:
#   * MODE is "full" (a CLIENT row) -- the master's own "median-only" mode never produces an
#     "unstable" verdict at all (sampled_offset_verdict skips the spread check entirely in that
#     mode), so this also excludes the master row structurally, not just via this explicit check.
#   * sampled_offset_verdict(...) is EXACTLY "unstable" -- median already within BOUND_US (which
#     may itself already be #1022-widened by client_chase_bound_us), spread over STABILITY_US.
#     Neither "drift"/"drift_unstable" (the median itself is a real problem, no resample can fix
#     that) nor "ok"/"insufficient" (nothing to resample for) ever resample.
#   * the WORST (largest |offset|) DISTINCT sample in PAYLOADS_NEWLINE still fits inside BOUND_US
#     -- nothing in this window looks bigger than what the SAME already-derived chase envelope
#     considers plausible for a single step. An outlier beyond that is never given a second
#     chance; it fails on THIS round.
# A malformed BOUND_US, or no valid samples to measure a max from, is "no" (never resample on
# data this function cannot prove is bounded -- the same "cannot prove it -> do not act" discipline
# every other fallback in this file follows).
should_resample_for_chase() {
  local payloads="$1" bound="$2" stability="$3" min_distinct="$4" mode="$5"
  local verdict samples max
  [ "$mode" = "full" ] || { printf 'no\n'; return 0; }
  verdict="$(sampled_offset_verdict "$payloads" "$bound" "$stability" "$min_distinct" "$mode")"
  # A malformed BOUND_US/STABILITY_US/MIN_DISTINCT already makes sampled_offset_verdict return
  # "insufficient" (never "unstable") -- so by the time this check passes, BOUND_US is guaranteed
  # a valid non-negative integer, and the `[ "$max" -le "$bound" ]` comparison below is safe
  # without a separate, unreachable validation of its own.
  [ "$verdict" = "unstable" ] || { printf 'no\n'; return 0; }
  samples="$(distinct_offset_samples_us "$payloads")"
  max="$(max_abs_of_ints "$samples")"
  [ -n "$max" ] || { printf 'no\n'; return 0; }
  if [ "$max" -le "$bound" ]; then
    printf 'yes\n'
  else
    printf 'no\n'
  fi
}

# --- Bimodal chase-signature exclusion (#1022, supersedes relying on resample-once alone) ------
#
# A live rerun proved resample-once (should_resample_for_chase, above) is a PROBABILISTIC
# mitigation, not a deterministic fix: a client's own elevated-offset duty cycle is ~30-60s per
# ~130-150s master step period (25-45%), so a FIXED resample delay collides with the SAME (or the
# NEXT) excursion roughly that often -- observed live, a 15s-delayed resample landed inside the
# same still-unresolved excursion and reported the SAME 2561us spread again. #861's 3-consecutive-
# green acceptance needs deterministic, not a coin flip.
#
# chase_bimodal_exclusion_verdict grades a window's samples DIRECTLY for the SIGNATURE a step-
# chase leaves, instead of hoping an independent resample lands outside the excursion: a tight
# baseline cluster near zero PLUS a tight, SAME-SIGN elevated cluster at (or under) the step size,
# all within the envelope. A CLIENT row (never the master) whose raw verdict would be EXACTLY
# "unstable" is explained -- passes -- when ALL of:
#
# #1041 finding (proven, not just observed): this function's own condition 2 (every ELEVATED
# sample <= BOUND_US) makes a "drift"/"drift_unstable" raw verdict STRUCTURALLY UNREACHABLE here
# in any sane config (STABILITY_US <= BOUND_US, true of every default and documented flag in this
# file -- NOT independently enforced by dantesync-gate.sh's own CLI parsing, since the
# comparison is against a per-node EFFECTIVE bound that only exists after widening at runtime;
# an operator who deliberately sets --stability-us far above --bound-us, wider than any
# widened envelope could ever reach, could theoretically defeat this argument, but that is a
# self-inflicted misconfiguration far outside anything this file's own defaults or
# documented flags would ever produce): sampled_offset_verdict's own "drift" flag is
# `abs(median) > BOUND_US`, and the MEDIAN is
# itself one of the very samples this function partitions -- a baseline sample has
# abs<=STABILITY_US<=BOUND_US by definition (can't be the median if median>BOUND_US), and an
# ELEVATED sample exceeding BOUND_US is EXACTLY what condition 2 already rejects. So a live chase
# excursion that would have false-DRIFTed the MEDIAN check (cam3's own incident: majority-
# elevated samples pull the median itself past the bound) is resolved ENTIRELY by
# client_chase_bound_us (above) correctly deriving a bound wide enough to cover the excursion --
# once BOUND_US covers it, sampled_offset_verdict can only report "unstable" (spread still over)
# or "ok", never "drift"/"drift_unstable", and this UNCHANGED function already explains "unstable"
# exactly as it did before #1041. See client_chase_bound_us_reproduces_the_live_cam3_envelope_1041
# and the transformation test right after it in tests/clock_offset_guard.rs for the proof.
#   1. Partition distinct samples into baseline (|s| <= STABILITY_US) and elevated (|s| >
#      STABILITY_US).
#   2. Every elevated sample fits inside BOUND_US (the SAME bound the median check already uses
#      -- possibly the #1022-widened chase envelope, not necessarily the bare fixed bound).
#   3. The baseline subset is non-empty and its own spread <= STABILITY_US (a single-sample
#      baseline has an UNDEFINED spread -- treated as vacuously fine, nothing to scatter from one
#      point).
#   4. The elevated subset's own spread <= STABILITY_US -- a chase is ONE tight mode at the
#      master's step size (live: 2561/2574us); genuine multi-modal scatter fails this (the #836
#      genuine-scatter class stays caught). A single-sample elevated subset is likewise vacuously
#      fine.
#   5. Elevated samples all share ONE sign -- a chase is a coherent phase offset, never noise
#      split around zero. NOTE: given the elevated magnitude range is bounded to
#      (STABILITY_US, BOUND_US], two opposite-sign elevated values v1>0>v2 always ALSO fail
#      condition 4: their difference v1-v2 > 2*STABILITY_US >= STABILITY_US (the ">=", not ">",
#      matters at the degenerate STABILITY_US=0 edge -- 2*0=0>=0 still holds even though 2*0 is
#      not itself >0) -- this condition is kept explicit anyway because it documents the INTENT
#      independently of condition 4's exact numeric form, not because any input can make it the
#      sole deciding factor today.
# A stuck/drifted node still fails on the MEDIAN check (verdict would be "drift"/"drift_unstable",
# which never reaches this path -- the leading verdict=="unstable" gate excludes it, and per the
# #1041 finding above, a "drift"/"drift_unstable" median could never pass conditions 2-5 anyway).
# The master's own median-only row (mode != "full") is never graded via this signature at all.
chase_bimodal_exclusion_verdict() {
  local payloads="$1" bound="$2" stability="$3" min_distinct="$4" mode="$5"
  local verdict samples baseline elevated bspread espread emax
  local n_elevated first_sign v sign mixed_sign=0
  [ "$mode" = "full" ] || { printf 'no\n'; return 0; }
  verdict="$(sampled_offset_verdict "$payloads" "$bound" "$stability" "$min_distinct" "$mode")"
  [ "$verdict" = "unstable" ] || { printf 'no\n'; return 0; }
  samples="$(distinct_offset_samples_us "$payloads")"
  baseline="$(chase_bimodal_partition_us "$samples" "$stability" baseline)"
  elevated="$(chase_bimodal_partition_us "$samples" "$stability" elevated)"
  # Condition 3: baseline non-empty, spread (if defined) <= STABILITY_US.
  [ -n "$baseline" ] || { printf 'no\n'; return 0; }
  bspread="$(spread_of_ints "$baseline")"
  if [ -n "$bspread" ] && [ "$bspread" -gt "$stability" ]; then
    printf 'no\n'
    return 0
  fi
  # Condition 2: every elevated sample fits inside BOUND_US (checking the WORST one covers all).
  emax="$(max_abs_of_ints "$elevated")"
  if [ -n "$emax" ] && [ "$emax" -gt "$bound" ]; then
    printf 'no\n'
    return 0
  fi
  # Condition 4: elevated subset's own spread (if defined) <= STABILITY_US.
  espread="$(spread_of_ints "$elevated")"
  if [ -n "$espread" ] && [ "$espread" -gt "$stability" ]; then
    printf 'no\n'
    return 0
  fi
  # Condition 5: elevated samples all share one sign. Also counts them, since an empty elevated
  # subset here is structurally impossible (see the doc comment above: it would require the
  # baseline -- which would then be ALL samples -- to already have spread > STABILITY_US, which
  # condition 3 above already rejected), but this loop still counts defensively rather than assume.
  n_elevated=0
  first_sign=""
  while IFS= read -r v; do
    [ -n "$v" ] || continue
    n_elevated=$((n_elevated + 1))
    if [ "$v" -lt 0 ]; then sign="-"; else sign="+"; fi
    if [ -z "$first_sign" ]; then
      first_sign="$sign"
    elif [ "$sign" != "$first_sign" ]; then
      mixed_sign=1
    fi
  done <<< "$elevated"
  if [ "$n_elevated" -eq 0 ] || [ "$mixed_sign" -eq 1 ]; then
    printf 'no\n'
    return 0
  fi
  printf 'yes\n'
}

# chase_bimodal_partition_us LIST_NEWLINE STABILITY_US WHICH -> the DISTINCT sample values from
# LIST_NEWLINE (one per line) belonging to the "baseline" (|v| <= STABILITY_US) or "elevated"
# (|v| > STABILITY_US) subset, selected via WHICH ("baseline"|"elevated"), one per output line.
# Non-integer lines / a malformed STABILITY_US yield nothing (empty) -- never a guessed partition.
chase_bimodal_partition_us() {
  local list="$1" stability="$2" which="$3" v mag
  if ! grep -qE '^[0-9]+$' <<<"$stability"; then
    return 0
  fi
  while IFS= read -r v; do
    grep -qE '^-?[0-9]+$' <<<"$v" || continue
    mag="$(abs_int "$v")"
    if [ "$which" = "baseline" ]; then
      [ "$mag" -le "$stability" ] && printf '%s\n' "$v"
    else
      [ "$mag" -gt "$stability" ] && printf '%s\n' "$v"
    fi
  done <<< "$list"
  # #1022 bash-gotcha: under this file's own `set -euo pipefail`, a `while` loop's own exit
  # status is that of the LAST command it executed -- including a conditionally-skipped
  # `[ cond ] && printf ...` whose LAST loop iteration's condition happened to be false (printf
  # never ran, so the compound's status is the failing `[ ]`'s). With nothing after the loop,
  # THAT non-zero would abort the calling shell the moment this function is invoked via command
  # substitution (`x="$(chase_bimodal_partition_us ...)"`) -- e.g. exactly when the loop's final
  # sample lands in the "else" branch (no match). This explicit `return 0` (matching
  # spread_of_ints's own trailing statement, above) makes the function's real return status
  # independent of which branch the last iteration happened to take.
  return 0
}

# chase_bimodal_exclusion_report PAYLOADS_NEWLINE STABILITY_US -> "N_ELEVATED BASELINE_SPREAD"
# (space-separated; BASELINE_SPREAD prints as "NA" when undefined -- a single-value baseline).
# The numbers behind the operator-facing "explained by master step-chase" note -- used ONLY after
# the caller already confirmed chase_bimodal_exclusion_verdict(...) == "yes" for the SAME inputs;
# this function does not re-decide, it only reports (mirrors sampled_offset_report's own
# compute-the-display-numbers-separately-from-the-decision shape).
chase_bimodal_exclusion_report() {
  local payloads="$1" stability="$2" samples baseline elevated n_elevated bspread
  samples="$(distinct_offset_samples_us "$payloads")"
  elevated="$(chase_bimodal_partition_us "$samples" "$stability" elevated)"
  baseline="$(chase_bimodal_partition_us "$samples" "$stability" baseline)"
  n_elevated="$(printf '%s\n' "$elevated" | grep -cE '^-?[0-9]+$' || true)"
  n_elevated="${n_elevated:-0}"
  bspread="$(spread_of_ints "$baseline")"
  printf '%s %s\n' "$n_elevated" "${bspread:-NA}"
}

# chase_bimodal_exclusion_check LABEL PAYLOADS_NEWLINE BOUND_US STABILITY_US [EXTRA_NOTE] ->
# prints ONE "OK" status line (median + spread + distinct count, SAME format as
# sampled_offset_check's own "ok" case) with an appended bimodal chase-signature explanation, and
# ALWAYS returns 0. The caller (dantesync-gate.sh's grade_http_node) must have already confirmed
# chase_bimodal_exclusion_verdict(...) == "yes" for the SAME inputs -- this function does not
# re-decide, it only formats + reports (mirrors sampled_offset_check's own compute-the-display-
# numbers-separately-from-the-decision shape, and chase_bimodal_exclusion_report's own doc
# comment above).
chase_bimodal_exclusion_check() {
  local label="$1" payloads="$2" bound="$3" stability="$4" extra_note="${5:-}"
  local report n median spread excl_report excl_n excl_bspread
  report="$(sampled_offset_report "$payloads")"
  n="$(printf '%s' "$report" | awk '{print $1}')"
  median="$(printf '%s' "$report" | awk '{print $2}')"
  spread="$(printf '%s' "$report" | awk '{print $3}')"
  excl_report="$(chase_bimodal_exclusion_report "$payloads" "$stability")"
  excl_n="$(printf '%s' "$excl_report" | awk '{print $1}')"
  excl_bspread="$(printf '%s' "$excl_report" | awk '{print $2}')"
  printf '  %-14s OK       (median %sus <= %sus bound; spread %sus, stability %sus; %s distinct samples)%s -- spread excursion explained by master step-chase (%s elevated samples in one tight mode <= envelope, #1022); baseline spread %sus\n' \
    "$label" "$median" "$bound" "$spread" "$stability" "$n" "$extra_note" "$excl_n" "$excl_bspread"
  return 0
}

# --- NTP-master PTP-locked deadband widening (#1021, dantesync PR #84/#86) ---------------------
#
# dantesync issue 83: a genuinely PTP-locked master now deliberately DEFERS its periodic UTC-phase
# step to a "deadband" (live-tuned to 2500us -- NOT the 25ms originally filed; see the #1021
# supervisor comment 2026-08-12) instead of the old tight ~200us threshold, because chasing the
# Dante grandmaster's own real oscillator error (measured live on strih: ~38-66ppm) in the
# UTC-phase step every ~20-40s served no purpose and produced a visible staircase. Consequence: a
# healthy master's OWN "ntp_offset_us" now legitimately ramps anywhere in roughly
# [0, ntp_deadband_us) between corrections (a multi-minute cycle at that ppm range), rather than
# staying within a couple hundred us like before. dantesync additively reports the CURRENTLY
# active threshold as "ntp_deadband_us" in its own /status (null on a client node; absent on a
# pre-dantesync-#84 payload).
#
# scripts/dantesync-gate.sh's GATE_NTP_MASTER_NAME/"median-only" mode (#1014, above) already skips
# the spread/stability check for the master -- but its MEDIAN (location) bound is deliberately
# NEVER skipped in either mode (this file's own #1014 doc comment). Without this widening, a 30s
# sample window landing anywhere in the later portion of the master's healthy ramp would grade a
# perfectly healthy master's median against the FIXED GATE_BOUND_US (2000us, sized for a client's
# tight NTP-vs-LAN-master offset) and false-DRIFT.

# ntp_deadband_us_from_pipe_json TEXT -> the RAW text of the "ntp_deadband_us" JSON value: a
# plain (possibly-negative, though never expected live) integer string, the literal "null", or ""
# if the field is absent entirely (a pre-dantesync-#84 payload, master or client). Mirrors
# ntp_age_s_raw_from_pipe_json's raw-accessor shape above, kept unparsed so a caller can
# distinguish "absent" from "present but null" if it ever needs to.
ntp_deadband_us_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_deadband_us"[[:space:]]*:[[:space:]]*(null|-?[0-9]+)' \
    | sed -n 's/.*:[[:space:]]*\(null\|-\{0,1\}[0-9][0-9]*\).*/\1/p' \
    | tail -1 || true
}

# ntp_master_effective_bound_us STATUS_JSON BOUND_US MARGIN_US -> the bound to grade the NTP
# master node's MEDIAN against (#1021). When STATUS_JSON carries a valid non-negative numeric
# "ntp_deadband_us", the effective bound is max(BOUND_US, ntp_deadband_us + MARGIN_US) -- never
# LOWER than the caller's own BOUND_US (a widening FLOOR, never a ceiling override), and widened
# by MARGIN_US to cover a healthy step's OVERSHOOT past the deadband threshold before the next
# correction actually lands (dantesync polls at roughly a 10s cadence; at strih's live measured
# 38-66ppm oscillator error that is up to ~660us of extra ramp between polls -- a live capture
# topped out at ~487us overshoot on a 2500us deadband, i.e. offset 2987us; see #1021's design
# comment on the issue for the full derivation the default MARGIN_US=1000 is sized against).
#
# "null" / absent / non-numeric / negative ntp_deadband_us, OR a malformed BOUND_US/MARGIN_US
# (validated with `grep -qE '^[0-9]+$'` BEFORE any arithmetic -- the #595 bash gotcha: an
# unvalidated numeric fed into a `[ N -gt M ]` comparison can silently misbehave rather than
# error), falls back to the UNMODIFIED BOUND_US -- exact pre-#1021 behavior, unchanged. This is
# both the backward-compat path during a partial fleet rollout (a pre-dantesync-#84 master) AND
# the normal path for every non-master node -- this function is only ever called by
# dantesync-gate.sh's grade_http_node for the GATE_NTP_MASTER_NAME/"median-only" node; a client
# row's own bound is never touched regardless of what its payload happens to contain.
ntp_master_effective_bound_us() {
  local status="$1" bound="$2" margin="$3" step_cap="${4:-0}" deadband floor step_cap_floor
  if ! grep -qE '^[0-9]+$' <<<"$bound"; then
    printf '%s' "$bound"
    return 0
  fi
  if ! grep -qE '^[0-9]+$' <<<"$margin"; then
    printf '%s' "$bound"
    return 0
  fi
  deadband="$(ntp_deadband_us_from_pipe_json "$status")"
  if [ -z "$deadband" ] || [ "$deadband" = "null" ] \
     || ! grep -qE '^-?[0-9]+$' <<<"$deadband"; then
    printf '%s' "$bound"
    return 0
  fi
  if [ "$deadband" -lt 0 ]; then
    printf '%s' "$bound"
    return 0
  fi
  # #1022 review hardening: `$((...))` arithmetic expansion (unlike the `[ -gt/-lt ]` test
  # comparisons above/below, which stay decimal) treats a leading "0" as an OCTAL prefix -- a
  # validated-but-zero-padded deadband/margin like "0900" contains a digit (9) that is not valid
  # octal and aborts the WHOLE calling shell under set -e ("value too great for base") instead of
  # reaching the graceful fallback this function exists to provide. `10#` forces base-10 on both
  # operands (deadband is already proven non-negative by the check above, so this never needs to
  # handle a sign) -- a leading zero always means decimal to a human reading a CLI flag/JSON
  # number, never octal.
  floor=$((10#$deadband + 10#$margin))
  # #1119: dantesync v1.8.46 reports ntp_deadband_us as the no-step THRESHOLD (live: 1000us), NOT
  # the <=2500us bounded PER-STEP cap the master's own UTC offset actually sawtooths toward under a
  # slow grandmaster (root-caused 2026-08-18). deadband(1000)+margin(1000)=2000 gives NO widening,
  # so a healthy sawtooth median (live failed run: 2699us) false-DRIFTs the bare 2000us bound. When
  # STEP_CAP_US is a valid positive int, the floor ALSO includes step_cap + margin (2500+1000=3500),
  # grading the master's median against the step-cap ceiling instead of the too-small deadband
  # floor. Gated on the numeric deadband already validated above (the dantesync-#84+ bounded-step
  # regime marker): a pre-#84 master (no field) returned before here and keeps the bare bound,
  # preserving #1021's backward-compat tests -- and the step-cap term only ever bites when the
  # reported deadband is SMALLER than the step-cap (exactly the v1.8.46 regime). "0"/absent/
  # non-numeric STEP_CAP_US (every pre-#1119 3-arg caller) skips the term entirely, so the result
  # stays byte-for-byte the pre-#1119 deadband-only floor. Same #595 validate-before-arithmetic
  # + `10#` octal-safety discipline as the deadband/margin above.
  if grep -qE '^[0-9]+$' <<<"$step_cap" && [ "$step_cap" -gt 0 ]; then
    step_cap_floor=$((10#$step_cap + 10#$margin))
    [ "$step_cap_floor" -gt "$floor" ] && floor="$step_cap_floor"
  fi
  if [ "$floor" -gt "$bound" ]; then
    printf '%s' "$floor"
  else
    printf '%s' "$bound"
  fi
}

# ntp_steps_last_hour_from_pipe_json TEXT -> the RAW text of the "ntp_steps_last_hour" JSON value
# (#1119): a plain non-negative integer string, the literal "null" (a client node, which never
# acts as NTP master), or "" if the field is absent (a pre-storm-field dantesync payload). Mirrors
# ntp_deadband_us_from_pipe_json's raw-accessor shape -- used ONLY for the operator-facing storm
# line, never for a numeric decision, so it stays unparsed.
ntp_steps_last_hour_from_pipe_json() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_steps_last_hour"[[:space:]]*:[[:space:]]*(null|-?[0-9]+)' \
    | sed -n 's/.*:[[:space:]]*\(null\|-\{0,1\}[0-9][0-9]*\).*/\1/p' \
    | tail -1 || true
}

# ntp_master_step_storm_verdict STATUS_JSON -> "storm" | "ok" | "unknown" (#1119). The NTP master's
# own UTC offset is a by-design bounded-step sawtooth under a slow grandmaster (graded via the
# step-cap-widened median above), so the raw median alone can no longer distinguish a HEALTHY
# sawtooth from a THRASHING master whose median happens to land in-band. dantesync's own
# "ntp_step_storm" boolean -- true once it crosses its 120-steps/hour alarm -- IS that honest
# pathology signal, single-sourced from the daemon rather than re-derived gate-side:
#   * "true"           -> "storm" (a hard master failure regardless of median; the caller fails it)
#   * "false"          -> "ok"
#   * null / absent    -> "unknown" (a pre-storm-field payload, or a client node that never carries
#                         the field) -> NEVER a fail; report-first, exactly like #834's GM UNKNOWN.
# A malformed value that is neither true nor false is also "unknown" (test-strictness: never grade a
# signal we could not read as an affirmative storm).
ntp_master_step_storm_verdict() {
  local storm
  storm="$(printf '%s' "$1" \
    | grep -oE '"ntp_step_storm"[[:space:]]*:[[:space:]]*(true|false|null)' \
    | sed -n 's/.*:[[:space:]]*\(true\|false\|null\).*/\1/p' | tail -1 || true)"
  case "$storm" in
    true) printf 'storm' ;;
    false) printf 'ok' ;;
    *) printf 'unknown' ;;
  esac
}

# --- CLIENT-row deadband step-chase widening (#1022) ------------------------------------------
#
# #1021 (above) widens ONLY the NTP-MASTER row's own median bound. Live evidence filed on #1022
# (camera-box PR #1020, dantesync v1.8.41 fleet-wide) showed a CLIENT node can ALSO false-DRIFT
# during the SAME master step-chase window, via a DIFFERENT mechanism: a client always reports
# its OWN "ntp_deadband_us":null (the field only exists on the box acting as NTP master), yet a
# client's own ntp_offset_us legitimately mirrors the master's sawtooth via the LAN NTP
# measurement -- "when the master finally steps, every fleet client steps by the same amount
# within its next NTP measurement cycle" (issue 1022, supervisor comment). A 30s sample window
# landing during that per-client catch-up window can show a TIGHT (non-noise) but ELEVATED
# median that exceeds the fixed GATE_BOUND_US -- a false DRIFT, not a real desync (live: stream
# median 2589us, spread only 82us across 6 samples).
#
# client_chase_bound_us is the sibling of ntp_master_effective_bound_us, but it DELIBERATELY
# reads a DIFFERENT node's status (the caller passes in the MASTER's own /status, not the client
# being graded) and CAPS the deadband component at CEILING_US before adding MARGIN_US:
#
#   effective_client_bound = max(BOUND_US,
#                                 min(ntp_deadband_us, CEILING_US) + client_step_threshold_us + MARGIN_US)
#
# The cap is the one deliberate difference from #1021's own (uncapped) master formula: #1021
# only ever widens ONE row (the master's), so an unbounded floor has a small blast radius even
# if ntp_deadband_us were ever misreported. #1022 can widen MANY client rows from the SAME live
# read, so a hard ceiling (default 5000us -- the ticket's own cited "upstream hard per-step
# ceiling", the documented maximum size of any single master step) keeps that blast radius
# bounded no matter what the master happens to report.
#
# #1041: the ORIGINAL #1022 formula above omitted a SECOND, independent contributor -- what a
# client observes during a chase is the master's own deadband excursion PLUS the CLIENT's OWN
# adaptive NTP step threshold (the size of offset ITS OWN daemon tolerates before correcting).
# dantesync's controller.rs clamps that threshold to [500,10000]us and logs it verbatim as
# "... (threshold:NNNus, adaptive)" / "... step candidate ... (threshold:NNNus) ..." -- the exact
# shape documented at the top of this file -- and it is NOT exposed over the HTTP /status JSON
# (dantesync's SyncStatus only ever carries ntp_deadband_us, the MASTER's own field, always null
# on a client), so it can only be read from the CLIENT's own journal text via
# client_step_threshold_us_from_journal (below). client_chase_bound_us now takes two new
# TRAILING, backward-compatible params: CLIENT_JOURNAL (the specific client's own freshest
# journal text, default "" -- a pre-#1041 4-arg call site computes byte-identical to before) and
# STEP_FALLBACK_US (a conservative constant used when the journal has no threshold: match --
# unreachable node, pre-dantesync-#84 client, or a caller that simply never fetched one; default
# "0", so an OMITTED fallback also reproduces the exact pre-#1041 formula -- a REAL caller, e.g.
# dantesync-gate.sh, always passes a genuinely conservative non-zero value, see
# GATE_CLIENT_STEP_THRESHOLD_FALLBACK_US).

# client_chase_bound_us MASTER_STATUS_JSON BOUND_US MARGIN_US CEILING_US [CLIENT_JOURNAL]
#   [STEP_FALLBACK_US] -> the bound to grade a CLIENT node's median against (#1022/#1041).
# MASTER_STATUS_JSON is the CONFIGURED NTP master's own freshly-read /status
# (dantesync-gate.sh's read_master_chase_status, a priming read SEPARATE from the master's own
# gather_http_samples calls -- see that function's doc comment for why). CLIENT_JOURNAL is THIS
# SPECIFIC client's own freshest journal text (default ""); its LAST "threshold:NNNus" match
# (client_step_threshold_us_from_journal) is added as a THIRD term, falling back to
# STEP_FALLBACK_US (default "0") when the journal is empty or carries no match.
#
# "null" / absent / non-numeric / negative ntp_deadband_us in MASTER_STATUS_JSON, OR a malformed
# BOUND_US/MARGIN_US/CEILING_US (validated with `grep -qE '^[0-9]+$'` BEFORE any arithmetic, same
# #595 bash-gotcha discipline as ntp_master_effective_bound_us), falls back to the UNMODIFIED
# BOUND_US -- exactly like a pre-dantesync-#84 master, an unreachable master, or any caller that
# simply never derived a live envelope. Never a blind widen: without a live, numeric, non-
# negative master deadband to derive from, the bound never moves -- and the client step-threshold
# term NEVER applies on its own either (there is no "chase" concept at all without a real master
# deadband to chase).

# client_step_threshold_us_from_journal TEXT -> the client's own CURRENTLY-active adaptive NTP
# step threshold: the integer value of the LAST "threshold:[0-9]+us" match anywhere in TEXT ("" if
# none). Matches BOTH literal shapes dantesync's controller.rs emits ("[NTP] offset:+300us
# (threshold:520us, adaptive)" and "[NTP] step candidate +2701us (threshold:665us)") -- the same
# "freshest = last match wins" convention offset_us_from_journal already uses above. This is a
# per-daemon CONFIG value (the currently-active adaptive threshold), not a drifting live
# measurement, so no separate freshness/age check is needed -- reading it from whatever journal
# window the caller already fetched is enough. `|| true` survives a no-match under set -e.
client_step_threshold_us_from_journal() {
  printf '%s\n' "$1" \
    | grep -oE 'threshold:[0-9]+us' \
    | sed -n 's/.*threshold:\([0-9][0-9]*\)us/\1/p' \
    | tail -1 || true
}
client_chase_bound_us() {
  local status="$1" bound="$2" margin="$3" ceiling="$4"
  local client_journal="${5:-}" step_fallback="${6:-0}"
  local deadband capped step floor
  if ! grep -qE '^[0-9]+$' <<<"$bound"; then
    printf '%s' "$bound"
    return 0
  fi
  if ! grep -qE '^[0-9]+$' <<<"$margin"; then
    printf '%s' "$bound"
    return 0
  fi
  if ! grep -qE '^[0-9]+$' <<<"$ceiling"; then
    printf '%s' "$bound"
    return 0
  fi
  deadband="$(ntp_deadband_us_from_pipe_json "$status")"
  if [ -z "$deadband" ] || [ "$deadband" = "null" ] \
     || ! grep -qE '^-?[0-9]+$' <<<"$deadband"; then
    printf '%s' "$bound"
    return 0
  fi
  if [ "$deadband" -lt 0 ]; then
    printf '%s' "$bound"
    return 0
  fi
  capped="$deadband"
  [ "$capped" -gt "$ceiling" ] && capped="$ceiling"
  # #1041: the client's own real adaptive step threshold, parsed from ITS OWN journal text; an
  # empty/no-match journal (or a malformed STEP_FALLBACK_US) falls back to "0" -- never a guess,
  # and never a crash. A pre-#1041 4-arg caller passes neither param, so client_journal="" and
  # step_fallback="0" here reproduce the exact pre-#1041 formula byte-for-byte.
  step="$(client_step_threshold_us_from_journal "$client_journal")"
  if [ -z "$step" ] || ! grep -qE '^[0-9]+$' <<<"$step"; then
    if grep -qE '^[0-9]+$' <<<"$step_fallback"; then
      step="$step_fallback"
    else
      step=0
    fi
  fi
  # #1022 review hardening -- same octal-prefix hazard as ntp_master_effective_bound_us above:
  # `10#` forces base-10 on all three operands (capped is already proven non-negative -- it is
  # either deadband, already checked >= 0, or ceiling, already validated `^[0-9]+$`; step is
  # either the parsed threshold, already validated `^[0-9]+$`, or the validated step_fallback, or
  # the literal "0" -- so none of the three ever needs sign handling) before the ONE arithmetic
  # expression this function contains.
  floor=$((10#$capped + 10#$step + 10#$margin))
  if [ "$floor" -gt "$bound" ]; then
    printf '%s' "$floor"
  else
    printf '%s' "$bound"
  fi
}

# --- CLIENT-row step-aware STABILITY (spread) widening (#1123) --------------------------------
#
# #1022/#1041 (client_chase_bound_us, above) made a CLIENT row's MEDIAN bound step-aware, but the
# STABILITY (spread) bound stayed fixed at GATE_STABILITY_US. A client chases the master's by-design
# UTC sawtooth with its OWN bounded steps; when a step lands mid-sample-window the samples straddle
# it, so the SPREAD ~= the client's step MAGNITUDE (live cam1 2026-08-19: 2938us, == its own
# "[NTP] Stepped +2938us"), which false-reads as #836 scatter against the fixed 2000us stability
# even though every sample is inside the (widened) median bound.
#
# The honest bound on a step-straddle SPREAD is the client's OWN journal step envelope. Unlike the
# MEDIAN widening -- which reads the tail-1 (freshest) adaptive threshold because the median is a
# point estimate of the CURRENT offset -- the SPREAD is a WINDOW-WIDE range, so it references the
# WINDOW-WIDE MAX of the client's own adaptive tolerance: client_max_step_threshold_us_from_journal
# (below) parses EVERY "threshold:NNNus" match and returns the LARGEST, i.e. the biggest offset its
# own daemon tolerated before stepping anywhere in the read window. A spread within that envelope is
# the client operating inside its own adaptive tolerance; a spread FAR beyond it (with no
# correspondingly-large threshold in its journal) is genuine scatter and still fails. A genuine
# gross desync fails on the MEDIAN (drift), never reaching this spread question.

# client_max_step_threshold_us_from_journal TEXT -> the LARGEST "threshold:[0-9]+us" value anywhere
# in TEXT ("" if none). The window-MAX sibling of client_step_threshold_us_from_journal's tail-1:
# same literal shapes (controller.rs "(threshold:520us, adaptive)" / "step candidate ...
# (threshold:665us)"), but `sort -n | tail -1` for the max instead of the freshest. `|| true`
# survives a no-match under set -e.
client_max_step_threshold_us_from_journal() {
  printf '%s\n' "$1" \
    | grep -oE 'threshold:[0-9]+us' \
    | sed -n 's/.*threshold:\([0-9][0-9]*\)us/\1/p' \
    | sort -n | tail -1 || true
}

# client_max_step_threshold_us_from_status_lines TEXT -> the LARGEST numeric "ntp_step_threshold_us"
# JSON value across ALL lines of TEXT (#1129). TEXT is the newline-joined HTTP `/status` payloads
# gather_http_samples already collected over the sampling window (one JSON object per line). This is
# the WINDOWS-client sibling of client_max_step_threshold_us_from_journal: a Windows client has no
# journald, so grade_http_node cannot read its "threshold:NNNus" log lines -- but dantesync (#1129)
# exposes the client's OWN currently-active adaptive step threshold in `/status` as
# `ntp_step_threshold_us` (the SAME quantity, `calculate_ntp_adaptive_threshold()`, the journal logs
# as "threshold:NNNus"). Taking the window-MAX across the sampled payloads mirrors the journal path's
# window-wide envelope exactly, so a Windows client gets the same step-aware median+spread widening
# cam2 gets from its journal. A `null` or ABSENT field on any line contributes nothing (the regex
# matches only a digit run, never "null"); all-null / all-absent / empty input -> "" (never a guess
# -> the caller's documented 700us fallback, always admitted in the gate note). `|| true` survives a
# no-match under set -e.
client_max_step_threshold_us_from_status_lines() {
  printf '%s\n' "$1" \
    | grep -oE '"ntp_step_threshold_us"[[:space:]]*:[[:space:]]*[0-9]+' \
    | sed -n 's/.*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    | sort -n | tail -1 || true
}

# client_chase_stability_us STABILITY_US MARGIN_US CLIENT_JOURNAL [STEP_FALLBACK_US] -> the spread
# bound to grade a CLIENT node's SPREAD against (#1123): max(STABILITY_US, max_journal_threshold +
# MARGIN_US), where max_journal_threshold is client_max_step_threshold_us_from_journal(CLIENT_JOURNAL),
# falling back to STEP_FALLBACK_US (default "0", so an OMITTED fallback reproduces the exact pre-#1123
# fixed bound -- byte-for-byte for every unreachable/no-threshold client) when the journal is empty or
# carries no threshold. NEVER LOWER than STABILITY_US (a widening FLOOR, never a ceiling override). A
# malformed STABILITY_US/MARGIN_US (validated with `grep -qE '^[0-9]+$'` BEFORE any arithmetic -- the
# #595 discipline) falls back to the UNMODIFIED STABILITY_US. Mirrors client_chase_bound_us's own
# shape/fallbacks; deliberately takes NO master status -- the client's own step envelope already
# reflects the full offset it chases (the master's excursion is baked into the client's own adaptive
# threshold), so no separate master-deadband term is needed for the spread.
client_chase_stability_us() {
  local stability="$1" margin="$2" client_journal="${3:-}" step_fallback="${4:-0}"
  local max_threshold step floor
  if ! grep -qE '^[0-9]+$' <<<"$stability"; then
    printf '%s' "$stability"
    return 0
  fi
  if ! grep -qE '^[0-9]+$' <<<"$margin"; then
    printf '%s' "$stability"
    return 0
  fi
  max_threshold="$(client_max_step_threshold_us_from_journal "$client_journal")"
  step="$max_threshold"
  if [ -z "$step" ] || ! grep -qE '^[0-9]+$' <<<"$step"; then
    if grep -qE '^[0-9]+$' <<<"$step_fallback"; then
      step="$step_fallback"
    else
      step=0
    fi
  fi
  # #595/#1022 octal-safety: `10#` forces base-10 on both operands (both proven `^[0-9]+$`
  # non-negative above) before the ONE arithmetic expression.
  floor=$((10#$step + 10#$margin))
  if [ "$floor" -gt "$stability" ]; then
    printf '%s' "$floor"
  else
    printf '%s' "$stability"
  fi
}

# offset_verdict_check LABEL JOURNAL FRESHNESS_S BOUND_US [STABILITY_US] -> prints a status line; returns 0 OK /
# 2 DRIFT / 3 UNKNOWN. The freshness-aware sibling of offset_check (#550/#591/#595/#607): it grades
# dantesync_offset_verdict's "ok"/"drift"/"stale"/"absent" verdict instead of pairing the age-blind
# offset_us_from_journal (tail -1) with offset_check the way this CLI's own main() loop did before
# #607 -- dantesync-gate.sh (#7) already made this same switch for its own Linux-node loop. A
# "stale" or "absent" verdict maps to UNKNOWN (3), NEVER a silent OK/DRIFT — a reading we could not
# prove fresh must never be graded at all (test-strictness: no silent pass on a possibly-stale
# value). STABILITY_US (#837, optional) is passed straight through to dantesync_offset_verdict --
# when present, a scattered-but-in-bound-median node grades "unstable"/"drift_unstable", both
# returning 2 (the same hard-fail class as drift). The spread value is reported on the line so a
# red says WHICH kind of bad it is (#836 point 4). JOURNAL must be gathered with `-o short-iso`
# (see query_node_journal).
offset_verdict_check() {
  local label="$1" journal="$2" fresh="$3" bound="$4" stability="${5:-}"
  local spread
  spread="$(_fresh_offset_spread_us "$journal" "$fresh" 11)"
  case "$(dantesync_offset_verdict "$journal" "$fresh" "$bound" "$stability")" in
    ok)
      printf '  %-14s OK       (fresh offset within %s us bound; spread %s us, stability %s us)\n' \
        "$label" "$bound" "${spread:-NA}" "${stability:-NA}"
      return 0 ;;
    drift)
      printf '  %-14s DRIFT    (fresh offset exceeds %s us bound; spread %s us)\n' \
        "$label" "$bound" "${spread:-NA}"
      return 2 ;;
    unstable)
      printf '  %-14s UNSTABLE (fresh offset within %s us bound but spread %s us > %s us stability -- #837 scattered/unusable)\n' \
        "$label" "$bound" "${spread:-NA}" "${stability:-NA}"
      return 2 ;;
    drift_unstable)
      printf '  %-14s DRIFT+UNSTABLE (fresh offset exceeds %s us bound; spread %s us > %s us stability -- #837)\n' \
        "$label" "$bound" "${spread:-NA}" "${stability:-NA}"
      return 2 ;;
    stale)
      printf '  %-14s UNKNOWN  (no FRESH [NTP] offset within %ss -- status incomplete, #550/#595/#607)\n' \
        "$label" "$fresh"
      return 3 ;;
    *)
      printf '  %-14s UNKNOWN  (no [NTP] offset line at all -- status incomplete)\n' "$label"
      return 3 ;;
  esac
}

# painter_offset_check LABEL DEV1_OFFSET_US PAINTER_OFFSET_US GUARD_US -> prints a status line;
# returns 0 OK / 2 DRIFT / 3 UNKNOWN. This is the #326 all-cambox-sweep comparator: the sweep
# stamps each program-switch window boundary on dev1's CLOCK_REALTIME, while the painted ticks
# (and the burns the verdict keys on) ride the painter (cam2) DanteSync clock. Both dev1 and the
# painter are DanteSync-slaved to the SAME NTP master (strih), so each reports its own absolute
# "[NTP] offset:+Nus". The dev1<->painter RELATIVE offset = |DEV1_OFFSET_US - PAINTER_OFFSET_US|
# on that shared basis; OK iff that relative offset <= GUARD_US (NUMERIC compare). If it exceeds
# the guard, window boundaries misalign with the painted-tick timeline by more than the verdict's
# ±guard discard zone -> frames get attributed to the WRONG cambox window (silent #312
# mis-attribution). An EMPTY or non-numeric EITHER offset is UNKNOWN (rc 3), never OK — an offset
# we could not read must never look in-bound (test-strictness: no silent pass).
painter_offset_check() {
  local label="$1" dev1="$2" painter="$3" guard="$4" rel mag
  if [ -z "$dev1" ] || [ -z "$painter" ]; then
    printf '  %-18s UNKNOWN  (offset unread: dev1=%s painter=%s; guard %s us)\n' \
      "$label" "${dev1:-<none>}" "${painter:-<none>}" "$guard"
    return 3
  fi
  if ! grep -qE '^[+-]?[0-9]+$' <<<"$dev1" \
     || ! grep -qE '^[+-]?[0-9]+$' <<<"$painter"; then
    printf '  %-18s UNKNOWN  (malformed offset: dev1=%s painter=%s; guard %s us)\n' \
      "$label" "$dev1" "$painter" "$guard"
    return 3
  fi
  rel=$(( dev1 - painter ))          # both are plain signed ints (offset_us_from_journal strips '+')
  mag="$(abs_int "$rel")"
  if [ "$mag" -le "$guard" ]; then
    printf '  %-18s OK       (dev1 %s us, painter %s us, |Δ|=%s <= %s)\n' \
      "$label" "$dev1" "$painter" "$mag" "$guard"
    return 0
  fi
  printf '  %-18s DRIFT    (dev1 %s us, painter %s us, |Δ|=%s > %s us guard)\n' \
    "$label" "$dev1" "$painter" "$mag" "$guard"
  return 2
}


# --- Slew-transient CLIENT exclusion via journal step-correlation (#1055) --------------------
#
# The gate samples a CLIENT via HTTP `/status` and grades the MEDIAN of the sampled ntp_offset_us.
# When the NTP master (strih) exits its ~2.5 ms deadband and steps, every fleet client observes a
# +2.7-3.3 ms slew TRANSIENT lasting ~30-60 s until its own daemon re-chases (captured LIVE
# 2026-08-14 on cam1/cam2). A 30 s HTTP window landing in that plateau reads a majority-elevated
# set -> the median IS a spike -> a false DRIFT of a us-healthy fleet. The #1022/#1041 CLIENT
# widening (client_chase_bound_us) covers this ONLY when the MASTER's own /status is readable at
# gate-prime time (it derives the widened bound from the master's ntp_deadband_us); when that
# Windows-box HTTP read momentarily fails during a live E2E run (the ~50 % intermittency), the
# client is graded against the bare bound and false-DRIFTs, and chase_bimodal_exclusion_verdict
# cannot help (its #1041 finding proves it only ever explains a SPREAD-side "unstable" verdict,
# never a MEDIAN-out-of-bound one).
#
# These functions are the EVIDENCE-based, master-independent rescue. The client's OWN journal
# co-timestamps every slew-transient "[NTP] offset:" sample with a "[NTP] step candidate" /
# "[NTP] Stepped" correction marker; a genuine sustained desync is NOT so bracketed (its
# step-EXCLUDED steady-state stays elevated, or the daemon steps every cycle and nothing survives).
# So excluding samples adjacent to a correction marker and grading the surviving BASELINE median is
# the honest discriminator (never a blind bound raise): a transient slew passes, a real desync
# fails. LINUX clients only -- the journal is the evidence source; the master row and Windows
# clients are untouched.

# slew_excluded_survivors_us JOURNAL FRESHNESS_S STEP_WINDOW_S [K] -> newline list of the SIGNED
# microsecond values of the fresh "[NTP] offset:" samples among the K (default 11) most recent
# whose OWN `-o short-iso` timestamp is NOT within STEP_WINDOW_S seconds of ANY "[NTP] (Stepped|
# step candidate)" correction marker anywhere in JOURNAL; empty when none survive or an input is
# malformed. Freshness is checked PER SAMPLE against the newest journal line -- the SAME #550/#595
# discipline (and the same numeric-input guard) as _fresh_offset_median_us above, so a died/hung
# daemon's stale line is never graded as current. The burst-summary "[NTP] burst offset:" lines are
# NOT matched by "\[NTP\] offset:" (they read "[NTP] burst offset:") and are correctly ignored,
# exactly as _fresh_offset_median_us ignores them. JOURNAL must be gathered with `-o short-iso`.
# The optional 5th arg MIN_EPOCH (#1055 review, the onset-drift hole below) restricts the output to
# samples strictly NEWER than MIN_EPOCH -- used by the verdict to grade ONLY the post-most-recent-
# correction samples, so stale pre-onset baseline cannot dilute an ongoing desync's median. Omitted
# / "" -> no recency floor (every step-excluded fresh sample, the count the reporter + window-sanity
# check use). A malformed MIN_EPOCH yields NOTHING (fail-closed), never an unfiltered pass.
slew_excluded_survivors_us() {
  local journal="$1" fresh="$2" win="$3" k="${4:-11}" min_epoch="${5:-}"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local now_iso now_e lines line off_iso off_e off_us se d near
  local -a steps=()
  # #595 bash-gotcha: validate every numeric input BEFORE any arithmetic/`-le` comparison; a
  # malformed value would otherwise throw "integer expression expected" (a FAILED test) and could
  # silently defeat the exclusion. A bad input yields NOTHING -- never a guessed survivor set.
  grep -qE '^[0-9]+$' <<<"$fresh" || return 0
  grep -qE '^[0-9]+$' <<<"$win" || return 0
  grep -qE '^[0-9]+$' <<<"$k" || return 0
  if [ -n "$min_epoch" ]; then
    grep -qE '^-?[0-9]+$' <<<"$min_epoch" || return 0
  fi
  now_iso="$(printf '%s\n' "$journal" | grep -oE "^$iso_re" | tail -1 || true)"
  now_e="$(_short_iso_epoch "$now_iso")"
  [ -n "$now_e" ] || return 0
  # Correction-event epochs: each "[NTP] Stepped" / "[NTP] step candidate" line's own timestamp.
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    off_iso="$(printf '%s' "$line" | grep -oE "^$iso_re" | head -1 || true)"
    off_e="$(_short_iso_epoch "$off_iso")"
    [ -n "$off_e" ] || continue
    steps+=("$off_e")
  done <<< "$(printf '%s\n' "$journal" | grep -E '\[NTP\] (Stepped|step candidate)' || true)"
  # The K most recent "[NTP] offset:" samples.
  lines="$(printf '%s\n' "$journal" | grep -E '\[NTP\] offset:' | tail -n "$k" || true)"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    off_iso="$(printf '%s' "$line" | grep -oE "^$iso_re" | head -1 || true)"
    off_e="$(_short_iso_epoch "$off_iso")"
    [ -n "$off_e" ] || continue
    [ "$((now_e - off_e))" -le "$fresh" ] || continue
    [ -z "$min_epoch" ] || [ "$off_e" -gt "$min_epoch" ] || continue
    off_us="$(printf '%s' "$line" | sed -n 's/.*\[NTP\] offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us.*/\1/p' | head -1 || true)"
    grep -qE '^-?[0-9]+$' <<<"$off_us" || continue
    near=0
    for se in ${steps[@]+"${steps[@]}"}; do
      d=$(( off_e - se ))
      [ "$d" -lt 0 ] && d=$(( -d ))
      if [ "$d" -le "$win" ]; then near=1; break; fi
    done
    [ "$near" -eq 0 ] && printf '%s\n' "$off_us"
  done <<< "$lines"
  # A conditionally-skipped `[ ] && printf` as the loop's last statement can leave a non-zero exit
  # under set -e when the final iteration was excluded -- mirror chase_bimodal_partition_us's own
  # explicit terminator so command-substitution callers never abort on it.
  return 0
}

# _newest_correction_epoch JOURNAL -> the epoch (seconds) of the NEWEST "[NTP] (Stepped|step
# candidate)" correction marker line in JOURNAL, "" if there is none or none carries a parseable
# `-o short-iso` timestamp. This is the recency anchor for the verdict's onset-drift guard below.
_newest_correction_epoch() {
  local journal="$1"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local line off_iso off_e newest=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    off_iso="$(printf '%s' "$line" | grep -oE "^$iso_re" | head -1 || true)"
    off_e="$(_short_iso_epoch "$off_iso")"
    [ -n "$off_e" ] || continue
    if [ -z "$newest" ] || [ "$off_e" -gt "$newest" ]; then
      newest="$off_e"
    fi
  done <<< "$(printf '%s\n' "$journal" | grep -E '\[NTP\] (Stepped|step candidate)' || true)"
  printf '%s' "$newest"
}

# slew_transient_exclusion_verdict JOURNAL FRESHNESS_S BOUND_US STEP_WINDOW_S MIN_SURVIVING [K] ->
# "yes" | "no". "yes" ONLY when ALL hold:
#   1. JOURNAL carries >= 1 "[NTP] (Stepped|step candidate)" correction marker (EVIDENCE a slew is
#      actively being corrected -- without it, elevated samples are unexplained, never excused).
#   2. Excluding fresh "[NTP] offset:" samples within STEP_WINDOW_S of any marker leaves
#      >= MIN_SURVIVING fresh samples overall (window sanity: too few baseline anywhere in the
#      window can't prove health -> "no", never a silent pass).
#   3. RECENCY (the #1055 review onset-drift guard): among the step-excluded survivors, restrict
#      to those NEWER than the newest correction marker -- the samples that testify to the clock's
#      state AFTER its most recent correction. At least ONE must exist (proof the clock returned to
#      and held a reading post-correction), and the MEDIAN of that post-correction set must be
#      within BOUND_US.
# Condition 3 is what makes the discriminator honest at drift ONSET: a transient slew RETURNS to a
# us-grade baseline after its step (post-correction survivors are baseline -> "yes"); a genuine
# desync that just onset (drift samples all step-adjacent -> excluded) leaves ZERO post-correction
# survivors (or post-correction survivors that are themselves still elevated) -> "no", so
# pre-onset healthy history in the ~K-sample window can no longer dilute an ongoing drift's median
# (the live-reproduced hole condition 2 alone missed). A sustained desync stepping every cycle
# still fails on 0 survivors; one with no markers fails cond 1. Mirrors
# chase_bimodal_exclusion_verdict's yes/no contract; that function explains the SPREAD-side
# "unstable" (median-in-bound) verdict, this one the MEDIAN-out-of-bound "drift"/"drift_unstable"
# case it structurally cannot (#1041). JOURNAL must be `-o short-iso`.
slew_transient_exclusion_verdict() {
  local journal="$1" fresh="$2" bound="$3" win="$4" min_surv="$5" k="${6:-11}"
  local survivors n newest_step recent n_recent median
  grep -qE '^[0-9]+$' <<<"$bound" || { printf 'no\n'; return 0; }
  grep -qE '^[0-9]+$' <<<"$min_surv" || { printf 'no\n'; return 0; }
  # Condition 1: evidence of an active correction (fresh/win/k are validated inside
  # slew_excluded_survivors_us, where a bad one simply yields no survivors -> "no").
  grep -qE '\[NTP\] (Stepped|step candidate)' <<<"$journal" \
    || { printf 'no\n'; return 0; }
  # Condition 2: window sanity -- enough step-excluded baseline SOMEWHERE in the window.
  survivors="$(slew_excluded_survivors_us "$journal" "$fresh" "$win" "$k")"
  n="$(printf '%s\n' "$survivors" | grep -cE '^-?[0-9]+$' || true)"
  [ "${n:-0}" -ge "$min_surv" ] || { printf 'no\n'; return 0; }
  # Condition 3: recency -- grade ONLY the survivors newer than the newest correction marker.
  newest_step="$(_newest_correction_epoch "$journal")"
  [ -n "$newest_step" ] || { printf 'no\n'; return 0; }
  recent="$(slew_excluded_survivors_us "$journal" "$fresh" "$win" "$k" "$newest_step")"
  n_recent="$(printf '%s\n' "$recent" | grep -cE '^-?[0-9]+$' || true)"
  [ "${n_recent:-0}" -ge 1 ] || { printf 'no\n'; return 0; }
  median="$(median_of_ints "$recent")"
  [ -n "$median" ] || { printf 'no\n'; return 0; }
  if [ "$(abs_int "$median")" -le "$bound" ]; then
    printf 'yes\n'
  else
    printf 'no\n'
  fi
}

# slew_transient_exclusion_check LABEL JOURNAL BOUND_US STEP_WINDOW_S MIN_SURVIVING FRESHNESS_S
#   [EXTRA_NOTE] -> prints ONE "OK" status line (surviving-baseline count + surviving median) with
# a note explaining the step-correlated master-slew exclusion, and ALWAYS returns 0. The caller
# must have already confirmed slew_transient_exclusion_verdict(...) == "yes" for the SAME inputs --
# this does not re-decide, it only formats + reports (mirrors chase_bimodal_exclusion_check's own
# compute-the-display-numbers-separately-from-the-decision shape).
slew_transient_exclusion_check() {
  local label="$1" journal="$2" bound="$3" win="$4" min_surv="$5" fresh="$6" extra_note="${7:-}"
  local newest_step recent n median
  # Report the SAME post-correction survivor set the verdict graded (its condition 3), so the
  # printed count/median match the decision -- never a wider set that could read differently.
  newest_step="$(_newest_correction_epoch "$journal")"
  recent="$(slew_excluded_survivors_us "$journal" "$fresh" "$win" 11 "$newest_step")"
  n="$(printf '%s\n' "$recent" | grep -cE '^-?[0-9]+$' || true)"
  n="${n:-0}"
  median="$(median_of_ints "$recent")"
  printf '  %-14s OK       (step-correlated master-slew transients excluded; clock returned to us-grade AFTER its last correction -- %s post-correction baseline samples, median %sus <= %sus bound; samples within %ss of a step ignored, #1055)%s\n' \
    "$label" "$n" "${median:-NA}" "$bound" "$win" "$extra_note"
  return 0
}
# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------

usage() {
  cat <<EOF
clock-offset-guard.sh — cluster clock-offset regression guard (#8).

Queries each REACHABLE Linux node's DanteSync-reported absolute clock offset (the periodic
"[NTP] offset:+Nus" journald line) and FAILS LOUDLY if any node's |offset| exceeds the bound,
so clock drift cannot silently re-break the wall-clock genlock in src/ndi.rs.

Usage:
  scripts/clock-offset-guard.sh [--bound-us N] [--stability-us N] [--targets "name=ip name=ip ..."]
  scripts/clock-offset-guard.sh --help

Default bound: ${DEFAULT_BOUND_US} us (2 ms) — ~8x under the 16.7 ms 60 fps frame period yet
above the observed steady-state offsets (cam ~300 us, strih master ~1249 us). See SETUP.md
"Cluster clock synchronization" for the baseline + rationale.

Default stability (spread) bound: ${DEFAULT_STABILITY_US} us (--stability-us / DANTESYNC_STABILITY_US).
A node whose FRESH offset samples scatter by more than this (max-min) grades UNSTABLE even
when the median is in-bound (#837, the journal twin of the #836 HTTP spread check).

Default targets (Linux cameras, over SSH): ${CLOCK_GUARD_TARGETS}
The Windows OBS boxes (strih/stream) are checked read-only via the win-* MCP tools using this
script's shared offset_us_from_pipe_json parser.

A node's offset must also be FRESH, not just in-bound: the freshest "[NTP] offset:" journal line
must be no older than DANTESYNC_OFFSET_FRESHNESS_S (default ${CLOCK_GUARD_OFFSET_FRESHNESS_S})
seconds behind that node's own newest journal line, or the reading is STALE -> UNKNOWN (never a
silent pass, #550/#591/#595/#607).

Exit codes: 0 = all reachable nodes within bound + stable, 20 = DRIFT or UNSTABLE (a node exceeds
the bound or its samples scatter past --stability-us),
11 = a node UNREACHABLE / offset UNKNOWN (incomplete, NOT clean), 1 = usage/IO error.
EOF
}

# query_node_journal NAME IP -> echoes the node's latest DanteSync journald lines, or returns
# nonzero if the node is unreachable / the daemon has no output. Read-only (journalctl). Requires
# sshpass; an unreachable node is reported by the caller as UNKNOWN, never a silent pass.
#
# Gathered with `-o short-iso` (+ a wider -n 400 window, #607) so dantesync_offset_verdict can
# prove the reading is FRESH (#550/#591/#595) -- the same window this file's other two callers
# (dantesync-gate.sh #7, clock-offset-painter-gate.sh #326) already use.
#
# Overridable for tests/offline via CLOCK_GUARD_JOURNAL_OVERRIDE=<file> (mirrors the
# DEV1_DANTE_JOURNAL/PAINTER_DANTE_JOURNAL override convention in clock-offset-painter-gate.sh) --
# every target reads the SAME override file, which is only ever exercised with a single target in
# tests. Pure test seam: when unset (the live/default case) behavior is unchanged (#607).
query_node_journal() {
  local ip="$2"   # $1 (name) is the caller's label; the query only needs the IP.
  if [ -n "${CLOCK_GUARD_JOURNAL_OVERRIDE:-}" ]; then
    cat "$CLOCK_GUARD_JOURNAL_OVERRIDE" 2>/dev/null || true
    return 0
  fi
  # BatchMode=no so sshpass can feed the password (BatchMode would disable password auth). Both
  # the remote journalctl and the local ssh suppress stderr: an auth failure and a down host are
  # DELIBERATELY collapsed to "empty output" here — the caller maps empty -> UNKNOWN (never a
  # silent pass), so the guard fails loudly either way without leaking SSH banners into the report.
  sshpass -p "$CLOCK_GUARD_SSH_PASS" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CLOCK_GUARD_SSH_TIMEOUT}" \
    "${CLOCK_GUARD_SSH_USER}@${ip}" \
    'journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null' 2>/dev/null
}

main() {
  local bound="$DEFAULT_BOUND_US" targets="$CLOCK_GUARD_TARGETS" stability="$DEFAULT_STABILITY_US"
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us) shift; bound="${1:-}" ;;
      --stability-us) shift; stability="${1:-}" ;;
      --targets)  shift; targets="${1:-}" ;;
      -h|--help)  usage; exit 0 ;;
      --*)        echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)          echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if ! grep -qE '^[0-9]+$' <<<"$bound"; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
    exit 1
  fi
  if ! grep -qE '^[0-9]+$' <<<"$stability"; then
    echo "ERROR: --stability-us must be a non-negative integer (got '${stability}')." >&2
    exit 1
  fi

  # Normalise the target list and fail LOUDLY if it is empty — a guard that checks nothing must
  # never report "all clear" (test-strictness: a check that can't run fails, not passes). Split on
  # whitespace with globbing DISABLED (set -f): a target token containing a shell glob char
  # (`*`/`?`/`[`) must NOT be expanded against the cwd into a bogus node name.
  local -a pairs=()
  set -f
  # shellcheck disable=SC2206
  pairs=($targets)
  set +f
  if [ "${#pairs[@]}" -eq 0 ]; then
    echo "ERROR: no targets to check (CLOCK_GUARD_TARGETS / --targets is empty)." >&2
    echo "The clock-offset guard cannot certify the cluster with zero nodes — refusing to pass." >&2
    exit 1
  fi

  # sshpass is only needed for the LIVE SSH read; CLOCK_GUARD_JOURNAL_OVERRIDE (tests/offline)
  # skips it entirely (mirrors clock-offset-painter-gate.sh's PAINTER_DANTE_JOURNAL check, #607).
  if [ -z "${CLOCK_GUARD_JOURNAL_OVERRIDE:-}" ] && ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to query the camera DanteSync offset over SSH." >&2
    exit 1
  fi

  echo "== clock-offset-guard (#8): bound ${bound} us (|offset| must stay within) =="
  echo "   master = strih (DanteSync NTP anchor + PTP servo); frame period @60fps = 16667 us"

  local freshness="$CLOCK_GUARD_OFFSET_FRESHNESS_S"
  local drift=0 unknown=0 ok=0 rc pair name ip journal
  for pair in "${pairs[@]}"; do
    name="${pair%%=*}"; ip="${pair#*=}"
    journal="$(query_node_journal "$name" "$ip" || true)"
    if [ -z "$journal" ]; then
      printf '  %-14s UNREACHABLE  (no DanteSync journal over SSH @ %s)\n' "$name" "$ip"
      unknown=$((unknown + 1))
      continue
    fi
    # #607: grade the FRESHEST "[NTP] offset:" reading (never the age-blind tail -1 offset_check
    # this loop used before) so a stale multi-hour-old boot-STEP line -- or a died/hung dantesync
    # whose journal simply stopped advancing -- can never be certified as the node's current
    # offset (the #550/#595 false-pass this CLI's own flow was still exposed to).
    rc=0
    offset_verdict_check "$name" "$journal" "$freshness" "$bound" "$stability" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      2) drift=$((drift + 1)) ;;
      3) unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$drift" -gt 0 ]; then
    echo "!! CLOCK DRIFT/UNSTABLE: ${drift} node(s) exceed the ${bound} us offset bound or scatter past ${stability} us." >&2
    echo "!! Genlock boundaries diverge — investigate DanteSync on the affected node(s)." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further node(s) UNREACHABLE/UNKNOWN — status also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! ${unknown} node(s) UNREACHABLE or offset UNKNOWN — cluster status INCOMPLETE, NOT clean." >&2
    echo "!! Power up / fix DanteSync on those nodes, then re-run. (${ok} node(s) were within bound.)" >&2
    exit 11
  fi
  echo "ALL CLEAR — ${ok} node(s) within the ${bound} us offset bound. Genlock clock assumption holds."
  exit 0
}

main "$@"
