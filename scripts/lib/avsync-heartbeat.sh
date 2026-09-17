#!/usr/bin/env bash
# scripts/lib/avsync-heartbeat.sh -- #812/#807 PURE heartbeat-freshness helpers.
set -euo pipefail
#
# For the dev1-side scripts/avsync-heartbeat-alert-watchdog.sh. No I/O, no ssh -- pure
# string/arithmetic, unit-tested directly (mirrors the "pure decision lib" shape of
# scripts/lib/obs-watchdog-decision.sh / scripts/lib/imag-obs-reachability.sh).
#
# Each on-box heartbeat file is ONE line: "<epoch_seconds>\t<status text>" (avsync-watchdog.ps1 /
# avsync-vlc-monitor.ps1's own Write-Heartbeat -- see issues 812/807's design comments). The
# dev1-side watchdog reads BOTH files in a single ssh round-trip (avsync_heartbeat_probe_cmd),
# separated by a marker line neither script's own status text could ever contain.
#
# Source-only: defines the functions below; runs nothing on its own.

AVSYNC_HB_SEP='---AVSYNC-HB-SEP---'

# avsync_heartbeat_probe_cmd [watchdog_path] [vlc_path] -> a remote cmd.exe command string (this
# box's default ssh shell is cmd.exe, confirmed live -- NOT bash/powershell). `type`-ing a missing
# file prints to stderr and fails; `2>nul` swallows that so a missing file just produces an EMPTY
# segment (never a false alert about the ssh call itself failing) and `&` (not `&&`) always runs
# the next segment regardless of the previous one's exit code.
# shellcheck disable=SC2120  # optional args are used by the test harness; in-file callers use defaults
avsync_heartbeat_probe_cmd() {
  local watchdog_path="${1:-C:\\avsync\\avsync-watchdog-heartbeat.txt}"
  local vlc_path="${2:-C:\\avsync\\avsync-vlc-monitor-heartbeat.txt}"
  printf 'type "%s" 2>nul & echo %s & type "%s" 2>nul' "$watchdog_path" "$AVSYNC_HB_SEP" "$vlc_path"
}

# avsync_heartbeat_extract_segment PROBE_OUTPUT WHICH -> echoes the "watchdog" or "vlc" half of a
# probe_cmd's combined output, split on the separator line. Empty when that half never appeared --
# INCLUDING when the separator itself is missing entirely (a truncated/partial ssh read): without
# the explicit guard below, the "watchdog" side's sed range (`1,/^SEP$/p`) would silently fall back
# to "print everything to EOF, then drop the last line" instead of failing safe symmetrically with
# its "vlc" sibling (whose range simply never matches and correctly prints nothing) -- caught by
# code review, fixed here.
#
# #968: the REAL remote probe runs via cmd.exe, whose `echo TEXT & next` syntax includes a
# TRAILING SPACE before the `&` as part of TEXT, and every line arrives CRLF-terminated over ssh
# -- neither survives an exact `$`-anchored sed match. Normalize BOTH away up front: strip every
# `\r` byte (heartbeat content never legitimately contains one) and tolerate optional trailing
# whitespace on the separator line itself. Without this, the separator never matched at all: the
# "vlc" side read permanently empty (a false CONFIRMED-stale alert every ~1h on a healthy box)
# while the "watchdog" side accidentally leaked the vlc line into its own segment instead (the
# unmatched end-pattern falls back to "print to EOF minus the trailing line", which is the
# probe's own trailing blank line, not the real vlc content).
avsync_heartbeat_extract_segment() {
  local out="$1" which="$2"
  out="${out//$'\r'/}"
  case "$out" in
    *"$AVSYNC_HB_SEP"*) : ;;
    *) return 0 ;;   # separator absent entirely -- fail safe: BOTH sides empty, never guess
  esac
  case "$which" in
    watchdog) printf '%s\n' "$out" | sed -n "1,/^${AVSYNC_HB_SEP}[[:space:]]*\$/p" | sed '$d' ;;
    vlc)      printf '%s\n' "$out" | sed -n "/^${AVSYNC_HB_SEP}[[:space:]]*\$/,\$p" | sed '1d' ;;
    *) return 1 ;;
  esac
}

# avsync_heartbeat_last_epoch SEGMENT -> the epoch (first TAB field) of the LAST non-empty line in
# SEGMENT that parses as digits -- robust to any stray earlier output. Empty when nothing parses.
avsync_heartbeat_last_epoch() {
  printf '%s\n' "$1" | awk -F'\t' '$1 ~ /^[0-9]+$/ {e=$1} END{if (e!="") print e}'
}

# avsync_heartbeat_last_status SEGMENT -> the status text (everything after the epoch's own TAB)
# of the LAST numeric-epoch line in SEGMENT -- issue 968's verdict-forward leg reads this to get
# the full "measured: ..." (or "no-signal: ...") text worth deciding on. Empty when nothing parses
# (mirrors avsync_heartbeat_last_epoch's own contract exactly).
avsync_heartbeat_last_status() {
  printf '%s\n' "$1" | awk -F'\t' '$1 ~ /^[0-9]+$/ {line=$0} END{if (line!="") {sub(/^[^\t]*\t/,"",line); print line}}'
}

# avsync_heartbeat_is_forwardable_verdict STATUS_TEXT -> exit 0 when STATUS_TEXT is a genuine
# measured MISALIGNMENT verdict worth forwarding to Discord (issue 968): it must start with the
# "measured: " prefix avsync-watchdog.ps1's Write-Heartbeat uses for a completed measurement pass
# AND carry one of av_sync_measure.py's own ZNIZ/ZVYS correction recommendations. This mirrors
# av_sync_measure.py's OWN threshold semantics exactly (silence when in sync, message when
# misaligned) -- a "measured: ... A/V sync OK (offset 0 ms)" line, a "measured: TIMEOUT: ..." line,
# and every "no-signal: ..." line are ALL heartbeat-only states and must NEVER forward. Exit 1
# otherwise.
avsync_heartbeat_is_forwardable_verdict() {
  local status="$1"
  case "$status" in
    "measured: "*) : ;;
    *) return 1 ;;
  esac
  case "$status" in
    *ZNIZ*|*ZVYS*) return 0 ;;
    *) return 1 ;;
  esac
}

# avsync_heartbeat_verdict_signature STATUS -> STATUS with only its "[YYYY-MM-DD HH:MM:SS]"
# timestamp bracket removed. #814's forwarder second net compares this against the last-forwarded
# signature to suppress a FROZEN input's re-forward: two passes measuring the SAME (frozen) clip
# differ ONLY in the stamp -- the offset, conf and verdict text are all deterministic from the
# measured offset -- so a byte-identical stamp-stripped signature is the frozen-input tell (mirrors
# the incident's own "dup-suppressed (frozen input?)" net). A genuine offset change alters the
# signature and re-posts. Pure: string transform only, no I/O.
avsync_heartbeat_verdict_signature() {
  printf '%s\n' "$1" | sed -E 's/\[[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}\] //'
}

# avsync_heartbeat_is_stale EPOCH NOW STALE_SEC -> exit 0 (STALE, including unparseable/missing) /
# 1 (fresh). Inverted sense vs a plain "is_fresh" check ON PURPOSE -- this lib's caller wants
# "wedged=1 means alert", so a missing/corrupt heartbeat must default to the ALERTING answer, never
# to "fresh" (this repo's standing fail-loud-not-guess convention -- never silently assume health).
#
# Each of the three args is validated INDIVIDUALLY (never by concatenating them first) -- code
# review caught that the original `case "$epoch$now$stale" in *[!0-9]*|"")` concatenation let an
# EMPTY $epoch silently vanish from the joined string instead of tripping the guard, so bash
# arithmetic below then read the missing epoch as 0 and could compute a small, in-window "age"
# purely by chance (masked in the original unit tests because their chosen $now/$stale happened to
# still land the resulting age outside the window; a smaller $now exposed it reading FRESH, wrongly).
avsync_heartbeat_is_stale() {
  local epoch="${1:-}" now="${2:-}" stale="${3:-}"
  case "$epoch" in '' | *[!0-9]*) return 0 ;; esac
  case "$now" in '' | *[!0-9]*) return 0 ;; esac
  case "$stale" in '' | *[!0-9]*) return 0 ;; esac
  local age=$(( now - epoch ))
  [ "$age" -ge 0 ] && [ "$age" -le "$stale" ] && return 1   # fresh
  return 0   # stale (negative age -- clock skew/corrupt -- or genuinely too old)
}

# ── #1331: heartbeat SOURCE selection (dev2 Linux measurer vs the retired stream Windows box) ────
# The lipsync measurement moved OFF the stream box onto dev2 (GPU) -- the heartbeat CONTRACT is
# unchanged (one "<epoch>\t<status>" line), only the HOST + transport + remote read command differ.
# `AVSYNC_HEARTBEAT_HOST` selects: "dev2" (default -- Linux box, key auth, plain ssh, `cat`) or
# "stream" (the retired Windows box, sshpass, cmd.exe `type`). Pure functions only (no I/O) so the
# watchdogs build the actual ssh call themselves under their OWN `set -uo pipefail` -- exactly the
# established shape (this lib's `set -euo pipefail` must never run an ssh inside a `-e` context).

AVSYNC_HEARTBEAT_HOST_DEFAULT="dev2"
# dev2 (Linux) heartbeat file -- `$HOME` is LEFT LITERAL so the REMOTE shell (dev2) expands it, not
# the local one (the whole cmd string is passed verbatim through the watchdog's `ssh host "$cmd"`).
AVSYNC_HB_DEV2_WATCHDOG_PATH="${AVSYNC_HB_DEV2_WATCHDOG_PATH:-\$HOME/.camera-box/avsync-watchdog-heartbeat.txt}"

# avsync_heartbeat_host -> the selected source host ("dev2" | "stream"), defaulting to dev2.
avsync_heartbeat_host() {
  printf '%s' "${AVSYNC_HEARTBEAT_HOST:-$AVSYNC_HEARTBEAT_HOST_DEFAULT}"
}

# avsync_heartbeat_linux_probe_cmd [watchdog_path] -> the remote LINUX (dev2) read command: `cat`
# the watchdog heartbeat file, then echo the SAME separator the Windows probe uses (so the shared
# avsync_heartbeat_extract_segment parses BOTH identically). There is NO vlc-monitor heartbeat on
# dev2 (the #807 VLC babysitter is a stream-box thing), so ONLY the watchdog file + separator are
# emitted -- the "vlc" half is intentionally absent (empty segment; the dev1 vlc leg is skipped for
# host=dev2 via avsync_heartbeat_has_vlc_leg, never a false stale-page). `2>/dev/null` swallows a
# not-yet-written file's error so a fresh box produces an empty (never a crashing) segment.
# shellcheck disable=SC2120  # optional arg is used by the test harness; in-file callers use the default
avsync_heartbeat_linux_probe_cmd() {
  local watchdog_path="${1:-$AVSYNC_HB_DEV2_WATCHDOG_PATH}"
  printf 'cat "%s" 2>/dev/null; echo "%s"' "$watchdog_path" "$AVSYNC_HB_SEP"
}

# avsync_heartbeat_remote_cmd [HOST] -> the remote read command string for the selected HOST:
# dev2 -> the Linux `cat` probe above; stream -> the existing Windows `type` probe (both files).
avsync_heartbeat_remote_cmd() {
  local host="${1:-$(avsync_heartbeat_host)}"
  case "$host" in
    dev2)   avsync_heartbeat_linux_probe_cmd ;;
    stream) avsync_heartbeat_probe_cmd ;;
    *) return 1 ;;
  esac
}

# avsync_heartbeat_ssh_prefix_argv [HOST] -> the ssh transport argv (ONE ARG PER LINE, WITHOUT the
# remote command) for the selected HOST. dev2 = plain `ssh ... newlevel@dev2` (KEY AUTH -- no
# sshpass, NO password anywhere in the argv). stream = `sshpass -p <pw> ssh ... newlevel@<ip>`
# (the retired Windows box, password auth). Emitted one-arg-per-line so the caller reads it with
# `mapfile -t` into an array and never re-splits a password containing spaces.
avsync_heartbeat_ssh_prefix_argv() {
  local host="${1:-$(avsync_heartbeat_host)}"
  case "$host" in
    dev2)
      printf '%s\n' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${AVSYNC_HB_DEV2_SSH:-newlevel@dev2}"
      ;;
    stream)
      printf '%s\n' sshpass -p "${STREAM_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${STREAM_USER:-newlevel}@${STREAM_IP:-10.77.9.204}"
      ;;
    *) return 1 ;;
  esac
}

# avsync_heartbeat_has_vlc_leg [HOST] -> exit 0 iff HOST has the #807 VLC-monitor heartbeat leg.
# ONLY the stream box ran the VLC babysitter; dev2 does not, so its vlc segment is always empty by
# design and the dev1 vlc leg MUST skip rather than page on a permanently-missing file.
avsync_heartbeat_has_vlc_leg() {
  local host="${1:-$(avsync_heartbeat_host)}"
  [ "$host" = "stream" ]
}
