#!/usr/bin/env bash
#
# dantesync-gate.sh — the recording-E2E precondition gate (#7): every measured node must be
# BOTH NTP-synced AND PTP-locked before the recording run is allowed to proceed.
#
# WHY THIS GATE EXISTS (the user's hard requirement): the recording-based 4-node E2E measures
# cross-node per-hop latency and aligns per-frame timestamps across cam1, cam2, strih and stream.
# Those numbers are ONLY meaningful when the cluster's FINE servo is the µs-grade PTP servo
# (grandmaster 10.77.9.184 up), NOT the ±1 ms NTP-stepping sawtooth fallback. If ANY node is on
# NTP-only, the latency/timestamps are garbage and the whole run is worthless. So this gate runs
# FIRST and FAILS FAST (non-zero, with a clear per-node diagnostic) if any node is not both
# NTP-within-bound AND PTP-locked — the run MUST NOT reach the recording step otherwise.
#
# It REUSES the unit-tested pure parsers in scripts/clock-offset-guard.sh (offset_us_from_journal,
# offset_us_from_pipe_json, offset_check, ptp_locked_from_journal, ptp_locked_from_pipe_json,
# ptp_check) — it does NOT reinvent any parsing. This script is the FLOW that gathers each node's
# DanteSync status and applies BOTH the offset check (NTP) and the PTP-lock check.
#
# NODE ACCESS (this rig):
#   * Linux cams (cam1, cam2): journald over SSH (root/newlevel) — gathered directly here via
#     read_linux_node_journal(), below. Overridable per-node for tests/offline via
#     DANTESYNC_GATE_LINUX_JOURNAL_<NAME> (NAME uppercased, e.g. DANTESYNC_GATE_LINUX_JOURNAL_CAM1)
#     -- the SAME "caller pre-fetches the status to a file" pattern
#     clock-offset-painter-gate.sh uses (DEV1_DANTE_JOURNAL/PAINTER_DANTE_JOURNAL, #608), keyed by
#     node name, since this gate can measure MULTIPLE Linux nodes at once, unlike the painter
#     gate's fixed dev1<->painter pair.
#   * Windows OBS boxes (strih, stream): queried LIVE over HTTP from dantesync#47's own network
#     status endpoint (http://HOST:PORT/status, #648) via --win-http, below — no win-* MCP, no
#     human/agent pre-fetch, unattended-CI-safe, and it grades freshness from the payload's own
#     "updated_ts" field. (#835: the PRIOR flow — a human/agent with the win-* MCP writing each
#     box's `\\.\pipe\dantesync` status-pipe JSON to a local file, passed via --win-status
#     NAME=FILE — was REMOVED outright, not merely deprecated: it had zero live callers left
#     in this repo once recording-e2e.sh switched to --win-http, and it was deliberately
#     AGE-BLIND with no way to detect a stale/leftover file — exactly the false-GREEN hazard a
#     stale runbook could walk an operator into. --win-http covers the same two nodes strictly
#     better. See `scripts/lib/win-status-args.sh` if you need the shared NAME=FILE parser for a
#     DIFFERENT gate — `scripts/w32time-gate.sh` still legitimately uses it for the W32Time
#     service-state invariant, which has no HTTP equivalent to migrate to.)
#
# Usage:
#   dantesync-gate.sh [--bound-us N] \
#       [--linux "cam1=10.77.9.61 cam2=10.77.9.62"] \
#       [--win-http strih=10.77.9.202] [--win-http stream=10.77.9.204]
#   dantesync-gate.sh --help
#
# Exit codes: 0 = ALL measured nodes NTP-within-bound AND PTP-locked (run may proceed),
#   20 = at least one node DRIFTED (offset) or PTP-DEGRADED (NTP-only fallback),
#   11 = at least one node UNREACHABLE / status UNKNOWN (incomplete — NOT clean),
#   1  = usage / environment error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Source the shared, unit-tested DanteSync parsers (its BASH_SOURCE!=$0 guard skips its own flow).
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"

GATE_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
# The four measured nodes by default: the two Linux cams over SSH; strih/stream need --win-http.
GATE_LINUX="${GATE_LINUX:-cam1=10.77.9.61 cam2=10.77.9.62}"
GATE_SSH_TIMEOUT="${CLOCK_GUARD_SSH_TIMEOUT:-8}"
# #550/#591/#595: a Linux node's freshest "[NTP] offset:" journal line must be no older than this
# many seconds behind its newest journal line, or the reading is STALE and must never be graded as
# the current offset (the #550 false-fail/false-pass bug this gate was still exposed to before
# #595 -- see dantesync_offset_verdict in clock-offset-guard.sh). Same default as
# verify-device.sh's DANTESYNC_OFFSET_FRESHNESS_S.
GATE_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"
# #648: the port dantesync#47's network status endpoint listens on (http://<box>:PORT/status),
# shared across all --win-http nodes (same convention as --bound-us: one value, all nodes) --
# same default/env-var name recording-e2e.sh already used for its own (now-removed) pre-fetch.
GATE_WIN_HTTP_PORT="${WIN_DANTE_PORT:-8898}"
GATE_WIN_HTTP_TIMEOUT="${CLOCK_GUARD_HTTP_TIMEOUT:-10}"
# #836: a single pipe-json read is close to a coin flip on a noisy node (live data: 22 reads 25s
# apart on the stream box, only 2/22 individually in-bound). Every --win-http / Linux-HTTP-first
# node is now sampled GATE_SAMPLE_COUNT times across roughly GATE_SAMPLE_WINDOW_S seconds, and
# graded on the MEDIAN (existing GATE_BOUND_US, unchanged) AND the SPREAD (new
# GATE_STABILITY_US) of the samples that are DISTINCT by updated_ts -- see
# clock-offset-guard.sh's sampled_offset_verdict/sampled_offset_check for the pure grading logic.
# Fewer than GATE_SAMPLE_MIN_DISTINCT distinct samples is itself a hard failure (never a silent
# pass on one lucky read). Defaults: 6 reads spread across a 30s window, needing at least 3
# distinct samples, with a stability bound the same order of magnitude as the location bound.
GATE_SAMPLE_COUNT="${DANTESYNC_SAMPLE_COUNT:-6}"
GATE_SAMPLE_WINDOW_S="${DANTESYNC_SAMPLE_WINDOW_S:-30}"
GATE_SAMPLE_MIN_DISTINCT="${DANTESYNC_SAMPLE_MIN_DISTINCT:-3}"
GATE_STABILITY_US="${DANTESYNC_STABILITY_US:-2000}"
# #1014: the NTP master's own ntp_offset_us is a by-design UTC-residual correction-lag SAWTOOTH
# since dantesync v1.8.30 (dantesync issue 71) -- its SPREAD says nothing about fleet coherence,
# unlike a client node's, so the node whose --win-http/--linux NAME matches this is graded on
# median+freshness ONLY (clock-offset-guard.sh's sampled_offset_verdict "median-only" MODE),
# never the spread/stability bound every other node keeps. This is name-based (matches the
# --win-http/--linux NAME= label), not path-based -- see the gate's own banner text below, which
# used to hardcode "strih" as decoration only and is now the SAME value this config drives.
GATE_NTP_MASTER_NAME="${DANTESYNC_NTP_MASTER_NAME:-strih}"
# #1021 (dantesync PR #84/#86, closes dantesync issue 83): the NTP master's own median bound
# widens to max(GATE_BOUND_US, ntp_deadband_us + this margin) when its /status reports a numeric
# "ntp_deadband_us" -- see clock-offset-guard.sh's ntp_master_effective_bound_us for the full
# derivation. Absent/null field (older dantesync, or any client node) -> unchanged fixed bound.
GATE_DEADBAND_MARGIN_US="${DANTESYNC_DEADBAND_MARGIN_US:-1000}"
# #1022 (dantesync-gate: client rows can ALSO false-DRIFT during the master's OWN deadband
# step-chase window -- #1021 explicitly left client rows untouched): the CAP on the deadband
# component of a CLIENT row's widened bound -- see clock-offset-guard.sh's client_chase_bound_us
# for the full derivation (effective = max(GATE_BOUND_US, min(ntp_deadband_us, this) +
# GATE_DEADBAND_MARGIN_US), the SAME margin flag #1021 already exposes, reused rather than a new
# one since both widenings cover the same physical oscillator-overshoot mechanism). Default 5000
# -- the ticket's own cited "upstream hard per-step ceiling", the documented maximum size of any
# single master step, so an absurd/misconfigured ntp_deadband_us can never blindly widen every
# client row to match it (unlike #1021's own uncapped master-only formula, which only ever
# widens ONE row).
GATE_CLIENT_CHASE_CEILING_US="${DANTESYNC_CLIENT_CHASE_CEILING_US:-5000}"
# #1022 review follow-up: read_master_chase_status's priming read runs SYNCHRONOUSLY in main(),
# before any per-node grade_http_node job is dispatched (#836's concurrent-sampling design starts
# only after this returns) -- so its own --max-time deliberately does NOT reuse the full
# GATE_WIN_HTTP_TIMEOUT (10s default): worst case (master unreachable) that would add up to a
# full extra 10s of pure serial delay ahead of the concurrent per-node phase, eroding #836's "total
# gate time stays close to ONE window" property, for a read whose ONLY purpose is an optional
# bound widening the gate runs correctly without (client rows simply keep the unwidened bound on a
# failed/timed-out priming read, exactly like an absent/null deadband). A short, dedicated timeout
# bounds that worst case tightly without touching the concurrency model.
GATE_MASTER_CHASE_TIMEOUT_S="${DANTESYNC_MASTER_CHASE_TIMEOUT_S:-3}"
# #1022 spread-side completion: a live E2E rerun showed the SAME master step-chase that the
# median fix (client_chase_bound_us, above) handles can ALSO inflate a CLIENT row's SPREAD past
# GATE_STABILITY_US, even though the median stays correctly in-bound -- and because the step is
# on ONE clock shared by the whole fleet, the SAME step tripped multiple clients (cam1, cam2,
# stream) simultaneously in one real run. When a client's verdict is "unstable" (median in bound,
# spread not) AND its worst sample still fits inside the SAME bound (clock-offset-guard.sh's
# should_resample_for_chase), grade_http_node takes ONE fresh resample round after this delay
# before failing -- never a retry loop; a resample that is ALSO unstable still fails. Default 15s
# gives the transient a good chance to have cleared (the original #1022 filing described the
# per-client catch-up/chase window itself as lasting "~10-30s").
GATE_CHASE_RESAMPLE_DELAY_S="${DANTESYNC_CHASE_RESAMPLE_DELAY_S:-15}"

# read_linux_node_journal NAME IP -> that Linux node's latest DanteSync journald lines over SSH,
# or "" if unreachable. Overridable for tests/offline via DANTESYNC_GATE_LINUX_JOURNAL_<NAME>
# (file path; NAME uppercased AND any "-" mapped to "_" so a hyphenated node name like "imag-nb"
# still yields a valid shell variable name, e.g. cam1 -> DANTESYNC_GATE_LINUX_JOURNAL_CAM1,
# imag-nb -> DANTESYNC_GATE_LINUX_JOURNAL_IMAG_NB) -- mirrors clock-offset-painter-gate.sh's
# read_painter_journal()/DEV1_DANTE_JOURNAL pattern (#608), so this gate's Linux SSH-gather path
# can be proven end-to-end offline instead of only indirectly via the shared
# dantesync_offset_verdict unit tests. Read-only; a down/absent daemon (or an unset override)
# collapses to empty output (caller maps empty -> UNKNOWN, never a silent pass).
read_linux_node_journal() {
  local name="$1" ip="$2" var
  var="DANTESYNC_GATE_LINUX_JOURNAL_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  # -o short-iso (+ a wider -n 400 window) so dantesync_offset_verdict can prove freshness
  # (#550/#595) -- the age-blind offset_us_from_journal/offset_check this loop used to call could
  # grade a stale multi-hour-old boot-STEP line as "the current offset".
  sshpass -p "${CLOCK_GUARD_SSH_PASS}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no -o "ConnectTimeout=${GATE_SSH_TIMEOUT}" \
    "${CLOCK_GUARD_SSH_USER}@${ip}" \
    'journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null' 2>/dev/null || true
}

# read_linux_node_http_status NAME IP -> that Linux camera's LIVE DanteSync status JSON, fetched
# from dantesync#47's own network endpoint (http://IP:GATE_WIN_HTTP_PORT/status, #648) -- the SAME
# authoritative signal read_win_http_status already reads for the Windows boxes, deployed
# fleet-wide and verified responding on cam1-cam6, 2026-07-11.
#
# #686: this is now the gate's PRIMARY signal for a Linux node (tried BEFORE the journal parser).
# Regression: after #679 throttled DanteSync's periodic journal reports (~1-in-30), the servo
# "[PTP] (NANO|LOCK) Drift:" lines and the "[NTP] offset:" lines land at nearly the SAME cadence,
# so ptp_locked_from_journal's "whichever logged last wins" POSITION comparison can flip a
# genuinely LOCKED node to DEGRADED with roughly coin-flip odds -- observed live, cam2 flip-
# flopped LOCKED->DEGRADED->LOCKED across three gate runs within 20 minutes while its own
# :8898/status continuously reported is_locked:true, mode NANO/LOCK. HTTP carries the daemon's
# own CURRENT is_locked/mode/ntp_offset_us/updated_ts directly -- no log-cadence ambiguity.
#
# "" if unreachable / non-200 (curl -fsS fails closed on either) -- the caller then FALLS BACK to
# read_linux_node_journal. That fallback is ONLY for the HTTP endpoint being unreachable/disabled
# -- never a second opinion once HTTP has answered (a reachable-but-STALE HTTP payload must fail
# the gate, not silently fall through to a possibly-misleading journal read).
#
# Overridable for tests/offline via DANTESYNC_GATE_LINUX_HTTP_<NAME> (NAME uppercased, "-" -> "_")
# -- mirrors read_win_http_status's DANTESYNC_GATE_WIN_HTTP_<NAME> and read_linux_node_journal's
# DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seams above. #836: this function is now
# called MULTIPLE times per gate run (gather_http_samples, below) to sample the node instead of
# reading it once -- so the override may point at either a STATIC file (cat'd every call, the
# pre-#836 behavior, useful for a fixture that legitimately never varies) OR an EXECUTABLE script
# (run every call, so a test fixture can return DIFFERENT content on successive invocations
# without any real network or sleep -- see tests/dantesync_gate.rs's write_multi_read_fixture).
# TRUST BOUNDARY: this is a TEST/OFFLINE-ONLY seam -- the env var is never set in a real gate run
# (production reads always fall through to the plain `curl` below), so widening it from "cat a
# file" to "run a file" adds no attacker-reachable surface; only a caller who already controls
# this process's environment (the same caller who could already point CLOCK_GUARD_SSH_PASS,
# CLOCK_GUARD_JOURNAL_OVERRIDE, etc. anywhere they like) can use it.
read_linux_node_http_status() {
  local name="$1" ip="$2" var
  var="DANTESYNC_GATE_LINUX_HTTP_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    if [ -x "${!var}" ]; then
      "${!var}" 2>/dev/null || true
    else
      cat "${!var}" 2>/dev/null || true
    fi
    return 0
  fi
  curl -fsS --max-time "$GATE_WIN_HTTP_TIMEOUT" "http://${ip}:${GATE_WIN_HTTP_PORT}/status" 2>/dev/null || true
}

# read_win_http_status NAME HOST -> that Windows box's LIVE DanteSync status JSON, fetched
# directly from dantesync#47's own network endpoint (http://HOST:GATE_WIN_HTTP_PORT/status,
# #648) — no win-* MCP, no human pre-fetch, so an unattended CI run has a real data source.
# "" if unreachable / non-200 (curl -fsS fails closed on either). Overridable for tests/offline
# via DANTESYNC_GATE_WIN_HTTP_<NAME> (NAME uppercased, "-" -> "_") -- mirrors
# read_linux_node_journal's DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seam above.
# #836: this function is now called MULTIPLE times per gate run (gather_http_samples, below) to
# sample the node instead of reading it once -- so the override may point at either a STATIC file
# (cat'd every call) OR an EXECUTABLE script (run every call, so a test fixture can return
# DIFFERENT content on successive invocations without any real network or sleep). TRUST BOUNDARY:
# same as read_linux_node_http_status above -- test/offline-only, never set in a real gate run.
read_win_http_status() {
  local name="$1" host="$2" var
  var="DANTESYNC_GATE_WIN_HTTP_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    if [ -x "${!var}" ]; then
      "${!var}" 2>/dev/null || true
    else
      cat "${!var}" 2>/dev/null || true
    fi
    return 0
  fi
  curl -fsS --max-time "$GATE_WIN_HTTP_TIMEOUT" "http://${host}:${GATE_WIN_HTTP_PORT}/status" 2>/dev/null || true
}

# read_master_chase_status IP -> ONE fresh status-JSON read of the CONFIGURED NTP master's own
# /status endpoint (http://IP:GATE_WIN_HTTP_PORT/status, #648/#686), used SOLELY to derive the
# #1022 CLIENT "chase envelope" bound (clock-offset-guard.sh's client_chase_bound_us) BEFORE
# dispatching the per-node grading jobs (main(), below).
#
# DELIBERATELY a SEPARATE read/override from read_win_http_status/read_linux_node_http_status's
# own DANTESYNC_GATE_{WIN,LINUX}_HTTP_<NAME> seams: those are exercised in tests via an
# EXECUTABLE multi-read fixture with a shared per-node call counter (#836) that several already-
# proven #1014/#1021 tests depend on for an EXACT sampled sequence. An extra priming call routed
# through that SAME per-node override would silently consume one of the counted calls and shift
# the master's OWN sampled sequence by one, corrupting those tests. Overridable via
# DANTESYNC_GATE_MASTER_DEADBAND_STATUS (a static file OR an executable script, same "cat or
# exec" convention as every other override in this file -- ntp_deadband_us changes slowly, so a
# per-call executable fixture is never actually needed here, but the convention costs nothing to
# keep). Unset in a real gate run, where this always does a genuine live curl.
#
# "" on ANY failure (unreachable, non-200, unset override + unreachable IP) -- never an error.
# client_chase_bound_us treats an empty/unparseable status exactly like a pre-dantesync-#84
# payload and falls back to the UNMODIFIED bound (#1022, the same "cannot prove it -> do not
# widen" discipline every other fallback in this file follows).
#
# Uses GATE_MASTER_CHASE_TIMEOUT_S (default 3s), NOT the full GATE_WIN_HTTP_TIMEOUT (10s) every
# other read here uses -- this read runs SYNCHRONOUSLY before the concurrent per-node sampling
# phase even starts (#1022 review follow-up), so a short, dedicated timeout bounds its worst-case
# added latency tightly instead of costing up to a full extra GATE_WIN_HTTP_TIMEOUT on an
# unreachable master.
read_master_chase_status() {
  local ip="$1"
  if [ -n "${DANTESYNC_GATE_MASTER_DEADBAND_STATUS:-}" ]; then
    if [ -x "$DANTESYNC_GATE_MASTER_DEADBAND_STATUS" ]; then
      "$DANTESYNC_GATE_MASTER_DEADBAND_STATUS" 2>/dev/null || true
    else
      cat "$DANTESYNC_GATE_MASTER_DEADBAND_STATUS" 2>/dev/null || true
    fi
    return 0
  fi
  curl -fsS --max-time "$GATE_MASTER_CHASE_TIMEOUT_S" "http://${ip}:${GATE_WIN_HTTP_PORT}/status" 2>/dev/null || true
}

# gather_http_samples READ_FN NAME ARG N WINDOW_S -> newline-joined raw status-JSON payloads from
# up to N sequential calls to READ_FN NAME ARG (#836: a single read of a noisy node is close to a
# coin flip -- see clock-offset-guard.sh's own header for the live data). Reads are spaced evenly
# across roughly WINDOW_S seconds (spacing = WINDOW_S/(N-1) seconds, floor; WINDOW_S=0 skips all
# spacing -- the offline/test seam, since a fixture doesn't need real elapsed time to vary its own
# output). If the VERY FIRST read is empty (the node's HTTP endpoint is simply not there), returns
# immediately with NO further attempts -- exactly the pre-#836 single-read "unreachable" behavior,
# so a genuinely-down endpoint fails just as fast as before instead of waiting out N timeouts for
# a node we already know isn't responding. A LATER empty read (transient) is silently dropped from
# the sequence; the caller's own MIN_DISTINCT gate (sampled_offset_verdict) decides whether what's
# left is enough to grade.
gather_http_samples() {
  local read_fn="$1" name="$2" arg="$3" n="$4" window="$5"
  local i space out="" one
  one="$("$read_fn" "$name" "$arg")"
  if [ -z "$one" ]; then
    printf ''
    return 0
  fi
  out="${one}"$'\n'
  space=0
  if [ "$n" -gt 1 ]; then
    space=$(( window / (n - 1) ))
  fi
  for ((i = 2; i <= n; i++)); do
    if [ "$space" -gt 0 ]; then
      sleep "$space"
    fi
    one="$("$read_fn" "$name" "$arg")"
    if [ -n "$one" ]; then
      out+="${one}"$'\n'
    fi
  done
  printf '%s' "$out"
}

# grade_http_node READ_FN NAME ARG KIND BOUND STABILITY MIN_DISTINCT SAMPLES WINDOW FRESHNESS_S
#   VERDICTFILE [MODE] [DEADBAND_MARGIN_US] [CLIENT_NOTE] [RESAMPLE_DELAY_S]
# -> samples + grades ONE node (the shared body for both the Linux-HTTP-first loop and the
# Windows --win-http loop -- extracted so the two loops don't carry two independently-editable
# copies of the same freshness -> sampled_offset_check -> ptp_check -> node_verdict sequence).
# Prints the node's human-readable status line(s) to STDOUT (unchanged wording/order from before
# this function existed) and writes its final OK/BAD/UNKNOWN verdict word to VERDICTFILE -- a
# SEPARATE channel from stdout, specifically so this function can be run inside a backgrounded
# subshell (`grade_http_node ... > "$outfile" &`, see the main loop below) with the verdict still
# recoverable after `wait` without parsing it back out of the captured report text. KIND is
# "linux" (tries the HTTP status endpoint first, falls back to read_linux_node_journal (#608) on
# total HTTP unreachability, #686) or "win" (HTTP only, #648, no journal to fall back to).
#
# #836 review follow-up: sampling a node now takes real wall-clock time (up to WINDOW seconds),
# and nodes are independent -- grading them one after another the way the pre-extraction code did
# multiplies that window by the node count (4 nodes x 30s window = 2 minutes). Backgrounding one
# call per node (below) instead runs every node's sampling window CONCURRENTLY, so the total gate
# time stays close to ONE window regardless of node count. This function's own body is completely
# unchanged sampling/grading LOGIC -- only where it is invoked from changed.
grade_http_node() {
  local read_fn="$1" name="$2" arg="$3" kind="$4" bound="$5" stability="$6" min_distinct="$7"
  local samples="$8" window="$9" freshness="${10}" verdictfile="${11}" mode="${12:-full}"
  local deadband_margin="${13:-0}" client_note="${14:-}" resample_delay="${15:-0}"
  local status samples_raw now rc_off rc_ptp ptp deadband_note="" resampled=0

  samples_raw="$(gather_http_samples "$read_fn" "$name" "$arg" "$samples" "$window")"
  if [ -z "$samples_raw" ]; then
    if [ "$kind" = "linux" ]; then
      # HTTP unreachable/disabled -> FALLBACK to the journal parser (unchanged pre-#686 path).
      status="$(read_linux_node_journal "$name" "$arg")"
      if [ -z "$status" ]; then
        printf '  %-14s UNREACHABLE  (no DanteSync HTTP @ %s:%s/status nor journal over SSH)\n' \
          "$name" "$arg" "$GATE_WIN_HTTP_PORT"
        printf 'UNKNOWN' > "$verdictfile"
        return 0
      fi
      ptp="$(ptp_locked_from_journal "$status")"
      rc_off=0
      case "$(dantesync_offset_verdict "$status" "$freshness" "$bound")" in
        ok)
          printf '  %-14s NTP OK       (fresh offset within %s us bound)\n' "$name" "$bound" ;;
        drift)
          printf '  %-14s NTP DRIFT    (fresh offset exceeds %s us bound)\n' "$name" "$bound"
          rc_off=2 ;;
        stale)
          printf '  %-14s NTP STALE    (no FRESH [NTP] offset within %ss -- status incomplete, #550/#595)\n' \
            "$name" "$freshness"
          rc_off=3 ;;
        *)
          printf '  %-14s NTP UNKNOWN  (no [NTP] offset line at all -- status incomplete)\n' "$name"
          rc_off=3 ;;
      esac
      rc_ptp=0; ptp_check "$name" "$ptp" || rc_ptp=$?
      printf '%s' "$(node_verdict "$rc_off" "$rc_ptp")" > "$verdictfile"
      return 0
    fi
    printf '  %-14s UNREACHABLE  (no DanteSync HTTP status @ %s:%s/status -- #648)\n' \
      "$name" "$arg" "$GATE_WIN_HTTP_PORT"
    printf 'UNKNOWN' > "$verdictfile"
    return 0
  fi

  status="$(printf '%s\n' "$samples_raw" | tail -1)"   # most recent payload -> freshness/PTP
  # #1021: ONLY the median-only (NTP master) node's own median bound ever widens ITSELF here, and
  # only when its freshest payload carries a numeric ntp_deadband_us -- see clock-offset-guard.sh's
  # ntp_master_effective_bound_us doc comment for the full derivation. Absent/null/non-numeric
  # falls back to the unmodified $bound (exact pre-#1021 behavior).
  #
  # #1022: a CLIENT node (mode "full") never widens itself here -- its ALREADY-widened bound (if
  # any) was computed ONCE in main() (client_chase_bound_us, derived from the master's OWN live
  # deadband) and handed in as this call's own $bound argument, with the matching human-readable
  # explanation handed in as $client_note. There is nothing left to compute per-node; just surface
  # the note main() already built, on the SAME line sampled_offset_check prints (mirroring how the
  # median-only branch above avoids a second "NTP MASTER:"-prefixed line).
  if [ "$mode" = "median-only" ]; then
    local orig_bound="$bound"
    bound="$(ntp_master_effective_bound_us "$status" "$bound" "$deadband_margin")"
    if [ "$bound" != "$orig_bound" ]; then
      # No "NTP MASTER:" prefix here -- sampled_offset_check's own median-only-mode note (#1014)
      # already opens with "NTP MASTER:" on this SAME printed line, so repeating it would render
      # as "NTP MASTER: ... -- NTP MASTER: ..." (review finding, #1021).
      deadband_note=" -- bound widened to ${bound}us (dantesync ntp_deadband_us + ${deadband_margin}us margin, #1021; base bound ${orig_bound}us)"
    fi
  elif [ -n "$client_note" ]; then
    deadband_note="$client_note"
  fi
  now="$(date +%s)"   # #836: recompute per node -- sampling itself takes real wall-clock time
  local issue_ref="648"
  if [ "$kind" = "linux" ]; then
    issue_ref="686"
  fi
  rc_off=0
  case "$(pipe_json_freshness_verdict "$status" "$now" "$freshness")" in
    stale)
      printf '  %-14s NTP STALE    (updated_ts older than %ss -- status incomplete, #%s)\n' \
        "$name" "$freshness" "$issue_ref"
      rc_off=3 ;;
    absent)
      printf '  %-14s NTP UNKNOWN  (no updated_ts field -- status incomplete, #%s)\n' \
        "$name" "$issue_ref"
      rc_off=3 ;;
    *)
      # #1014: the STATUS payload as a whole is fresh (the box's HTTP server is alive and
      # self-reporting) -- but that "updated_ts" is PTP-driven and stays fresh even when the NTP
      # MEASUREMENT itself is dead or (dantesync issue 68) intentionally free-running after a
      # one-time startup sync. Grade the NTP measurement's OWN freshness separately, BEFORE
      # trusting its ntp_offset_us as a live value.
      case "$(ntp_freshness_verdict "$status" "$freshness")" in
        fresh)
          # #1022 spread-side completion: a CLIENT row (never the master -- see
          # should_resample_for_chase's own MODE gate) whose verdict is EXACTLY "unstable" (median
          # already in bound, spread not) AND whose worst sample still fits the SAME bound gets
          # ONE fresh resample round before the final grade. A resample that is ALSO unstable
          # still fails, graded on ITS OWN (fresh) numbers.
          if [ "$(should_resample_for_chase "$samples_raw" "$bound" "$stability" "$min_distinct" "$mode")" = "yes" ]; then
            sleep "$resample_delay"
            samples_raw="$(gather_http_samples "$read_fn" "$name" "$arg" "$samples" "$window")"
            status="$(printf '%s\n' "$samples_raw" | tail -1)"
            deadband_note="${deadband_note} -- resampled once after a ${resample_delay}s delay (spread looked like a transient master step-chase excursion, #1022; grading the fresh round)"
            resampled=1
          fi
          # #1022 review follow-up: the resample itself takes real wall-clock time (the delay +
          # another full sampling window) -- long enough for a borderline-fresh NTP measurement to
          # cross into staleness during that gap (the SAME #1014 "frozen/free-running measurement
          # graded as live" class this whole freshness case-statement exists to catch). Re-verify
          # the RESAMPLED data's own freshness before trusting its median/spread -- never grade a
          # measurement that went stale during the wait, even though the ORIGINAL round was proven
          # fresh a moment ago.
          if [ "$resampled" = 1 ] && [ "$(ntp_freshness_verdict "$status" "$freshness")" != "fresh" ]; then
            printf '  %-14s NTP STALE    (NTP measurement went stale during the #1022 resample delay -- status incomplete)\n' \
              "$name"
            rc_off=3
          else
            sampled_offset_check "$name" "$samples_raw" "$bound" "$stability" "$min_distinct" \
              "$mode" "$deadband_note" || rc_off=$?
          fi
          ;;
        stale)
          printf '  %-14s NTP STALE    (NTP measurement age exceeds %ss, or ntp_failed -- status incomplete, dantesync issue 68/71, #1014)\n' \
            "$name" "$freshness"
          rc_off=3 ;;
        never)
          printf '  %-14s NTP UNKNOWN  (NTP never measured -- ntp_age_s null, dantesync issue 68, #1014)\n' \
            "$name"
          rc_off=3 ;;
        absent)
          # Pre-1.8.30 payload -- no ntp_age_s field to grade freshness directly. Fall back to
          # the frozen-sample heuristic this ticket originally proposed: a byte-identical
          # ntp_offset_us across every distinct sample this run already gathered is the frozen-
          # measurement signature; anything else falls through to today's unchanged grading.
          case "$(frozen_sample_verdict "$samples_raw" "$min_distinct")" in
            frozen)
              printf '  %-14s NTP STALE    (ntp_offset_us frozen across %s+ distinct samples -- pre-1.8.30 payload has no ntp_age_s, #1014 frozen-sample fallback)\n' \
                "$name" "$min_distinct"
              rc_off=3 ;;
            *)
              sampled_offset_check "$name" "$samples_raw" "$bound" "$stability" "$min_distinct" \
                "$mode" "${deadband_note} -- pre-1.8.30 payload, no ntp_age_s field, graded via legacy sampled-offset check (#1014)" \
                || rc_off=$? ;;
          esac
          ;;
      esac
      ;;
  esac
  ptp="$(ptp_locked_from_pipe_json "$status")"
  rc_ptp=0; ptp_check "$name" "$ptp" || rc_ptp=$?
  printf '%s' "$(node_verdict "$rc_off" "$rc_ptp")" > "$verdictfile"
}

# resolve_node_grading NAME -> sets node_mode/node_bound/node_note (bash's dynamic scoping means
# these are the CALLER's own `local` variables -- this function never declares them itself, so an
# assignment here resolves to main()'s already-`local` node_mode/node_bound/node_note, exactly
# like any other bash function that mutates a caller's locals by convention) for the grade_http_node
# call for node NAME. #1022 review follow-up: this bootstrap logic (master row = median-only mode
# against the bare bound; every other row = full mode against the once-derived client chase-
# envelope bound + its note) was previously duplicated verbatim between the --linux and --win-http
# dispatch loops below -- extracted here so the master-vs-client decision lives in exactly ONE
# place, the same "one source of truth" rationale the file's own #675/#309 pattern already
# documents for scripts/lib/*.sh helpers.
resolve_node_grading() {
  local name="$1"
  node_mode="full"; node_bound="$bound"; node_note=""
  if [ "$name" = "$GATE_NTP_MASTER_NAME" ]; then
    node_mode="median-only"
  else
    node_bound="$client_bound"; node_note="$client_note"
  fi
}

usage() {
  cat <<EOF
dantesync-gate.sh — recording-E2E NTP+PTP precondition gate (#7).

FAILS FAST unless EVERY measured node is BOTH NTP-synced (|offset| <= bound) AND PTP-locked
(fine servo NANO/LOCK, not the NTP-only sawtooth fallback with GM 10.77.9.184 down). The
recording run must NOT proceed otherwise — cross-node latency/timestamps would be meaningless.

Usage:
  dantesync-gate.sh [--bound-us N] [--linux "name=ip ..."] \
                     [--win-http NAME=HOST ...] [--win-http-port N] \
                     [--samples N] [--window-s S] [--min-distinct N] [--stability-us N] \
                     [--ntp-master NAME]

Options:
  --bound-us N        max tolerated |NTP offset| in us (default ${GATE_BOUND_US}; see #8 rationale).
  --linux "n=ip ..."  Linux nodes -- HTTP status endpoint FIRST, journald-over-SSH fallback
                      (#686; default: ${GATE_LINUX}).
  --win-http N=HOST    a Windows node N queried LIVE over HTTP from dantesync#47's own network
                       status endpoint (http://HOST:PORT/status, #648) -- no win-* MCP, no human
                       pre-fetch; unattended-CI-safe. Repeatable.
  --win-http-port N    port for the HTTP status endpoint (default ${GATE_WIN_HTTP_PORT}) -- shared
                       by --win-http nodes AND the --linux nodes' HTTP-first reads (#686); the
                       whole fleet serves one port, so one knob covers both.
  --samples N          #836: how many times to sample each --win-http / Linux-HTTP-first node's
                       ntp_offset_us instead of reading it once (default ${GATE_SAMPLE_COUNT}) --
                       a single read is close to a coin flip on a noisy node (live data: 2/22
                       reads in-bound on one real box). The gate grades the MEDIAN of the samples
                       against --bound-us (unchanged bound, better estimator) AND the SPREAD of
                       the same samples against --stability-us (a NEW check a single read can
                       never make: a node whose median looks fine but whose readings scatter
                       wildly now FAILS too).
  --window-s S         roughly how many seconds to spread the --samples reads across (default
                       ${GATE_SAMPLE_WINDOW_S}; 0 = no spacing at all, for tests/offline).
  --min-distinct N     minimum DISTINCT (by the payload's own updated_ts) samples required before
                       the gate will grade a node at all (default ${GATE_SAMPLE_MIN_DISTINCT}) --
                       consecutive reads whose updated_ts repeats are the daemon re-serving a
                       cached value between refreshes, not independent measurements, and are
                       never counted; too few distinct samples is itself a hard failure, never a
                       silent pass on one lucky read.
  --stability-us N     max tolerated SPREAD (max-min) of the distinct samples, in us (default
                       ${GATE_STABILITY_US}) -- see --samples above.
  --ntp-master NAME    the --win-http/--linux NAME that is the NTP master (default
                       ${GATE_NTP_MASTER_NAME}) -- #1014: graded on median+freshness ONLY, never
                       --stability-us, because the master's own ntp_offset_us is a by-design
                       UTC-residual correction-lag sawtooth (dantesync issue 71), not a fleet-
                       coherence signal. Every OTHER node keeps the full median+spread/stability
                       bar unchanged. A genuinely drifted master still fails on its median.
                       When at least one --win-http node is configured, NAME must match one of
                       the configured --linux/--win-http node names, or the gate refuses (usage
                       error, 1) -- a typo here (or in the master's own --win-http/--linux NAME=)
                       would otherwise silently grade the INTENDED master with the full spread/
                       stability bar, reintroducing #1014's false-DRIFT through a misspelling.
                       Pass --ntp-master "" to explicitly opt out (no NTP master among this
                       invocation's nodes at all).
  --deadband-margin-us N  #1021 (dantesync PR #84/#86, closes dantesync issue 83): when the NTP
                       master's own /status reports a numeric "ntp_deadband_us" (its currently
                       active PTP-locked step-deferral threshold), the master's median bound
                       widens to max(--bound-us, ntp_deadband_us + N) instead of the bare fixed
                       --bound-us -- a genuinely PTP-locked master deliberately ramps its own
                       ntp_offset_us up toward roughly the deadband between corrections, and the
                       fixed bound alone would false-DRIFT on that by-design behavior. Default
                       ${GATE_DEADBAND_MARGIN_US} (covers the live-observed step overshoot past
                       the deadband before the next correction lands). Absent/null
                       ntp_deadband_us (older dantesync, or any client node) -> unmodified
                       --bound-us, exactly as before #1021.
  --client-chase-ceiling-us N  #1022 (dantesync-gate: client rows can ALSO false-DRIFT during
                       the master's OWN deadband step-chase window -- #1021 explicitly left
                       client rows untouched): a CLIENT (non-master) row's median bound ALSO
                       widens, to max(--bound-us, min(ntp_deadband_us, N) + --deadband-margin-us)
                       -- derived from a ONE-TIME priming read of the CONFIGURED NTP master's own
                       live /status (never the client's OWN payload, which always reports
                       ntp_deadband_us:null). N caps the deadband component BEFORE the margin is
                       added, unlike #1021's own uncapped master-only formula -- #1022 can widen
                       MANY client rows from the SAME live read, so an absurd/misconfigured
                       ntp_deadband_us can never blindly widen every client row to match it.
                       Default ${GATE_CLIENT_CHASE_CEILING_US} (the documented maximum size of
                       any single master step). Only applies when a master is CONFIGURED among
                       this invocation's --linux/--win-http nodes (with a non-empty host/IP --
                       --ntp-master "" opts out entirely, a name matching no configured node, OR
                       a matching name whose OWN --win-http/--linux entry has an empty host/IP,
                       e.g. "--win-http strih=", ALL skip the priming read the same way) AND at
                       least one OTHER node is also configured (a master-only invocation never
                       pays the priming read); an unreachable/unreadable priming read falls back
                       to the unmodified --bound-us, same "cannot prove it -> do not widen"
                       discipline as an absent/null deadband.
  --chase-resample-delay-s N  #1022 spread-side completion: the SAME master step-chase can ALSO
                       inflate a CLIENT row's SPREAD past --stability-us even though its median
                       stays correctly in-bound -- and because the step is on ONE clock shared by
                       the fleet, the SAME step can trip MULTIPLE clients simultaneously in one
                       run. A client row whose verdict is EXACTLY "unstable" (median in bound,
                       spread not) AND whose worst sample still fits inside its OWN effective
                       bound (the SAME per-node bound the median check already uses -- for a
                       client row that may already be the #1022-widened
                       --client-chase-ceiling-us envelope, not necessarily the bare --bound-us
                       value) gets ONE fresh resample round after this delay before the final
                       grade -- never a retry loop; a resample that is ALSO unstable still fails,
                       graded on its own fresh numbers, and a resample whose NTP measurement goes
                       stale during the wait reports STALE rather than trusting aged-out data. The
                       master's own median-only row, a genuine "drift" (median out of bound), and
                       a worst sample that already exceeds the effective bound (the #836 genuine-
                       scatter class, or a real clock fault) are NEVER resampled -- see
                       clock-offset-guard.sh's should_resample_for_chase for the exact decision.
                       Default ${GATE_CHASE_RESAMPLE_DELAY_S} (gives the transient a good chance
                       to have cleared -- the original filing described the per-client catch-up
                       window as lasting "~10-30s").

A node's NTP MEASUREMENT itself must also be FRESH, independently of the payload's general
"updated_ts" (#1014, dantesync v1.8.30 / dantesync issue 68): "ntp_age_s" must be a plain integer
no older than the freshness window, and "ntp_failed" must not be true, or the reading is STALE ->
UNKNOWN, never graded as a live offset. "ntp_age_s":null means NEVER measured -> UNKNOWN. A
payload predating v1.8.30 (no ntp_age_s field at all) falls back to detecting a FROZEN
ntp_offset_us across this run's own sampled reads.

#836: net effect vs the old single-read gate is strictly MORE ways to fail, never fewer -- the
location bound (--bound-us) itself never moves; sampling only ADDS the spread/stability check and
the insufficient-distinct-samples check. Every --win-http / Linux-HTTP-first status line now
reports the median, the spread, and the distinct sample count, so a red line says which kind of
bad it is: an out-of-bound median, an unstable spread, or too few distinct reads.

#686: LINUX nodes now try the SAME network status endpoint (http://IP:PORT/status) FIRST --
authoritative, immune to journal log-cadence throttling (#679). The journal parser is the
FALLBACK, used ONLY when a Linux node's HTTP endpoint is unreachable/disabled -- never a second
opinion once HTTP has answered (a reachable-but-stale HTTP payload fails the gate, it does not
fall through to the journal).

A Linux node's NTP offset must be FRESH, not just in-bound. Via HTTP (the primary path, #686):
its "updated_ts" field must be no older than DANTESYNC_OFFSET_FRESHNESS_S seconds behind the
gate's own wall clock, exactly like a --win-http node (#648). Via the journal (the fallback
path): the freshest "[NTP] offset:" journal line must be no older than
DANTESYNC_OFFSET_FRESHNESS_S (default ${GATE_OFFSET_FRESHNESS_S}) seconds behind that node's
newest journal line -- see dantesync_offset_verdict() in clock-offset-guard.sh (#550/#591/#595).
Either way a stale reading is STALE -> UNKNOWN, never a silent OK. (#835: the old Windows-node
FILE-RELAY path, which WAS age-blind, was removed outright -- --win-http covers the same nodes
and always grades freshness.)

Exit: 0 = all nodes NTP+PTP OK, 20 = a node DRIFTED or PTP-DEGRADED, 11 = a node UNREACHABLE/
UNKNOWN, 1 = usage error.
EOF
}

main() {
  local bound="$GATE_BOUND_US" linux="$GATE_LINUX" win_http_port="$GATE_WIN_HTTP_PORT"
  local samples="$GATE_SAMPLE_COUNT" window="$GATE_SAMPLE_WINDOW_S"
  local min_distinct="$GATE_SAMPLE_MIN_DISTINCT" stability="$GATE_STABILITY_US"
  local ntp_master="$GATE_NTP_MASTER_NAME" deadband_margin="$GATE_DEADBAND_MARGIN_US"
  local client_chase_ceiling="$GATE_CLIENT_CHASE_CEILING_US"
  local chase_resample_delay="$GATE_CHASE_RESAMPLE_DELAY_S"
  local -a win_http=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --bound-us)           shift; bound="${1:-}" ;;
      --linux)              shift; linux="${1:-}" ;;
      --win-http)           shift; win_http+=("${1:-}") ;;
      --win-http-port)      shift; win_http_port="${1:-}" ;;
      --samples)            shift; samples="${1:-}" ;;
      --window-s)           shift; window="${1:-}" ;;
      --min-distinct)       shift; min_distinct="${1:-}" ;;
      --stability-us)       shift; stability="${1:-}" ;;
      --ntp-master)         shift; ntp_master="${1:-}" ;;
      --deadband-margin-us) shift; deadband_margin="${1:-}" ;;
      --client-chase-ceiling-us) shift; client_chase_ceiling="${1:-}" ;;
      --chase-resample-delay-s) shift; chase_resample_delay="${1:-}" ;;
      -h|--help)            usage; exit 0 ;;
      --*)             echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)               echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done
  GATE_NTP_MASTER_NAME="$ntp_master"

  if ! printf '%s' "$bound" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --bound-us must be a positive integer (got '${bound}')." >&2
    exit 1
  fi
  if ! printf '%s' "$win_http_port" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --win-http-port must be a positive integer (got '${win_http_port}')." >&2
    exit 1
  fi
  GATE_WIN_HTTP_PORT="$win_http_port"
  if ! printf '%s' "$samples" | grep -qE '^[0-9]+$' || [ "$samples" -lt 1 ]; then
    echo "ERROR: --samples must be a positive integer (got '${samples}')." >&2
    exit 1
  fi
  if ! printf '%s' "$window" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --window-s must be a non-negative integer (got '${window}')." >&2
    exit 1
  fi
  if ! printf '%s' "$min_distinct" | grep -qE '^[0-9]+$' || [ "$min_distinct" -lt 1 ]; then
    echo "ERROR: --min-distinct must be a positive integer (got '${min_distinct}')." >&2
    exit 1
  fi
  if ! printf '%s' "$stability" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --stability-us must be a non-negative integer (got '${stability}')." >&2
    exit 1
  fi
  if ! printf '%s' "$deadband_margin" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --deadband-margin-us must be a non-negative integer (got '${deadband_margin}')." >&2
    exit 1
  fi
  if ! printf '%s' "$client_chase_ceiling" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --client-chase-ceiling-us must be a non-negative integer (got '${client_chase_ceiling}')." >&2
    exit 1
  fi
  if ! printf '%s' "$chase_resample_delay" | grep -qE '^[0-9]+$'; then
    echo "ERROR: --chase-resample-delay-s must be a non-negative integer (got '${chase_resample_delay}')." >&2
    exit 1
  fi
  if [ "$min_distinct" -gt "$samples" ]; then
    echo "ERROR: --min-distinct (${min_distinct}) cannot exceed --samples (${samples}) --" \
      "no node could ever gather that many distinct reads (#836)." >&2
    exit 1
  fi
  if ! command -v sshpass >/dev/null 2>&1; then
    echo "ERROR: sshpass not found — required to query the Linux cam DanteSync over SSH." >&2
    exit 1
  fi

  local -a linux_pairs=()
  set -f
  # shellcheck disable=SC2206
  linux_pairs=($linux)
  set +f
  # #686: Linux nodes now try the network status endpoint FIRST too (read_linux_node_http_status),
  # not just --win-http nodes -- curl is required whenever EITHER array is non-empty.
  if { [ "${#linux_pairs[@]}" -gt 0 ] || [ "${#win_http[@]}" -gt 0 ]; } && ! command -v curl >/dev/null 2>&1; then
    echo "ERROR: curl not found — required to query DanteSync status over HTTP (#648/#686)." >&2
    exit 1
  fi
  if [ "${#linux_pairs[@]}" -eq 0 ] && [ "${#win_http[@]}" -eq 0 ]; then
    echo "ERROR: no nodes to gate (--linux and --win-http are both empty)." >&2
    echo "The recording-E2E gate cannot certify the cluster with zero nodes — refusing to pass." >&2
    exit 1
  fi

  # #1014 review follow-up: a typo'd --ntp-master / DANTESYNC_NTP_MASTER_NAME (or a typo'd
  # --win-http NAME=HOST for the box that WAS meant to be the master, e.g. "strhi" instead of
  # "strih") must never silently fall back to grading the intended master with the full
  # spread/stability bar -- that is the EXACT false-DRIFT this ticket exists to fix, now
  # reachable again through a typo instead of an old payload shape. Only checked when at least
  # one --win-http node is configured (the master, strih, is exclusively a Windows/--win-http
  # node on this rig -- a pure --linux-only invocation has no master concept in play at all) AND
  # GATE_NTP_MASTER_NAME is non-empty (an explicit --ntp-master "" opts OUT of the master concept
  # entirely -- e.g. a test that only cares about generic client grading).
  if [ -n "$GATE_NTP_MASTER_NAME" ] && [ "${#win_http[@]}" -gt 0 ]; then
    local master_found=0 check_pair check_name
    for check_pair in "${linux_pairs[@]}" "${win_http[@]}"; do
      check_name="${check_pair%%=*}"
      if [ "$check_name" = "$GATE_NTP_MASTER_NAME" ]; then
        master_found=1
        break
      fi
    done
    if [ "$master_found" -eq 0 ]; then
      echo "ERROR: --ntp-master '${GATE_NTP_MASTER_NAME}' matches NO configured --linux/--win-http node." >&2
      echo "A typo here silently grades the INTENDED master with the full spread/stability bar --" >&2
      echo "the exact false-DRIFT class #1014 fixed, reachable again through a misspelled name." >&2
      echo "Fix the --ntp-master/--win-http/--linux spelling, or pass --ntp-master \"\" if this" >&2
      echo "invocation genuinely has no NTP master among its nodes." >&2
      exit 1
    fi
  fi

  # #1022: derive the CLIENT chase-envelope bound ONCE, before dispatching any per-node job --
  # every client row in this gate run shares the SAME envelope (one live read of the master's
  # own /status). Only attempted when a master is genuinely CONFIGURED among this invocation's
  # nodes with a non-empty host/IP (an opted-out `--ntp-master ""`, a master name matching
  # nothing, OR a matching name whose OWN entry has an empty host/IP e.g. "strih=" -- all leave
  # master_arg empty -- never trigger this; client_bound/client_note simply stay the unmodified
  # base values in every such case) AND at least one OTHER (client) node is also configured -- a
  # master-only invocation (every #1014/#1021 test) never pays the extra priming read at all.
  local client_bound="$bound" client_note=""
  if [ -n "$GATE_NTP_MASTER_NAME" ]; then
    local master_arg="" other_node_count=0 lookup_pair lookup_name
    for lookup_pair in "${linux_pairs[@]}" "${win_http[@]}"; do
      lookup_name="${lookup_pair%%=*}"
      if [ "$lookup_name" = "$GATE_NTP_MASTER_NAME" ]; then
        master_arg="${lookup_pair#*=}"
      else
        other_node_count=$((other_node_count + 1))
      fi
    done
    if [ -n "$master_arg" ] && [ "$other_node_count" -gt 0 ]; then
      local master_chase_status
      master_chase_status="$(read_master_chase_status "$master_arg")"
      client_bound="$(client_chase_bound_us "$master_chase_status" "$bound" "$deadband_margin" "$client_chase_ceiling")"
      if [ "$client_bound" != "$bound" ]; then
        client_note=" -- bound widened to ${client_bound}us for the master's own PTP-locked step-chase envelope (${GATE_NTP_MASTER_NAME}'s ntp_deadband_us capped at ${client_chase_ceiling}us + ${deadband_margin}us margin, #1022; base bound ${bound}us)"
      fi
    fi
  fi

  echo "== dantesync-gate (#7): recording-E2E precondition — NTP within ${bound} us AND PTP LOCKED =="
  echo "   GM = 10.77.9.184 (PTP grandmaster); NTP master = ${GATE_NTP_MASTER_NAME}; degraded PTP => meaningless latency"
  echo "   #1014: ${GATE_NTP_MASTER_NAME} is graded on median+freshness only (its spread is a by-design"
  echo "   correction-lag sawtooth, dantesync issue 71); every other node keeps the full spread/stability bar."

  local bad=0 unknown=0 ok=0 name ip node_mode node_bound node_note

  # #836 review follow-up: sampling a node now takes up to WINDOW seconds (was instant with the
  # old single read). Nodes are independent, so gather+grade every node CONCURRENTLY instead of
  # one after another -- each node's grade_http_node call runs in its OWN backgrounded subshell,
  # writing its human-readable report to a tmp file and its OK/BAD/UNKNOWN verdict to a sibling
  # tmp file; the parent waits for every job, then replays each report IN THE SAME ORDER as
  # before (Linux nodes first, then Windows nodes, each in their given order -- deterministic
  # output byte-for-byte) and tallies the verdicts. Total wall time is now close to ONE sampling
  # window regardless of node count, instead of window x node_count.
  local tmpdir
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT

  local -a job_outfiles=() job_verdictfiles=()
  local idx=0 outfile vfile

  # --- Linux nodes (HTTP status endpoint FIRST -- #686; journald over SSH as FALLBACK) ------
  local pair
  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"; ip="${pair#*=}"
    outfile="$tmpdir/$idx.out"; vfile="$tmpdir/$idx.verdict"
    resolve_node_grading "$name"
    grade_http_node read_linux_node_http_status "$name" "$ip" linux \
      "$node_bound" "$stability" "$min_distinct" "$samples" "$window" "$GATE_OFFSET_FRESHNESS_S" \
      "$vfile" "$node_mode" "$deadband_margin" "$node_note" "$chase_resample_delay" > "$outfile" &
    job_outfiles+=("$outfile"); job_verdictfiles+=("$vfile")
    idx=$((idx + 1))
  done

  # --- Windows nodes fetched LIVE over HTTP (dantesync#47's network status endpoint, #648) ----
  # No win-* MCP, no human pre-fetch -- the whole point of #648 is an unattended CI run has
  # neither. (#835: this is now the ONLY Windows-node input path -- the old --win-status
  # file-relay path, which was age-blind, was removed.)
  local entry
  for entry in "${win_http[@]}"; do
    name="${entry%%=*}"; ip="${entry#*=}"
    outfile="$tmpdir/$idx.out"; vfile="$tmpdir/$idx.verdict"
    resolve_node_grading "$name"
    grade_http_node read_win_http_status "$name" "$ip" win \
      "$node_bound" "$stability" "$min_distinct" "$samples" "$window" "$GATE_OFFSET_FRESHNESS_S" \
      "$vfile" "$node_mode" "$deadband_margin" "$node_note" "$chase_resample_delay" > "$outfile" &
    job_outfiles+=("$outfile"); job_verdictfiles+=("$vfile")
    idx=$((idx + 1))
  done

  # `wait`'s own exit status is the LAST job's exit status -- irrelevant here (each node's
  # verdict is read back from its own file below, not from the job's return code) and must never
  # abort this script under `set -e` if some job happens to exit non-zero.
  wait || true

  local i verdict
  for ((i = 0; i < idx; i++)); do
    cat "${job_outfiles[$i]}"
    verdict="$(cat "${job_verdictfiles[$i]}" 2>/dev/null || printf 'UNKNOWN')"
    case "$verdict" in
      OK) ok=$((ok + 1)) ;;
      BAD) bad=$((bad + 1)) ;;
      *) unknown=$((unknown + 1)) ;;
    esac
  done

  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} node(s) DRIFTED or PTP-DEGRADED." >&2
    echo "!! Cross-node latency/timestamps would be MEANINGLESS — recording run REFUSED." >&2
    echo "!! Bring GM 10.77.9.184 up + let DanteSync re-lock (NANO/LOCK), then re-run." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further node(s) UNREACHABLE/UNKNOWN — also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} node(s) UNREACHABLE or status UNKNOWN — NOT clean." >&2
    echo "!! Every measured node must report NTP+PTP before recording. (${ok} node(s) were OK.)" >&2
    exit 11
  fi
  echo "GATE PASS — ${ok} node(s) NTP-synced AND PTP-locked. Cross-node latency is meaningful; proceed."
  exit 0
}

# node_verdict OFFSET_RC PTP_RC -> OK | BAD | UNKNOWN. A node passes ONLY when BOTH the offset
# check (rc 0) AND the PTP-lock check (rc 0) pass. A DRIFT/DEGRADED (rc 2) on either => BAD. Any
# UNKNOWN (rc 3) with no hard failure => UNKNOWN. (Hard failure dominates UNKNOWN so a degraded
# node is reported as the actionable failure, not masked as merely "unknown".)
node_verdict() {
  local off="$1" ptp="$2"
  if [ "$off" = 2 ] || [ "$ptp" = 2 ]; then printf 'BAD'; return 0; fi
  if [ "$off" = 3 ] || [ "$ptp" = 3 ]; then printf 'UNKNOWN'; return 0; fi
  printf 'OK'
}

# Source-guard: when sourced by the unit tests, expose the functions and stop (do not run main).
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

main "$@"
