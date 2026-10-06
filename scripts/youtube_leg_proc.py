#!/usr/bin/env python3
"""Issue 1404 -- bounded subprocesses for the YouTube-leg tool (ffmpeg, ffprobe, yt-dlp, the probe).

Every child runs in its own process group, so a timeout kills the whole group (an ffmpeg the probe
or yt-dlp started included, never only the direct child), and `install_cleanup()` (called by the
CLI) kills every still-running group when the tool itself is stopped (SIGTERM / SIGINT: a cancelled
CI job must not leave a recording-verdict or a download running on dev1). A failed child's error
carries the tail of its stderr, so a reason like "video is processing" reaches the verdict.
"""
import os
import signal
import subprocess
import sys

_ACTIVE = set()  # process-group ids of the children still running


def _kill_group(pgid):
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return False
    return True


def run_bounded(cmd, timeout, text=False, check=False):
    """subprocess.run with a whole-process-group timeout; check=True raises RuntimeError naming the
    command, its exit code and the last 500 characters of its stderr."""
    cmd = [str(c) for c in cmd]
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=text, start_new_session=True)
    _ACTIVE.add(p.pid)
    try:
        out, err = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        _kill_group(p.pid)
        p.communicate()
        raise
    finally:
        _ACTIVE.discard(p.pid)
    if check and p.returncode != 0:
        tail = (err if text else (err or b"").decode("utf-8", "replace"))[-500:].strip()
        raise RuntimeError(f"{os.path.basename(cmd[0])} exited {p.returncode}: {tail}")
    return subprocess.CompletedProcess(cmd, p.returncode, out, err)


def _on_signal(signum, _frame):
    for pgid in list(_ACTIVE):
        _kill_group(pgid)
    print(f"youtube_leg: stopped by signal {signum}, child process groups killed", file=sys.stderr)
    raise SystemExit(128 + signum)


def install_cleanup():
    """Kill every running child group when this process gets SIGTERM / SIGINT."""
    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGINT, _on_signal)
