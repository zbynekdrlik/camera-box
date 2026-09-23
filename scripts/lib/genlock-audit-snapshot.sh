#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function lib (never executed directly) -- must NOT set -e,
# which would propagate into recording-e2e.sh's own carefully-scoped set +e/-e regions when this
# is sourced there (same convention as scripts/lib/strih-platform.sh / genlock-settle.sh).
#
# scripts/lib/genlock-audit-snapshot.sh -- issue 1354 scope 3 remainder: capture strih's per-input
# `genlock-fifo audit '<name>':` counter tail BEFORE the recording step and AFTER the stop step,
# then run the merged pure parser (scripts/genlock_audit_snapshot.py) to write the per-input window
# deltas JSON that lets the full-path E2E report NAME the conveyor-ladder victim input (instead of a
# supervisor hand-reading the strih log). REPORT-ONLY: every function is FAIL-OPEN and returns 0 on
# every input -- an empty/timed-out read leaves no file, and the report composer's `[ -s ]` guard
# (scripts/lib/e2e-discord-report.sh) then omits the section. Nothing here ever touches $GATE.
#
# Design-by: the main session (issue 1354, comment 5784872333) -- Prístup 1: a NEW sourced helper
# called as single lines from scripts/recording-e2e.sh (#675 anchor-safe pattern -- every existing
# anchored line in recording-e2e.sh stays byte-identical; only NEW `. lib` + call lines are added).
# Reads strih ONLY through the ONE shared platform-resolved reader scripts/lib/strih-log-read.sh
# (issue 1360 part 3: `strih_log_tail`, sourced below when the caller has not already done so).
#
# #1133 discipline (a report-only helper called as a BARE statement under the caller's
# `set -euo pipefail` MUST return 0 on every input): an early `[ -n ... ] || return 0`, a `|| true`
# on every grep/tail pipeline (a `grep -aF` no-match exits 1; a `tail` early-close SIGPIPEs the
# upstream); the shared reader bounds the remote read with its own timeout and ALWAYS returns 0.

_GENLOCK_AUDIT_SNAPSHOT_DIR="${BASH_SOURCE[0]%/*}"
# shellcheck source=scripts/lib/strih-log-read.sh
command -v strih_log_tail >/dev/null 2>&1 || . "$_GENLOCK_AUDIT_SNAPSHOT_DIR/strih-log-read.sh"

# genlock_audit_snapshot_read HOST -> stdout: the newest strih OBS log's LAST
# GENLOCK_AUDIT_SNAPSHOT_TAIL (default 400) `genlock-fifo audit '` lines. The raw read is the shared
# reader's `strih_log_tail` of the last GENLOCK_AUDIT_SNAPSHOT_READ_LINES (default 3000) log lines --
# on strih-lx ~8 min of log, which carries every input's audit line (one per input per ~5 s, ~26 %
# of the log, measured live 23.9.2026); genlock_audit_snapshot.py parses only the LAST line per
# input. The filter is LOCAL: CRs stripped (a Windows strih's PowerShell tail), then LC_ALL=C
# grep -aF (the audit line carries a non-ASCII glyph, the approx-sign in "(approx F frames @ ...)",
# the mv-reverify-escalate.sh #1258 byte-safety). Both knobs are numeric-only, so an override can
# never reach the remote command or the local tail. STRIH_USER / STRIH_PW /
# GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT feed the reader's transport. Empty output = the read itself
# failed (unreachable / no log / no audit line in the window) -- the caller treats that as
# fail-open, never a fault.
genlock_audit_snapshot_read() {
  local host="$1"
  local user="${STRIH_USER:-newlevel}" pw="${STRIH_PW:-newlevel}"
  local tmo="${GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT:-20}" tail_n="${GENLOCK_AUDIT_SNAPSHOT_TAIL:-400}"
  local read_n="${GENLOCK_AUDIT_SNAPSHOT_READ_LINES:-3000}"
  case "$tail_n" in '' | *[!0-9]*) tail_n=400 ;; esac
  case "$read_n" in '' | *[!0-9]*) read_n=3000 ;; esac
  strih_log_tail "$host" "$user" "$pw" "$read_n" "$tmo" |
    tr -d '\r' | LC_ALL=C grep -aF "genlock-fifo audit '" | tail -n "$tail_n" || true
}

# genlock_audit_snapshot_capture LABEL OUTFILE -> write the strih genlock-fifo audit tail to OUTFILE.
# LABEL (before|after) is for logging only. Fail-open, returns 0 on every path:
#   * GENLOCK_AUDIT_SNAPSHOT_READER_CMD (a shell command "HOST LABEL" -> raw audit text on stdout) is
#     the Tier-0 / alternate-tap seam -- a test overrides it so the whole capture runs with no ssh.
#   * else the shared-reader read above (either strih platform -- strih-lx or a Windows strih).
# A non-empty read is persisted verbatim to OUTFILE; an empty/timed-out read writes nothing.
genlock_audit_snapshot_capture() {
  local label="${1:-}" outfile="${2:-}"
  [ -n "$outfile" ] || return 0
  local host="${STRIH:-10.77.9.202}" raw=""
  if [ -n "${GENLOCK_AUDIT_SNAPSHOT_READER_CMD:-}" ]; then
    raw="$($GENLOCK_AUDIT_SNAPSHOT_READER_CMD "$host" "$label" 2>/dev/null || true)"
  else
    raw="$(genlock_audit_snapshot_read "$host" 2>/dev/null || true)"
  fi
  if [ -n "$raw" ]; then
    printf '%s\n' "$raw" >"$outfile" 2>/dev/null || true
    local n=""
    n="$(grep -c "genlock-fifo audit '" "$outfile" 2>/dev/null || true)"
    [ -n "$n" ] || n=0
    echo "    [genlock-audit/$label] #1354 captured ${n} strih audit line(s) -> $outfile"
  else
    echo "    [genlock-audit/$label] #1354 no strih genlock-fifo audit lines read (empty/timed-out) -- the E2E report omits the genlock-conveyor section (fail-open)." >&2
  fi
  return 0
}

# genlock_audit_snapshot_compute BEFORE AFTER OUT -> run the merged pure parser
# (scripts/genlock_audit_snapshot.py) to write the per-input window deltas JSON. Fail-open, returns
# 0 on every path: a missing/empty BEFORE or AFTER tail (unreachable strih, early abort) writes NO
# JSON and the report omits the section; a python error is swallowed (the `[ -s OUT ]` guard downstream
# handles a missing/empty JSON), NEVER a traceback that could abort the run under `set -e`.
genlock_audit_snapshot_compute() {
  local before="${1:-}" after="${2:-}" out="${3:-}"
  [ -n "$out" ] || return 0
  local here="${GENLOCK_AUDIT_SNAPSHOT_HERE:-${HERE:-.}}"
  if [ ! -s "$before" ] || [ ! -s "$after" ]; then
    echo "    [genlock-audit] #1354 before/after audit tail missing or empty -- skipping compute; the E2E report omits the genlock-conveyor section (fail-open)." >&2
    return 0
  fi
  python3 "$here/genlock_audit_snapshot.py" --before-log "$before" --after-log "$after" --out "$out" \
    2>&1 | sed 's/^/    [genlock-audit] /' || true
  return 0
}
