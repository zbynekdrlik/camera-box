#!/usr/bin/env python3
"""Scope the camera-under-test's capture-drop sidecar to the recording window (issue 1482).

camera-box writes `v4l2_dropped` / `frames_captured` once, when its burn instance shuts down, so
the count spans the whole burn life: deploy, the painter relaunch, teardown. In the direct feed
(cam2's HDMI straight into the splitter) every painter start/stop drops a few frames on the Cam
Link, all outside the recording. This tool reads the persisted burn log (#716), counts the
`capture device dropped N frame(s)` lines whose UTC timestamp lies inside
[start, end] (whole seconds, both ends included) and rewrites the sidecar:

    v4l2_dropped=<in-window>
    frames_captured=<unchanged>
    v4l2_dropped_whole_run=<the original count>
    v4l2_drop_window=<start>..<end>

Fail-safe: when the window cannot be proven (no log, an empty or reversed window, a log whose
last `total dropped` disagrees with the sidecar, i.e. a truncated log) the sidecar is left
untouched, so the stricter whole-run count stays, and a WARNING names why. Exit 0 always: the
gate decision stays with recording-verdict.
"""

import argparse
import datetime
import os
import re
import sys

_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_DROP = re.compile(
    r"^(?P<ts>\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.\d+)?Z\s.*"
    r"capture device dropped (?P<n>\d+) frame\(s\):.*\(total dropped (?P<total>\d+)\)"
)


def window_drops(text, start, end):
    """(in-window dropped frames, the log's last `total dropped`; 0 when the log has none)."""
    inside = 0
    last_total = 0
    for raw in text.splitlines():
        m = _DROP.match(_ANSI.sub("", raw).strip())
        if not m:
            continue
        ts = datetime.datetime.strptime(m["ts"], "%Y-%m-%dT%H:%M:%S")
        epoch = int(ts.replace(tzinfo=datetime.timezone.utc).timestamp())
        last_total = int(m["total"])
        if start <= epoch <= end:
            inside += int(m["n"])
    return inside, last_total


def _read_sidecar(path):
    kv = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and "=" in line:
                k, v = line.split("=", 1)
                kv[k.strip()] = v.strip()
    return kv


def scope(sidecar, burn_log, start, end):
    """Rewrite `sidecar` to the window count; returns a one-line report, or raises ValueError."""
    if end < start:
        raise ValueError(f"the recording window {start}..{end} is reversed")
    kv = _read_sidecar(sidecar)
    # A rerun reads the whole-run count the first run kept, never its own window count.
    whole = int(kv.get("v4l2_dropped_whole_run", kv["v4l2_dropped"]))
    try:
        with open(burn_log, encoding="utf-8", errors="replace") as f:
            text = f.read()
    except OSError as exc:
        raise ValueError(f"burn log unreadable: {exc}") from exc
    inside, last_total = window_drops(text, start, end)
    if last_total != whole:
        raise ValueError(
            f"the burn log's last total dropped ({last_total}) is not the sidecar's {whole}"
        )
    kv["v4l2_dropped"] = str(inside)
    kv["v4l2_dropped_whole_run"] = str(whole)
    kv["v4l2_drop_window"] = f"{start}..{end}"
    # temp file + rename: a failed write leaves the original sidecar, never a truncated one
    tmp = f"{sidecar}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        for k, v in kv.items():
            f.write(f"{k}={v}\n")
    os.replace(tmp, sidecar)
    return (
        f"capture drops scoped to the recording window {start}..{end}: {inside} counted, "
        f"{whole - inside} of them outside the recording window (issue 1482)"
    )


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--sidecar", required=True)
    p.add_argument("--burn-log", required=True)
    p.add_argument("--start", type=int, required=True)
    p.add_argument("--end", type=int, required=True)
    a = p.parse_args(argv)
    try:
        print("    " + scope(a.sidecar, a.burn_log, a.start, a.end))
    except (ValueError, KeyError, OSError) as exc:
        print(
            f"WARNING issue 1482: capture drops not scoped ({exc}); the whole-run count kept",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
