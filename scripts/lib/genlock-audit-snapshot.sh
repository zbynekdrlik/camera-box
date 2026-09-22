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
# Depends on the already-sourced scripts/lib/strih-platform.sh `strih_platform` resolver.
#
# #1133 discipline (a report-only helper called as a BARE statement under the caller's
# `set -euo pipefail` MUST return 0 on every input): an early `[ -n ... ] || return 0`, a `|| true`
# on every ssh/grep/tail pipeline (a `grep -aF` no-match exits 1; a `tail` early-close SIGPIPEs the
# upstream), and the `timeout` bounds the ssh so a wedged strih can never hang the run.

# genlock_audit_snapshot_linux_read HOST -> stdout: the newest strih-lx OBS log's LAST
# GENLOCK_AUDIT_SNAPSHOT_TAIL (default 400) `genlock-fifo audit '` lines, via ONE bounded flat ssh
# (a session-agnostic FILE read -- Context B of win-ssh-vs-mcp.md; strih-lx is a Linux box reached
# over plain ssh, NEVER win_ssh_run/CIM). LC_ALL=C + grep -aF keep the read byte-agnostic: the audit
# line carries a non-ASCII glyph (the approx-sign in "(approx F frames @ ...)"), the same
# byte-safety mv-reverify-escalate.sh #1258 documents. Empty output = the read itself failed
# (unreachable / no log / no audit line yet) -- the caller treats that as fail-open, never a fault.
genlock_audit_snapshot_linux_read() {
  local host="$1"
  local user="${STRIH_USER:-newlevel}" pw="${STRIH_PW:-newlevel}"
  local tmo="${GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT:-20}" tail_n="${GENLOCK_AUDIT_SNAPSHOT_TAIL:-400}"
  # numeric-only tail -> the override can never inject shell metachars into the remote command.
  case "$tail_n" in '' | *[!0-9]*) tail_n=400 ;; esac
  timeout "$tmo" sshpass -p "$pw" \
    ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 \
    "${user}@${host}" \
    'f="$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -1)"; [ -n "$f" ] && LC_ALL=C grep -aF "genlock-fifo audit '\''" "$f" 2>/dev/null | tail -n '"$tail_n"' || true' \
    2>/dev/null || true
}

# genlock_audit_snapshot_capture LABEL OUTFILE -> write the strih genlock-fifo audit tail to OUTFILE.
# LABEL (before|after) is for logging only. Fail-open, returns 0 on every path:
#   * GENLOCK_AUDIT_SNAPSHOT_READER_CMD (a shell command "HOST LABEL" -> raw audit text on stdout) is
#     the Tier-0 / alternate-tap seam -- a test overrides it so the whole capture runs with no ssh.
#   * else, on a Linux strih (strih_platform == linux), the plain-ssh read above.
#   * else (a Windows strih, or any other platform) a LOGGED SKIP -- no file, the report omits the
#     section (a Windows-strih tail is a follow-up; the strih role is the Linux strih-lx post-M4).
# A non-empty read is persisted verbatim to OUTFILE; an empty/timed-out read writes nothing.
genlock_audit_snapshot_capture() {
  local label="${1:-}" outfile="${2:-}"
  [ -n "$outfile" ] || return 0
  local host="${STRIH:-10.77.9.202}" raw="" plat
  if [ -n "${GENLOCK_AUDIT_SNAPSHOT_READER_CMD:-}" ]; then
    raw="$($GENLOCK_AUDIT_SNAPSHOT_READER_CMD "$host" "$label" 2>/dev/null || true)"
  else
    plat="$(strih_platform "$host" 2>/dev/null || echo windows)"
    if [ "$plat" = "linux" ]; then
      raw="$(genlock_audit_snapshot_linux_read "$host" 2>/dev/null || true)"
    else
      echo "    [genlock-audit/$label] #1354 strih platform '$plat' (not linux) -- SKIP; the E2E report omits the genlock-conveyor section (a Windows-strih tail is a follow-up)." >&2
      return 0
    fi
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
