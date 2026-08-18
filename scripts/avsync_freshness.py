#!/usr/bin/env python3
"""#814 -- the grab-freshness gate as a PURE, single-source-of-truth decider.

WHY THIS FILE EXISTS: the A/V-sync watchdog on the stream box emitted false desync verdicts for
2h09m after the live stream ended. `ffmpeg -y` overwrites its output only on SUCCESS, so when the
RTMP relay stopped serving (stream ended -> ffmpeg rc=-5, "I/O error") it LEFT the previous 35 s
clip on disk; the old loop discarded rc and gated on `Test-Path` (always true), re-measuring that
SAME stale clip forever and logging its verdict as if live. The fix is a freshness assert on the
grabbed sample -- and this module is that assert as ONE pure, exhaustively-unit-testable decision
core (the "pure decision library" shape of scripts/lib/obs-watchdog-decision.sh /
scripts/lib/avsync-heartbeat.sh), so avsync-watchdog.ps1, av_sync_measure.py and the installer
self-test all decide fresh-vs-stale IDENTICALLY instead of each duplicating the thresholds.

The grab IS the liveness probe (reuse, don't invent a second one): OBS stops streaming -> the local
RTMP relay stops serving -> ffmpeg rc!=0 and no fresh clip is produced. So rc + clip-freshness is a
faithful signal of the stream's real state; no OBS-WS / bundle-state probe is needed for this.

CLI (what the ps1 / installer / test harness call):
  avsync_freshness.py --grab-rc R --size-bytes S --mtime-age-s A --duration-s D
    -> "OK"                (exit 0)  when the grab is proven CURRENT
    -> "NO-SIGNAL: <reason>" (exit 10) otherwise (fail-CLOSED on any malformed input)
"""

import argparse
import os
import subprocess
import sys
import time

# The exact live-verified thresholds (confirmed on the box 2026-07-26). This is now their SINGLE
# source of truth -- no consumer duplicates these numbers.
MAX_AGE_S = 180           # a clip older than this was NOT freshly grabbed (the stale-clip incident)
MIN_SIZE_BYTES = 200_000  # a real 35 s clip is far bigger; a truncated/failed grab is smaller
MIN_DUR_S = 20            # a usable measurement window; a stub/short clip is not one

# CLI exit codes: 0 = fresh (measure allowed), 10 = NO-SIGNAL (emit no verdict).
EXIT_OK = 0
EXIT_NO_SIGNAL = 10


def freshness_verdict(grab_rc, size_bytes, mtime_age_s, duration_s,
                      max_age_s=MAX_AGE_S, min_size_bytes=MIN_SIZE_BYTES, min_dur_s=MIN_DUR_S):
    """PURE: given the four grab facts, return (allowed: bool, reason: str).

    Order matters -- the grab return code is checked FIRST so a failed grab that LEFT a
    plausible-looking stale clip on disk (the 2h09m frozen-input incident) can never slip through
    as a verdict. A NEGATIVE duration means 'ffprobe unavailable/unknown' and is deliberately NOT
    penalized (only a POSITIVE-but-too-short duration fails), mirroring the live gate's own
    `$dur -ge 0 -and $dur -lt 20`. A negative size means 'no clip on disk'.
    """
    if grab_rc != 0:
        return False, "grab failed: ffmpeg rc={} (relay/stream down)".format(grab_rc)
    if size_bytes < 0:
        return False, "no clip produced (grab left no file)"
    if size_bytes < min_size_bytes:
        return False, "clip too small ({} B < {} B)".format(size_bytes, min_size_bytes)
    if mtime_age_s > max_age_s:
        return False, "clip STALE (age {}s > {}s) - grab did not run".format(int(mtime_age_s), max_age_s)
    if 0 <= duration_s < min_dur_s:
        return False, "clip too short ({:g}s < {}s)".format(duration_s, min_dur_s)
    return True, "OK"


def clip_facts(path, ffprobe="ffprobe"):
    """I/O helper (NOT pure) used by callers that only have a clip PATH (e.g. av_sync_measure.py's
    --media path): returns (size_bytes, mtime_age_s, duration_s). A missing file -> size=-1. An
    unavailable/failing ffprobe -> duration=-1 (unknown -> deliberately not a failure, per the pure
    gate's contract above). Failures are turned into the honest 'unknown' sentinels, never silently
    swallowed into a false 'fresh'.
    """
    try:
        st = os.stat(path)
    except OSError:
        return -1, -1.0, -1.0
    size = st.st_size
    age = max(0.0, time.time() - st.st_mtime)
    dur = -1.0
    try:
        proc = subprocess.run(
            [ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
            capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return size, age, -1.0
    if proc.returncode == 0 and proc.stdout.strip():
        try:
            dur = float(proc.stdout.strip())
        except ValueError:
            dur = -1.0
    return size, age, dur


def main(argv=None):
    ap = argparse.ArgumentParser(description="#814 pure grab-freshness gate")
    # Kept as raw strings (not type=int/float) so a malformed value fails CLOSED to NO-SIGNAL
    # below, never an argparse crash / a silent OK.
    ap.add_argument("--grab-rc", required=True, help="ffmpeg exit code of the grab")
    ap.add_argument("--size-bytes", required=True, help="clip size in bytes (-1 = no file)")
    ap.add_argument("--mtime-age-s", required=True, help="clip mtime age in seconds")
    ap.add_argument("--duration-s", required=True, help="ffprobe clip duration in seconds (-1 = unknown)")
    ap.add_argument("--max-age-s", type=int, default=MAX_AGE_S)
    ap.add_argument("--min-size-bytes", type=int, default=MIN_SIZE_BYTES)
    ap.add_argument("--min-dur-s", type=int, default=MIN_DUR_S)
    args = ap.parse_args(argv)

    try:
        grab_rc = int(args.grab_rc)
        size_bytes = int(args.size_bytes)
        mtime_age_s = float(args.mtime_age_s)
        duration_s = float(args.duration_s)
    except (TypeError, ValueError) as exc:
        # Fail CLOSED: a corrupt/absent grab fact is NO-SIGNAL, never a silently-allowed verdict
        # (this repo's standing discipline -- cf. avsync_heartbeat_is_stale's "missing = stale").
        print("NO-SIGNAL: malformed freshness input ({})".format(exc))
        return EXIT_NO_SIGNAL

    allowed, reason = freshness_verdict(
        grab_rc, size_bytes, mtime_age_s, duration_s,
        max_age_s=args.max_age_s, min_size_bytes=args.min_size_bytes, min_dur_s=args.min_dur_s,
    )
    if allowed:
        print("OK")
        return EXIT_OK
    print("NO-SIGNAL: {}".format(reason))
    return EXIT_NO_SIGNAL


if __name__ == "__main__":
    sys.exit(main())
