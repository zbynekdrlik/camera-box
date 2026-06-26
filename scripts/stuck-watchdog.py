#!/usr/bin/env python3
"""#266 — DETECT + ALERT the strih/stream NDI-receive stuck state before it bites a live event.

THE STUCK STATE (user-confirmed, intermittent — see #265): after long uptime the cam->OBS NDI
receive on a broadcast box collapses to ~10 fps (genlock starved, huge underruns) while
`dantesync.exe` runs away pegging ~1 of 16 cores. Restarting the dantesync service alone does NOT
fix it; only a full PC reboot does (2026-06-26: cam1->strih ~10fps->30.2fps, underruns 290k->0
after reboot). The operator needs to catch it BEFORE a show, not discover a stuttering camera mid-air.

This is a LEAN, standalone DETECTION + ALERT check (NOT Prometheus, NOT #138-143 observability).
Recovery is a documented PC reboot (#265) — the watchdog only detects and alerts.

It reads, per box, two signals and alerts on Discord (via airuleset notify) when either trips:

  (a) per-source NDI received-fps — from the genlock-fifo audit log the genlock OBS build emits
      every ~5 s (vendor/obs-studio/libobs/obs-source.c::genlock_audit_log):
        `genlock-fifo audit 'NDI cam5': received=N consumed=N underruns=N overruns=N depth=.. ...`
      received-fps = delta(received) / delta(time) across two consecutive audit lines for that
      source. A broadcast input well below 30 fps == the stuck state.
  (b) dantesync CPU — the percent of ONE core dantesync.exe is burning (a runaway pegs ~100% of a
      single core). Provided by the poller (see "ACQUISITION" below) via --dantesync-cpu.

ACQUISITION (read-only — no rig deploy, no Windows SSH):
  The deterministic parse/threshold/alert core lives here and is unit-tested
  (tests/python/test_stuck_watchdog.py). The two raw inputs are gathered per box with the win-*
  MCP (or SMB) and handed to this script:
    - OBS log: `win-strih` / `win-stream-snv` FileRead the live OBS log
      (`%APPDATA%\\obs-studio\\logs\\<latest>.txt`), OR SMB-read it from dev1
      (`\\10.77.9.202\\C$\\Users\\...\\AppData\\Roaming\\obs-studio\\logs\\`), then pass --obs-log.
    - dantesync CPU: `win-strih` ListProcesses / GetSystemInfo for dantesync.exe CPU%-of-one-core,
      pass --dantesync-cpu. (Optional; omit to check only the NDI receive.)
  Run once per box (e.g. cron / the win-MCP recipe), repeatable for strih + stream.

Exit code: 0 = healthy (no alert), 1 = at least one threshold tripped (alert fired unless --dry-run),
2 = bad input / no audit data found. --dry-run prints the alert instead of sending it.
"""
import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

# The genlock build logs the audit line every ~5 s
# (GENLOCK_AUDIT_LOG_INTERVAL_NS in vendor/obs-studio/libobs/obs-source.c). Used as the fallback
# delta-t when the two audit lines carry no parseable timestamp prefix.
AUDIT_INTERVAL_SECS = 5.0

# Default thresholds (overridable via CLI):
DEFAULT_FPS_FLOOR = 25.0  # a broadcast input below this (norm 30) == collapsing/stuck
DEFAULT_IDLE_FLOOR = 1.0  # below this an UNNAMED source is treated as idle (not broadcasting)
DEFAULT_UNDERRUN_SPIKE = 50  # underruns added between two 5 s samples above this == starvation
DEFAULT_DANTESYNC_CORE_PCT = 85.0  # dantesync burning > this % of ONE core == runaway

# `genlock-fifo audit 'NAME': received=N consumed=N underruns=N overruns=N depth=N peak=N ...`
_AUDIT_RE = re.compile(
    r"genlock-fifo audit '(?P<source>[^']*)':\s+"
    r"received=(?P<received>\d+)\s+"
    r"consumed=(?P<consumed>\d+)\s+"
    r"underruns=(?P<underruns>\d+)\s+"
    r"overruns=(?P<overruns>\d+)"
)
# The OBS log-line timestamp prefix at the START of the line (`HH:MM:SS.mmm:` / `HH:MM:SS.mmm `),
# extracted independently of the message so any intervening token never hides it.
_TS_PREFIX_RE = re.compile(r"^\s*(\d{2}:\d{2}:\d{2}\.\d{3})\b")


@dataclass
class AuditSample:
    """One parsed genlock-fifo audit line for a source (cumulative counters + optional log ts)."""

    source: str
    received: int
    consumed: int
    underruns: int
    overruns: int
    ts_secs: Optional[float]  # seconds-of-day from the HH:MM:SS.mmm prefix, or None


@dataclass
class Alert:
    """One tripped threshold, ready to render into the Discord body."""

    box: str
    kind: str  # "low_fps" | "underrun_spike" | "dantesync_runaway"
    detail: str


def _ts_to_secs(ts: str) -> Optional[float]:
    """`HH:MM:SS.mmm` -> seconds since midnight (float). None if malformed."""
    m = re.fullmatch(r"(\d{2}):(\d{2}):(\d{2})\.(\d{3})", ts)
    if not m:
        return None
    h, mi, s, ms = (int(x) for x in m.groups())
    return h * 3600 + mi * 60 + s + ms / 1000.0


def parse_audit_line(line: str) -> Optional[AuditSample]:
    """Parse ONE genlock-fifo audit log line. Returns None for any non-matching line."""
    m = _AUDIT_RE.search(line)
    if not m:
        return None
    tsm = _TS_PREFIX_RE.match(line)
    return AuditSample(
        source=m.group("source"),
        received=int(m.group("received")),
        consumed=int(m.group("consumed")),
        underruns=int(m.group("underruns")),
        overruns=int(m.group("overruns")),
        ts_secs=_ts_to_secs(tsm.group(1)) if tsm else None,
    )


def last_two_per_source(lines: List[str]) -> Dict[str, Tuple[AuditSample, AuditSample]]:
    """Scan log lines; keep the LAST two audit samples (prev, curr) per source.

    Sources with only one audit line yield no entry (cannot compute a rate). The newest line is
    `curr`, the one before it `prev`.
    """
    per_source: Dict[str, List[AuditSample]] = {}
    for line in lines:
        s = parse_audit_line(line)
        if s is None:
            continue
        buf = per_source.setdefault(s.source, [])
        buf.append(s)
        if len(buf) > 2:
            buf.pop(0)
    return {src: (b[0], b[1]) for src, b in per_source.items() if len(b) == 2}


def received_fps(prev: AuditSample, curr: AuditSample) -> Optional[float]:
    """Received-fps between two consecutive samples = delta(received) / delta(time).

    delta-time from the parsed log timestamps when both are present (and positive, handling
    midnight wrap), else the known ~5 s audit interval. A backwards `received` (counter reset /
    OBS restart) yields None — no meaningful rate across a reset.
    """
    if curr.received < prev.received:
        return None
    dt = AUDIT_INTERVAL_SECS
    if prev.ts_secs is not None and curr.ts_secs is not None:
        diff = curr.ts_secs - prev.ts_secs
        if diff <= 0:
            diff += 86400.0  # crossed midnight
        if diff > 0:
            dt = diff
    if dt <= 0:
        return None
    return (curr.received - prev.received) / dt


def evaluate(
    box: str,
    per_source: Dict[str, Tuple[AuditSample, AuditSample]],
    dantesync_cpu: Optional[float],
    *,
    monitored_sources: Optional[List[str]] = None,
    fps_floor: float = DEFAULT_FPS_FLOOR,
    idle_floor: float = DEFAULT_IDLE_FLOOR,
    underrun_spike: int = DEFAULT_UNDERRUN_SPIKE,
    dantesync_core_pct: float = DEFAULT_DANTESYNC_CORE_PCT,
) -> List[Alert]:
    """Apply the stuck-state thresholds. Returns one Alert per tripped condition (empty == healthy).

    A source NAMED in `monitored_sources` is a declared broadcast input — any received-fps below
    `fps_floor` (incl. ~0) alerts. An UNNAMED source below `idle_floor` is treated as idle (probe /
    not broadcasting) and skipped, so #70 idle-source underrun spam never false-alarms; between
    idle_floor and fps_floor it is a collapsing broadcast input and alerts.
    """
    alerts: List[Alert] = []
    named = {s for s in (monitored_sources or [])}
    for source, (prev, curr) in sorted(per_source.items()):
        fps = received_fps(prev, curr)
        is_named = source in named
        if fps is not None and fps < fps_floor:
            if is_named or fps >= idle_floor:
                alerts.append(
                    Alert(
                        box=box,
                        kind="low_fps",
                        detail=f"NDI príjem '{source}' = {fps:.1f} fps (norma 30)",
                    )
                )
        # Underrun SPIKE only counts for a source we are actually treating as a broadcast input
        # (named, or delivering >= idle_floor) — an idle source's underrun spam is expected (#70).
        if is_named or (fps is not None and fps >= idle_floor):
            d_underruns = curr.underruns - prev.underruns
            if d_underruns > underrun_spike:
                alerts.append(
                    Alert(
                        box=box,
                        kind="underrun_spike",
                        detail=f"'{source}' underruns +{d_underruns} za ~5s (genlock hladuje)",
                    )
                )
    if dantesync_cpu is not None and dantesync_cpu > dantesync_core_pct:
        alerts.append(
            Alert(
                box=box,
                kind="dantesync_runaway",
                detail=f"dantesync.exe žerie {dantesync_cpu:.0f}% jedného jadra (runaway)",
            )
        )
    return alerts


def compose_alert_body(box: str, host: Optional[str], alerts: List[Alert]) -> str:
    """Render the tripped alerts into a short Slovak Discord body (recovery = documented reboot)."""
    where = f"{box} ({host})" if host else box
    lines = [f"⚠️ camera-box watchdog — {where}: zaseknutý NDI príjem (#266)"]
    for a in alerts:
        lines.append(f"• {a.detail}")
    lines.append(f"Náprava: reštart PC {where} a over príjem ~30 fps. Pozri #265.")
    return "\n".join(lines)


def send_alert(body: str, airuleset: str, dry_run: bool) -> None:
    """Fire the Discord alert via airuleset notify (or just print it on --dry-run)."""
    if dry_run:
        print("[dry-run] would alert:\n" + body)
        return
    cmd = ["python3", os.path.expanduser(airuleset), "notify", "--body", body]
    try:
        subprocess.run(cmd, check=False)
    except OSError as e:
        print(f"WARNING: could not fire watchdog alert: {e}", file=sys.stderr)


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="camera-box NDI-receive stuck-state watchdog (#266)")
    ap.add_argument("--box", required=True, help="box label, e.g. strih | stream")
    ap.add_argument("--host", help="box IP/host (for the alert text only)")
    ap.add_argument(
        "--obs-log",
        required=True,
        help="path to the box's OBS log (read-only; fetched via win-* MCP or SMB)",
    )
    ap.add_argument(
        "--source",
        action="append",
        default=[],
        help="a declared broadcast input name to monitor (repeatable). Omit to scan all sources.",
    )
    ap.add_argument(
        "--dantesync-cpu",
        type=float,
        help="dantesync.exe CPU as percent of ONE core (from the poller). Omit to skip the check.",
    )
    ap.add_argument("--fps-floor", type=float, default=DEFAULT_FPS_FLOOR)
    ap.add_argument("--idle-floor", type=float, default=DEFAULT_IDLE_FLOOR)
    ap.add_argument("--underrun-spike", type=int, default=DEFAULT_UNDERRUN_SPIKE)
    ap.add_argument("--dantesync-core-pct", type=float, default=DEFAULT_DANTESYNC_CORE_PCT)
    ap.add_argument(
        "--airuleset",
        default="~/devel/airuleset/airuleset.py",
        help="path to airuleset.py for the Discord alert",
    )
    ap.add_argument("--dry-run", action="store_true", help="print the alert instead of sending it")
    args = ap.parse_args(argv)

    try:
        with open(args.obs_log, "r", encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
    except OSError as e:
        print(f"ERROR: cannot read OBS log {args.obs_log}: {e}", file=sys.stderr)
        return 2

    per_source = last_two_per_source(lines)
    if not per_source and args.dantesync_cpu is None:
        print(
            f"ERROR: no genlock-fifo audit data (>=2 lines/source) in {args.obs_log} "
            "and no --dantesync-cpu given — nothing to evaluate",
            file=sys.stderr,
        )
        return 2

    alerts = evaluate(
        args.box,
        per_source,
        args.dantesync_cpu,
        monitored_sources=args.source,
        fps_floor=args.fps_floor,
        idle_floor=args.idle_floor,
        underrun_spike=args.underrun_spike,
        dantesync_core_pct=args.dantesync_core_pct,
    )
    if not alerts:
        srcs = ", ".join(
            f"{s}={received_fps(p, c):.1f}fps" if received_fps(p, c) is not None else f"{s}=?"
            for s, (p, c) in sorted(per_source.items())
        )
        print(f"OK [{args.box}]: no stuck state — {srcs or 'no source samples'}")
        return 0

    body = compose_alert_body(args.box, args.host, alerts)
    send_alert(body, args.airuleset, args.dry_run)
    print(f"ALERT [{args.box}]: {len(alerts)} condition(s) tripped", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
