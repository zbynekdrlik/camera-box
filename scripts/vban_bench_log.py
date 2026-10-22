#!/usr/bin/env python3
"""Issue 1460 slice 1 -- the OBS-log rows of the VBAN receiver bench (scripts/vban-receiver-bench.sh).

The bench reads the strih-snv OBS log lines written while one profile ran (the issue-1360 reader's
line-count mark, then the lines after it) and grades the rows the issue-1439 design names for its
acceptance (designs 6338127636 + 6338457001):

  * `buffering-guard: '<input>' ...` (the issue-1381/1425 mix guard, obs-audio.c): the test input
    fell behind the mix window and was re-anchored -- a pop. Any line naming the test input FAILs;
    lines naming other inputs are counted as a note (not the bench's input).
  * `asrc: source '<input>' estimated=..ppm ... steps=N ... place_jumps=N ... basis=..` (the genlock
    ASRC, obs-source.c, one line a minute): the LAST line must exist (the input is on the ASRC
    path), carry `steps=0` and `place_jumps=0`, and its `estimated` must follow the injected sender
    offset within the bound (a fast sender reads positive: the regression's raw-vs-master slope).
  * the receive rows: every other line that names the input in quotes. The stock obs-vban source
    logs `source '<input>': missing N packet(s) at frame F` per gap; the issue-1439 receiver adds a
    10 s status line (received / lost / late / re-anchors / fitted ppm / R). Its exact text is not
    fixed yet, so this grader reads any `key=value` integer it finds there: a `late`, `reanchors`
    (`re_anchors`, `re-anchors`) value above 0 on the LAST status row FAILs, and a `lost` value (or
    the sum of the stock `missing N packet(s)` lines) different from the packets the emulator
    dropped FAILs. A row the grader cannot read is listed, never guessed at.

PURE (no I/O outside `main`). Tier-0 tests: tests/python/test_vban_bench_obs_1460.py.

Usage: vban_bench_log.py --log FILE --input-name NAME [--expected-lost N] [--expect-ppm X]
                         [--ppm-bound P] [--json]
Exit: 0 PASS, 1 FAIL, 3 usage / unreadable log.
"""
from __future__ import annotations

import argparse
import json
import re
import sys

PPM_BOUND = 5.0
_KV_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_-]*)=(-?\d+(?:\.\d+)?)")
_MISSING_RE = re.compile(r"missing (\d+) packet\(s\)")


def _kv(text):
    return {k.replace("-", "_"): float(v) for k, v in _KV_RE.findall(text)}


def rows(lines, input_name):
    """The bench's rows out of OBS log lines (the module doc). Pure."""
    quoted = "'%s'" % input_name
    guard_ours, guard_other, asrc, receive, missing = [], 0, [], [], []
    for raw in lines:
        line = raw.rstrip("\r\n")
        if "buffering-guard:" in line:
            if "buffering-guard: %s" % quoted in line:
                guard_ours.append(line)
            else:
                guard_other += 1
            continue
        if "asrc: source %s" % quoted in line:
            asrc.append(_kv(line.split("asrc: source %s" % quoted, 1)[1]))
            continue
        if quoted in line:
            receive.append(line)
            m = _MISSING_RE.search(line)
            if m:
                missing.append(int(m.group(1)))
    # a status row = a non-`missing` row that carries key=value numbers (an add/remove line has none)
    status = [r for r in receive if _MISSING_RE.search(r) is None and _KV_RE.search(r.split(quoted, 1)[1])]
    return {
        "buffering_guard": guard_ours, "buffering_guard_other_inputs": guard_other,
        "asrc_lines": len(asrc), "asrc_last": asrc[-1] if asrc else None,
        "receive_rows": receive, "missing_packets_events": len(missing),
        "missing_packets_total": sum(missing),
        "status_last": _kv(status[-1]) if status else None,
    }


def grade(r, expected_lost=None, expect_ppm=None, ppm_bound=PPM_BOUND):
    """PASS/FAIL over the rows. Returns {verdict, reasons, notes}."""
    reasons, notes = [], []
    if r["buffering_guard"]:
        reasons.append("%d buffering-guard line(s) re-anchored the test input (a pop): %s"
                       % (len(r["buffering_guard"]), r["buffering_guard"][0][:200]))
    if r["buffering_guard_other_inputs"]:
        notes.append("%d buffering-guard line(s) named other inputs" % r["buffering_guard_other_inputs"])
    last = r["asrc_last"]
    if last is None:
        reasons.append("no `asrc: source` line for the test input: it is not on the genlock ASRC path")
    else:
        for key in ("steps", "place_jumps"):
            if last.get(key, 0) > 0:
                reasons.append("asrc %s=%d on the test input (want 0)" % (key, int(last[key])))
        est = last.get("estimated")
        if expect_ppm is not None:
            if est is None:
                reasons.append("the asrc line carries no estimated= value")
            elif abs(est - expect_ppm) > ppm_bound:
                reasons.append("asrc estimated=%+.2f ppm vs the injected %+.3f ppm (bound %.1f)"
                               % (est, expect_ppm, ppm_bound))
    st = r["status_last"]
    if st is not None:
        for key in ("late", "reanchors", "re_anchors"):
            if st.get(key, 0) > 0:
                reasons.append("the receive status reports %s=%d (want 0)" % (key, int(st[key])))
        if expected_lost is not None and "lost" in st and int(st["lost"]) != expected_lost:
            reasons.append("the receive status reports lost=%d, the emulator dropped %d"
                           % (int(st["lost"]), expected_lost))
    if expected_lost is not None and r["missing_packets_events"] and r["missing_packets_total"] != expected_lost:
        reasons.append("the receiver logged %d missing packet(s), the emulator dropped %d"
                       % (r["missing_packets_total"], expected_lost))
    if st is None and not r["missing_packets_events"]:
        notes.append("no receive status row for the test input in the log")
    return {"verdict": "FAIL" if reasons else "PASS", "reasons": reasons, "notes": notes}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--log", required=True)
    ap.add_argument("--input-name", required=True)
    ap.add_argument("--expected-lost", type=int, default=None)
    ap.add_argument("--expect-ppm", type=float, default=None)
    ap.add_argument("--ppm-bound", type=float, default=PPM_BOUND)
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    try:
        with open(a.log, encoding="utf-8", errors="replace") as fh:
            lines = fh.readlines()
    except OSError as e:
        print("vban-bench-log: %s" % e, file=sys.stderr)
        return 3
    r = rows(lines, a.input_name)
    g = grade(r, a.expected_lost, a.expect_ppm, a.ppm_bound)
    if a.json:
        print(json.dumps({"verdict": g["verdict"], "grade": g, "rows": r}, sort_keys=True))
    else:
        print("log verdict=%s asrc_lines=%d buffering_guard=%d receive_rows=%d"
              % (g["verdict"], r["asrc_lines"], len(r["buffering_guard"]), len(r["receive_rows"])))
        for x in g["reasons"]:
            print("  FAIL: " + x)
        for x in g["notes"]:
            print("  note: " + x)
    return 0 if g["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
