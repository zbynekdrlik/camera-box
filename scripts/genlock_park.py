#!/usr/bin/env python3
"""genlock_park.py -- the ONE parser of the strih connect-on-show PARK state (issue 1242).

strih-lx pulls FULL bandwidth only for the cameras that are SHOWN. The vendored DistroAV receiver
(`vendor/distroav/src/ndi-source.cpp`, `genlock_connect_on_show_park_decision`) PARKS a genlocked
program-path input (`genlock_connect_on_show`) while nothing shows it: it releases its NDI receiver,
so the input's `genlock-fifo audit received=` counter stops advancing (and its `recv-timing #797`
line stops). It logs the state on the OBS log:

    genlock-park '<src>': state=parked parked_s=N (connect-on-show, hidden; ...)    # every 5 s
    genlock-park '<src>': state=unparked parked_s=N (shown; reconnecting, ...)      # once, on show

The always-connected low-bandwidth monitor twin `MV <input>` (the #501 `genlock_monitor` role that
feeds the built-in multiview) keeps each camera leg observable while its main is parked.

Every consumer of the received= tap (dev1 watchdogs, rig-health-audit) reads the park state through
THIS module (python) or its byte-for-byte bash twin `scripts/lib/genlock-park.sh` (pinned by
`tests/python/test_genlock_park_1242.py`) and treats a parked input as HIDDEN BY DESIGN -> SKIP,
never FROZEN / wrong-cadence / halved / arrivals-low.

A source's state is the state of its LAST park line in the given log window (file order). No park
line in the window -> None (never parked recently, or unparked long ago) -> the consumer's normal
classification applies. A parked heartbeat is re-logged every 5 s, so any bounded tail that carries
audit lines also carries the park line of a parked input.

CLI (for bash callers that prefer one python process):
    python3 genlock_park.py parked < obs.log        # one parked source name per line
    python3 genlock_park.py state '<src>' < obs.log # parked | unparked | (empty)
"""
import re
import sys

# Quote-anchored: 'NDI cam2' never matches the twin 'MV NDI cam2' (the quote precedes the name).
PARK_RE = re.compile(r"genlock-park '([^']+)': state=(parked|unparked)\b")

TWIN_PREFIX = "MV "


def park_states(text):
    """{source: 'parked'|'unparked'} -- the state of each source's LAST park line, in file order."""
    states = {}
    for m in PARK_RE.finditer(text or ""):
        states[m.group(1)] = m.group(2)
    return states


def park_state_of(text, source):
    """The state ('parked'|'unparked') of `source`'s last park line in `text`, or None."""
    return park_states(text).get(source)


def parked_sources(text):
    """The set of sources whose last park line says state=parked."""
    return {s for s, st in park_states(text).items() if st == "parked"}


def is_monitor_twin(name):
    """True for an always-connected low-bandwidth multiview twin input ('MV NDI cam3')."""
    return (name or "").startswith(TWIN_PREFIX)


def twin_of(main):
    """'NDI cam3' -> 'MV NDI cam3'."""
    return TWIN_PREFIX + main


def main_of(twin):
    """'MV NDI cam3' -> 'NDI cam3' (the twin's program-path main)."""
    return twin[len(TWIN_PREFIX):] if is_monitor_twin(twin) else twin


def watch_set(names, text):
    """Given the enumerated source `names` (order kept) and the log `text`, return exactly ONE live
    receiver to watch per camera:
      - a main that is NOT parked is watched, and its twin (if also enumerated) is dropped -- the main
        already proves the leg, and watching both would double-page one camera;
      - a PARKED main is dropped (hidden by design); its twin (if enumerated) is watched instead;
      - a twin whose main is not enumerated at all is kept (it is the only receiver of that camera).
    A parked main with no twin is dropped entirely -- nothing observes it while it is hidden, which
    is the design (a shown input is watched again the moment it unparks)."""
    names = [n for n in names if n]
    present = set(names)
    parked = parked_sources(text)
    out = []
    for n in names:
        if is_monitor_twin(n):
            main = main_of(n)
            if main in present and main not in parked:
                continue
            out.append(n)
        elif n not in parked:
            out.append(n)
    return out


def main(argv):
    if len(argv) >= 2 and argv[1] == "parked":
        text = sys.stdin.buffer.read().decode("utf-8", "replace")
        for s in sorted(parked_sources(text)):
            print(s)
        return 0
    if len(argv) >= 3 and argv[1] == "state":
        text = sys.stdin.buffer.read().decode("utf-8", "replace")
        print(park_state_of(text, argv[2]) or "")
        return 0
    print("usage: genlock_park.py parked < log | genlock_park.py state '<src>' < log", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
