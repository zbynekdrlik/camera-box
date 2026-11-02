#!/usr/bin/env python3
"""issue 1387 slice 2 (design 6558387590) -- the JPEG XS LINK CONTINUITY gate of the full-path E2E.

On the JPEG XS path every camera reaches strih over its own JPEG XS link and the strih program reaches
stream over one more. A slice lost on any of them is a stripe of the previous frame on screen
(concealed), never a black frame, so the picture verdict alone can miss it. The owner's 23.10.2026
ruling holds here too: a masked loss is a loss. This gate reads each link's own 5 s window lines over
the recording window and FAILS on any loss.

The evidence is each box's OWN OBS log, read between two BYTE marks the E2E takes on its newest log
(the size just before StartRecord, the size right after StopRecord). Positions in the log decide what
is inside the window; the `HH:MM:SS.mmm` stamps only check that the windows cover it.

Read in the window (formats from the shipped C; every count is the window's DELTA, not cumulative):
  * every camera_box_jxs_source writes one line per 5 s (vendor/distroav/src/jxs-source.cpp
    `jxs_log_status`, CB_JXS_STATUS_PERIOD_NS):
      jxs-source '<name>': fps=X frames=N lost_slices=N concealed=N undecodable=N reopens=N
        decoder_reopens=N bpp=X reports=N report_errors=N dropped_unread=N foreign=N camera=N
        link_mbps=N fec_repaired=N (issue 1387)
  * the strih program output writes one line per sender window (jxs-output.cpp
    `jxs_output_log_status`):
      jxs-output '<name>': fps=X frames=N dropped_busy=N offered=N taken=N not_taken=N refused=N
        stamp_dup=N ... send_errors=N encode_errors=N ... fec_sent=N fec_repaired=N (issue 1387)
  * the link's own event lines (`not connected`, `receiver failed`, `listening on UDP`, an output
    `started` / `stopped`): one inside the window is a hole in the link.

Which windows count: the first window line written after the START mark (it covers the start) through
the first one written at or after the STOP mark (it covers the stop) -- one window of slack at each
edge, counted (conservative: a loss up to 5 s outside the recording fails it too).

Verdict per link:
  PASS     the windows cover the recording (no gap over GAP_MAX_S between two window lines from the
           last one before the start to the closing one; a link with no window before the start may
           begin up to one window late), no counted window has frames=0, no hole event, and
             input:  lost_slices + concealed + undecodable + dropped_unread + foreign == 0, reopens 0
             output: dropped_busy + not_taken + refused + send_errors + encode_errors == 0;
           a CONCEALED slice is a LOST slice (concealed > 0 fails on its own, named as lost);
  FAIL     any of those;
  UNKNOWN  no window line of the link in the window (the link is missing), a gated token absent, an
           unreadable box, a missing mark, OBS restarted inside the window. The E2E treats UNKNOWN as
           FAIL (fail-closed). `retryable` = the closing window is not in the log yet.
Report-only: fec_repaired (a datagram the FEC rebuilt, nothing lost), decoder_reopens (a bpp step),
reports / report_errors, stamp_dup and fec_sent on the output, the lowest fps.

The block (`jxs_link_continuity` in the verdict JSON) is PASS only when every link passed; any FAIL
makes it FAIL, else any UNKNOWN makes it UNKNOWN.

No I/O in the decision (`decide_box`, `combine`) -- pytest tests/python/test_jxs_link_continuity_1387.py.
CLI:
  --box ROLE --slice FILE --name NAME --from N --len N --start N --stop N --start-time T --stop-time T
      [--input NAME]... [--program-output] --box-out FILE       one box's links (exit 0)
  --box ROLE --unknown REASON [--input NAME]... [--program-output] --box-out FILE
  --combine FILE... [--out BLOCK] [--merge-into VERDICT]      the block; exit 0 PASS, 1 FAIL, 2 UNKNOWN
  --from-block BLOCK [--merge-into VERDICT]                    print (and merge) a written block
The slice file is what the box printed: `#newest\\t<newest log name>\\t<its size>`, then one
`<byte offset from N>:<line>` per JPEG XS line (`grep -b` of the bytes [N, N + len)).
"""
import argparse
import json
import os
import re
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)
from obs_jxs_inputs import OUTPUT_NAME  # noqa: E402  the ONE name of the strih program output
from program_audio_continuity import file_order_times  # noqa: E402  the ONE midnight-aware clock

KEY = "jxs_link_continuity"
PERIOD_S = 5.0  # CB_JXS_STATUS_PERIOD_NS (jxs-source-decide.h); the sender's window is 5 s too
GAP_MAX_S = PERIOD_S + 1.0  # a receiver reopen costs its 1 s delay + a fresh window: > 6 s
INPUT_GATED = ("lost_slices", "concealed", "undecodable", "dropped_unread", "foreign")
INPUT_REPORT = ("fec_repaired", "decoder_reopens", "reports", "report_errors")
OUTPUT_GATED = ("dropped_busy", "not_taken", "refused", "send_errors", "encode_errors")
OUTPUT_REPORT = ("stamp_dup", "offered", "taken", "fec_sent", "fec_repaired")
# a link's own lines that mean it did not deliver for a while: one inside the window is a hole
INPUT_HOLES = ("not connected", "receiver failed", "listening on UDP", "cannot listen on UDP",
               "settings changed -- reopening")
OUTPUT_HOLES = ("started --", "stopped (issue 1387)", "cannot start the sender", "did not start")

_TS_RE = re.compile(r"^(\d\d):(\d\d):(\d\d)\.(\d\d\d): ")
_LINE_RE = re.compile(r"^\d\d:\d\d:\d\d\.\d\d\d: \[distroav\] jxs-(source|output) '(.*?)': (.*)$")
_FIELD_RE = re.compile(r"(?:^|\s)([a-z_/]+)=(\S+)")
_INT_RE = re.compile(r"\d+")


def _sod(text):
    m = _TS_RE.match(text or "")
    if not m:
        return None
    h, mi, s, ms = (int(x) for x in m.groups())
    return h * 3600 + mi * 60 + s + ms / 1000.0


def _int(v):
    return int(v) if isinstance(v, str) and _INT_RE.fullmatch(v) else None


def _float(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def parse_slice(data, from_off, length):
    """The box's slice output (bytes) -> (header {newest, size} or None, [(abs_pos, text)]).
    The line at offset 0 may start mid-line when FROM > 0 and is dropped; a line the length cap or
    the end of the file cut (no newline inside the bytes read) is dropped too."""
    header, entries = None, []
    for raw in data.split(b"\n"):
        if raw.startswith(b"#newest\t"):
            parts = raw.decode("utf-8", "replace").split("\t")
            if len(parts) == 3 and _INT_RE.fullmatch(parts[2]):
                header = {"newest": parts[1], "size": int(parts[2])}
            continue
        m = re.match(rb"^(\d+):", raw)
        if not m:
            continue
        rel, text = int(m.group(1)), raw[m.end():]
        if rel == 0 and from_off > 0:
            continue
        end = rel + len(text) + 1  # + its newline
        if end > length or (header and from_off + end > header["size"]):
            continue
        entries.append((from_off + rel, text.decode("utf-8", "replace")))
    return header, entries


def parse_line(text):
    """A JPEG XS log line -> {kind: source|output, name, window: bool, fields, rest}, else None."""
    m = _LINE_RE.match(text)
    if not m:
        return None
    kind, name, rest = m.group(1), m.group(2), m.group(3)
    window = rest.startswith("fps=")
    fields = dict(_FIELD_RE.findall(rest)) if window else {}
    return {"kind": kind, "name": name, "window": window, "fields": fields, "rest": rest}


def _stamp(text):
    m = _TS_RE.match(text or "")
    return text[: m.end() - 2] if m else None


def _place(sod, ref):
    """A mark's seconds-of-day on the file-order axis, the day nearest a reference time."""
    if sod is None or ref is None:
        return None
    return sod + 86400.0 * round((ref - sod) / 86400.0)


# Owner 2.11.2026: the strih records with NVENC, and tearing that encoder down at StopRecord costs the program
# one frame (measured 3 of 3, x264 0 of 3). It never happens during production, so it is accepted for now: at
# most this many frames in the output's last two windows together (the stop mark comes ~2 s after StopRecord, so
# the frame can land in the window before the closing one), reported as `stop_transient`, never anywhere else.
# One busy frame counts in BOTH keys (`cbjxs_send_frame` answers 0 for it, which is `not_taken`), so 1 + 1 is one
# frame; two frames read 2 + 2, or 1 + 2 with an off-grid one, and fail.
STOP_TRANSIENT_KEYS = ("dropped_busy", "not_taken")
STOP_TRANSIENT_MAX = 1


def _link_result(role, kind, name):
    return {"box": role, "link": name, "kind": "input" if kind == "source" else "output",
            "verdict": "UNKNOWN", "pass": False, "windows": 0, "frames": 0, "counters": {},
            "report": {}, "zero_frame_windows": 0, "min_fps": None, "max_gap_s": None,
            "interval": None, "events": [], "failures": [], "unknown_reasons": [], "retryable": False}


def grade_link(role, kind, name, entries, times, start_off, stop_off, t_start, t_stop, advanced):
    """One link over the window. `entries` [(abs_pos, text)] in file order, `times` their file-order
    seconds; `t_start` / `t_stop` the marks' times on that axis (None when unknown); `advanced` = the
    box's log reaches past the stop by more than two windows (a missing closing window is not coming).
    Pure."""
    res = _link_result(role, kind, name)
    unknown, failures = res["unknown_reasons"], res["failures"]
    gated, report = (INPUT_GATED, INPUT_REPORT) if kind == "source" else (OUTPUT_GATED, OUTPUT_REPORT)
    holes = INPUT_HOLES if kind == "source" else OUTPUT_HOLES
    wins, events = [], []
    for i, (pos, text) in enumerate(entries):
        p = parse_line(text)
        if not p or p["kind"] != kind or p["name"] != name:
            continue
        (wins if p["window"] else events).append((i, pos, p))
    opening = None
    counted, closing = [], None
    for w in wins:
        if w[1] < start_off:
            opening = w
            continue
        counted.append(w)
        if w[1] >= stop_off:
            closing = w
            break
    label = "JPEG XS input" if kind == "source" else "JPEG XS output"
    if not counted:
        unknown.append(f"no window line of the {label} '{name}' in the recording window (the link is "
                       "missing, or it is not on this box)")
        res["retryable"] = not advanced
        return _finish(res)
    first_t = times[counted[0][0]]
    last_t = times[counted[-1][0]]
    res["interval"] = {"from": _stamp(entries[(opening or counted[0])[0]][1]),
                       "to": _stamp(entries[counted[-1][0]][1])}
    if closing is None:
        if not advanced:
            unknown.append(f"no window line of '{name}' after the stop yet (not in the log yet?)")
            res["retryable"] = True
        elif t_stop is None or last_t is None:
            unknown.append(f"'{name}' has no window after the stop and the stop time is unknown")
        elif last_t < t_stop - PERIOD_S:
            failures.append(f"its windows end {t_stop - last_t:.1f} s before the stop: the link stopped "
                            "delivering inside the recording")
    if opening is None:
        if t_start is None or first_t is None:
            unknown.append(f"'{name}' has no window before the start and the start time is unknown")
        elif first_t - PERIOD_S > t_start + PERIOD_S:
            failures.append(f"its first window begins {first_t - PERIOD_S - t_start:.1f} s after the "
                            "start: the link was not delivering when the recording began")
    chain = ([opening] if opening else []) + counted
    gaps = []
    for a, b in zip(chain, chain[1:]):
        ta, tb = times[a[0]], times[b[0]]
        if ta is not None and tb is not None:
            gaps.append((tb - ta, b))
    if gaps:
        res["max_gap_s"] = round(max(g for g, _ in gaps), 3)
    for g, b in gaps:
        if g > GAP_MAX_S:
            failures.append(f"a {g:.1f} s gap between two windows (to {_stamp(entries[b[0]][1])}): "
                            "frames in it were never counted")
    res["windows"] = len(counted)
    keys = gated + (("reopens",) if kind == "source" else ())  # a reopen is a hole: gated too
    missing, sums, fps = set(), {k: 0 for k in ("frames",) + keys + report}, []
    for _, _, p in counted:
        f = p["fields"]
        for k in ("frames",) + keys + report:
            n = _int(f.get(k))
            if n is not None:
                sums[k] += n
            elif k not in report:
                missing.add(k)
        if _int(f.get("frames")) == 0:
            res["zero_frame_windows"] += 1
        if _float(f.get("fps")) is not None:
            fps.append(_float(f.get("fps")))
    for k in sorted(missing):
        unknown.append(f"a window line of '{name}' carries no `{k}=` token (another build?)")
    res["frames"] = sums["frames"]
    res["counters"] = {k: sums[k] for k in keys if k not in missing}
    res["report"] = {k: sums[k] for k in report}
    res["min_fps"] = min(fps) if fps else None
    if res["zero_frame_windows"]:
        failures.append(f"{res['zero_frame_windows']} window(s) with frames=0")
    excused = {}
    if kind != "source" and closing is not None:
        vals = {k: sum(_int(w[2]["fields"].get(k)) or 0 for w in counted[-2:]) for k in STOP_TRANSIENT_KEYS}
        if any(vals.values()) and max(vals.values()) <= STOP_TRANSIENT_MAX:
            excused = vals
            res["stop_transient"] = vals
    for k in gated:
        if res["counters"].get(k, 0) - excused.get(k, 0) > 0:
            failures.append(f"{k} +{res['counters'][k]}" + (" (a concealed slice is a LOST slice)"
                                                              if k == "concealed" else ""))
    if res["counters"].get("reopens"):
        failures.append(f"the receiver reopened {res['counters']['reopens']} time(s): frames before each "
                        "reopen were never counted")
    lo = opening[1] if opening else start_off - 1
    hi = counted[-1][1]
    for _, pos, p in events:
        if lo < pos <= hi and any(h in p["rest"] for h in holes):
            res["events"].append(f"jxs-{kind} '{name}': {p['rest']}")
    for ev in res["events"]:
        failures.append(f"a hole in the link: {ev}")
    return _finish(res)


def _finish(res):
    if res["failures"]:
        res["verdict"] = "FAIL"
        res["retryable"] = False
    elif res["unknown_reasons"]:
        res["verdict"] = "UNKNOWN"
    else:
        res["verdict"] = "PASS"
        res["retryable"] = False
    res["pass"] = res["verdict"] == "PASS"
    return res


def unknown_box(role, reason, inputs, program_output):
    links = []
    for kind, name in [("source", n) for n in inputs] + ([("output", OUTPUT_NAME)] if program_output else []):
        r = _link_result(role, kind, name)
        r["unknown_reasons"].append(reason)
        links.append(_finish(r))
    if not links:
        r = _link_result(role, "source", "?")
        r["unknown_reasons"].append(reason)
        links.append(_finish(r))
    return {"box": role, "links": links, "box_reasons": [reason]}


def decide_box(role, data, name, from_off, length, start_off, stop_off, start_time, stop_time,
               inputs, program_output):
    """One box: the slice the box printed (bytes) + the marks -> {box, links[], box_reasons[]}. Pure."""
    if not (0 <= from_off <= start_off <= stop_off) or length < stop_off - from_off:
        return unknown_box(role, f"the window marks do not fit the read (from {from_off}, start "
                                 f"{start_off}, stop {stop_off}, length {length})", inputs, program_output)
    header, entries = parse_slice(data, from_off, length)
    if header is None:
        return unknown_box(role, "the box answered without its log header (the read failed)", inputs,
                           program_output)
    if header["newest"] != name:
        return unknown_box(role, f"OBS started a new log ('{header['newest']}') after the window start "
                                 f"('{name}'): OBS restarted, the window cannot be proven", inputs, program_output)
    if header["size"] < stop_off:
        return unknown_box(role, f"the OBS log '{name}' is shorter ({header['size']} bytes) than the stop "
                                 f"mark ({stop_off}): the log was rewritten", inputs, program_output)
    times = file_order_times([t for _, t in entries])

    def ref(off):
        before = [times[i] for i, (p, _) in enumerate(entries) if p < off and times[i] is not None]
        if before:
            return before[-1]
        after = [times[i] for i, (p, _) in enumerate(entries) if p >= off and times[i] is not None]
        return after[0] if after else None

    t_start = _place(_sod(f"{start_time}: ") if start_time else None, ref(start_off))
    t_stop = _place(_sod(f"{stop_time}: ") if stop_time else None, ref(stop_off))
    known = [t for t in times if t is not None]
    advanced = bool(known) and t_stop is not None and max(known) > t_stop + 2 * PERIOD_S
    # the read stopped at its length cap: the log goes on past what was read, nothing more will come
    if from_off + length <= header["size"]:
        advanced = True
    links = [grade_link(role, "source", n, entries, times, start_off, stop_off, t_start, t_stop, advanced)
             for n in inputs]
    if program_output:
        links.append(grade_link(role, "output", OUTPUT_NAME, entries, times, start_off, stop_off,
                                t_start, t_stop, advanced))
    return {"box": role, "links": links, "box_reasons": []}


def combine(boxes):
    """Every box's links -> the block. Pure."""
    links = [link for b in boxes for link in b.get("links", [])]
    failures = [f"{l['box']} '{l['link']}': {f}" for l in links for f in l["failures"]]
    unknown = [f"{l['box']} '{l['link']}': {u}" for l in links for u in l["unknown_reasons"]]
    if not links:
        unknown.append("no JPEG XS link was graded")
    if failures:
        verdict = "FAIL"
    elif unknown:
        verdict = "UNKNOWN"
    else:
        verdict = "PASS"
    retryable = verdict == "UNKNOWN" and all(l["retryable"] for l in links if l["verdict"] == "UNKNOWN") \
        and bool(links)
    return {"verdict": verdict, "pass": verdict == "PASS", "gates_overall_pass": True, "links": links,
            "failures": failures, "unknown_reasons": unknown, "retryable": retryable,
            "period_s": PERIOD_S, "gap_max_s": GAP_MAX_S}


def summary_lines(block):
    """Plain English lines for the E2E log (the Discord report renders its own Slovak lines)."""
    out = [f"JPEG XS link continuity: {block.get('verdict')}"]
    for l in block.get("links") or []:
        c = l.get("counters") or {}
        r = l.get("report") or {}
        iv = l.get("interval") or {}
        out.append(f"  {l.get('box')} {l.get('kind')} '{l.get('link')}': {l.get('verdict')} -- "
                   f"{l.get('windows')} window(s) {iv.get('from')} .. {iv.get('to')}, {l.get('frames')} frames"
                   + ("".join(f", {k} {v}" for k, v in c.items()))
                   + ("".join(f", {k} {v} (report-only)" for k, v in r.items() if v)))
    for f in block.get("failures") or []:
        out.append(f"  FAIL: {f}")
    for u in block.get("unknown_reasons") or []:
        out.append(f"  UNKNOWN: {u}")
    if block.get("verdict") == "UNKNOWN":
        out.append("  UNKNOWN fails the run: the JPEG XS links could not be proven loss-free")
    return out


def merge_into(path, block):
    """Write the block into the verdict JSON as `jxs_link_continuity` (temp file + rename)."""
    with open(path, encoding="utf-8") as f:
        verdict = json.load(f)
    verdict[KEY] = block
    fd, tmp = tempfile.mkstemp(prefix=".jxs-", dir=os.path.dirname(os.path.abspath(path)))
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(verdict, f, indent=2)
    os.replace(tmp, path)


def _write(path, obj):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2)


def _read_block(path):
    try:
        with open(path, encoding="utf-8") as f:
            b = json.load(f)
        if not isinstance(b, dict) or "verdict" not in b:
            raise ValueError("not a continuity block")
        return b
    except (OSError, ValueError) as e:
        return combine([unknown_box("?", f"the block {path} is unreadable: {e}", [], False)])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--box", help="the box role (strih|stream)")
    ap.add_argument("--slice", help="the box's slice output")
    ap.add_argument("--name", help="the OBS log name at the window start")
    ap.add_argument("--from", dest="from_off", type=int)
    ap.add_argument("--len", dest="length", type=int)
    ap.add_argument("--start", type=int)
    ap.add_argument("--stop", type=int)
    ap.add_argument("--start-time", default="")
    ap.add_argument("--stop-time", default="")
    ap.add_argument("--input", action="append", default=[], help="a measured JPEG XS input")
    ap.add_argument("--program-output", action="store_true", help="grade the strih program output")
    ap.add_argument("--unknown", help="the box reads UNKNOWN with this reason (no read)")
    ap.add_argument("--box-out", help="write the box result here")
    ap.add_argument("--combine", nargs="+", help="box result files -> the block")
    ap.add_argument("--from-block", help="print (and merge) a block an earlier call wrote")
    ap.add_argument("--out", help="write the block here")
    ap.add_argument("--merge-into", help="the verdict JSON to merge the block into")
    a = ap.parse_args(argv)
    if a.box is not None:
        if a.unknown:
            res = unknown_box(a.box, a.unknown, a.input, a.program_output)
        else:
            if None in (a.slice, a.name, a.from_off, a.length, a.start, a.stop):
                ap.error("--slice, --name, --from, --len, --start and --stop are required (or --unknown)")
            try:
                with open(a.slice, "rb") as f:
                    data = f.read()
            except OSError as e:
                res = unknown_box(a.box, f"the slice read failed: {e}", a.input, a.program_output)
            else:
                res = decide_box(a.box, data, a.name, a.from_off, a.length, a.start, a.stop, a.start_time,
                                 a.stop_time, a.input, a.program_output)
        if a.box_out:
            _write(a.box_out, res)
        return 0
    if a.combine:
        boxes = []
        for p in a.combine:
            try:
                with open(p, encoding="utf-8") as f:
                    boxes.append(json.load(f))
            except (OSError, ValueError) as e:
                boxes.append(unknown_box("?", f"the box result {p} is unreadable: {e}", [], False))
        block = combine(boxes)
    elif a.from_block:
        block = _read_block(a.from_block)
    else:
        ap.error("--box, --combine or --from-block")
    if a.out:
        _write(a.out, block)
    if a.merge_into:
        try:
            merge_into(a.merge_into, block)
        except (OSError, ValueError) as e:
            print(f"WARNING: could not merge into {a.merge_into}: {e}", file=sys.stderr)
    print("\n".join(summary_lines(block)))
    return {"PASS": 0, "FAIL": 1}.get(block.get("verdict"), 2)


if __name__ == "__main__":
    sys.exit(main())
