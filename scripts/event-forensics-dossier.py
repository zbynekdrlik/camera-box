#!/usr/bin/env python3
"""#707 EVENT-FORENSICS collector — pull the strih genlock-FIFO-audit lines + per-camera sender
journal lines matching each residual copy/gap event's wall-clock second into a per-event dossier.

The user's binding #707 decision (2026-07-13): every residual copy/gap event a run's
`all_cambox_continuity` reports must get its OWN documented reason. `src/residual_events.rs`
locates each event (frame index, tick values, switch-schedule offset, and a
`wall_clock_epoch_s` cross-reference key); THIS script is the OFFLINE second half — given the
verdict JSON plus log files ALREADY PULLED from the rig (this script does no SSH/MCP itself, per
the #707 decision's own scoping: "the tooling gets BUILT and fixture-tested now; live harvesting
resumes the moment cam2 is reflashed"), it groups the matching log lines under each event so a
human/tool reviewing the dossier does not have to hand-grep them.

Log-pulling recipes (run these FIRST, save the output to files, then feed this script):
  - strih genlock-FIFO-audit lines: `.claude/skills/e2e`'s "Diagnosing all_cambox_continuity
    copies/gaps growth" section, step 2 (PowerShell `Select-String -Path $log -Pattern
    "genlock-fifo audit"` against strih's OBS log, scoped to the run's StartRecord..StopRecord
    window).
  - per-camera sender journal lines: the SAME section, step 3 (`journalctl -u camera-box --since
    <window start> --until <window end>` on the physical cambox, translating the schedule label
    to the physical box per the project CLAUDE.md's #708 correction — labels are 1:1 with boxes).

Both log sources print bare time-of-day timestamps (`HH:MM:SS[.fff]`, no date) — this script
matches on time-of-day only (seconds-since-midnight), which is sufficient because every log
pulled for one dossier is from the SAME calendar day as the run. It does NOT attempt full
date/timezone reconciliation; `--tz-offset-hours` lets the caller align the event's UTC
`wall_clock_epoch_s` with a log source printed in local time (the rig boxes run various TZs —
see the project CLAUDE.md's imag-nb Europe/Prague CEST note for a worked example of this exact
local-vs-UTC gotcha).

Pure functions (`extract_time_of_day`, `epoch_to_time_of_day`, `match_lines_for_event`,
`assemble_dossier`) are unit-tested with NO rig, NO real logs, in
tests/python/test_event_forensics_dossier.py — matching the project's established pure-module
pattern (scripts/switch_schedule.py, scripts/e2e_discord_report.py).
"""
from __future__ import annotations

import argparse
import json
import re
import sys

# Matches HH:MM:SS or HH:MM:SS.fff anywhere in a line — both the strih OBS log's own timestamp
# prefix and journalctl's default `Mon DD HH:MM:SS` format carry this shape.
_TIME_OF_DAY_RE = re.compile(r"\b(\d{1,2}):(\d{2}):(\d{2})(?:\.(\d+))?")

SECONDS_PER_DAY = 86_400


def extract_time_of_day(line: str) -> float | None:
    """Return the FIRST `HH:MM:SS[.fff]` timestamp found in `line` as seconds-since-midnight, or
    `None` if the line carries no such timestamp. Pure string parsing, no date/TZ awareness."""
    m = _TIME_OF_DAY_RE.search(line)
    if not m:
        return None
    hh, mm, ss, frac = m.groups()
    hh_i, mm_i, ss_i = int(hh), int(mm), int(ss)
    if hh_i > 23 or mm_i > 59 or ss_i > 59:
        return None
    total = hh_i * 3600 + mm_i * 60 + ss_i
    if frac:
        total += int(frac) / (10 ** len(frac))
    return total


def epoch_to_time_of_day(epoch_s: int, tz_offset_hours: float = 0.0) -> float:
    """Convert a Unix epoch second (`ResidualEvent.wall_clock_epoch_s`, always computed on the
    UTC-domain `gen_ts_ns` timeline — see src/residual_events.rs) to seconds-since-midnight,
    shifted by `tz_offset_hours` to align with a log source printed in local time. Wraps
    correctly across a UTC day boundary."""
    shifted = epoch_s + tz_offset_hours * 3600
    return shifted % SECONDS_PER_DAY


def _tod_distance(a: float, b: float) -> float:
    """Absolute distance between two seconds-since-midnight values, correctly handling the
    midnight wrap (23:59:59 vs 00:00:01 are ~2s apart, not ~86398s apart)."""
    d = abs(a - b)
    return min(d, SECONDS_PER_DAY - d)


def match_lines_for_event(
    event_tod: float, lines: list[str], tolerance_s: float = 2.0
) -> list[str]:
    """Return every line in `lines` whose OWN extracted time-of-day is within `tolerance_s` of
    `event_tod`. A line carrying no parseable timestamp never matches (never a false positive)."""
    matched = []
    for line in lines:
        tod = extract_time_of_day(line)
        if tod is None:
            continue
        if _tod_distance(tod, event_tod) <= tolerance_s:
            matched.append(line)
    return matched


def _collect_residual_events(verdict: dict) -> list[dict]:
    """The SAME flat-then-fallback read `scripts/e2e_discord_report.py`'s residual-events section
    uses — kept as its own tiny helper here (not imported cross-script) since the two tools serve
    different purposes and this repo's scripts are independently runnable/testable."""
    events = verdict.get("all_cambox_continuity", {}).get("residual_events")
    if events is not None:
        return events
    segments = verdict.get("all_cambox_continuity", {}).get("segments") or []
    collected = []
    for seg in segments:
        if isinstance(seg, dict):
            collected.extend(seg.get("residual_events") or [])
    return collected


def assemble_dossier(
    verdict: dict,
    strih_lines: list[str] | None = None,
    camera_lines_by_cambox: dict[str, list[str]] | None = None,
    tolerance_s: float = 2.0,
    tz_offset_hours: float = 0.0,
) -> dict:
    """Pure: verdict JSON dict + already-pulled log lines -> a per-event dossier dict.

    Returns ``{"events": [ {..event fields.., "strih_fifo_lines": [...],
    "camera_journal_lines": [...]}, ... ], "summary": {"total": N, "with_reason": N, "open": N}}``.
    An event's own ``reason`` field (see src/residual_events.rs) is carried through unchanged —
    this tool ASSEMBLES evidence, it does not itself decide a reason.
    """
    strih_lines = strih_lines or []
    camera_lines_by_cambox = camera_lines_by_cambox or {}
    events = _collect_residual_events(verdict)

    dossier_events = []
    for ev in events:
        epoch_s = ev.get("wall_clock_epoch_s")
        tod = epoch_to_time_of_day(epoch_s, tz_offset_hours) if epoch_s is not None else None
        strih_matches = match_lines_for_event(tod, strih_lines, tolerance_s) if tod is not None else []
        cam_lines = camera_lines_by_cambox.get(str(ev.get("cambox", "")).upper(), [])
        cam_matches = match_lines_for_event(tod, cam_lines, tolerance_s) if tod is not None else []
        entry = dict(ev)
        entry["strih_fifo_lines"] = strih_matches
        entry["camera_journal_lines"] = cam_matches
        dossier_events.append(entry)

    with_reason = sum(1 for e in dossier_events if e.get("reason"))
    return {
        "events": dossier_events,
        "summary": {
            "total": len(dossier_events),
            "with_reason": with_reason,
            "open": len(dossier_events) - with_reason,
        },
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--verdict", required=True, help="path to the run's verdict-<run>.json")
    ap.add_argument(
        "--strih-log",
        default=None,
        help="path to a text file of already-pulled strih genlock-FIFO-audit lines",
    )
    ap.add_argument(
        "--camera-log",
        action="append",
        default=[],
        metavar="CAMBOX=<path>",
        help="e.g. --camera-log CAM1=cam1-journal.txt (repeatable, one per cambox)",
    )
    ap.add_argument("--tolerance-s", type=float, default=2.0)
    ap.add_argument(
        "--tz-offset-hours",
        type=float,
        default=0.0,
        help="hours to add to the event's UTC wall-clock second before comparing against "
        "local-time log timestamps (e.g. the imag-nb Europe/Prague CEST offset, +2.0)",
    )
    ap.add_argument("--out", default=None, help="write the dossier JSON here (default: stdout)")
    args = ap.parse_args(argv)

    with open(args.verdict, encoding="utf-8") as f:
        verdict = json.load(f)

    strih_lines = []
    if args.strih_log:
        with open(args.strih_log, encoding="utf-8") as f:
            strih_lines = f.readlines()

    camera_lines_by_cambox: dict[str, list[str]] = {}
    for spec in args.camera_log:
        if "=" not in spec:
            print(f"ERROR: --camera-log expects CAMBOX=<path>, got {spec!r}", file=sys.stderr)
            return 2
        cambox, path = spec.split("=", 1)
        with open(path, encoding="utf-8") as f:
            camera_lines_by_cambox[cambox.upper()] = f.readlines()

    dossier = assemble_dossier(
        verdict,
        strih_lines,
        camera_lines_by_cambox,
        tolerance_s=args.tolerance_s,
        tz_offset_hours=args.tz_offset_hours,
    )
    text = json.dumps(dossier, indent=2)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text)
        print(f"wrote dossier for {dossier['summary']['total']} event(s) -> {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
