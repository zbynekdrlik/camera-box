#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors the
# sibling scripts/lib/capture-rate-guard.sh convention which is also `set -euo pipefail`-free for
# the same reason.
#
# scripts/lib/leg-health-guard.sh — #1133: the per-box capture-LEG-HEALTH preflight signal set.
#
# WHY this exists on top of the #656 capture-delivery-rate preflight (capture-rate-guard.sh):
# the #656 preflight greps ONLY for the appliance's own `#656 capture-delivery-rate DEFECTIVE`
# WARN, which src/capture_rate_health.rs emits ONLY once captured fps has broken its per-model
# tolerance (>9% for the ShadowCast 2, widened in #685). The #1130/#1110 incident had cam1
# delivering 61-63 fps (~2-5% over 60) — INSIDE that tolerance — so the DEFECTIVE WARN never fired
# and the [0/8] preflight said "ok: no sustained capture-rate defect", even though the box was, for
# HOURS, emitting genuine degradation signals from ENTIRELY DIFFERENT producers that the #656
# preflight never looks at:
#   * src/capture_stall.rs  -> `#707 V4L2 capture DEQUEUE STALL: N.Nms ...` (the blocking
#     VIDIOC_DQBUF itself stalled -> a capture-device/driver/USB fault)
#   * src/emit_skip_log.rs  -> `#707 genlock emit-gate SKIPPED boundaries in N gate call(s)
#     totalling M boundary interval(s) ...` (frames that were never emitted)
#   * kernel uvcvideo       -> `uvcvideo ...: Non-zero status (-71) in video completion handler`
#     (-EPROTO -> a USB protocol error ON THE WIRE: grabber/cable/port electrically defective)
# so the leg was silently sick and the run measured on a broken tool (violating the
# rig-degradation-alert-immediately mandate). This lib adds those signals to the preflight so a sick
# capture leg is a NAMED ABORT (escalation: fix the hardware, or drop the box from
# CAMERA_ACTIVE_SET / CAMBOX_OFFLINE_ACK), never a silent "ok".
#
# HONEST NUANCE (#1130 comment body 1/4, issue #909): the VISIBLE judder from that incident was
# ultimately EXONERATED as an emit-gate regression (#1111, fixed by deploying dev.481), not these
# signals — and cam1's chronic 61-64 fps OVER-RATE is NOT itself a defect (it is absorbed by the
# genlock emit-gate / wall-pacing decimation; that is exactly why src/capture_rate_health.rs's
# `#717 SUSTAINED band` is deliberately report-only, issue #909). BUT the DEQUEUE-STALL / emit-SKIP
# / EPROTO signals WERE real degraded-state signals either way (the box genuinely stalled/skipped;
# EPROTO/DQBUF remain a "secondary observation, monitor" item on #1110). So:
#   * DEQUEUE STALL + emit-gate SKIPPED aggregates + uvcvideo EPROTO -> HARD FAIL (genuine
#     capture/USB degradation).
#   * cap-1s buckets sustained outside 60+-1 -> REPORT-ONLY WARN. Making the over-rate a hard fail
#     would permanently red every ShadowCast-2 box on every run, recreating the issue-#909 mistake
#     one layer up. It is surfaced for diagnostics only, never aborts.
#
# CALIBRATION (bands do NOT overlap — issue #1133 + live cam1 read 2026-08-20, this-instance):
#   sick incident (#1130): ~1 SKIPPED aggregate / 5s, tens of DEQUEUE STALLs, 61-63 fps, 9 EPROTO
#                          / 1.5h  ==>  over a 5-min window: ~60 skips, tens of stalls.
#   post-.481 residual (live cam1): 8 DEQUEUE STALL / hour + 6 SKIPPED / hour + cap-1s 60-62 + 0
#                          EPROTO  ==>  over a 5-min window: ~1 skip, ~1 stall.
#   healthy box: 0 / 0 / 60.0+-0.2 / 0.
# Thresholds are placed with wide margin in the non-overlapping gap (see the *_threshold fns).
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# ---------------------------------------------------------------------------------------------
# Per-signal journal/kernel grep patterns. Each keys on the STABLE #-tagged substring of the real
# producer's message (see the file header for the producer source), never the volatile numbers
# around it — same discipline as capture_rate_defect_grep_pattern.
# ---------------------------------------------------------------------------------------------

# HARD-fail signal 1: a blocking VIDIOC_DQBUF that itself stalled (src/capture_stall.rs).
leg_health_dequeue_stall_grep_pattern() { echo '#707 V4L2 capture DEQUEUE STALL'; }

# HARD-fail signal 2: the rate-limited emit-gate skip aggregate (src/emit_skip_log.rs). Anchors on
# the "#707 genlock emit-gate SKIPPED boundaries" head so it matches the #752 aggregate line
# regardless of the events/total numbers that follow.
leg_health_emit_skip_grep_pattern() { echo '#707 genlock emit-gate SKIPPED boundaries'; }

# HARD-fail signal 3: kernel uvcvideo -EPROTO (a USB protocol error on the wire). The kernel line
# is `uvcvideo <bus>: Non-zero status (-71) in video completion handler`; the pattern spans the
# variable bus-id between "uvcvideo" and "Non-zero status" with `.*`.
leg_health_eproto_grep_pattern() { echo 'uvcvideo.*Non-zero status'; }

# REPORT-ONLY signal: the periodic emit-1s/cap-1s bucket dump (src/main.rs). Used only to surface a
# sustained capture over-rate (never to abort — see the file header's HONEST NUANCE).
leg_health_cap1s_grep_pattern() { echo '#707 emit-1s:.*cap-1s:'; }

# ---------------------------------------------------------------------------------------------
# Window + threshold constants (pure -- calibrated, see the file header CALIBRATION block).
# ---------------------------------------------------------------------------------------------

# The recent-journal lookback for the HARD stall/skip signals AND the report-only cap-1s read, in
# seconds. 5 minutes: long enough that a sick box's ~1-per-5s pattern accumulates dozens of hits,
# short enough that the post-.481 residual (~1 stall + ~1 skip per 5 min) stays far under the
# thresholds below.
leg_health_journal_window_secs() { echo 300; }

# The kernel-EPROTO lookback, in seconds. 1 hour, exactly as issue #1133 item 1 specifies ("v
# poslednej hodine").
leg_health_eproto_window_secs() { echo 3600; }

# HARD-fail thresholds (>= this count in the corresponding window aborts the run, naming the box +
# signal). Sick >> threshold >> healthy/residual, by construction:
#   STALL 8:  sick ~tens..60 / 5min  >> 8 >>  residual ~1 / 5min.
#   SKIP  8:  sick ~60 / 5min        >> 8 >>  residual ~1 / 5min.
#   EPROTO 6: sick ~6+/hr (the 2026-08-19 cam1 burst: 9/1.5h WITH stalls+skips) > 6 > the CHRONIC
#   ShadowCast-model baseline measured fleet-wide 2026-08-20: cam1 0.66/hr, cam2 1.05/hr,
#   cam3 0.88/hr lifetime averages arriving in 2-3-event clumps (3 events inside one hour happens
#   ROUTINELY on a functionally healthy leg — capture 60fps, no stalls; only the NZXT cam4 reads 0).
#   The original threshold 3 sat ON that baseline and chronically false-aborted (live: two MEQ runs
#   2026-08-20 aborted on cam1 then cam2 back-to-back with zero functional symptoms).
leg_health_stall_fail_threshold() { echo 8; }
leg_health_skip_fail_threshold() { echo 8; }
leg_health_eproto_fail_threshold() { echo 6; }

# Report-only cap-1s band: a captured 1-second bucket OUTSIDE [low, high] fps is "over/under rate".
# 60 +- 1 == [59, 61] inclusive (a 61 fps bucket is IN band; 62 is out). WARN only when a SUSTAINED
# majority (>= _warn_fraction_pct percent) of at least _warn_min_buckets recent buckets are out of
# band, so the chronic ShadowCast wobble ([60,62,61,60,61] = 1/5 out) does NOT warn while a
# sustained 62-63 (incident) does.
leg_health_cap1s_band_low() { echo 59; }
leg_health_cap1s_band_high() { echo 61; }
leg_health_cap1s_warn_fraction_pct() { echo 60; }
leg_health_cap1s_warn_min_buckets() { echo 5; }

# ---------------------------------------------------------------------------------------------
# Remote read-command builders (pure string builders -- no ssh, no I/O -- so they are directly
# unit-testable without a live rig, exactly like capture_rate_journalctl_cmd's convention). The
# caller substitutes an already-resolved INVOCATION_ID + epoch bounds and runs the string over ssh.
# ---------------------------------------------------------------------------------------------

# leg_health_journal_count_cmd INVOCATION_ID SINCE_EPOCH UNTIL_EPOCH PATTERN -> the remote command
# that counts PATTERN occurrences in the CURRENT camera-box.service instance's journal within
# [SINCE_EPOCH, UNTIL_EPOCH] (#693 _SYSTEMD_INVOCATION_ID scoping so a killed prior instance's sick
# lines can never leak in, same freshness contract as capture_rate_window_journalctl_cmd). Falls
# back to the unscoped `-u camera-box` form when INVOCATION_ID is empty. `grep -Ec` always prints a
# count on stdout (0 on no match), so the caller captures a number regardless of grep's exit code.
leg_health_journal_count_cmd() {
  local invocation_id="${1:-}" since_epoch="${2:-}" until_epoch="${3:-}" pattern="${4:-}"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --since=@%s --until=@%s --no-pager 2>/dev/null | grep -Ec '\''%s'\''' \
      "$invocation_id" "$since_epoch" "$until_epoch" "$pattern"
  else
    printf 'journalctl -u camera-box --since=@%s --until=@%s --no-pager 2>/dev/null | grep -Ec '\''%s'\''' \
      "$since_epoch" "$until_epoch" "$pattern"
  fi
}

# leg_health_kmsg_count_cmd SINCE_EPOCH UNTIL_EPOCH PATTERN -> the remote command that counts
# PATTERN occurrences in the KERNEL log within [SINCE_EPOCH, UNTIL_EPOCH]. Reads via
# `journalctl -k` (the same kernel ring buffer `dmesg` shows -- issue #1133 names dmesg; journalctl
# -k gives the identical uvcvideo lines with clean absolute-epoch windowing and no dmesg
# boot-relative-timestamp parsing). No InvocationID here: kernel messages are not scoped to a
# userspace unit instance; the time window IS the scope.
leg_health_kmsg_count_cmd() {
  local since_epoch="${1:-}" until_epoch="${2:-}" pattern="${3:-}"
  printf 'journalctl -k --since=@%s --until=@%s --no-pager 2>/dev/null | grep -Ec '\''%s'\''' \
    "$since_epoch" "$until_epoch" "$pattern"
}

# leg_health_cap1s_read_cmd INVOCATION_ID SINCE_EPOCH UNTIL_EPOCH -> the remote command that reads
# the RAW recent cap-1s dump lines (up to the last 40) for the report-only band analysis. Same
# #693 instance scoping + `--since/--until` window as the count builder.
leg_health_cap1s_read_cmd() {
  local invocation_id="${1:-}" since_epoch="${2:-}" until_epoch="${3:-}"
  local pat
  pat="$(leg_health_cap1s_grep_pattern)"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --since=@%s --until=@%s --no-pager 2>/dev/null | grep -E '\''%s'\'' | tail -40' \
      "$invocation_id" "$since_epoch" "$until_epoch" "$pat"
  else
    printf 'journalctl -u camera-box --since=@%s --until=@%s --no-pager 2>/dev/null | grep -E '\''%s'\'' | tail -40' \
      "$since_epoch" "$until_epoch" "$pat"
  fi
}

# ---------------------------------------------------------------------------------------------
# Pure decision + message formatters.
# ---------------------------------------------------------------------------------------------

# leg_health_classify BOX STALL_COUNT SKIP_COUNT EPROTO_COUNT
#   -> prints a SINGLE-LINE, greppable operator ABORT message naming the box + EVERY tripped
#      HARD signal (with its count + threshold) + the escalation, and returns 1, when any of the
#      three HARD signals is at/over its threshold; prints nothing and returns 0 when the leg is
#      healthy. Non-numeric/empty counts are treated as 0 (a failed ssh read must never itself
#      manufacture a fail -- an unreadable journal is a separate, honest concern for the caller,
#      not a phantom leg-health defect).
leg_health_classify() {
  local box="${1:-?}" stall="${2:-0}" skip="${3:-0}" eproto="${4:-0}"
  # sanitize to integers (an empty/garbled ssh capture becomes 0)
  case "$stall" in '' | *[!0-9]*) stall=0 ;; esac
  case "$skip" in '' | *[!0-9]*) skip=0 ;; esac
  case "$eproto" in '' | *[!0-9]*) eproto=0 ;; esac
  local stall_t skip_t eproto_t win
  stall_t="$(leg_health_stall_fail_threshold)"
  skip_t="$(leg_health_skip_fail_threshold)"
  eproto_t="$(leg_health_eproto_fail_threshold)"
  win="$(leg_health_journal_window_secs)"
  local win_min=$((win / 60))
  local reasons=""
  if [ "$stall" -ge "$stall_t" ]; then
    reasons="${reasons:+$reasons; }V4L2 DEQUEUE STALL x${stall} (>=${stall_t}/${win_min}min — blocking VIDIOC_DQBUF stalling, capture-device/USB fault)"
  fi
  if [ "$skip" -ge "$skip_t" ]; then
    reasons="${reasons:+$reasons; }emit-gate SKIPPED-boundary aggregates x${skip} (>=${skip_t}/${win_min}min — frames never emitted)"
  fi
  if [ "$eproto" -ge "$eproto_t" ]; then
    reasons="${reasons:+$reasons; }uvcvideo Non-zero status/-EPROTO x${eproto} (>=${eproto_t}/hr — USB protocol error on the wire: grabber/cable/port)"
  fi
  if [ -n "$reasons" ]; then
    printf '%s' "$(leg_health_fail_message "$box" "$reasons")"
    return 1
  fi
  return 0
}

# leg_health_fail_message BOX REASONS -> the operator-facing single-line ABORT message. Pure
# formatter (no I/O), so the exact escalation wording is unit-tested here. REASONS is the
# "; "-joined tripped-signal list leg_health_classify built.
leg_health_fail_message() {
  local box="$1" reasons="$2"
  echo "${box} capture leg UNHEALTHY — ${reasons} — nemeriam na pokazenom nástroji: vyraď ${box} z CAMERA_ACTIVE_SET (alebo CAMBOX_OFFLINE_ACK=\"${box}:<dôvod>\") ALEBO oprav hardvér (kábel/port/grabber), nespúšťaj beh na chorej vetve (#1133)"
}

# leg_health_read_all_cmd INVOCATION_ID SINCE UNTIL EPROTO_SINCE EPROTO_UNTIL -> ONE remote script
# (pure string) that reads all four signals in a single ssh round-trip and prints them in a
# stable, parseable form:
#   LEGHEALTH_STALL=<n>
#   LEGHEALTH_SKIP=<n>
#   LEGHEALTH_EPROTO=<n>
#   LEGHEALTH_CAP1S_BEGIN
#   <raw cap-1s dump lines>
#   LEGHEALTH_CAP1S_END
# Composed from the single-signal builders above (single source of truth for each pattern/window),
# so a caller substitutes it once per box instead of issuing four separate ssh reads. Directly
# unit-testable (no ssh/I/O) -- the caller runs the string over ssh and feeds the output to
# leg_health_extract / leg_health_extract_cap1s below.
leg_health_read_all_cmd() {
  local inv="${1:-}" since="${2:-}" until="${3:-}" ep_since="${4:-}" ep_until="${5:-}"
  local stall_cmd skip_cmd eproto_cmd cap_cmd
  stall_cmd="$(leg_health_journal_count_cmd "$inv" "$since" "$until" "$(leg_health_dequeue_stall_grep_pattern)")"
  skip_cmd="$(leg_health_journal_count_cmd "$inv" "$since" "$until" "$(leg_health_emit_skip_grep_pattern)")"
  eproto_cmd="$(leg_health_kmsg_count_cmd "$ep_since" "$ep_until" "$(leg_health_eproto_grep_pattern)")"
  cap_cmd="$(leg_health_cap1s_read_cmd "$inv" "$since" "$until")"
  printf 'echo LEGHEALTH_STALL=$(%s); echo LEGHEALTH_SKIP=$(%s); echo LEGHEALTH_EPROTO=$(%s); echo LEGHEALTH_CAP1S_BEGIN; %s; echo LEGHEALTH_CAP1S_END' \
    "$stall_cmd" "$skip_cmd" "$eproto_cmd" "$cap_cmd"
}

# leg_health_extract FIELD OUTPUT -> the integer value of the `LEGHEALTH_<FIELD>=` line in the
# read_all OUTPUT, or 0 when absent/garbled (a truncated/failed ssh read must never manufacture a
# non-zero count -- leg_health_classify treats 0 as healthy). FIELD is STALL|SKIP|EPROTO.
leg_health_extract() {
  local field="$1" output="$2" val
  # `sed | head -1` is closed early by head on a multi-match input, which SIGPIPEs sed; the
  # trailing `|| true` keeps that from failing under a `pipefail`+`-e` caller (defensive -- the
  # read_all output emits exactly one line per field today, so it is not yet triggerable, #1133
  # review 🔵). An empty/garbled read then falls through to 0 (never a phantom non-zero count).
  val="$(printf '%s\n' "$output" | sed -n "s/^LEGHEALTH_${field}=\\([0-9][0-9]*\\).*/\\1/p" | head -1 || true)"
  case "$val" in '' | *[!0-9]*) echo 0 ;; *) echo "$val" ;; esac
}

# leg_health_extract_cap1s OUTPUT -> just the raw cap-1s dump lines between the BEGIN/END markers
# (for leg_health_cap1s_band_warn). Empty when the markers/lines are absent.
leg_health_extract_cap1s() {
  local output="$1"
  printf '%s\n' "$output" | sed -n '/^LEGHEALTH_CAP1S_BEGIN$/,/^LEGHEALTH_CAP1S_END$/p' \
    | sed '/^LEGHEALTH_CAP1S_\(BEGIN\|END\)$/d'
}

# leg_health_cap1s_band_warn BOX CAP1S_TEXT -> a REPORT-ONLY WARN line (to stdout) IFF a sustained
# majority of the recent captured 1-second buckets are out of the 60+-1 band; prints nothing
# otherwise. NEVER affects any exit code -- the over-rate is chronic-benign (absorbed by the
# emit-gate decimation, issue #909); this only surfaces the datum for diagnostics. CAP1S_TEXT is
# the raw `#707 emit-1s: [...] cap-1s: [...]` line(s) leg_health_cap1s_read_cmd returned.
leg_health_cap1s_band_warn() {
  local box="$1" text="$2"
  # A REPORT-ONLY probe must NEVER abort the run: an empty read (failed/timed-out ssh, or a
  # just-restarted box whose instance-scoped window has no cap-1s dump yet) returns immediately.
  # This is load-bearing because the caller (recording-e2e.sh, `set -euo pipefail`) invokes this
  # as a bare statement, NOT an `if`-condition -- so any non-zero return would `set -e`-abort the
  # whole E2E run, a silent phantom-fail (#1133 review, 🔴).
  [ -n "$text" ] || return 0
  local low high frac_pct min_buckets
  low="$(leg_health_cap1s_band_low)"
  high="$(leg_health_cap1s_band_high)"
  frac_pct="$(leg_health_cap1s_warn_fraction_pct)"
  min_buckets="$(leg_health_cap1s_warn_min_buckets)"
  # Extract every integer that appears inside a `cap-1s: [ ... ]` array across all lines. Strip
  # everything up to and including `cap-1s: [`, then take digits up to the closing `]`. The
  # trailing `|| true` keeps a ZERO-match pipeline (text present but no cap-1s line -- log-format
  # drift, a window with only stall/skip lines) from failing under the caller's `pipefail`+`-e`
  # (grep -oE exits 1 on no match; without this it would abort the run, same phantom-fail class).
  local caps total=0 out=0 n
  caps="$(printf '%s\n' "$text" \
    | grep -oE 'cap-1s: \[[0-9, ]*\]' \
    | grep -oE '\[[0-9, ]*\]' \
    | tr -d '[]' \
    | tr ',' ' ' || true)"
  for n in $caps; do
    case "$n" in '' | *[!0-9]*) continue ;; esac
    total=$((total + 1))
    if [ "$n" -lt "$low" ] || [ "$n" -gt "$high" ]; then
      out=$((out + 1))
    fi
  done
  [ "$total" -ge "$min_buckets" ] || return 0
  # out * 100 >= frac_pct * total  (integer-only, no bc)
  if [ $((out * 100)) -ge $((frac_pct * total)) ]; then
    echo "WARNING #1133: ${box} capture takt sustained mimo 60±1 fps (${out}/${total} 1s-bucketov out-of-band [${low},${high}]) — REPORT-ONLY: chronický ShadowCast over-rate je pohltený emit-gate/wall-pacing decimáciou (issue #909), NEabortuje beh; surfacnuté len pre diagnostiku"
  fi
  return 0
}
