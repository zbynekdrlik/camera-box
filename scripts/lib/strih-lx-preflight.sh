#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function lib (never executed directly) -- must NOT set -e,
# which would propagate into recording-e2e.sh's own carefully-scoped set +e/-e regions when this
# is sourced there (same convention as scripts/lib/strih-platform.sh / obs-session-visibility.sh).
#
# scripts/lib/strih-lx-preflight.sh -- issue 1351: the ONE source of truth for BOUNDING every
# strih-touching `[0/8]` preflight call on the Linux-strih path (strih-lx = 10.77.9.202 after the
# M4 cut-over) with a `timeout` + a NAMED failure banner, so a strih-touching call can never hang
# silently and swallow RUN_ID (recording-e2e.sh exports RUN_ID only AFTER the `[0/8]` band; a
# ~23-min silent hang there aborts before any verdict and the #703 fail-closed guard then reports
# "no verdict" with NO named stage -- the release-gate blocker this closes).
#
# Design-by main (issue 1351, comment 5760764625) -- Approach 1: wrap EVERY strih-touching `[0/8]`
# call on the `strih_platform "$STRIH" == linux` branch in a bounded `timeout` that, on
# expiry/failure, emits a named `[0/8] strih-lx <call>` banner and aborts fast (fail-closed). The
# Windows path stays behaviorally byte-identical: on a non-linux strih the prefix is EMPTY (the gate
# runs exactly as before) and the timeout banner never fires (nothing is `timeout`-wrapped, so a
# genuine gate failure never carries the 124/137 timeout exit codes this banner keys on).
#
# Depends on strih_platform() from scripts/lib/strih-platform.sh being sourced first (recording-e2e.sh
# sources strih-platform.sh immediately above this lib).

# strih_lx_gate_prefix SECS HOST -> a `timeout <SECS>` command PREFIX for a Linux strih, else "".
# Embed UNQUOTED so a linux value word-splits into the `timeout <secs>` prefix, and an empty value
# (a Windows / other strih) vanishes, keeping that path byte-identical:
#     ${STRIH_LX_GATE_PREFIX:-} DANTESYNC_GATE_...=1 "$HERE/dantesync-gate.sh" ...
# (The env-var prefixes may sit before OR after the timeout prefix -- `timeout` inherits its
# environment and passes it to the wrapped command either way.)
strih_lx_gate_prefix() {
  local secs="${1:-180}" host="${2:-}"
  if [ "$(strih_platform "$host")" = "linux" ]; then
    printf 'timeout %s' "$secs"
  fi
}

# strih_lx_preflight_timeout_banner RC LABEL SECS -> emit a named `[0/8] strih-lx <LABEL>` banner to
# stderr ONLY when RC is a `timeout` KILL code (124 = TERM at expiry, 137 = 128+SIGKILL from a
# `timeout --kill-after`), otherwise stay SILENT. A genuine gate FAILURE (the gate ran to a non-zero
# verdict, e.g. clock-not-locked -> 20) is NOT a hang -- the gate already printed its own diagnosis,
# so re-labelling it "strih-lx timeout" would mislead. Called from a `|| { ...; exit "$RC"; }` tail,
# so the caller preserves the gate's real exit code; this only ADDS the attributable banner on the
# hang path. Always returns 0 (safe as a bare statement under the caller's set -euo pipefail).
strih_lx_preflight_timeout_banner() {
  local rc="${1:-0}" label="${2:-a strih-lx [0/8] call}" secs="${3:-?}"
  case "$rc" in
    124|137)
      printf 'ERROR: [0/8] strih-lx %s exceeded its %ss bound (exit %s) -- bounded by issue 1351 so a silent hang can never swallow RUN_ID on the M4 Linux strih; the box or this call is wedged, fix the call, not the timeout.\n' \
        "$label" "$secs" "$rc" >&2
      ;;
  esac
  return 0
}
