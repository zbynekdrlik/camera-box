#!/usr/bin/env python3
"""issue 1367 -- the 8 h A/V soak's pure RIG-STATE decisions (no rig I/O; pytest Tier-0).

The soak (scripts/av-soak.sh) reads the rig with `obs_phase2.py rig-busy-check`, one JSON line:
``{"busy": bool|None, "diagnostics": [{"host": "strih"|"stream", "streaming": bool,
"recording": bool, "recordTimecode": "HH:MM:SS.mmm"|None}, ...]}`` (exit 3 with busy=None when a
box is unreachable; the readable box's diagnostics are still printed). Two decisions are made
here, never by hand-parsing that line in the shell:

- **broadcast_state** -- is a broadcast live right now? `live` when any READABLE box streams (a
  live broadcast on a readable box wins over an unreadable other box), `unknown` when a box is
  missing / the read failed, else `idle`. Cleanup restores the strih program only on `idle`:
  strih's program feeds the stream box's program, so a cut there during a broadcast is a cut on
  air (the issue-1271 rule: never mutate the rig while a broadcast may be live).
- **leftovers_plan** -- which of the soak's flagged recordings may `--stop-leftovers` (the unit's
  ExecStopPost) stop. Strih never streams: "recording and not streaming" on strih alone is strih's
  NORMAL broadcast state (a Companion recording on both boxes), so a per-box check proves nothing.
  The plan therefore:
    * touches nothing while ANY box streams (a broadcast) or a box is unreadable;
    * clears a flagged box that is not recording;
    * stops a flagged box's recording only when it provably is the soak's: the recording's own
      age (`recordTimecode`) puts its START inside [flag time - SINCE_SLACK_S, flag time +
      start window] -- the soak writes the flag time into recording.state right BEFORE its
      StartRecord, and that bounded call cannot start a recording later than its own timeout
      (the window is written by the run itself, `start_window_s`; the caller's value is only the
      fallback for an older state file);
    * keeps everything else (no flag time, an unreadable age, a start outside the window), always
      with the reason. Outside the window is "cannot prove it is the soak's", never "someone
      else's": the age is obs-websocket's frame-count duration (total frames x frame time), which
      undercounts wall time by every lagged render frame, so over hours even the soak's own
      recording can read as started later than it did.
  An unflagged box is never in the plan.

recording.state (written by the soak, one `key=value` per line):
``strih=0|1``, ``strih_since=<epoch s>``, ``stream=0|1``, ``stream_since=<epoch s>``,
``lease=<the soak's rig-lease run id>``, ``start_window_s=<s>``. The pre-round-3 two-line shape
still parses.

CLI (for the shell): ``broadcast`` (stdin = the rig-busy JSON -> prints live|unknown|idle) and
``leftovers --state FILE --now EPOCH --start-window-s S`` (stdin = the rig-busy JSON -> one
``<box>\\t<stop|clear|keep>\\t<reason>`` line per flagged box). Exit 0, 3 on a usage/input error.
"""
import argparse
import json
import re
import sys

LIVE, IDLE, UNKNOWN = "live", "idle", "unknown"
STOP, CLEAR, KEEP = "stop", "clear", "keep"
BOXES = ("strih", "stream")
# The soak stamps the flag time just before its StartRecord: a recording that started a few
# seconds EARLIER than the stamp is still the soak's (clock/rounding slack), never more.
SINCE_SLACK_S = 5

_TC_RE = re.compile(r"^(\d+):([0-5]\d):([0-5]\d)(?:\.(\d+))?$")


def parse_timecode(tc):
    """OBS `outputTimecode` ("HH:MM:SS.mmm") -> seconds, None when unreadable."""
    if not isinstance(tc, str):
        return None
    m = _TC_RE.match(tc.strip())
    if not m:
        return None
    h, mi, s, frac = m.groups()
    return int(h) * 3600 + int(mi) * 60 + int(s) + (float("0." + frac) if frac else 0.0)


def _diagnostics(busy_text):
    """-> (per-box diagnostics dict, complete) from the rig-busy JSON; ({}, False) on garbage."""
    try:
        d = json.loads(busy_text or "")
    except ValueError:
        return {}, False
    if not isinstance(d, dict):
        return {}, False
    diags = {}
    for x in d.get("diagnostics") or []:
        if isinstance(x, dict) and x.get("host") in BOXES:
            diags[x["host"]] = x
    complete = d.get("busy") is not None and all(b in diags for b in BOXES)
    return diags, complete


def broadcast_state(busy_text):
    """LIVE when any readable box streams, UNKNOWN when the read is incomplete, else IDLE."""
    diags, complete = _diagnostics(busy_text)
    if any(x.get("streaming") is True for x in diags.values()):
        return LIVE
    return IDLE if complete else UNKNOWN


def parse_state(text):
    """recording.state text -> {"flags": {box: bool}, "since": {box: float|None}, "lease": str|None}"""
    kv = {}
    for line in (text or "").splitlines():
        k, sep, v = line.strip().partition("=")
        if sep:
            kv[k.strip()] = v.strip()
    since = {}
    for b in BOXES:
        try:
            since[b] = float(kv.get(f"{b}_since", ""))
        except ValueError:
            since[b] = None
    try:
        window = float(kv.get("start_window_s", ""))
    except ValueError:
        window = None
    return {"flags": {b: kv.get(b) == "1" for b in BOXES}, "since": since,
            "lease": kv.get("lease") or None, "start_window_s": window}


def leftovers_plan(state_text, busy_text, now_s, start_window_s):
    """-> [(box, STOP|CLEAR|KEEP, reason)] for every flagged box, in BOXES order."""
    st = parse_state(state_text)
    flagged = [b for b in BOXES if st["flags"][b]]
    if not flagged:
        return []
    diags, complete = _diagnostics(busy_text)
    streaming = [b for b in BOXES if diags.get(b, {}).get("streaming") is True]
    if streaming:
        why = (f"a broadcast is live ({', '.join(streaming)} streaming) -- nothing is touched "
               f"(strih never streams: its recording may be the broadcast's)")
        return [(b, KEEP, why) for b in flagged]
    if not complete:
        return [(b, KEEP, "the rig state is unreadable (a box did not answer) -- nothing is touched")
                for b in flagged]
    window = st["start_window_s"] if st["start_window_s"] is not None else start_window_s
    plan = []
    for b in flagged:
        diag = diags[b]
        if diag.get("recording") is not True:
            plan.append((b, CLEAR, "not recording"))
            continue
        age = parse_timecode(diag.get("recordTimecode"))
        since = st["since"][b]
        if since is None:
            plan.append((b, KEEP, "no flag start time in recording.state -- cannot prove the "
                                  "recording is the soak's"))
            continue
        if age is None:
            plan.append((b, KEEP, f"the recording's age is unreadable "
                                  f"({diag.get('recordTimecode')!r}) -- cannot prove it is the soak's"))
            continue
        start = now_s - age
        if since - SINCE_SLACK_S <= start <= since + window:
            plan.append((b, STOP, f"the soak's own recording (started {start - since:+.0f} s from "
                                  f"its flag)"))
        else:
            plan.append((b, KEEP, f"the recording started {start - since:+.0f} s from the soak's "
                                  f"flag, outside [-{SINCE_SLACK_S}, +{window:g}] s -- cannot prove "
                                  f"it is the soak's"))
    return plan


def _cmd_broadcast(_a):
    print(broadcast_state(sys.stdin.read()))
    return 0


def _cmd_leftovers(a):
    try:
        with open(a.state, encoding="utf-8") as f:
            state = f.read()
    except OSError as e:
        print(f"av_soak_rig_state: cannot read {a.state}: {e}", file=sys.stderr)
        return 3
    for box, action, reason in leftovers_plan(state, sys.stdin.read(), a.now, a.start_window_s):
        print(f"{box}\t{action}\t{reason}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("broadcast", help="stdin rig-busy JSON -> live|unknown|idle")
    lo = sub.add_parser("leftovers", help="stdin rig-busy JSON -> the stop-leftovers plan")
    lo.add_argument("--state", required=True)
    lo.add_argument("--now", type=float, required=True)
    lo.add_argument("--start-window-s", type=float, required=True)
    try:
        a = ap.parse_args(argv)
    except SystemExit as e:
        return 3 if e.code else 0
    return {"broadcast": _cmd_broadcast, "leftovers": _cmd_leftovers}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
