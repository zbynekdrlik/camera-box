#!/usr/bin/env python3
"""#1354 -- per-input genlock-fifo audit BEFORE/AFTER snapshot for the full-path E2E report.

WHAT it measures: for every strih genlock receiver input, the DELTA over the recording window of
the `holds`, `relocks` and `converge_sheds` counters (plus `dropped_due` for context) that the
vendored OBS `genlock-fifo audit '<name>':` log line emits (obs-source.c). A source that suffers
receive-side arrival jitter accumulates holds/single-mature phase steps and rides the depth-15
relock ladder -- exactly the 100-250 ms per-camera delivery ladder issue 1354 fixes with the N>=2
arrival-jitter budget. Surfacing the per-input delta lets the E2E report NAME the victim input
instead of printing a bare cross-camera spread number (scope 3).

WHERE the data comes from: the run already reads the strih OBS-log audit tail during the recording
window (the `[4c/8]` received= tap and scripts/lib/mv-reverify-escalate.sh / genlock-settle.sh). The
smallest persisted pair is a BEFORE snapshot (just before StartRecord) and an AFTER snapshot (just
after StopRecord) of the audit tail; this script parses the LAST audit line per input in each and
writes the per-input deltas as JSON. The composer (e2e_discord_report.py `_section_genlock_conveyor`,
reading this JSON via `--genlock-audit-json`) is a PURE formatter -- REPORT-ONLY, it never changes a
verdict.

DESIGN (mirrors mv_skew_snapshot.py's split): this file is BOTH the pure decision logic
(`parse_audit_counters` / `compute_window_deltas` -- unit-tested with NO rig in
tests/python/test_genlock_audit_snapshot.py) AND the thin I/O CLI (read two log-tail files, write
the deltas JSON). No heavy deps.

Usage (a SUPERVISOR / recording-e2e.sh step captures the two log tails around the recording window):
  python3 scripts/genlock_audit_snapshot.py \
      --before-log /tmp/recording-e2e-<run>/genlock-audit-before.txt \
      --after-log  /tmp/recording-e2e-<run>/genlock-audit-after.txt \
      --out        /tmp/recording-e2e-<run>/genlock-audit-<run>.json
"""
from __future__ import annotations

import argparse
import json
import sys

# The counters carried by the `genlock-fifo audit '<name>':` line that this snapshot tracks as
# window deltas. `holds`/`relocks`/`converge_sheds` are the ladder signals surfaced in the report;
# `dropped_due` is kept for context (it structurally advances on a 60->30 strih input -- issue 1221
# -- so it is context, never a ladder signal by itself).
_DELTA_FIELDS = ("holds", "relocks", "converge_sheds", "dropped_due")

_AUDIT_MARK = "genlock-fifo audit '"


def parse_audit_counters(log_text: str) -> dict[str, dict[str, int]]:
    """Pure: return {input_name: {counter: int}} for the LAST `genlock-fifo audit '<name>':` line
    of each input in `log_text`. Counters are parsed by a whitespace key=value scan (the SAME shape
    genlock-settle.sh's awk uses); a recognised counter missing from an otherwise-matching line
    defaults to 0 (the vendored line always carries all of them). A line with no parseable name is
    skipped. Later lines overwrite earlier ones, so the returned dict is each input's newest state.
    """
    out: dict[str, dict[str, int]] = {}
    if not log_text:
        return out
    for line in log_text.splitlines():
        pos = line.find(_AUDIT_MARK)
        if pos < 0:
            continue
        rest = line[pos + len(_AUDIT_MARK):]
        end = rest.find("'")
        if end < 0:
            continue
        name = rest[:end]
        counters: dict[str, int] = {}
        for tok in line.split():
            eq = tok.find("=")
            if eq <= 0:
                continue
            key = tok[:eq]
            val = tok[eq + 1:]
            try:
                counters[key] = int(val)
            except ValueError:
                # decoration tokens (@fps, (=N ms), names) carry a non-integer value -- skip.
                continue
        if counters:
            out[name] = counters
    return out


def compute_window_deltas(
    before: dict[str, dict[str, int]], after: dict[str, dict[str, int]]
) -> dict:
    """Pure: per-input after-minus-before deltas of the tracked counters, plus the named VICTIM.

    Only inputs present in BOTH snapshots get a delta (an input that appeared mid-window has no
    honest baseline -> it is listed under `partial` instead of a fabricated delta). The victim is
    the input with the largest positive `holds` delta (tie-break: `relocks`); None when every input
    was quiet. Deltas are clamped at 0 on the lower side -- a counter that went backward means OBS
    restarted between the snapshots, which is reported as `restarted: true` for that input, never a
    negative delta.
    """
    inputs: dict[str, dict[str, int]] = {}
    partial: list[str] = []
    restarted: list[str] = []
    for name, a in sorted(after.items()):
        b = before.get(name)
        if b is None:
            partial.append(name)
            continue
        row: dict[str, int] = {}
        went_back = False
        for f in _DELTA_FIELDS:
            d = a.get(f, 0) - b.get(f, 0)
            if d < 0:
                went_back = True
                d = 0
            row[f] = d
        inputs[name] = row
        if went_back:
            restarted.append(name)

    victim = None
    best = (0, 0)
    for name, row in inputs.items():
        key = (row.get("holds", 0), row.get("relocks", 0))
        if key[0] > 0 and key > best:
            best = key
            victim = name

    result: dict = {"inputs": inputs, "victim": victim}
    if partial:
        result["partial"] = partial
    if restarted:
        result["restarted"] = restarted
    return result


def _read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--before-log", required=True, help="OBS-log tail captured BEFORE StartRecord")
    ap.add_argument("--after-log", required=True, help="OBS-log tail captured AFTER StopRecord")
    ap.add_argument("--out", required=True, help="path to write the per-input deltas JSON")
    args = ap.parse_args(argv)

    before = parse_audit_counters(_read(args.before_log))
    after = parse_audit_counters(_read(args.after_log))
    deltas = compute_window_deltas(before, after)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(deltas, f, ensure_ascii=False, indent=2)
    n = len(deltas.get("inputs", {}))
    print(f"#1354 genlock_audit_snapshot: wrote {n} per-input delta(s) to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
