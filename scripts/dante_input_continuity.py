#!/usr/bin/env python3
"""issue 1485: the Dante program input's continuity over the E2E recording window (strih-snv).

The strih program audio comes into OBS through the `dante_input` source (the obs release's
dante-input.so, a dstack dlink device named `strih-obs`). The device logs one status line every 10 s
(`plugins/dante-input/ffi/src/status.rs` in zbynekdrlik/obs, the obs#1 log contract):

  dante-input status: clock=media holdover=0 gm=<hex>|none polls=N re_anchors=N rx_packets=N rx_loss=N
    rx_gaps=N rx_late=N socket_drops=N blocks=N underruns=N overruns=N delivered_frames=N rx_holes=N
    rx_written_late=N late_blocks=N rebootstraps=N max_process_us=N   (the last five since obs v0.3.0)
    obs_dropped=N max_obs_us=N                                         (since obs v0.4.0: obs_dropped is loss)

Counters are totals since the device started. Audio has the same zero-loss bar as video (ROZHODNUTE
6359382787): over the window from the last status line at or before the START mark to the SECOND status
line after the STOP mark (every block up to the stop is counted by then: the first one can land right
after the stop), any increase of a counter that means audio other than FOH sent FAILs: rx_loss,
rx_holes, underruns (skipped blocks), rx_gaps, socket_drops, rebootstraps, rx_written_late, re_anchors
(the obs project's field meanings, 3.11.2026). rx_late, overruns and late_blocks are timing only and are
reported, never graded: a late packet that cost samples shows in rx_written_late and rx_holes. A line
from a release before v0.3.0 has no rx_holes and reads UNKNOWN. A status line FAILs too, and so does a status line in the window that reads clock=free or holdover=1 (the
device left the media clock), and a `dante-input error:` line in the window. UNKNOWN fails too: no
opening line, a counter that went backwards or a `dante-input start:` inside the window (the device
restarted), delivered_frames that did not move (no audio reached OBS). Pure; the CLI reads a log slice.

The verdict folds into the E2E's `program_audio_continuity` block (scripts/lib/program-audio-continuity.sh),
so the report and the gate need no second key: `--fold-into BLOCK` writes the Dante result under
`strih_dante` and makes the block's verdict the worse of the two.

issue 1495: the stream box (stream-snv) takes its program from its own Dante device `stream-obs` the
same way; `--box stream --device stream-obs` grades the STREAM OBS log slice and folds the result under
`stream_dante` with `stream Dante:` reasons. The defaults (`strih`, `strih-obs`) are the issue-1485 run.
"""
import argparse
import json
import re
import sys

STATUS_MARKER = "dante-input status:"
GATED = ("rx_loss", "rx_holes", "underruns", "rx_gaps", "socket_drops", "rebootstraps", "rx_written_late",
         "re_anchors", "obs_dropped")
NAMED = ("rx_late", "overruns", "late_blocks", "rx_packets", "blocks", "delivered_frames")
_FIELD_RE = re.compile(r"(?:^|\s)([a-z_]+)=(\S+)")
_TS_RE = re.compile(r"^\d\d:\d\d:\d\d\.\d\d\d: ")


def parse_status(line):
    """A `dante-input status:` line -> its fields {name: text}, else None."""
    i = line.find(STATUS_MARKER)
    if i < 0:
        return None
    return dict(_FIELD_RE.findall(line[i + len(STATUS_MARKER):].rstrip("\r\n")))


def _int(v):
    return int(v) if isinstance(v, str) and re.fullmatch(r"\d+", v) else None


def _text(line):
    m = _TS_RE.match(line)
    return (line[m.end():] if m else line).rstrip("\r\n")


def unknown_block(reason, retryable=False, device="strih-obs"):
    return {"verdict": "UNKNOWN", "pass": False, "device": device, "counters": {}, "failures": [],
            "unknown_reasons": [reason], "retryable": retryable, "error_lines": []}


def decide(lines, start_idx, end_idx, device="strih-obs"):
    """`lines`: the log text from some line on; `start_idx` / `end_idx`: the index of the first line
    written after the window START / STOP mark (the marks are line counts)."""
    status = [(i, parse_status(l)) for i, l in enumerate(lines) if STATUS_MARKER in l]
    status = [(i, f) for i, f in status if f is not None]
    opening = [(i, f) for i, f in status if i < start_idx]
    after = [(i, f) for i, f in status if i >= end_idx]
    if not opening:
        return unknown_block("no `dante-input status:` line at or before the window start: the device "
                             "was not running (no ~/.camera-box/dante-input.toml, or its start failed)",
                             device=device)
    if len(after) < 2:
        return unknown_block("the closing `dante-input status:` line (the second one after the stop) is "
                             "not in the log yet", retryable=True, device=device)
    o_i, before = opening[-1]
    c_i, closing = after[1]
    res = {"verdict": "PASS", "pass": True, "device": device, "counters": {}, "failures": [],
           "unknown_reasons": [], "retryable": False, "error_lines": [],
           "interval": {"from": _text(lines[o_i])[:12], "to": _text(lines[c_i])[:12]}}
    for i in range(o_i + 1, c_i + 1):
        t = _text(lines[i])
        if "dante-input start:" in t:
            res["unknown_reasons"].append(f"the device restarted inside the window: {t}")
        if "dante-input error:" in t:
            res["error_lines"].append(t)
    for i, f in status:
        if o_i <= i <= c_i:
            if f.get("clock") != "media":
                res["failures"].append(f"clock={f.get('clock')} (not on the dsync media clock): {_text(lines[i])}")
                break
            if f.get("holdover") != "0":
                res["failures"].append(f"holdover={f.get('holdover')} (the clock poller lost the grandmaster): {_text(lines[i])}")
                break
    for k in GATED + NAMED:
        b, a = _int(before.get(k)), _int(closing.get(k))
        if b is None or a is None:
            res["unknown_reasons"].append(f"`{k}=` is not readable on both sides of the window")
            continue
        if a < b:
            res["unknown_reasons"].append(f"`{k}` went backwards ({b} -> {a}): the device restarted")
            continue
        res["counters"][k] = {"before": b, "after": a, "delta": a - b}
        if k in GATED and a > b:
            res["failures"].append(f"{k} +{a - b}")
    if res["counters"].get("delivered_frames", {}).get("delta", 1) == 0:
        res["unknown_reasons"].append("delivered_frames did not move: no Dante audio reached OBS in the window")
    if res["error_lines"]:
        res["failures"].append(f"{len(res['error_lines'])} `dante-input error:` line(s) in the window")
    if res["failures"]:
        res["verdict"], res["pass"] = "FAIL", False
    elif res["unknown_reasons"]:
        res["verdict"], res["pass"] = "UNKNOWN", False
    return res


_RANK = {"PASS": 0, "UNKNOWN": 1, "FAIL": 2}


def fold(block, dante, box="strih"):
    """The E2E block with the Dante result folded in: `<box>_dante` = the result, the verdict the worse
    of the two, the Dante failures / unknowns named with a `<box> Dante:` prefix (the report lists them)."""
    out = dict(block or {})
    out[f"{box}_dante"] = dante
    if dante.get("verdict") != "PASS":
        out["failures"] = list(out.get("failures") or []) + [f"{box} Dante: {f}" for f in dante.get("failures") or []]
        out["unknown_reasons"] = list(out.get("unknown_reasons") or []) + [
            f"{box} Dante: {u}" for u in dante.get("unknown_reasons") or []]
        v = max((out.get("verdict") or "UNKNOWN", dante.get("verdict") or "UNKNOWN"), key=lambda x: _RANK.get(x, 1))
        out["verdict"], out["pass"] = v, v == "PASS"
    out.setdefault("gates_overall_pass", True)
    return out


def summary_lines(res, box="strih"):
    head = f"{box} Dante program input '{res.get('device') or 'strih-obs'}': {res.get('verdict')}"
    iv = res.get("interval")
    if iv:
        head += f" (status lines {iv.get('from')} .. {iv.get('to')})"
    out = [head]
    c = res.get("counters") or {}
    if c:
        out.append("  deltas: " + " ".join(f"{k}={v['delta']}" for k, v in c.items()))
    for l in (res.get("error_lines") or [])[:10]:
        out.append(f"  error: {l}")
    out += [f"  FAIL: {f}" for f in res.get("failures") or []]
    out += [f"  UNKNOWN: {u}" for u in res.get("unknown_reasons") or []]
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--log", help="the strih OBS log slice (a file)")
    ap.add_argument("--first-line", type=int, default=1, help="the log line number of the slice's first line")
    ap.add_argument("--start-line", type=int, help="the log's line count at the window start")
    ap.add_argument("--end-line", type=int, help="the log's line count at the window end")
    ap.add_argument("--unknown", help="an UNKNOWN result with this reason (no log read)")
    ap.add_argument("--out", help="write the result JSON here")
    ap.add_argument("--fold-into", help="fold the result into this E2E program-audio block JSON (rewritten)")
    ap.add_argument("--box", choices=("strih", "stream"), default="strih",
                    help="whose OBS log this is (issue 1495: the stream box's own Dante device)")
    ap.add_argument("--device", default="strih-obs", help="the Dante device's name (the result's label)")
    args = ap.parse_args(argv)
    if args.unknown:
        res = unknown_block(args.unknown, device=args.device)
    elif not args.log or args.start_line is None or args.end_line is None:
        ap.error("--log, --start-line and --end-line are required (or --unknown)")
    else:
        try:
            with open(args.log, encoding="utf-8", errors="replace") as f:
                lines = f.read().split("\n")   # newlines only, like `wc -l` / `tail -n +N` on the box
        except OSError as e:
            res = unknown_block(f"the {args.box} log read failed: {e}", device=args.device)
        else:
            if lines and lines[-1] == "":
                lines.pop()
            res = decide(lines, args.start_line - args.first_line + 1, args.end_line - args.first_line + 1,
                         device=args.device)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            json.dump(res, f, indent=2)
    if args.fold_into:
        try:
            with open(args.fold_into, encoding="utf-8") as f:
                block = json.load(f)
        except (OSError, ValueError):
            block = {"verdict": "UNKNOWN", "pass": False, "gates_overall_pass": True, "failures": [],
                     "unknown_reasons": ["the stream program-audio block was not readable"]}
        with open(args.fold_into, "w", encoding="utf-8") as f:
            json.dump(fold(block, res, box=args.box), f, indent=2)
    print("\n".join(summary_lines(res, box=args.box)))
    return {"PASS": 0, "FAIL": 1}.get(res["verdict"], 2)


if __name__ == "__main__":
    sys.exit(main())
