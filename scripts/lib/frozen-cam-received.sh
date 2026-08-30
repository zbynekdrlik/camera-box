#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions + one env-overridable I/O wrapper; no
# top-level statements beyond guarded dependency sources) -- deliberately NO `set -euo pipefail`
# here: sourcing runs in the CALLER's shell (scripts/recording-e2e.sh, which sets strict mode
# itself), so imposing `-e` here would leak into the caller. Mirrors every sibling scripts/lib/*.sh
# source-only lib (frozen-input-health.sh, mv-reverify-escalate.sh, ...).
#
# scripts/lib/frozen-cam-received.sh -- #1233: content-INDEPENDENT leg-liveness for the [4c/8]
# frozen-camera gate in scripts/recording-e2e.sh.
#
# WHY: the old [4c/8] gate keyed its ABORT decision on PIXEL HASHES of strih preview screenshots
# (frozen-camera-gate.py: GetSourceScreenshot -> SHA1; N identical hashes = FROZEN -> exit 1). That
# is CONTENT-DEPENDENT: when strih's DistroAV receiver holds the LAST frame (during the [2b/8]
# cambox deploy wave while a restarting box re-attaches, or on a genuinely static scene -- one
# camera through a splitter feeds every box), the hashes are identical even though the leg is
# DELIVERING frames. With 7 cameras the deploy wave is long enough that the gate lands inside it and
# false-aborts a ~40-min run (run 33311702636 attempt 3: FROZEN on cam1,3,4,5,6,7 while all boxes
# captured 60 fps colour and the QR sweep decoded the live painter minutes later).
#
# The fix decides liveness from strih's `genlock-fifo audit '<input>': received=N` counter DELTA
# instead -- the SAME content-independent tap the #1052 frozen-input watchdog and the #1093
# mv-reverify escalation use. `received=` is the cumulative count of frames the FIFO RECEIVED from
# the source, so a source whose received= ADVANCES across the sample window is ALIVE regardless of
# static screenshot content, and a genuinely stuck leg keeps it flat.
#
# Reuse, never re-implement (the building blocks):
#   * mv_reverify_probe_raw / mv_reverify_extract_received (scripts/lib/mv-reverify-escalate.sh,
#     #1093) -- the flat-ssh strih OBS-log tail read + newest per-source `received=` extract.
#   * frozen_input_classify (scripts/lib/frozen-input-health.sh, #1052) -- the pure
#     (prev,curr,expected_live,sender_reachable) -> FROZEN|ADVANCING|UNKNOWN|SKIP decision.
# This lib adds only the ORCHESTRATION (aggregate + gate decision + the two-read I/O wrapper).
#
# GOTCHA respected (#797, .claude/skills/genlock): NEVER divide an audit-counter delta by a
# wall-clock sleep (the "phantom 50.1 fps" bug). We compare the raw counter VALUE across two reads
# (delta>0 = advancing, delta==0 = frozen) -- no rate, no division. The read GAP must EXCEED the
# ~5.017s audit emit cadence, or a live source's newest audit line is read twice unchanged = false
# FROZEN; default 12s mirrors mv-reverify's MV_REVERIFY_WEDGE_SAMPLE_GAP_S (>= 2x the cadence).
#
# Source-only: pure functions + one I/O wrapper, no side effects at source time.

# Lazily pull the reused building blocks, guarded so re-sourcing in the harness (where
# mv-reverify-escalate.sh is already sourced) is a no-op and the Tier-0 test (which sources ONLY
# this lib) is still self-contained. Same `command -v ... || . ...` idiom mv-reverify-escalate.sh
# itself uses for cam2-paint-signal.sh.
command -v frozen_input_classify >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/frozen-input-health.sh"
command -v mv_reverify_extract_received >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/mv-reverify-escalate.sh"

# frozen_cam_received_classify_raw <sources_csv> <raw0> <raw1>
#   PURE (no I/O, no sleep). Given a comma-separated list of strih NDI input names (each may carry
#   an internal space, e.g. "NDI cam1,NDI cam3") and two RAW OBS-log tails (sample0 then sample1),
#   prints a per-source detail line to STDERR and ONE overall verdict token to STDOUT:
#     READ_FAIL            -- BOTH raw samples empty: the log READ itself failed (a healthy 400-line
#                             tail is never empty). NOT a proven freeze -> the caller must never
#                             abort on it (the #1093 READ_FAIL / frozen_input_classify UNKNOWN
#                             discipline: never act on absence-of-evidence).
#     FROZEN:<a,b,...>     -- >=1 source's received= did NOT advance (numeric prev==curr): a PROVEN
#                             stuck leg. This is the abort signal.
#     INCONCLUSIVE:<a,...> -- no source proven FROZEN, but >=1 source could not be proven ALIVE
#                             (no audit line / cumulative-counter reset / unreadable) -> caller
#                             retries; NEVER aborts on it.
#     ALIVE                -- every checked source's received= advanced -> healthy legs.
#   Precedence: READ_FAIL > FROZEN > INCONCLUSIVE > ALIVE. An empty source list yields
#   INCONCLUSIVE:no-sources (fail-loud, never a false ALIVE on nothing checked).
frozen_cam_received_classify_raw() {
  local sources_csv="${1:-}" raw0="${2:-}" raw1="${3:-}"

  # Both tails empty => the log read itself failed (not "no recv") -> READ_FAIL, never a freeze.
  if [ -z "$raw0" ] && [ -z "$raw1" ]; then
    printf 'READ_FAIL\n'
    return 0
  fi

  # Split the CSV on COMMA only (source names keep their internal spaces). Positional params are
  # function-local, so `set --` is safe here.
  local _oldifs="$IFS"
  IFS=','
  # shellcheck disable=SC2086  # deliberate comma-split; names keep internal spaces
  set -- $sources_csv
  IFS="$_oldifs"

  local frozen_list="" unknown_list="" alive_n=0 checked_n=0
  local src prev curr verdict
  for src in "$@"; do
    # trim leading/trailing whitespace
    src="${src#"${src%%[![:space:]]*}"}"
    src="${src%"${src##*[![:space:]]}"}"
    [ -n "$src" ] || continue
    checked_n=$((checked_n + 1))
    # `|| true` keeps a no-match (grep exit 1 under pipefail) a NORMAL empty result, never a set -e
    # abort -- the empty value is the UNKNOWN "no audit line" signal frozen_input_classify expects.
    prev="$(printf '%s\n' "$raw0" | mv_reverify_extract_received "$src" || true)"
    curr="$(printf '%s\n' "$raw1" | mv_reverify_extract_received "$src" || true)"
    verdict="$(frozen_input_classify "$prev" "$curr" 1 1)"
    printf "    [frozen-camera-gate] received= %s: prev=%s curr=%s -> %s\n" \
      "$src" "${prev:-none}" "${curr:-none}" "$verdict" >&2
    case "$verdict" in
      ADVANCING) alive_n=$((alive_n + 1)) ;;
      FROZEN)    frozen_list="${frozen_list:+$frozen_list,}$src" ;;
      *)         unknown_list="${unknown_list:+$unknown_list,}$src" ;;  # UNKNOWN (no line / reset)
    esac
  done

  if [ "$checked_n" -eq 0 ]; then
    printf 'INCONCLUSIVE:no-sources\n'
  elif [ -n "$frozen_list" ]; then
    printf 'FROZEN:%s\n' "$frozen_list"
  elif [ -n "$unknown_list" ]; then
    printf 'INCONCLUSIVE:%s\n' "$unknown_list"
  else
    printf 'ALIVE\n'
  fi
}

# frozen_cam_gate_should_abort <frozen_ok 0|1> <final_verdict>
#   PURE. Turns (did any attempt prove the legs ALIVE?, the FINAL attempt's verdict token) into the
#   gate outcome:
#     PASS      -- some attempt proved ALIVE (frozen_ok=1).
#     ABORT     -- no attempt proved ALIVE AND the final attempt PROVED a freeze (verdict FROZEN:*):
#                  a genuinely stuck camera fails every attempt -> abort (preserves #365).
#     WARN_PASS -- no attempt proved ALIVE and the final verdict was INCONCLUSIVE/READ_FAIL/other:
#                  we could NOT prove a freeze, so NEVER false-abort a ~40-min CI run on absence of
#                  evidence (the mv-reverify/mv-fps discipline). The caller emits a loud WARN; the
#                  leg is re-proven downstream by the QR sweep.
frozen_cam_gate_should_abort() {
  local frozen_ok="${1:-0}" final_verdict="${2:-}"
  if [ "$frozen_ok" = "1" ]; then
    printf 'PASS\n'
  elif [ "${final_verdict%%:*}" = "FROZEN" ]; then
    printf 'ABORT\n'
  else
    printf 'WARN_PASS\n'
  fi
}

# _frozen_cam_received_read_tail <strih_ip> -> RAW newest OBS-log tail (all sources, one flat-ssh
#   read). Reuses mv_reverify_probe_raw (the #1093 session-agnostic FILE tail; its <source> arg is a
#   LABEL only -- the real read returns the whole tail). Override the WHOLE read with
#   FROZEN_CAM_RECEIVED_CMD (run with "<ip>", stdout = raw log text) for a Tier-0 / offline test.
_frozen_cam_received_read_tail() {
  local ip="$1"
  if [ -n "${FROZEN_CAM_RECEIVED_CMD:-}" ]; then
    $FROZEN_CAM_RECEIVED_CMD "$ip" 2>/dev/null || true
  else
    mv_reverify_probe_raw "$ip" "frozen-cam-gate" 2>/dev/null || true
  fi
}

# frozen_cam_received_read_and_verdict <strih_ip> <sources_csv>
#   I/O wrapper: read strih's OBS-log tail TWICE, FROZEN_CAM_RECEIVED_GAP_S apart (default 12s, >
#   the ~5.017s audit cadence so a live source's newest line advances between reads), and classify.
#   Prints per-source detail to stderr (via the pure classifier) and the verdict token to stdout.
frozen_cam_received_read_and_verdict() {
  local ip="$1" sources_csv="$2" raw0 raw1
  raw0="$(_frozen_cam_received_read_tail "$ip")"
  sleep "${FROZEN_CAM_RECEIVED_GAP_S:-12}"
  raw1="$(_frozen_cam_received_read_tail "$ip")"
  frozen_cam_received_classify_raw "$sources_csv" "$raw0" "$raw1"
}
