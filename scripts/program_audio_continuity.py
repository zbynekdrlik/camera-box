#!/usr/bin/env python3
"""issue 1439 (ROZHODNUTE 6359382787, owner 23.10.2026) -- the PROGRAM-AUDIO CONTINUITY gate of the
full-path E2E: audio has the SAME zero-frame-loss bar as video. A concealed audio dropout is a LOSS,
never a pass, and the SNV E2E goes RED on any program-audio loss inside its recording window.

The evidence is the stream box's OWN OBS log, read between two line marks the E2E takes on the
newest log file: the line count just before StartRecord (the window START) and the line count right
after StopRecord (the window END). Positions in the log, never wall time, decide what is inside; the
log's `HH:MM:SS.mmm` stamps are read only to space the closing status line from the stop.

Read in the window (formats from the shipped C):
  * the vendored obs-vban receive source's status line, one per 10 s while packets arrive
    (vendor/obs-vban/src/vban-source.c `vban_src_log_status`):
      [obs-vban] obs-vban receive: received=N lost=N late=N re_anchors=N stale=N reordered=N
        concealed=N silence_ms=X ... emit_missed=N ... stream='S' source='<input>'
    Counters are cumulative since the source started, so the window's loss is a DELTA: the last
    status line of the input at/before the window start against the first one at least one status
    period (STATUS_PERIOD_S, 10 s) after the stop (every slot up to the stop is decided by then:
    its deadline is the stop + about R). An older build lacks `stale=` / `reordered=` /
    `concealed=` / `emit_missed=`: what is present is graded, what is absent is named.
  * the per-run loss lines the playout worker writes when a run of lost slots ends
    (`source '<input>': missing N packet(s) before frame F -- concealed: ... | -- silence ...`),
    the `obs-vban receive late:` and `obs-vban receive re-anchor:` lines -- named as evidence; the
    counters decide.
  * libobs's `buffering-guard: '<input>' ...` line (obs-audio.c, issues 1381 + 1425: the input was
    re-anchored against the mix window, its audio dropped) -- any such line for the program-audio
    input in the window FAILS.
  * libobs's `asrc: source '<input>' ... steps=N ... place_jumps=N ...` line (obs-source.c, every
    60 s, cumulative): a placement jump or a STEP re-base moved the audio -- a delta FAILS.
  * `audio-telemetry #800 mix: frozen= mixed_late=` -- mix-wide (every member, not only the program
    input): its delta is REPORTED, never gated here (a named buffering-guard line is the gate).

Verdict:
  PASS     every gated delta is 0 and no buffering-guard line names the input;
  FAIL     a delta > 0 (lost -- `concealed` is a SUBSET of lost and named as lost, late,
           re_anchors, stale, emit_missed, ASRC place_jumps / steps) or a buffering-guard line;
  UNKNOWN  no status line on either side, a counter that went backwards (OBS restarted or the
           source was reconfigured inside the window), a status-line format change inside the
           window, a missing ASRC line on either side, an unreadable log. The E2E treats UNKNOWN as
           FAIL (fail-closed, like the video gates). `retryable` marks an UNKNOWN that only means
           "the closing line is not in the log yet" (the gate waits and reads again).

On a Windows stream box the program audio (`mbc`) is a Dante device, there is no obs-vban source:
`--no-vban` grades only the buffering-guard + ASRC checks and says which ran (never a fake PASS).

No I/O in the decision (`decide`) -- pytest Tier-0 (tests/python/test_program_audio_continuity_1439.py).
CLI: `program_audio_continuity.py --log FILE|- --input NAME --start-line S --end-line E
[--first-line F] [--no-vban] [--merge-into VERDICT_JSON] [--out JSON]`; exit 0 PASS, 1 FAIL,
2 UNKNOWN. `--unknown REASON` writes an UNKNOWN block without a log (a mark or the read failed).
"""
import argparse
import json
import os
import re
import sys
import tempfile

STATUS_PERIOD_S = 10.0  # VBAN_RECV_LOG_INTERVAL_NS: one obs-vban receive status line per 10 s
STATUS_MARKER = "obs-vban receive: received="
# gated obs-vban counters, in report order; `lost` / `late` / `re_anchors` are on every build
VBAN_GATED = ("lost", "late", "re_anchors", "stale", "emit_missed")
VBAN_ALWAYS = ("lost", "late", "re_anchors")
VBAN_NAMED = ("concealed", "reordered", "received")
ASRC_GATED = ("place_jumps", "steps")

_TS_RE = re.compile(r"^(\d\d):(\d\d):(\d\d)\.(\d\d\d): ")
_FIELD_RE = re.compile(r"(?:^|\s)([a-z_]+)=([^\s']+)")
_TAIL_RE = re.compile(r" stream='(.*)' source='(.*)'$")
_MIX_RE = re.compile(r"audio-telemetry #800 mix: frozen=(\d+) mixed_late=(\d+)")


def _sod(line):
    """Seconds of day of an OBS log line (`HH:MM:SS.mmm: `), None when the line carries no stamp."""
    m = _TS_RE.match(line)
    if not m:
        return None
    h, mi, s, ms = (int(x) for x in m.groups())
    return h * 3600 + mi * 60 + s + ms / 1000.0


def file_order_times(lines):
    """Absolute seconds per line in FILE ORDER (a step back of more than 12 h is a midnight: one day
    on; a smaller step back is threads logging out of order and keeps the time). A line without a
    stamp takes the previous line's time (None before the first stamp)."""
    out, day, prev, cur = [], 0, None, None
    for line in lines:
        sod = _sod(line)
        if sod is not None:
            if prev is not None and sod < prev - 43200:
                day += 1
            prev = sod
            cur = day * 86400 + sod
        out.append(cur)
    return out


def _int(v):
    return int(v) if v is not None and re.fullmatch(r"\d+", v) else None


def _float(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def parse_status(line):
    """The obs-vban receive status line -> {source, stream, fields{}}, else None."""
    i = line.find(STATUS_MARKER)
    if i < 0:
        return None
    body = line[i + len("obs-vban receive: "):].rstrip("\r\n")
    tail = _TAIL_RE.search(body)
    if not tail:
        return None
    fields = dict(_FIELD_RE.findall(body[: tail.start()]))
    return {"source": tail.group(2), "stream": tail.group(1), "fields": fields}


def parse_asrc(line, name):
    """`asrc: source '<name>' ...` -> {place_jumps, steps} (ints, absent keys missing), else None."""
    key = f"asrc: source '{name}' "
    i = line.find(key)
    if i < 0:
        return None
    fields = dict(_FIELD_RE.findall(line[i + len(key):]))
    return {k: _int(fields.get(k)) for k in ASRC_GATED if _int(fields.get(k)) is not None}


def parse_mix(line):
    m = _MIX_RE.search(line)
    return (int(m.group(1)), int(m.group(2))) if m else None


def _strip_prefix(line):
    """The line without its `HH:MM:SS.mmm: ` stamp (for the evidence lists)."""
    m = _TS_RE.match(line)
    text = line[m.end():] if m else line
    return text.rstrip("\r\n")


def _stamp(line):
    m = _TS_RE.match(line or "")
    return line[: m.end() - 2] if m else None


def unknown_block(reason, name="", vban=True, retryable=False):
    return {
        "verdict": "UNKNOWN",
        "pass": False,
        "gates_overall_pass": True,
        "input": name,
        "vban": vban,
        "checks": {},
        "counters": {},
        "asrc": {},
        "failures": [],
        "unknown_reasons": [reason],
        "retryable": retryable,
        "loss_runs": [],
        "guard_lines": [],
        "late_lines": [],
        "reanchor_lines": [],
    }


def _delta_table(before, after, keys, unknown, label):
    """{key: {before, after, delta}} for every key present on BOTH sides; a key present on one side
    only (a format change inside the window) or a value that went down is UNKNOWN."""
    table = {}
    for k in keys:
        b, a = _int(before.get(k)), _int(after.get(k))
        if b is None and a is None:
            continue
        if b is None or a is None:
            unknown.append(f"{label} `{k}=` is on one side of the window only: the log format changed "
                           "inside the window (OBS restarted with another build)")
            continue
        if a < b:
            unknown.append(f"{label} `{k}` went backwards ({b} -> {a}): OBS restarted or the source "
                           "was reconfigured inside the window")
            continue
        table[k] = {"before": b, "after": a, "delta": a - b}
    return table


def decide(lines, name, start_idx, end_idx, vban=True):
    """The window verdict. `lines`: the log text from some line on, one entry per line; `start_idx`
    / `end_idx`: the index (into `lines`) of the first line written after the window START / END
    mark. Pure."""
    res = unknown_block("", name, vban)
    res["unknown_reasons"] = []
    unknown, failures = res["unknown_reasons"], res["failures"]
    if not (0 <= start_idx <= end_idx <= len(lines)):
        res["unknown_reasons"].append(
            f"the window marks do not fit the log read (start {start_idx}, end {end_idx}, {len(lines)} lines)")
        return res
    times = file_order_times(lines)
    stop_t = next((times[i] for i in range(end_idx - 1, -1, -1) if times[i] is not None), None)

    lo, hi = start_idx, None  # the event interval: after the opening line, through the closing one
    checks = res["checks"]
    if vban:
        before = after = None
        status = [(i, parse_status(l)) for i, l in enumerate(lines) if STATUS_MARKER in l]
        status = [(i, s) for i, s in status if s and s["source"] == name]
        for i, s in status:
            if i < start_idx:
                before = (i, s)
            elif i >= end_idx and after is None:
                if stop_t is None or times[i] is None:
                    # no stamp to space it from the stop: the SECOND status line after the stop is
                    # one whole status period after the first, so at least one after the stop
                    if sum(1 for j, _ in status if end_idx <= j <= i) >= 2:
                        after = (i, s)
                elif times[i] - stop_t >= STATUS_PERIOD_S:
                    after = (i, s)
        checks["obs_vban"] = "ran"
        if before is None:
            unknown.append(f"no obs-vban receive status line of '{name}' at or before the window start "
                           "(no VBAN packet reached OBS before the recording, or the log read starts too late)")
        if after is None:
            unknown.append(f"no obs-vban receive status line of '{name}' one status period after the stop "
                           "(the console stopped sending, or the line is not in the log yet)")
            res["retryable"] = True
        if before and after:
            lo, hi = before[0] + 1, after[0] + 1
            res["interval"] = {"from": _stamp(lines[before[0]]), "to": _stamp(lines[after[0]])}
            bf, af = before[1]["fields"], after[1]["fields"]
            for k in VBAN_ALWAYS:
                if _int(bf.get(k)) is None and _int(af.get(k)) is None:
                    unknown.append(f"the status line carries no `{k}=` token")
            counters = _delta_table(bf, af, VBAN_GATED + VBAN_NAMED, unknown, "obs-vban")
            res["counters"] = counters
            res["not_graded"] = [k for k in VBAN_GATED + ("concealed",) if k not in counters
                                 and _int(bf.get(k)) is None and _int(af.get(k)) is None]
            for k in VBAN_GATED:
                if k in counters and counters[k]["delta"] > 0:
                    failures.append(f"{k} +{counters[k]['delta']}")
            if "lost" in counters:
                lost = counters["lost"]["delta"]
                conc = counters["concealed"]["delta"] if "concealed" in counters else None
                res["lost_slots"] = lost
                res["lost_concealed"] = conc
                res["lost_silence"] = lost - conc if conc is not None else None
            sb, sa = _float(bf.get("silence_ms")), _float(af.get("silence_ms"))
            if sb is not None and sa is not None:
                res["silence_ms_delta"] = round(sa - sb, 1)
            if af.get("receive_buffer_ms") != bf.get("receive_buffer_ms"):
                res["receive_buffer_changed"] = [bf.get("receive_buffer_ms"), af.get("receive_buffer_ms")]
    else:
        checks["obs_vban"] = ("n/a: no obs-vban source on this stream box (its program audio is a Dante "
                              "device), the VBAN loss counters do not exist there")

    # ASRC: the last line at/before the start against the first at/after the stop (60 s cadence)
    a_before = a_after = None
    for i, l in enumerate(lines):
        a = parse_asrc(l, name)
        if a is None:
            continue
        if i < start_idx:
            a_before = (i, a)
        elif i >= end_idx and a_after is None:
            a_after = (i, a)
    checks["asrc"] = "ran"
    if a_before is None:
        unknown.append(f"no `asrc: source '{name}'` line at or before the window start")
    if a_after is None:
        unknown.append(f"no `asrc: source '{name}'` line after the stop (not in the log yet?)")
        res["retryable"] = True
    if a_before and a_after:
        res["asrc"] = _delta_table(
            {k: str(v) for k, v in a_before[1].items()}, {k: str(v) for k, v in a_after[1].items()},
            ASRC_GATED, unknown, "asrc")
        for k in ASRC_GATED:
            if k in res["asrc"] and res["asrc"][k]["delta"] > 0:
                failures.append(f"asrc {k} +{res['asrc'][k]['delta']}")
            elif k not in res["asrc"] and not any(f"`{k}" in u for u in unknown):
                unknown.append(f"the `asrc: source '{name}'` line carries no `{k}=` token")
    if hi is None:
        hi = a_after[0] + 1 if a_after else end_idx

    # the named evidence + the buffering-guard gate, over the event interval
    checks["buffering_guard"] = "ran"
    guard_key = f"buffering-guard: '{name}' "
    loss_key = f"source '{name}': "
    for l in lines[lo:hi]:
        if guard_key in l:
            res["guard_lines"].append(_strip_prefix(l))
        elif loss_key in l and ("missing" in l or "more run(s)" in l):
            res["loss_runs"].append(_strip_prefix(l))
        elif f"obs-vban receive late: source '{name}' " in l:
            res["late_lines"].append(_strip_prefix(l))
        elif f"obs-vban receive re-anchor: source '{name}' " in l:
            res["reanchor_lines"].append(_strip_prefix(l))
    if res["guard_lines"]:
        failures.append(f"buffering-guard x{len(res['guard_lines'])}")

    # report-only: the mix-wide MIXED_LATE count over the same marks
    m_before = m_after = None
    for i, l in enumerate(lines):
        m = parse_mix(l)
        if m is None:
            continue
        if i < start_idx:
            m_before = m
        elif i >= end_idx and m_after is None:
            m_after = m
    if m_before and m_after and m_after[1] >= m_before[1]:
        res["mix_mixed_late_delta"] = m_after[1] - m_before[1]

    if unknown:
        res["verdict"] = "UNKNOWN"
    elif failures:
        res["verdict"] = "FAIL"
        res["retryable"] = False
    else:
        res["verdict"] = "PASS"
        res["retryable"] = False
    res["pass"] = res["verdict"] == "PASS"
    if res["verdict"] != "UNKNOWN":
        res["retryable"] = False
    return res


def summary_lines(res):
    """Plain English lines for the E2E log (the Discord report renders its own Slovak line)."""
    name = res.get("input") or "?"
    head = f"program-audio continuity '{name}': {res.get('verdict')}"
    iv = res.get("interval")
    if iv:
        head += f" (status lines {iv.get('from')} .. {iv.get('to')})"
    out = [head]
    c = res.get("counters") or {}
    if c:
        out.append("  obs-vban deltas: " + " ".join(
            f"{k}={v['delta']}" for k, v in c.items() if k != "received")
            + (f" (received +{c['received']['delta']})" if "received" in c else ""))
    if res.get("lost_slots"):
        conc = res.get("lost_concealed")
        out.append(f"  {res['lost_slots']} slot(s) LOST in the window"
                   + (f": {conc} concealed (still LOST), {res.get('lost_silence')} as silence" if conc is not None else ""))
    a = res.get("asrc") or {}
    if a:
        out.append("  asrc deltas: " + " ".join(f"{k}={v['delta']}" for k, v in a.items()))
    for k, title in (("loss_runs", "loss run"), ("late_lines", "late"), ("reanchor_lines", "re-anchor"),
                     ("guard_lines", "buffering-guard")):
        for l in (res.get(k) or [])[:10]:
            out.append(f"  {title}: {l}")
        if len(res.get(k) or []) > 10:
            out.append(f"  {title}: ... {len(res[k]) - 10} more")
    if res.get("mix_mixed_late_delta"):
        out.append(f"  note: the mix-wide MIXED_LATE count rose by {res['mix_mixed_late_delta']} (any mix member, report-only)")
    for k, v in (res.get("checks") or {}).items():
        if v != "ran":
            out.append(f"  {k}: {v}")
    for f in res.get("failures") or []:
        out.append(f"  FAIL: {f}")
    for u in res.get("unknown_reasons") or []:
        out.append(f"  UNKNOWN: {u}")
    if res.get("verdict") == "UNKNOWN":
        out.append("  UNKNOWN fails the run: the program audio could not be proven continuous")
    return out


def merge_into(path, res):
    """Write the block into the verdict JSON as `program_audio_continuity` (temp file + rename)."""
    with open(path, encoding="utf-8") as f:
        verdict = json.load(f)
    verdict["program_audio_continuity"] = res
    d = os.path.dirname(os.path.abspath(path))
    fd, tmp = tempfile.mkstemp(prefix=".pac-", dir=d)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(verdict, f, indent=2)
    os.replace(tmp, path)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--log", help="the log text (a file, or - for stdin)")
    ap.add_argument("--input", default="", help="the program-audio OBS input name")
    ap.add_argument("--first-line", type=int, default=1, help="the log line number of the text's first line")
    ap.add_argument("--start-line", type=int, help="the log's line count at the window start")
    ap.add_argument("--end-line", type=int, help="the log's line count at the window end")
    ap.add_argument("--no-vban", action="store_true", help="the program audio is no obs-vban source")
    ap.add_argument("--unknown", help="write an UNKNOWN block with this reason (no log read)")
    ap.add_argument("--merge-into", help="the verdict JSON to merge the block into")
    ap.add_argument("--out", help="write the block JSON here")
    args = ap.parse_args(argv)
    vban = not args.no_vban
    if args.unknown:
        res = unknown_block(args.unknown, args.input, vban)
    elif not args.log or args.start_line is None or args.end_line is None:
        ap.error("--log, --start-line and --end-line are required (or --unknown)")
    else:
        try:
            if args.log == "-":
                text = sys.stdin.read()
            else:
                with open(args.log, encoding="utf-8", errors="replace") as f:
                    text = f.read()
        except OSError as e:
            res = unknown_block(f"the log read failed: {e}", args.input, vban)
        else:
            lines = text.splitlines()
            res = decide(lines, args.input, args.start_line - args.first_line + 1,
                         args.end_line - args.first_line + 1, vban)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            json.dump(res, f, indent=2)
    if args.merge_into:
        try:
            merge_into(args.merge_into, res)
        except (OSError, ValueError) as e:
            print(f"WARNING: could not merge into {args.merge_into}: {e}", file=sys.stderr)
    print("\n".join(summary_lines(res)))
    return {"PASS": 0, "FAIL": 1}.get(res["verdict"], 2)


if __name__ == "__main__":
    sys.exit(main())
