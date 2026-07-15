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
#   scripts/clock-offset-guard.sh [--bound-us N] [--targets "cam1=10.77.9.61 ..."]
#   scripts/clock-offset-guard.sh --help
#
# Exit codes: 0 = all reachable nodes within bound, 20 = DRIFT (a node exceeds the bound),
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
  if ! printf '%s' "$now" | grep -qE '^[0-9]+$' || ! printf '%s' "$fresh" | grep -qE '^[0-9]+$'; then
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
  if ! printf '%s' "$fresh" | grep -qE '^[0-9]+$'; then
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
  if [ -z "$off_us" ] || ! printf '%s' "$off_us" | grep -qE '^-?[0-9]+$'; then
    printf ''
    return 0
  fi
  printf '%s' "$off_us"
}

# _fresh_offset_median_us JOURNAL FRESHNESS_S [K] -> the lower MEDIAN of the individually-FRESH
# samples among the K (default 5) most recent "[NTP] offset:" lines; "" when none is fresh and
# parseable. #767-era measurement-noise rejection (live, 2026-07-15): under E2E network load a
# SINGLE [NTP] offset sample spikes to ~2-3ms (cam5 -2787us, cam7 -2316us) while PTP stays
# NANO-locked and the surrounding samples read tens of us -- the CLOCK is fine, the one
# measurement is noisy, and a verdict graded on the single freshest sample flakes exactly when
# an E2E run loads the LAN. The median across the recent fresh samples rejects a lone spike
# while a SUSTAINED out-of-bound offset (the real cam5/6 5.28s class) still lands every sample
# out of bound -> median out of bound -> drift. Freshness is checked PER SAMPLE against the
# newest journal line, same knob + same fail-closed non-numeric handling as freshest_offset_us.
# (freshest_offset_us itself is left single-sample: its #326 painter-gate caller compares two
# boxes' raw values relatively and owns its own tolerance.)
_fresh_offset_median_us() {
  local journal="$1" fresh="$2" k="${3:-5}"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local now_iso now_e lines line off_iso off_e off_us
  local -a vals=()
  if ! printf '%s' "$fresh" | grep -qE '^[0-9]+$'; then
    printf ''
    return 0
  fi
  now_iso="$(printf '%s\n' "$journal" | grep -oE "^$iso_re" | tail -1 || true)"
  now_e="$(_short_iso_epoch "$now_iso")"
  [ -n "$now_e" ] || { printf ''; return 0; }
  lines="$(printf '%s\n' "$journal" | grep -E '\[NTP\] offset:' | tail -n "$k" || true)"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    off_iso="$(printf '%s' "$line" | grep -oE "^$iso_re" | head -1 || true)"
    off_e="$(_short_iso_epoch "$off_iso")"
    [ -n "$off_e" ] || continue
    [ "$((now_e - off_e))" -le "$fresh" ] || continue
    off_us="$(printf '%s' "$line" | sed -n 's/.*\[NTP\] offset:+\{0,1\}\(-\{0,1\}[0-9][0-9]*\)us.*/\1/p' | head -1 || true)"
    printf '%s' "$off_us" | grep -qE '^-?[0-9]+$' || continue
    vals+=("$off_us")
  done <<< "$lines"
  [ "${#vals[@]}" -gt 0 ] || { printf ''; return 0; }
  printf '%s\n' "${vals[@]}" | sort -n | awk -v n="${#vals[@]}" 'NR == int((n+1)/2) { print; exit }'
}

# dantesync_offset_verdict JOURNAL FRESHNESS_S BOUND_US -> "ok" | "drift" | "stale" | "absent".
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
#   ok     -- a FRESH offset line within BOUND_US.
# JOURNAL must be gathered with `-o short-iso` (ISO-timestamped lines).
dantesync_offset_verdict() {
  local journal="$1" fresh="$2" bound="$3"
  local off_line off_us mag
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
  if [ "$mag" -le "$bound" ]; then
    printf 'ok\n'
  else
    printf 'drift\n'
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
  # Servo line is the more-recent of the two -> servo currently ticking -> LOCKED. Otherwise an
  # NTP line is newer than the last servo line -> the servo has stopped -> DEGRADED.
  if [ "$last_servo_n" -ge "$last_ntp_n" ]; then
    printf 'LOCKED'
  else
    printf 'DEGRADED'
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
  if ! printf '%s' "$offset" | grep -qE '^[+-]?[0-9]+$'; then
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

# offset_verdict_check LABEL JOURNAL FRESHNESS_S BOUND_US -> prints a status line; returns 0 OK /
# 2 DRIFT / 3 UNKNOWN. The freshness-aware sibling of offset_check (#550/#591/#595/#607): it grades
# dantesync_offset_verdict's "ok"/"drift"/"stale"/"absent" verdict instead of pairing the age-blind
# offset_us_from_journal (tail -1) with offset_check the way this CLI's own main() loop did before
# #607 -- dantesync-gate.sh (#7) already made this same switch for its own Linux-node loop. A
# "stale" or "absent" verdict maps to UNKNOWN (3), NEVER a silent OK/DRIFT — a reading we could not
# prove fresh must never be graded at all (test-strictness: no silent pass on a possibly-stale
# value). JOURNAL must be gathered with `-o short-iso` (see query_node_journal).
offset_verdict_check() {
  local label="$1" journal="$2" fresh="$3" bound="$4"
  case "$(dantesync_offset_verdict "$journal" "$fresh" "$bound")" in
    ok)
      printf '  %-14s OK       (fresh offset within %s us bound)\n' "$label" "$bound"
      return 0 ;;
    drift)
      printf '  %-14s DRIFT    (fresh offset exceeds %s us bound)\n' "$label" "$bound"
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
  if ! printf '%s' "$dev1" | grep -qE '^[+-]?[0-9]+$' \
     || ! printf '%s' "$painter" | grep -qE '^[+-]?[0-9]+$'; then
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
  scripts/clock-offset-guard.sh [--bound-us N] [--targets "name=ip name=ip ..."]
  scripts/clock-offset-guard.sh --help

Default bound: ${DEFAULT_BOUND_US} us (2 ms) — ~8x under the 16.7 ms 60 fps frame period yet
above the observed steady-state offsets (cam ~300 us, strih master ~1249 us). See SETUP.md
"Cluster clock synchronization" for the baseline + rationale.

Default targets (Linux cameras, over SSH): ${CLOCK_GUARD_TARGETS}
The Windows OBS boxes (strih/stream) are checked read-only via the win-* MCP tools using this
script's shared offset_us_from_pipe_json parser.

A node's offset must also be FRESH, not just in-bound: the freshest "[NTP] offset:" journal line
must be no older than DANTESYNC_OFFSET_FRESHNESS_S (default ${CLOCK_GUARD_OFFSET_FRESHNESS_S})
seconds behind that node's own newest journal line, or the reading is STALE -> UNKNOWN (never a
silent pass, #550/#591/#595/#607).

Exit codes: 0 = all reachable nodes within bound, 20 = DRIFT (a node exceeds the bound),
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
  local bound="$DEFAULT_BOUND_US" targets="$CLOCK_GUARD_TARGETS"
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us) shift; bound="${1:-}" ;;
      --targets)  shift; targets="${1:-}" ;;
      -h|--help)  usage; exit 0 ;;
      --*)        echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)          echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if ! printf '%s' "$bound" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
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
    offset_verdict_check "$name" "$journal" "$freshness" "$bound" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      2) drift=$((drift + 1)) ;;
      3) unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$drift" -gt 0 ]; then
    echo "!! CLOCK DRIFT: ${drift} node(s) exceed the ${bound} us offset bound." >&2
    echo "!! Genlock boundaries diverge by that offset — investigate DanteSync on the drifted node(s)." >&2
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
