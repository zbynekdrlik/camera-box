#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (network-reach-health.sh, obs-watchdog-decision.sh,
# optical-chain-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (frozen-input-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/frozen-input-health.sh -- #1052 (network-reach watchdog PHASE 2): the SHARED, PURE
# decision core for the dev1-side STREAM FROZEN-INPUT alert watchdog. No I/O, no ssh, no OBS, so it
# can be unit-tested exhaustively (mirrors scripts/lib/network-reach-health.sh).
#
# WHY (#1052, phase-2 of #1001): reachability (#1001) only classifies a box REACHABLE/UNREACHABLE --
# it is blind to a box that is fully REACHABLE while its NDI input is silently frozen on the last
# frame (the #767 receiver-rebind class: the cambox is fine but stream's DistroAV receiver froze).
# The stream OBS log prints `genlock-fifo audit '<src>': received=N ...` ~every 5 s; `received=` is
# the cumulative count of frames the FIFO RECEIVED from the source, so a frozen input STOPS advancing
# it (immune to the static-content false-positive that a screenshot-hash tap suffers). The dev1-side
# watchdog samples the newest `received=` per watched source each pass, PERSISTS it, and compares to
# the prior pass -- this lib turns (prev, curr, expected_live, sender_reachable) into the verdict.
# The confirm-counter + alert throttle stay the SAME shared obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use.
#
# Source-only: pure functions, no side effects at source time.

# frozen_input_classify <prev_received> <curr_received> <expected_live 0|1> <sender_reachable 0|1>
#   -> stdout: FROZEN | ADVANCING | UNKNOWN | SKIP
#   Decides a single watched source from two consecutive per-pass `received=` samples.
#     SKIP      -- out of scope this pass: the input is not expected live (an idle/paused input may
#                  legitimately stop), OR the sender box is not reachable (issue-1001 already owns the
#                  page -- never double-page a downstream symptom). Checked FIRST, before any counter
#                  comparison. ANY value other than "1" for either flag is treated as out-of-scope.
#     UNKNOWN   -- nothing to decide: no prior sample (first pass), an unreadable current sample (the
#                  audit line was absent / the log read failed), or curr < prev (the cumulative
#                  counter reset because OBS restarted). Reseed; NEVER page on a missing/reset reading.
#     ADVANCING -- curr > prev: the input received new frames this window -> healthy.
#     FROZEN    -- curr == prev (both numeric): the received counter did not move across the pass
#                  while the input is expected live and the box is reachable -> frozen on last frame.
frozen_input_classify() {
  local prev="${1:-}" curr="${2:-}" expected_live="${3:-0}" sender_reachable="${4:-0}"

  # Out-of-scope gates first (independent of the counter values).
  if [ "$expected_live" != "1" ] || [ "$sender_reachable" != "1" ]; then
    printf 'SKIP\n'
    return 0
  fi

  # Need a numeric prior AND current sample to compare; anything else -> seed/UNKNOWN, never page.
  case "$prev" in '' | *[!0-9]*) printf 'UNKNOWN\n'; return 0 ;; esac
  case "$curr" in '' | *[!0-9]*) printf 'UNKNOWN\n'; return 0 ;; esac

  # Cumulative counter reset (OBS restarted) -> reseed, never a false FROZEN.
  if [ "$curr" -lt "$prev" ]; then
    printf 'UNKNOWN\n'
  elif [ "$curr" -gt "$prev" ]; then
    printf 'ADVANCING\n'
  else
    printf 'FROZEN\n'
  fi
}

# frozen_input_recovery_decision <was_alerted 0|1> <now_advancing 0|1> -> stdout: recover=0 | recover=1
#   Fire ONE recovery ("advancing again") ping only when a source we actually PAGED for (was_alerted=1)
#   starts advancing again (now_advancing=1). A source that froze but was never confirmed/paged, or one
#   that was healthy all along, never emits a recovery ping. Any value other than "1" counts as 0.
#   Sibling of net_reach_recovery_decision (scripts/lib/network-reach-health.sh).
frozen_input_recovery_decision() {
  local was_alerted="${1:-0}" now_advancing="${2:-0}"
  if [ "$was_alerted" = "1" ] && [ "$now_advancing" = "1" ]; then
    printf 'recover=1\n'
  else
    printf 'recover=0\n'
  fi
}

# frozen_input_alert_detail <source> <received> -> stdout: one human line for the Discord alert body,
#   naming the frozen source and the stuck cumulative `received=` counter value.
frozen_input_alert_detail() {
  local source="${1:-?}" received="${2:-?}"
  printf "%s: received=%s not advancing (input frozen on last frame while box reachable)\n" \
    "$source" "$received"
}

# frozen_input_cambox_sources [include_regex] [exclude_regex]
#   stdin: raw OBS-log text (a `genlock-fifo audit '<src>': …` tail); stdout: newline-separated,
#   UNIQUE (first-seen order) source names that name a CAMBOX camera input on the receiver.
#   DYNAMIC enumeration (#1069): the watched cambox set is derived from live OBS-log reality each
#   pass, never a static cam-number list. A source is a cambox input iff its name (a) appears in a
#   `genlock-fifo audit '<src>':` line, (b) MATCHES include_regex (default `cam`, case-insensitive),
#   and (c) does NOT match exclude_regex (default the program/preview/multiview feed labels). On the
#   live rig the camera branch is `NDI cam1`..`NDI camN`; `NDI 2ME PGM (mv)` / `NDI 2ME PVW` are the
#   program/preview feeds and are excluded. Self-filters to the EXPECTED-LIVE set: a source only
#   prints an audit line while OBS is receiving it, and a WEDGED receiver keeps printing the line with
#   a STUCK `received=` (issue-1096), so a frozen input stays enumerated (the classify then flags it
#   FROZEN) while a legitimately-removed source simply drops out (never a false page). Pure: no I/O.
frozen_input_cambox_sources() {
  local inc="${1:-cam}" exc="${2:-2me|pgm|pvw|multiview|preview|program}"
  # Extract the quoted source name from every audit line, then include/exclude filter + dedup. The
  # trailing `|| true` makes "no cambox sources this pass" a NORMAL exit-0 result (grep exits 1 on no
  # match) instead of a pipeline failure -- the callers key on the empty OUTPUT (the enum-blind guard),
  # never on the exit code, and must not treat "nothing matched" as an error under `set -o pipefail`.
  # #1258 layer 2: LC_ALL=C + grep -a on the FIRST stage -- the stdin here is the raw PS-fetched
  # OBS-log tail (mv_reverify_probe_raw), which can carry invalid-UTF-8 bytes (PowerShell 5.1's
  # `gc` re-encoding ANSI on a non-ASCII glyph elsewhere in the SAME tail); a UTF-8-locale grep
  # then flags the WHOLE stdin binary and returns nothing, so the enumeration goes blind. Byte-safe
  # end to end on this stage; the downstream filters only ever see already-extracted plain names.
  { LC_ALL=C grep -aoE "genlock-fifo audit '[^']*':" \
    | LC_ALL=C sed -E "s/^genlock-fifo audit '(.*)':$/\1/" \
    | grep -iE -- "$inc" \
    | grep -ivE -- "$exc" \
    | awk '!seen[$0]++'; } || true
}
