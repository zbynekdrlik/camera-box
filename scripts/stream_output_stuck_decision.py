#!/usr/bin/env python3
"""issue 1460 (owner ROZHODNUTÉ 12.11.2026): PURE decision core of the dev1 stuck-output page.

The owner's ruling, verbatim: "na discord do alerts keď je scéna PRO na stream obs a je zapnuté
streamovanie, vtedy má prísť správa do discordu, že sa zasekol výstup". So the page fires ONLY while
the stream OBS programs the production scene (`PRO`, declared once in scripts/lib/stream-dev-scene.sh
and passed in, never retyped here) AND its stream output is active AND that output is stuck. Outside
that state it never pages.

"Stuck" is read from two snapshots the probe (scripts/stream_output_stuck_probe.py) takes a few
seconds apart inside ONE obs-websocket session:
  render       -- GetStats.renderTotalFrames did not advance: the program render froze.
  encode       -- GetStreamStatus.outputTotalFrames did not advance: the stream output sends no frames.
  bytes        -- GetStreamStatus.outputBytes did not advance: nothing leaves OBS.
  reconnecting -- GetStreamStatus.outputReconnecting is true in either snapshot: the output lost its
                  connection.
A live stream advances every counter many times over in a few seconds (12.11.2026 live read: render
~90 frames per 3 s), so a counter standing still is a stuck output, not jitter. A counter that went
BACKWARDS is a new stream session started between the snapshots, never a stall.

Verdicts:
  SKIP           -- the stream OBS was not reachable (the network-reach watchdog owns that page).
  UNKNOWN        -- a fact needed to decide is missing (scene, stream state or a counter unread).
                    Never a page: an unreadable state is never a guessed production.
  NOT_PRODUCTION -- the program scene is not the production scene, or the stream is not active in
                    both snapshots. Never a page (the owner's rule).
  OK             -- production, streaming, every counter advanced, no reconnect.
  STUCK          -- production, streaming, and at least one reason above. The watchdog pages it after
                    its 2-pass confirm.

No I/O beyond the CLI wrapper (stdin -> stdout); pytest: tests/python/test_stream_output_stuck_1460.py.
"""
import argparse
import sys

COUNTERS = (("render", "render"), ("encode", "frames"), ("bytes", "bytes"))


def parse_probe(text):
    """The probe's `key=value` lines -> dict of strings. Unknown/garbled lines are ignored; a missing
    key is simply absent (read as unknown downstream)."""
    out = {}
    for ln in (text or "").splitlines():
        k, sep, v = ln.partition("=")
        if sep and k.strip():
            out[k.strip()] = v.strip()
    return out


def _int(p, key):
    raw = p.get(key, "")
    if raw == "":
        return None
    try:
        return int(float(raw))
    except (ValueError, TypeError):
        return None


def _flag(p, key):
    """A 0/1 flag, or None when absent / not exactly 0 or 1."""
    raw = p.get(key, "")
    return int(raw) if raw in ("0", "1") else None


def classify(p, box_reachable, production_scene):
    """-> (verdict, reasons list). See the module docstring."""
    if box_reachable != 1:
        return "SKIP", []
    if not production_scene:
        return "UNKNOWN", ["no production scene name"]
    scene = p.get("scene", "")
    if scene == "":
        return "UNKNOWN", ["program scene unread"]
    if scene != production_scene:
        return "NOT_PRODUCTION", []
    a0, a1 = _flag(p, "active0"), _flag(p, "active1")
    if a0 is None or a1 is None:
        return "UNKNOWN", ["stream state unread"]
    if a0 != 1 or a1 != 1:
        return "NOT_PRODUCTION", []
    reasons = []
    for name, key in COUNTERS:
        v0, v1 = _int(p, key + "0"), _int(p, key + "1")
        if v0 is None or v1 is None:
            return "UNKNOWN", [name + " counter unread"]
        if v1 == v0:
            reasons.append(name)
    r0, r1 = _flag(p, "reconnecting0"), _flag(p, "reconnecting1")
    if r0 == 1 or r1 == 1:
        reasons.append("reconnecting")
    return ("STUCK" if reasons else "OK"), reasons


def analyze(text, box_reachable, production_scene):
    """Fetch result -> dict with verdict, reasons, scene and the three counter deltas (None when
    unread). An unreachable box returns SKIP without parsing the (empty) body."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "reasons": [], "scene": "", "deltas": {}}
    p = parse_probe(text)
    verdict, reasons = classify(p, box_reachable, production_scene)
    deltas = {}
    for name, key in COUNTERS:
        v0, v1 = _int(p, key + "0"), _int(p, key + "1")
        deltas[name] = None if v0 is None or v1 is None else v1 - v0
    return {"verdict": verdict, "reasons": reasons, "scene": p.get("scene", ""), "deltas": deltas}


def _main(argv):
    ap = argparse.ArgumentParser(description="pure stuck-output page decision (issue 1460)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze", help="the probe's key=value stdin -> verdict, reasons, scene, deltas")
    a.add_argument("--box-reachable", type=int, required=True)
    # REQUIRED -- the production scene comes from scripts/lib/stream-dev-scene.sh, never a default here.
    a.add_argument("--production-scene", required=True)
    ns = ap.parse_args(argv)
    text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
    res = analyze(text, ns.box_reachable, ns.production_scene)
    print(f"verdict={res['verdict']}")
    print(f"reasons={','.join(res['reasons'])}")
    print(f"scene={res['scene']}")
    for name, _ in COUNTERS:
        d = res["deltas"].get(name)
        print(f"{name}_delta={'' if d is None else d}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
