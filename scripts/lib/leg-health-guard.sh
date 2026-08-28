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
#   * emit-gate SKIPPED aggregates + uvcvideo EPROTO -> HARD FAIL (genuine emit/USB degradation).
#   * captured FRAME LOSS (sent-vs-captured from the `Streaming:` lines), sustained over threshold
#     -> HARD FAIL (the #1133 replacement for the DEQUEUE STALL gate — the quantity that actually
#     measures capture health; see leg_health_frame_loss_* + leg_health_streaming_grep_pattern).
#   * DEQUEUE STALL count -> REPORT-ONLY (was HARD until #1133). It gated on the WRONG quantity:
#     VIDIOC_DQBUF is a BLOCKING wait, so its duration is ANTI-correlated with real frame loss — the
#     arm losing 4.5x FEWER frames reported MORE stalls, the worst arm reported ZERO (issue 1198).
#     Hard-failing it blocked every good cam1 run while it would pass a genuinely worse config, and
#     its "capture-device/USB fault, replace cable/port/grabber" wording was a misattribution (raw
#     v4l2-ctl on the same card held 60fps with no loss). It is surfaced for diagnostics only now.
#   * cap-1s buckets sustained outside 60+-1 -> REPORT-ONLY WARN. Making the over-rate a hard fail
#     would permanently red every ShadowCast-2 box on every run, recreating the issue-#909 mistake
#     one layer up. It is surfaced for diagnostics only, never aborts.
#
# CALIBRATION (bands do NOT overlap):
#   FRAME LOSS (supervisor-measured live fleet 2026-08-20, 12 windows/box; issue #1133):
#     healthy: cam1 10/3605=0.277%, cam2 8/3605=0.222%, cam3 1/3607=0.028%, cam4 0/3607=0.000%.
#     historically DEFECTIVE cam1: 58.48/60=2.53% and 55.44/60=7.60%. Gate at 1.25% (see the
#     leg_health_frame_loss_* threshold block) — 4.5x above worst healthy, 2.0x below least-bad
#     defective, PASSES all four healthy boxes, FAILS both defective reads.
#   SKIP/EPROTO (issue #1133 + live cam1 read 2026-08-20, this-instance):
#     sick incident (#1130): ~1 SKIPPED aggregate / 5s, 9 EPROTO / 1.5h ==> over 5 min ~60 skips.
#     post-.481 residual (live cam1): 6 SKIPPED / hour + 0 EPROTO ==> over 5 min ~1 skip.
#     healthy box: 0 / 0.
# Thresholds are placed with wide margin in the non-overlapping gap (see the *_threshold fns).
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# ---------------------------------------------------------------------------------------------
# Per-signal journal/kernel grep patterns. Each keys on the STABLE #-tagged substring of the real
# producer's message (see the file header for the producer source), never the volatile numbers
# around it — same discipline as capture_rate_defect_grep_pattern.
# ---------------------------------------------------------------------------------------------

# REPORT-ONLY diagnostic (was HARD-fail signal 1 until #1133): a blocking VIDIOC_DQBUF that itself
# stalled (src/capture_stall.rs). The DEQUEUE STALL COUNT is NO LONGER a gate — it was gating on the
# wrong quantity. VIDIOC_DQBUF is a BLOCKING call that WAITS for the next frame, so its duration
# measures whether the capture thread arrives at the dequeue EARLY (a well-protected thread finishes
# its work in a fraction of a ms, arrives early and legitimately waits ~a whole frame interval → ~8ms
# of jitter tips it past the warn line) or LATE (a poorly-scheduled thread arrives late, a buffer is
# already queued, the call returns almost instantly → reports NOTHING while frames are genuinely
# lost). Fair paired test (issue 1198): the arm losing 4.5x FEWER frames (1.30% vs 5.93%) reported
# MORE stalls, the worst arm reported ZERO — the count is ANTI-correlated with real health, so the
# gate would pass the broken config and block the good one. It stays as a report-only diagnostic
# (leg_health_dequeue_stall_report) and in the `ok:` line; the HARD capture-health signal is now
# FRAME LOSS (leg_health_frame_loss_*, below). The pattern is retained for the diagnostic read.
leg_health_dequeue_stall_grep_pattern() { echo '#707 V4L2 capture DEQUEUE STALL'; }

# HARD-fail signal (NEW #1133): sustained CAPTURE FRAME LOSS, derived from the `Streaming:` report
# the appliance already logs every ~5s (src/main.rs): `Streaming: <e> fps emitted / <c> fps captured
# (<N> sent, <M> captured, <K> capture-dropped, <C> corrupted)`. N=emit_count (the genlock SEND
# cadence — what the output demanded), M=frame_count (frames actually captured from the device); when
# capture drops frames the emit gate repeats to fill, so per-window LOST = max(0, N-M) is exactly the
# frames the device failed to deliver (over-rate M>N is benign, absorbed by the decimation, issue
# #909 → clamped to 0). This is the quantity the DEQUEUE STALL count only correlated with backwards.
# The pattern matches ONLY the genlock-box parenthetical form (` <n> sent, <m> captured`); the
# non-genlock `Streaming: <c> fps (<f> frames, ...)` form has no ` sent, ` and never matches.
leg_health_streaming_grep_pattern() { echo 'Streaming:.*[0-9]+ sent, [0-9]+ captured'; }

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
#   SKIP  8:  sick ~60 / 5min        >> 8 >>  residual ~1 / 5min.
#   EPROTO 6: sick ~6+/hr (the 2026-08-19 cam1 burst: 9/1.5h WITH stalls+skips) > 6 > the CHRONIC
#   ShadowCast-model baseline measured fleet-wide 2026-08-20: cam1 0.66/hr, cam2 1.05/hr,
#   cam3 0.88/hr lifetime averages arriving in 2-3-event clumps (3 events inside one hour happens
#   ROUTINELY on a functionally healthy leg — capture 60fps, no stalls; only the NZXT cam4 reads 0).
#   The original threshold 3 sat ON that baseline and chronically false-aborted (live: two MEQ runs
#   2026-08-20 aborted on cam1 then cam2 back-to-back with zero functional symptoms).
leg_health_skip_fail_threshold() { echo 8; }
leg_health_eproto_fail_threshold() { echo 6; }

# REPORT-ONLY threshold for the DEQUEUE STALL diagnostic (was leg_health_stall_fail_threshold, HARD,
# until #1133). No longer gates anything — leg_health_dequeue_stall_report surfaces a report-only
# note at/over this count (the value at which it USED to abort) so the datum stays visible.
leg_health_stall_report_threshold() { echo 8; }

# HARD-fail FRAME-LOSS gate (NEW #1133 — replaces the DEQUEUE STALL gate). A run ABORTS only when
# ALL THREE hold (single-condition would either false-fail a healthy box or fail on one bad window):
#   (a) at least _min_windows recent `Streaming:` windows exist (else insufficient data -> PASS: a
#       just-restarted box is never judged on <25s of data);
#   (b) the AGGREGATE loss over the window is >= _fail_pct_x100 (in units of 0.01%, so 125 == 1.25%)
#       — this is the direct calibration comparison;
#   (c) at least _min_bad_windows individual windows are themselves over that per-window loss line —
#       the SUSTAIN guard, so a single catastrophic window (which inflates the aggregate) does NOT
#       fail the run. ACCEPTED RESIDUAL: a leg losing frames in short bursts of <= 2 bad windows per
#       5-min read (with clean windows between) passes regardless of aggregate — deliberate (the
#       mandate is "a single bad window must not fail a run"). Both historical defective reads were
#       SUSTAINED (every window elevated), so no known defect shape escapes; a future burst-shaped
#       defect would need its own signal, not a loosening of this guard.
# CALIBRATION (supervisor-measured live fleet 2026-08-20, 12 windows/box; bands do NOT overlap):
#   healthy: cam1 10/3605=0.277%, cam2 8/3605=0.222%, cam3 1/3607=0.028%, cam4 0/3607=0.000%.
#   historically DEFECTIVE cam1: 58.48/60=2.53% and 55.44/60=7.60%.
# 1.25% sits in the non-overlapping gap: 4.5x headroom above the worst healthy read (0.277%), 2.0x
# margin below the least-bad defective read (2.53%). It PASSES all four healthy boxes and FAILS both
# historical defective reads. (Not a gate relaxation: the metric is swapped from one ANTI-correlated
# with health to the one that measures it, and the new threshold still fails both defective reads.)
leg_health_frame_loss_fail_pct_x100() { echo 125; } # 1.25%, in 0.01% units
leg_health_frame_loss_min_windows() { echo 5; }
leg_health_frame_loss_min_bad_windows() { echo 3; }

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

# leg_health_streaming_read_cmd INVOCATION_ID SINCE_EPOCH UNTIL_EPOCH -> the remote command that
# reads the RAW recent `Streaming:` report lines (up to the last 80 — ~1 line per 5s over the 5-min
# window is ~60) for the HARD frame-loss gate. Same #693 instance scoping + `--since/--until` window
# as the count builder. Only the genlock-box `Streaming: ... (<n> sent, <m> captured, ...)` form
# matches (leg_health_streaming_grep_pattern); the non-genlock form has no ` sent, ` and is skipped.
leg_health_streaming_read_cmd() {
  local invocation_id="${1:-}" since_epoch="${2:-}" until_epoch="${3:-}"
  local pat
  pat="$(leg_health_streaming_grep_pattern)"
  if [ -n "$invocation_id" ]; then
    printf 'journalctl _SYSTEMD_INVOCATION_ID=%s --since=@%s --until=@%s --no-pager 2>/dev/null | grep -E '\''%s'\'' | tail -80' \
      "$invocation_id" "$since_epoch" "$until_epoch" "$pat"
  else
    printf 'journalctl -u camera-box --since=@%s --until=@%s --no-pager 2>/dev/null | grep -E '\''%s'\'' | tail -80' \
      "$since_epoch" "$until_epoch" "$pat"
  fi
}

# ---------------------------------------------------------------------------------------------
# Pure decision + message formatters.
# ---------------------------------------------------------------------------------------------

# leg_health_classify BOX SKIP_COUNT EPROTO_COUNT
#   -> prints a SINGLE-LINE, greppable operator ABORT message naming the box + EVERY tripped
#      count-based HARD signal (with its count + threshold) + the escalation, and returns 1, when
#      the emit-SKIP or EPROTO signal is at/over its threshold; prints nothing and returns 0 when
#      neither trips. Non-numeric/empty counts are treated as 0 (a failed ssh read must never itself
#      manufacture a fail -- an unreadable journal is a separate, honest concern for the caller,
#      not a phantom leg-health defect).
#   #1133: the DEQUEUE STALL count is NO LONGER a signal here (it was ANTI-correlated with real
#      health — see leg_health_dequeue_stall_grep_pattern). The capture-health HARD gate is now
#      FRAME LOSS (leg_health_frame_loss_classify), checked separately by the caller from the
#      `Streaming:` lines; the stall count is surfaced report-only by leg_health_dequeue_stall_report.
leg_health_classify() {
  local box="${1:-?}" skip="${2:-0}" eproto="${3:-0}"
  # sanitize to integers (an empty/garbled ssh capture becomes 0)
  case "$skip" in '' | *[!0-9]*) skip=0 ;; esac
  case "$eproto" in '' | *[!0-9]*) eproto=0 ;; esac
  local skip_t eproto_t win
  skip_t="$(leg_health_skip_fail_threshold)"
  eproto_t="$(leg_health_eproto_fail_threshold)"
  win="$(leg_health_journal_window_secs)"
  local win_min=$((win / 60))
  local reasons=""
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
  local stall_cmd skip_cmd eproto_cmd cap_cmd stream_cmd
  stall_cmd="$(leg_health_journal_count_cmd "$inv" "$since" "$until" "$(leg_health_dequeue_stall_grep_pattern)")"
  skip_cmd="$(leg_health_journal_count_cmd "$inv" "$since" "$until" "$(leg_health_emit_skip_grep_pattern)")"
  eproto_cmd="$(leg_health_kmsg_count_cmd "$ep_since" "$ep_until" "$(leg_health_eproto_grep_pattern)")"
  cap_cmd="$(leg_health_cap1s_read_cmd "$inv" "$since" "$until")"
  stream_cmd="$(leg_health_streaming_read_cmd "$inv" "$since" "$until")"
  printf 'echo LEGHEALTH_STALL=$(%s); echo LEGHEALTH_SKIP=$(%s); echo LEGHEALTH_EPROTO=$(%s); echo LEGHEALTH_CAP1S_BEGIN; %s; echo LEGHEALTH_CAP1S_END; echo LEGHEALTH_STREAMING_BEGIN; %s; echo LEGHEALTH_STREAMING_END' \
    "$stall_cmd" "$skip_cmd" "$eproto_cmd" "$cap_cmd" "$stream_cmd"
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

# leg_health_extract_streaming OUTPUT -> just the raw `Streaming:` report lines between the
# STREAMING BEGIN/END markers (for leg_health_frame_loss_classify). Empty when absent.
leg_health_extract_streaming() {
  local output="$1"
  printf '%s\n' "$output" | sed -n '/^LEGHEALTH_STREAMING_BEGIN$/,/^LEGHEALTH_STREAMING_END$/p' \
    | sed '/^LEGHEALTH_STREAMING_\(BEGIN\|END\)$/d'
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

# ---------------------------------------------------------------------------------------------
# HARD frame-loss gate (#1133) — the capture-health signal that REPLACES the DEQUEUE STALL gate.
# Pure (no I/O), Tier-0 unit-tested in tests/harness_leg_health_guard_1133.rs.
# ---------------------------------------------------------------------------------------------

# leg_health_frame_loss_stats STREAMING_TEXT -> echoes `N SENT LOST BAD` computed over every
# `Streaming: ... (<n> sent, <m> captured, ...)` line in STREAMING_TEXT:
#   N    = number of parsed windows (lines)
#   SENT = sum of <n> (the send-cadence count)  ; LOST = sum of per-window max(0, n-m)
#   BAD  = number of windows whose OWN loss >= the per-window fail line (_fail_pct_x100)
# Over-rate windows (m > n) contribute 0 loss (issue #909). A line that does not parse is skipped.
# Prints `0 0 0 0` on empty/garbled input (a failed ssh read must never manufacture a defect).
leg_health_frame_loss_stats() {
  local text="${1:-}"
  local fail_x100 n=0 sent_sum=0 lost_sum=0 bad=0
  fail_x100="$(leg_health_frame_loss_fail_pct_x100)"
  # Pull `<n> sent, <m> captured` from each matching line: strip up to `(`, then read the two
  # leading integers. `|| true` keeps a zero-match pipeline from set -e-aborting a subshell caller.
  local line n_i m_i lost_i
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    # extract the "<int> sent, <int> captured" pair (first occurrence)
    n_i="$(printf '%s\n' "$line" | grep -oE '[0-9]+ sent, [0-9]+ captured' | head -1 || true)"
    [ -n "$n_i" ] || continue
    m_i="${n_i##* sent, }"      # "<m> captured"
    m_i="${m_i%% captured}"     # "<m>"
    n_i="${n_i%% sent,*}"       # "<n>"
    case "$n_i" in '' | *[!0-9]*) continue ;; esac
    case "$m_i" in '' | *[!0-9]*) continue ;; esac
    n=$((n + 1))
    sent_sum=$((sent_sum + n_i))
    if [ "$n_i" -gt "$m_i" ]; then
      lost_i=$((n_i - m_i))
    else
      lost_i=0
    fi
    lost_sum=$((lost_sum + lost_i))
    # per-window bad? lost_i/n_i >= fail_x100/10000  <=>  lost_i*10000 >= fail_x100*n_i
    if [ "$n_i" -gt 0 ] && [ $((lost_i * 10000)) -ge $((fail_x100 * n_i)) ]; then
      bad=$((bad + 1))
    fi
  done <<EOF
$(printf '%s\n' "$text")
EOF
  echo "$n $sent_sum $lost_sum $bad"
}

# leg_health_frame_loss_is_unhealthy N SENT LOST BAD -> returns 0 (UNHEALTHY, abort) iff ALL THREE
# hold; else returns 1 (healthy). Empty/garbled args -> 0 (treated as healthy, never a phantom fail);
# SENT<=0 -> healthy (no data / clamp). See the threshold block for the (a)/(b)/(c) rationale.
leg_health_frame_loss_is_unhealthy() {
  local n="${1:-0}" sent="${2:-0}" lost="${3:-0}" bad="${4:-0}"
  case "$n" in '' | *[!0-9]*) n=0 ;; esac
  case "$sent" in '' | *[!0-9]*) sent=0 ;; esac
  case "$lost" in '' | *[!0-9]*) lost=0 ;; esac
  case "$bad" in '' | *[!0-9]*) bad=0 ;; esac
  local min_win min_bad fail_x100
  min_win="$(leg_health_frame_loss_min_windows)"
  min_bad="$(leg_health_frame_loss_min_bad_windows)"
  fail_x100="$(leg_health_frame_loss_fail_pct_x100)"
  # (a) enough windows to judge
  [ "$n" -ge "$min_win" ] || return 1
  # (b) aggregate loss over the threshold (SENT>0 guaranteed by n>=min_win>0 on real data, but guard)
  [ "$sent" -gt 0 ] || return 1
  [ $((lost * 10000)) -ge $((fail_x100 * sent)) ] || return 1
  # (c) sustained across at least min_bad individually-elevated windows
  [ "$bad" -ge "$min_bad" ] || return 1
  return 0
}

# leg_health_frame_loss_pct_str SENT LOST -> a human "X.XX%" string (integer-only, 0.01% precision).
leg_health_frame_loss_pct_str() {
  local sent="${1:-0}" lost="${2:-0}" x100 whole frac
  case "$sent" in '' | *[!0-9]*) sent=0 ;; esac
  case "$lost" in '' | *[!0-9]*) lost=0 ;; esac
  if [ "$sent" -le 0 ]; then
    echo "0.00%"
    return 0
  fi
  x100=$((lost * 10000 / sent)) # loss in 0.01% units
  whole=$((x100 / 100))
  frac=$((x100 % 100))
  printf '%d.%02d%%\n' "$whole" "$frac"
}

# leg_health_frame_loss_fail_message BOX N SENT LOST BAD -> the operator-facing single-line ABORT
# message. NAMES FRAME LOSS and does NOT assert a cable/port/grabber fault (issue 1198 proved the
# DEQUEUE STALL's hardware attribution wrong — raw v4l2-ctl on the same card/cable/port under the
# same load held 59.97-60.00 fps with no loss; the loss arises in our OWN capture path).
leg_health_frame_loss_fail_message() {
  local box="$1" n="$2" sent="$3" lost="$4" bad="$5" pct thr_pct
  pct="$(leg_health_frame_loss_pct_str "$sent" "$lost")"
  local thr_x100
  thr_x100="$(leg_health_frame_loss_fail_pct_x100)"
  thr_pct="$((thr_x100 / 100)).$(printf '%02d' $((thr_x100 % 100)))%"
  echo "${box} capture leg UNHEALTHY — stratené snímky ${lost}/${sent} (${pct}) sustained cez ${bad}/${n} okien (aggregate >= ${thr_pct}) — nemeriam na pokazenom nástroji: vyraď ${box} z CAMERA_ACTIVE_SET (alebo CAMBOX_OFFLINE_ACK=\"${box}:<dôvod>\") ALEBO vyšetri našu capture cestu (NIE nutne kábel/port/grabber — DQBUF stall nie je dôkaz hardvér-faultu, raw v4l2-ctl na tej istej karte drží 60fps bez straty, issue 1198), nespúšťaj beh na chorej vetve (#1133)"
}

# leg_health_frame_loss_classify BOX STREAMING_TEXT -> prints a SINGLE-LINE, greppable ABORT message
# and returns 1 when the leg's captured frame loss is UNHEALTHY (sustained, over threshold); prints
# nothing and returns 0 otherwise. STREAMING_TEXT is the raw `Streaming:` lines
# leg_health_extract_streaming returned. Empty/no-parse input -> healthy (return 0), so a failed ssh
# read never manufactures a defect (same contract as leg_health_classify).
leg_health_frame_loss_classify() {
  local box="${1:-?}" text="${2:-}"
  local stats n sent lost bad
  stats="$(leg_health_frame_loss_stats "$text")"
  # stats is "N SENT LOST BAD"
  n="${stats%% *}"
  stats="${stats#* }"
  sent="${stats%% *}"
  stats="${stats#* }"
  lost="${stats%% *}"
  bad="${stats##* }"
  if leg_health_frame_loss_is_unhealthy "$n" "$sent" "$lost" "$bad"; then
    printf '%s' "$(leg_health_frame_loss_fail_message "$box" "$n" "$sent" "$lost" "$bad")"
    return 1
  fi
  return 0
}

# leg_health_dequeue_stall_report BOX STALL_COUNT -> a REPORT-ONLY diagnostic WARN line (to stdout)
# IFF the DEQUEUE STALL count is at/over the report threshold (the value at which it USED to abort,
# pre-#1133); prints nothing otherwise. NEVER affects any exit code — the stall count is
# ANTI-correlated with real health (issue 1198) and no longer gates. Called as a bare statement
# under `set -euo pipefail`, so it must always return 0 (mirrors leg_health_cap1s_band_warn).
leg_health_dequeue_stall_report() {
  local box="${1:-?}" stall="${2:-0}" thr
  case "$stall" in '' | *[!0-9]*) stall=0 ;; esac
  thr="$(leg_health_stall_report_threshold)"
  if [ "$stall" -ge "$thr" ]; then
    echo "WARNING #1133: ${box} #707 V4L2 DEQUEUE STALL x${stall} (>=${thr}) — REPORT-ONLY: dĺžka blokujúceho VIDIOC_DQBUF je proti-korelovaná s reálnou stratou snímok (chránené vlákno príde k dequeue skoro a čaká skoro celý interval → viac stallov, menej straty; issue 1198), NEabortuje beh; HARD gate je teraz strata snímok"
  fi
  return 0
}
