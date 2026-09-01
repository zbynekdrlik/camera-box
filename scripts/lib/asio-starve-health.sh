#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (frozen-input-health.sh, cadence-health.sh,
# network-reach-health.sh, obs-watchdog-decision.sh) of deliberately NOT setting `set -euo pipefail`
# here: sourcing this file executes it in the CALLER's shell, so strict mode here would leak into
# whichever caller sources it. The caller (asio-starve-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/asio-starve-health.sh -- #1023 (ASIO device in OBS sometimes loses audio): the SHARED,
# PURE decision core for the dev1-side STREAM ASIO-STARVED alert watchdog. No I/O, no ssh, no OBS, so
# it can be unit-tested exhaustively (mirrors scripts/lib/frozen-input-health.sh).
#
# WHY (#1023): when OBS starts BEFORE the ASIO device/matrix is ready, an ASIO source connects but
# its audio callback perpetually STARVES (no samples) -> the source is silent and only an OBS reset
# fixes it. The observability surface (issue 800/803/806/960) is the per-source line the vendored
# genlock build prints once per ASRC_LOG_INTERVAL_S (=60 s) to the stream OBS log:
#   asrc: source '<name>' estimated=…ppm applied=…ppm outer_bias=…ppm cumulative_correction=…ms/60s starved_blocks=N (#803/#806/#960)
# `starved_blocks=N` is PER-INTERVAL (reset-on-read, vendor/obs-studio/libobs/media-io/asrc-compensator.c),
# NOT a lifetime cumulative -- so the newest line's value is a self-contained 60 s measurement. A
# healthy source reads `starved_blocks=0` every interval; a source that started before its ASIO
# matrix was ready reads ~2946 (≈100 % of ~2900 audio callbacks/60 s) SUSTAINED every interval, while
# a sibling ASIO source on a different device stays at 0. Reproduced LIVE on the stream box today
# (2026-08-17): 'ASIO Input Capture' ≈2946 for 11.5 h while 'mbc' = 0.
#
# The dev1-side watchdog samples the newest `starved_blocks` per watched source each pass and turns
# (blocks, threshold, healthy_sibling, expected_live) into a verdict here. The confirm-counter +
# alert throttle stay the SAME shared obs_watchdog_confirm / obs_watchdog_alert_throttle
# (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use.
#
# Source-only: pure functions, no side effects at source time.

# asio_starve_parse_blocks <source> -- stdin: raw OBS-log text (an `asrc: source '<name>' … ` tail);
#   stdout: the NEWEST `starved_blocks=N` integer for that source, or EMPTY when the source has no
#   `asrc:` line in the text / the value is unreadable (the caller then treats it as a blind tap ->
#   UNKNOWN, never a false page). Pure text (no I/O). The trailing `'` in the fixed-string match
#   anchors the exact name (`source 'mbc'` never matches a `source 'mbc2'` line). Always exits 0 (a
#   grep no-match must be a normal "no sample this pass", not a pipeline failure under pipefail).
#   #1262: `LC_ALL=C` on BOTH greps AND the sed -- `-a` alone already stops the "binary file
#   matches" abort against an adversarially-constructed invalid byte co-resident on this line
#   (constructed -- e.g. a corrupted genlock-fifo audit line glued onto it with no `\n` separator
#   at a PS-5.1-ANSI-reencode / transport-chunk boundary, not observed live -- separate,
#   `\n`-terminated corrupted lines never blind it, STEP-0 comment on #1262), but WITHOUT
#   `LC_ALL=C` the sed's `.*` refuses to consume that invalid byte in a UTF-8 locale and returns
#   GARBAGE (the whole glued line's leading text), not a clean empty string -- worse than "none",
#   since a differently-byte-shaped garble would not by chance still contain the right digits. The
#   ONE genuinely realistic same-line trigger: a WATCHED SOURCE NAME containing a non-ASCII
#   character (e.g. an operator-renamed diacritic name) would put the ANSI-mangled byte directly
#   on THIS line via the `%s` -- today's watched names (`ASIO Input Capture`/`mbc`) are plain
#   ASCII, so this is latent, not active. Verified locally: on the adversarial fixture, `-a`-only
#   extracts "...late_hold=0 (<0xA0>2946" (garbage); `LC_ALL=C` on both stages extracts "2946" (clean).
asio_starve_parse_blocks() {
  local source="${1:-}" line blocks
  line="$(LC_ALL=C grep -aF "asrc: source '$source'" 2>/dev/null | LC_ALL=C grep -aF 'starved_blocks=' | tail -1)" || true
  if [ -n "$line" ]; then
    blocks="$(printf '%s\n' "$line" | LC_ALL=C sed -n 's/.*starved_blocks=\([0-9][0-9]*\).*/\1/p' | tail -1)"
    [ -n "$blocks" ] && printf '%s\n' "$blocks"
  fi
  return 0
}

# asio_starve_is_healthy <blocks> <threshold> -> stdout: 1 (healthy) | 0 (not healthy / unknown)
#   A source is HEALTHY iff `blocks` is a valid non-negative integer BELOW the threshold (a source
#   reading 0 is healthy; ~2946 is not). A missing / non-numeric value is NOT proof of health -> 0.
#   Used by the caller to decide, for a starved source, whether ANY sibling is proven-healthy (the
#   per-source discriminator) -- and it is a clean stand-alone unit-test target.
asio_starve_is_healthy() {
  local blocks="${1:-}" thr="${2:-1000}"
  case "$thr" in '' | *[!0-9]*) thr=1000 ;; esac
  case "$blocks" in '' | *[!0-9]*) printf '0\n'; return 0 ;; esac
  if [ "$blocks" -lt "$thr" ]; then printf '1\n'; else printf '0\n'; fi
}

# asio_starve_classify <blocks> <threshold> <healthy_sibling 0|1> <expected_live 0|1>
#   -> stdout: STARVED | OK | UNKNOWN | SKIP
#   Decides a single watched source from its newest per-interval `starved_blocks` value.
#     SKIP     -- out of scope this pass: the source is not expected live (not in the watch set, or
#                 the box is unreachable so issue-1001 already owns the page). Checked FIRST. ANY
#                 value other than "1" for expected_live is out-of-scope.
#     UNKNOWN  -- nothing to page on: no numeric sample (the source's asrc line was absent / the read
#                 failed), OR the source IS starved (blocks >= threshold) but NO sibling is proven
#                 healthy. The all-sources-starving / box-wide-audio-outage case is deliberately
#                 UNKNOWN here (obs-liveness #391 / audio-presence own a box-wide outage -- never
#                 double-page it, and never turn the precise per-source discriminator into a
#                 box-wide alarm). Fail-safe: reseed, never page on a missing/ambiguous reading.
#     OK       -- blocks < threshold: the source is receiving audio (starving below the noise floor).
#     STARVED  -- blocks >= threshold AND a sibling source is proven healthy: THIS source's ASIO
#                 callback is perpetually starving (silent) while the box's audio subsystem is
#                 otherwise fine -> exactly the #1023 startup-order defect. The cure is an OBS reset.
asio_starve_classify() {
  local blocks="${1:-}" thr="${2:-1000}" healthy_sibling="${3:-0}" expected_live="${4:-0}"
  case "$thr" in '' | *[!0-9]*) thr=1000 ;; esac

  # Out-of-scope gate first (independent of the sample).
  if [ "$expected_live" != "1" ]; then
    printf 'SKIP\n'
    return 0
  fi

  # Need a numeric per-interval sample; anything else -> seed/UNKNOWN, never page.
  case "$blocks" in '' | *[!0-9]*) printf 'UNKNOWN\n'; return 0 ;; esac

  if [ "$blocks" -lt "$thr" ]; then
    printf 'OK\n'
    return 0
  fi

  # Starved. Only the per-source defect (a proven-healthy sibling) pages; a box-wide all-starving
  # condition is UNKNOWN here (owned elsewhere), never a false / double page.
  if [ "$healthy_sibling" = "1" ]; then
    printf 'STARVED\n'
  else
    printf 'UNKNOWN\n'
  fi
}

# asio_starve_recovery_decision <was_alerted 0|1> <now_ok 0|1> -> stdout: recover=0 | recover=1
#   Fire ONE recovery ("receiving audio again") ping only when a source we actually PAGED for
#   (was_alerted=1) reads OK again (now_ok=1). A source that starved but was never confirmed/paged,
#   or one healthy all along, never emits a recovery ping. Any value other than "1" counts as 0.
#   Sibling of frozen_input_recovery_decision (scripts/lib/frozen-input-health.sh).
asio_starve_recovery_decision() {
  local was_alerted="${1:-0}" now_ok="${2:-0}"
  if [ "$was_alerted" = "1" ] && [ "$now_ok" = "1" ]; then
    printf 'recover=1\n'
  else
    printf 'recover=0\n'
  fi
}

# asio_starve_alert_detail <source> <blocks> <threshold> -> stdout: one human line for the alert body,
#   naming the starved source and its per-interval starved_blocks value.
asio_starve_alert_detail() {
  local source="${1:-?}" blocks="${2:-?}" thr="${3:-?}"
  printf "%s: starved_blocks=%s/60s (>= %s) -- ASIO source silent (audio callbacks starving) while a sibling ASIO source is healthy\n" \
    "$source" "$blocks" "$thr"
}
