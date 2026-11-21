#!/usr/bin/env python3
"""issue 1534 (owner ROZHODNUTE 18.11.2026): the full-path E2E is a STRICT zero frame-loss gate for the
WHOLE OBS session, not only the recording window -- the video half (the audio half is
scripts/dante_input_continuity.py's `session` check).

Input = one box's whole-session read, `strih_log_remote_cmd linux session` (scripts/lib/strih-log-read.sh):
a `#newest<TAB>name<TAB>bytes` header, `<byte offset>:<line>` lines of the two kinds below, and
`#count<TAB>audit|jxs<TAB>lines<TAB>lossy windows<TAB>input` lines. The box's awk keeps per input the
first 40 lines of each kind, every audit line whose underruns/relocks changed or whose received went
backwards, every lossy JPEG XS window and the last line, so the read stays small on a multi-day log.

  genlock-fifo audit '<input>': received=N ... underruns=N holds=N ... dropped_due=N relocks=N ...
      cumulative since the source started (libobs obs-source.c). An underrun = the render tick found
      no frame and repeated the previous one; a relock = the FIFO re-anchored.
  [distroav] jxs-source '<input>': fps=X frames=N lost_slices=N concealed=N undecodable=N reopens=N ...
      dropped_unread=N foreign=N ...   counts of ONE 5 s window (read live 18.11.2026), never cumulative.

Per input:
  * SETTLE = the input's first audit line + SETTLE_S (60 s): the startup underruns are normal (0-4 in the
    first 30 s live). `underruns` and `relocks` at the log's last audit line minus their value at the
    settle point (the last audit line at or before it) must be 0: any growth = FAIL naming the input, the
    counter, the count and the first line it grew on;
  * a JPEG XS input: JXS_GATED summed over every window after its first window WITH FRAMES + SETTLE_S
    must be 0 (the window gate's own set, scripts/jxs_link_continuity.py; a concealed slice is a LOST
    slice). The first window after the decoder opens always loses slices, a camera can connect late;
  * `dropped_due`, `holds`, `late_holds`, `n2_early`, `stamp_gap` are reported as their growth, never
    graded (`dropped_due` advances by design on a 60 -> 30 input, issue 1221).
UNKNOWN (fails the run): a declared input with no audit line, a declared JPEG XS input with no window
line, `received` going backwards after the settle (the source restarted: its counters restarted), the
settle not covered by the read, another log than the window's (OBS restarted), a log that has not
reached the window's stop mark (`retryable`), an empty read. Every input the log carries is graded, the
declared ones must be there. Pure; the CLI reads the box reads and writes the verdict block
`genlock_session_continuity`.
"""
import argparse
import json
import re
import sys

KEY = "genlock_session_continuity"
SETTLE_S = 60.0
GATED_AUDIT = ("underruns", "relocks")
CONTEXT_AUDIT = ("dropped_due", "holds", "late_holds", "n2_early", "stamp_gap")
JXS_GATED = ("lost_slices", "concealed", "undecodable", "dropped_unread", "foreign", "reopens")
AUDIT_MARK = "genlock-fifo audit '"
JXS_MARK = "] jxs-source '"
_DAY = re.compile(r"^D(-?\d+):")
_TS = re.compile(r"^(\d\d):(\d\d):(\d\d(?:\.\d+)?): ")
_FIELD = re.compile(r"(?:^|\s)([a-z_0-9]+)=(-?\d+)(?=\s|$)")
_RANK = {"PASS": 0, "UNKNOWN": 1, "FAIL": 2}
# the strih camera inputs the E2E itself restarts (burn units and back): a growth inside the run's
# recorded camera-handling range of the strih log (`touch`) + SETTLE_S is the run's own, never a loss
STALE_TOUCH_S = 3600.0  # an open touch of ANOTHER run (it died before its cleanup) covers one hour, never more
_CAMERA_INPUT = re.compile(r"^(?:JXS cam[0-9]+ src|NDI cam[0-9]+)$")


def _name(text, mark):
    p = text.find(mark)
    if p < 0:
        return None
    rest = text[p + len(mark):]
    q = rest.find("'")
    return rest[:q] if q >= 0 else None


def _fields(text):
    """The FIRST `key=` of each name, like the box's awk `fv` (both sides read the same field)."""
    out = {}
    for k, v in _FIELD.findall(text):
        out.setdefault(k, int(v))
    return out


def _sod(text):
    m = _TS.match(text)
    if not m:
        return None
    return int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3))


def parse_read(text):
    """The box's read -> (newest name, newest bytes, [entry], {(kind, input): (lines, lossy)}).
    entry = {off, text, t, kind, input, f}; t = seconds since the first line's midnight, unwrapped in
    file order (a log over midnight keeps counting)."""
    name, size, counts, rows = None, None, {}, {}
    for raw in (text or "").split("\n"):
        raw = raw.rstrip("\r")
        if raw.startswith("#newest\t"):
            parts = raw.split("\t")
            if len(parts) >= 3 and parts[2].isdigit():
                name, size = parts[1], int(parts[2])
            continue
        if raw.startswith("#count\t"):
            parts = raw.split("\t", 4)
            if len(parts) == 5 and parts[2].isdigit() and parts[3].isdigit():
                counts[(parts[1], parts[4])] = (int(parts[2]), int(parts[3]))
            continue
        off, sep, line = raw.partition(":")
        if not sep or not off.isdigit():
            continue
        day = None
        m = _DAY.match(line)
        if m:  # the box's awk counted midnights over EVERY line (the read itself is sparse)
            day, line = int(m.group(1)), line[m.end():]
        if AUDIT_MARK in line:
            kind, inp = "audit", _name(line, AUDIT_MARK)
        elif JXS_MARK in line and " fps=" in line:
            kind, inp = "jxs", _name(line, JXS_MARK)
        else:
            continue
        sod = _sod(line)
        if inp is None or sod is None:
            continue
        rows[int(off)] = {"off": int(off), "text": line, "sod": sod, "kind": kind, "input": inp,
                          "f": _fields(line), "day": day}
    entries = [rows[k] for k in sorted(rows)]
    if entries and all(e["day"] is not None for e in entries):
        for e in entries:
            e["t"] = e["sod"] + 86400.0 * e["day"]
        return name, size, entries, counts
    # a dense read without day stamps: a line more than 12 h behind the previous one starts the next
    # day; a line a moment before midnight written right after one past it stays on its own day
    day, prev = 0.0, None
    for e in entries:
        t = e["sod"] + day
        if prev is not None and t < prev - 43200:
            day += 86400.0
            t += 86400.0
        elif prev is not None and t > prev + 43200:
            t -= 86400.0
        e["t"] = prev = t
    return name, size, entries, counts


def _short(text):
    return text[:12]


def _in_zone(t, zones):
    return any(a <= t <= b for a, b in zones)


def touch_zones(entries, touches):
    """`touches` = [(start byte, end byte | None | "stale")] -> [(t from, t to)]: from the first line at
    or after the start to the first line at or after the end + SETTLE_S; None = this run's own, still
    open (to the end of the read); "stale" = another run's open touch, STALE_TOUCH_S from its start."""
    zones = []
    for start, end in touches or ():
        first = next((e["t"] for e in entries if e["off"] >= start), None)
        if first is None:
            continue
        if end is None:
            zones.append((first, float("inf")))
            continue
        if end == "stale":
            zones.append((first, first + STALE_TOUCH_S))
            continue
        last = next((e["t"] for e in entries if e["off"] >= end), float("inf"))
        zones.append((first, last + SETTLE_S))
    return zones


def _grade_audit(inp, rows, total, res, zones=()):
    """`rows`: the input's audit entries in file order; `zones` = the run's own camera handling."""
    t0 = rows[0]["t"]
    settle = t0 + SETTLE_S
    base = rows[0]
    for e in rows[1:]:  # file order: the last line before the first one past the settle
        if e["t"] > settle:
            break
        base = e
    if total > len(rows) and all(e["t"] <= settle for e in rows):
        res["unknown_reasons"].append(f"'{inp}': the read does not reach past its settle point")
        return
    res["settle"] = _short(base["text"])
    last = rows[-1]
    res["closing"] = _short(last["text"])
    for prev, e in zip(rows, rows[1:]):
        if _in_zone(e["t"], zones):
            continue
        if e["t"] > settle and e["f"].get("received", 0) < prev["f"].get("received", 0):
            res["unknown_reasons"].append(
                f"'{inp}': the source restarted at {_short(e['text'])} (received "
                f"{prev['f'].get('received')} -> {e['f'].get('received')}): its counters restarted")
            return
    after = rows[rows.index(base):]
    for k in GATED_AUDIT:
        b, a = base["f"].get(k), last["f"].get(k)
        if b is None or a is None:
            res["unknown_reasons"].append(f"'{inp}': `{k}=` is not on its audit lines")
            continue
        counted = excluded = 0
        for prev, e in zip(after, after[1:]):
            inc = e["f"].get(k, 0) - prev["f"].get(k, 0)
            if inc <= 0:
                continue
            if _in_zone(e["t"], zones):
                excluded += inc
            else:
                counted += inc
        res["counters"][k] = {"settle": b, "closing": a, "delta": counted}
        if excluded:
            res["context"][f"run_touch_excluded_{k}"] = excluded
        if counted > 0:
            res["failures"].append(f"'{inp}' {k} +{counted} after its start settle")
    for k in CONTEXT_AUDIT:
        b, a = base["f"].get(k), last["f"].get(k)
        if b is not None and a is not None:
            res["context"][k] = a - b
    for prev, e in zip(after, after[1:]):
        if not _in_zone(e["t"], zones) and any(e["f"].get(k, 0) > prev["f"].get(k, 0) for k in GATED_AUDIT):
            res["first_growth"] = _short(e["text"])
            break


def _grade_jxs(inp, rows, total_lossy, res, zones=()):
    with_frames = [e for e in rows if e["f"].get("frames", 0) > 0]
    j = {"windows": len(rows), "lossy_windows": 0, "unlisted_lossy_windows": 0,
         "sums": {k: 0 for k in JXS_GATED}, "lossy": []}
    res["jxs"] = j
    if not with_frames:
        res["unknown_reasons"].append(f"'{inp}': no JPEG XS window with frames in this OBS session")
        return
    settle = with_frames[0]["t"] + SETTLE_S
    j["settle_after"] = _short(with_frames[0]["text"])
    listed_lossy = 0
    for e in rows:
        bad = {k: e["f"].get(k, 0) for k in JXS_GATED if e["f"].get(k, 0)}
        if bad:
            listed_lossy += 1
        if bad and e["t"] > settle and _in_zone(e["t"], zones):
            j["run_touch_excluded_windows"] = j.get("run_touch_excluded_windows", 0) + 1
            continue
        if bad and e["t"] > settle:
            j["lossy_windows"] += 1
            for k, v in bad.items():
                j["sums"][k] += v
            if len(j["lossy"]) < 10:
                j["lossy"].append(e["text"])
    j["windows"] = max(j["windows"], res.pop("_jxs_total", 0))
    unlisted = max(0, total_lossy - listed_lossy)
    if unlisted:
        # the box lists every lossy window up to its cap in file order, so the unlisted ones are later
        # than every listed one: past the settle once the listed span reaches it
        if rows[-1]["t"] > settle or j["lossy_windows"]:
            j["unlisted_lossy_windows"] = unlisted
            j["lossy_windows"] += unlisted
        else:
            res["unknown_reasons"].append(f"'{inp}': {unlisted} lossy JPEG XS window(s) not in the read")
    if j["lossy_windows"]:
        sums = ", ".join(f"{k} +{v}" for k, v in j["sums"].items() if v)
        more = f" (+{j['unlisted_lossy_windows']} more windows not listed)" if j["unlisted_lossy_windows"] else ""
        res["failures"].append(f"'{inp}' JPEG XS {j['lossy_windows']} lossy window(s) after its start settle: "
                               f"{sums or 'counts not listed'}{more} (a concealed slice is a LOST slice)")


def _finish(res):
    if res["failures"]:
        res["verdict"], res["pass"] = "FAIL", False
    elif res["unknown_reasons"]:
        res["verdict"], res["pass"] = "UNKNOWN", False
    else:
        res["verdict"], res["pass"] = "PASS", True
    return res


def unknown_box(box, reason, declared=(), retryable=False):
    return {"box": box, "verdict": "UNKNOWN", "pass": False, "declared": list(declared), "inputs": [],
            "failures": [], "unknown_reasons": [reason], "retryable": retryable}


# Owner ROZHODNUTE 20.11.2026 (issue 1534): the cg feed from the resolume cg OBS (minimal priority) is
# graded and reported, never gating. Exact input names only; every camera and stream input gates.
REPORT_ONLY_INPUTS = {
    "CG-obs": "the cg feed from the resolume cg OBS (minimal priority, owner 20.11.2026): report-only",
}


def decide_box(box, text, declared, *, log_name, start_byte, stop_byte, excluded=(), touches=()):
    """One box: `text` = the box's read, `declared` = the inputs the run measures (each must be there),
    `log_name` / `start_byte` / `stop_byte` = the window marks of this box, `excluded` = the inputs of
    cameras the run acked offline (graded and reported, never gating), `touches` = the byte ranges of
    this log in which an E2E run handled the camboxes (camera inputs only)."""
    name, size, entries, counts = parse_read(text)
    if name is None:
        return unknown_box(box, f"the {box} OBS log could not be read", declared)
    if log_name and name != log_name:
        return unknown_box(box, f"the {box} OBS log is now '{name}', the window's was '{log_name}': "
                                f"OBS restarted after the window, its session cannot be proven", declared)
    if size is None or size <= stop_byte:
        return unknown_box(box, f"the {box} OBS log ({size} bytes) has not reached the window's stop mark "
                                f"({stop_byte}) yet", declared, retryable=True)
    res = {"box": box, "log": name, "verdict": "PASS", "pass": True, "declared": list(declared),
           "inputs": [], "failures": [], "unknown_reasons": [], "retryable": False}
    declared = [d for d in declared if d not in excluded]
    zones = touch_zones(entries, touches)
    if touches:
        res["run_touch"] = [[a, b] for a, b in touches]
    names = sorted({e["input"] for e in entries} | set(declared))
    if not names:
        res["unknown_reasons"].append(f"no genlock input on the {box} OBS log")
    for inp in names:
        r = {"input": inp, "declared": inp in declared, "counters": {}, "context": {}, "failures": [],
             "unknown_reasons": [], "excluded": inp in excluded,
             "report_only": inp in REPORT_ONLY_INPUTS}
        audit = [e for e in entries if e["input"] == inp and e["kind"] == "audit"]
        jxs = [e for e in entries if e["input"] == inp and e["kind"] == "jxs"]
        if audit:
            z = zones if _CAMERA_INPUT.match(inp) else ()
            _grade_audit(inp, audit, counts.get(("audit", inp), (len(audit), 0))[0], r, z)
            if audit[0]["off"] >= start_byte:
                r["note"] = "the input's first audit line is inside the recording window"
        elif inp in declared:
            r["unknown_reasons"].append(f"'{inp}': no `genlock-fifo audit` line in this OBS session")
        if jxs or (inp in declared and inp.startswith("JXS ")):
            if jxs:
                total, lossy = counts.get(("jxs", inp), (len(jxs), 0))
                r["_jxs_total"] = total
                _grade_jxs(inp, jxs, lossy, r, zones if _CAMERA_INPUT.match(inp) else ())
            else:
                r["unknown_reasons"].append(f"'{inp}': no `jxs-source` window line in this OBS session")
        r.pop("_jxs_total", None)
        _finish(r)
        res["inputs"].append(r)
        if r["excluded"]:
            r["note"] = "the run acked this camera offline: report-only"
            continue
        if r["report_only"]:
            r["note"] = REPORT_ONLY_INPUTS[inp]
            continue
        res["failures"].extend(r["failures"])
        res["unknown_reasons"].extend(r["unknown_reasons"])
    return _finish(res)


def combine(boxes):
    """The verdict block: every box's result, FAIL over UNKNOWN over PASS, failures named by box."""
    blk = {"verdict": "PASS", "pass": True, "gates_overall_pass": True, "settle_s": SETTLE_S,
           "boxes": {}, "failures": [], "unknown_reasons": [], "retryable": False}
    if not boxes:
        blk["unknown_reasons"].append("no box was graded")
    for b in boxes:
        blk["boxes"][b.get("box", "?")] = b
        blk["failures"] += [f"{b.get('box')} {f}" for f in b.get("failures") or []]
        blk["unknown_reasons"] += [f"{b.get('box')}: {u}" for u in b.get("unknown_reasons") or []]
        blk["retryable"] = blk["retryable"] or bool(b.get("retryable"))
    return _finish(blk)


def summary_lines(blk):
    out = [f"whole-session video continuity (genlock inputs + JPEG XS since each OBS start, settle "
           f"{blk.get('settle_s', SETTLE_S):g} s): {blk.get('verdict')}"]
    for box, b in (blk.get("boxes") or {}).items():
        out.append(f"  {box} ({b.get('log', '?')}): {b.get('verdict')}")
        for i in b.get("inputs") or []:
            c = i.get("counters") or {}
            parts = [f"{k} {v['settle']}->{v['closing']}" for k, v in c.items()]
            j = i.get("jxs")
            if j:
                parts.append(f"JPEG XS lossy windows {j.get('lossy_windows', 0)}")
            ctx = i.get("context") or {}
            if ctx.get("dropped_due"):
                parts.append(f"dropped_due +{ctx['dropped_due']} (context)")
            if i.get("first_growth"):
                parts.append(f"first growth {i['first_growth']}")
            if i.get("report_only") or i.get("excluded"):
                parts.append(i.get("note") or "report-only")
            out.append(f"    '{i['input']}': {i.get('verdict')} -- {', '.join(parts) or 'no counters'}")
    out += [f"  FAIL: {f}" for f in blk.get("failures") or []]
    out += [f"  UNKNOWN: {u}" for u in blk.get("unknown_reasons") or []]
    return out


def merge_into(path, blk):
    with open(path, encoding="utf-8") as f:
        verdict = json.load(f)
    verdict[KEY] = blk
    with open(path, "w", encoding="utf-8") as f:
        json.dump(verdict, f, indent=2)


def _write(path, obj):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2)


def _load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--box", choices=("strih", "stream"))
    ap.add_argument("--read", help="the box's whole-session read (a file)")
    ap.add_argument("--input", action="append", default=[], help="a declared input (repeat)")
    ap.add_argument("--exclude", action="append", default=[],
                    help="an input of a camera the run acked offline: report-only (repeat)")
    ap.add_argument("--log-name", default="")
    ap.add_argument("--start-byte", type=int, default=0)
    ap.add_argument("--stop-byte", type=int, default=0)
    ap.add_argument("--touch", action="append", default=[],
                    help="START-END (END empty = this run's, still open; ~ = another run's, left open): a byte range of this log in which an E2E run "
                         "handled the camboxes; growth there on a camera input is the run's own (repeat)")
    ap.add_argument("--unknown", help="an UNKNOWN box result with this reason (no read)")
    ap.add_argument("--box-out", help="write the box result here")
    ap.add_argument("--combine", nargs="+", help="box result files -> the block")
    ap.add_argument("--out", help="write the block here (with --combine)")
    ap.add_argument("--from-block", help="print a block's summary; exit 0 only on PASS")
    ap.add_argument("--merge-into", help="merge the block into this verdict JSON (with --from-block)")
    a = ap.parse_args(argv)
    if a.combine:
        boxes = []
        for p in a.combine:
            b = _load(p)
            boxes.append(b if isinstance(b, dict) else unknown_box("?", f"the box result {p} is not readable"))
        blk = combine(boxes)
        if a.out:
            _write(a.out, blk)
        print("\n".join(summary_lines(blk)))
        return {"PASS": 0, "FAIL": 1}.get(blk["verdict"], 2)
    if a.from_block:
        blk = _load(a.from_block)
        if not isinstance(blk, dict):
            blk = combine([unknown_box("?", "the session continuity block was not written")])
        if a.merge_into:
            merge_into(a.merge_into, blk)
        print("\n".join(summary_lines(blk)))
        return 0 if blk.get("pass") is True else 1
    if not a.box:
        ap.error("--box is required (or --combine / --from-block)")
    if a.unknown:
        res = unknown_box(a.box, a.unknown, a.input)
    else:
        try:
            with open(a.read or "", encoding="utf-8", errors="replace") as f:
                text = f.read()
        except OSError as e:
            res = unknown_box(a.box, f"the {a.box} read is not readable: {e}", a.input)
        else:
            touches = []
            for t in a.touch:
                lo, _, hi = t.partition("-")
                if lo.isdigit() and (hi in ("", "~") or hi.isdigit()):
                    touches.append((int(lo), "stale" if hi == "~" else (int(hi) if hi else None)))
            res = decide_box(a.box, text, a.input, log_name=a.log_name, start_byte=a.start_byte,
                             stop_byte=a.stop_byte, excluded=a.exclude, touches=touches)
    if a.box_out:
        _write(a.box_out, res)
    print(f"{a.box}: {res['verdict']}")
    if a.box_out:
        return 0  # the result is written; --combine / --from-block decide the gate
    return {"PASS": 0, "FAIL": 1}.get(res["verdict"], 2)


if __name__ == "__main__":
    sys.exit(main())
