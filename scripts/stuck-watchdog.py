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
    kind: str  # "low_fps" | "underrun_spike" | "vanished" | "dantesync_runaway"
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


# A real day-rollover sends the log timestamp BACKWARDS by nearly a full day; anything less
# negative is clock jitter or two same-second samples, NOT a midnight wrap.
_MIDNIGHT_WRAP_THRESHOLD_SECS = 43200.0  # half a day


def pair_dt(prev: AuditSample, curr: AuditSample) -> float:
    """Elapsed seconds between two consecutive samples.

    Uses the parsed OBS log timestamps when BOTH are present, handling a GENUINE midnight wrap (a
    backwards jump of MORE than half a day → add 86400). A 0 / tiny / small-negative delta is two
    same-second samples or clock jitter — it carries no usable interval, so we fall back to the
    genuine ~5 s genlock audit cadence (AUDIT_INTERVAL_SECS, the real
    GENLOCK_AUDIT_LOG_INTERVAL_NS) rather than (a) fabricating a near-zero rate via a bogus +86400
    wrap on a 0-delta (#266 finding :145), or (b) trusting a hardcoded dt where the real cadence is
    the correct, documented fallback (#266 finding :142). The fallback is the genuine cadence, not
    an arbitrary number.
    """
    if prev.ts_secs is not None and curr.ts_secs is not None:
        diff = curr.ts_secs - prev.ts_secs
        if diff < -_MIDNIGHT_WRAP_THRESHOLD_SECS:
            diff += 86400.0  # genuine day rollover
        if diff > 0:
            return diff
        # diff <= 0: same-second samples / clock jitter — no usable interval; use the audit cadence.
    return AUDIT_INTERVAL_SECS


def received_fps(prev: AuditSample, curr: AuditSample) -> Optional[float]:
    """Received-fps between two consecutive samples = delta(received) / delta(time).

    The interval comes from [`pair_dt`]. A backwards `received` (counter reset / OBS restart)
    yields None — no meaningful rate across a reset.
    """
    if curr.received < prev.received:
        return None
    dt = pair_dt(prev, curr)
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

    The #265 stuck state is a STARVED genlock receive: the locked output keeps pulling while NDI
    delivery collapses, so `underruns` climb on a drained FIFO while almost nothing arrives
    (`overruns` stay flat). That MUST alert whether or not the source was explicitly declared — a
    broadcast input frozen to ~0 fps in default (scan-all) cron mode is the WORST case and must NOT
    be silently skipped as "idle" (#266 :177/:178).

    `overruns` is the discriminator that keeps a genuinely idle input quiet (#266 :110/:70): a
    PARKED source (added but not on program) RECEIVES frames it never consumes, so its FIFO
    OVERFLOWS — overruns climb (dominating its underrun churn). An overrun-dominated source is
    alive, not starved → treated as idle and skipped. A source with NO activity at all (nothing
    arriving, nothing starving) is dormant → skipped too.

    A source NAMED in `monitored_sources` is a declared, required broadcast input: it always alerts
    below `fps_floor`, and if it produced no usable audit pair at all (vanished — NDI dropped, the
    fifo stopped logging) it alerts as `vanished` (#266 :130).

    `idle_floor` separates an unambiguously-degraded broadcast (idle_floor..fps_floor, e.g. ~10 fps
    — always alerts) from the ambiguous ~0 fps floor (frozen broadcast vs idle probe — resolved by
    the underrun/overrun starvation signature above). All counter deltas are normalized to the ~5 s
    audit interval so a non-5 s gap between two audit lines cannot distort the spikes (#266 :189).
    """
    alerts: List[Alert] = []
    named = {s for s in (monitored_sources or [])}
    for source, (prev, curr) in sorted(per_source.items()):
        fps = received_fps(prev, curr)
        is_named = source in named
        scale = AUDIT_INTERVAL_SECS / pair_dt(prev, curr)  # normalize counter deltas to ~5 s
        d_underruns = max(0, curr.underruns - prev.underruns) * scale
        d_overruns = max(0, curr.overruns - prev.overruns) * scale

        # PARKED/idle (#70): frames ARE arriving but overflow the FIFO (overruns dominate the
        # underrun churn) — the source is alive, just not consumed. NOT the starved stuck state.
        parked_idle = d_overruns > 0 and d_overruns >= d_underruns
        # STARVED: the genlock output is pulling an empty FIFO — underruns climbing past the spike
        # threshold while NOT overrun-dominated.
        starved = d_underruns > underrun_spike and not parked_idle

        if fps is not None and fps < fps_floor:
            if fps >= idle_floor:
                # Delivering some frames but well below norm (the ~10 fps #265 collapse) —
                # unambiguously a degraded broadcast input. Always alert.
                trip = True
            else:
                # ~0 fps (frozen). A declared source must be alive → always alert. Otherwise alert
                # only on the STARVED signature (a frozen real cam), never a parked/dormant one.
                trip = is_named or starved
            if trip:
                alerts.append(
                    Alert(
                        box=box,
                        kind="low_fps",
                        detail=f"NDI príjem '{source}' = {fps:.1f} fps (norma 30)",
                    )
                )

        # Underrun SPIKE on its own (receive may still look ~ok but the FIFO is starving). Skip a
        # parked/idle source — its underrun churn is the expected #70 idle pattern, not starvation.
        if (is_named or not parked_idle) and d_underruns > underrun_spike:
            alerts.append(
                Alert(
                    box=box,
                    kind="underrun_spike",
                    detail=f"'{source}' underruns +{d_underruns:.0f} za ~5s (genlock hladuje)",
                )
            )

    # A DECLARED (named/required) source that produced no usable audit pair has VANISHED — NDI
    # dropped, the fifo stopped logging — so it is absent from `per_source` and must still alert
    # (a required input we can no longer confirm is alive), never silently disappear (#266 :130).
    for source in sorted(named):
        if source not in per_source:
            alerts.append(
                Alert(
                    box=box,
                    kind="vanished",
                    detail=f"deklarovaný vstup '{source}' zmizol — žiadne NDI audit dáta",
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


def send_alert(body: str, airuleset: str, dry_run: bool) -> bool:
    """Fire the Discord alert via airuleset notify (or just print it on --dry-run).

    Returns True when the alert was delivered (or printed on --dry-run), False when delivery FAILED
    — airuleset notify exited non-zero (broken Discord webhook / missing env / network outage), or
    could not be spawned at all. A failed delivery must NEVER be swallowed as "alert fired": main()
    turns a False here into a distinct, loud exit code so the un-delivered stuck-state is itself
    surfaced (#266 finding :226, script-failure-policy / fail-loudly).
    """
    if dry_run:
        print("[dry-run] would alert:\n" + body)
        return True
    cmd = ["python3", os.path.expanduser(airuleset), "notify", "--body", body]
    try:
        result = subprocess.run(cmd, check=False)
    except OSError as e:
        print(f"ERROR: could not spawn watchdog alert ({airuleset}): {e}", file=sys.stderr)
        return False
    if result.returncode != 0:
        print(
            f"ERROR: watchdog alert delivery FAILED — airuleset notify exited "
            f"{result.returncode} (Discord webhook / env / network?)",
            file=sys.stderr,
        )
        return False
    return True


def read_recent_lines(path: str, max_bytes: int = 2_000_000) -> List[str]:
    """Read only the TAIL of the (possibly multi-GB) OBS log.

    Only the last two audit lines per active source are needed, and the genlock audit cadence
    (~5 s per source) means thousands of recent lines fit in a few hundred KB — so reading just the
    last ~`max_bytes` bounds memory on a huge live OBS log instead of slurping the whole file (#266
    finding :265). A source whose audit lines fell off the tail is treated as having no recent
    samples — which is correct: a declared source that stopped logging has VANISHED and alerts via
    the named-source check (#266 :130).
    """
    with open(path, "rb") as fh:
        fh.seek(0, os.SEEK_END)
        size = fh.tell()
        start = max(0, size - max_bytes)
        fh.seek(start)
        data = fh.read()
    lines = data.decode("utf-8", errors="replace").splitlines()
    if start > 0 and lines:
        lines = lines[1:]  # drop the partial first line from the mid-file seek
    return lines


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
        lines = read_recent_lines(args.obs_log)
    except OSError as e:
        print(f"ERROR: cannot read OBS log {args.obs_log}: {e}", file=sys.stderr)
        return 2

    per_source = last_two_per_source(lines)
    # Missing NDI audit data is a BAD-INPUT condition (exit 2), NOT a healthy run — supplying
    # --dantesync-cpu must NOT mask it into a green exit 0 with the NDI check having evaluated
    # nothing (#266 finding :271). When --source names declared inputs, an absent one is an ALERT
    # (vanished, handled in evaluate), so only the default scan-all mode with no audit data at all
    # is treated as un-monitorable bad input.
    if not per_source and not args.source:
        print(
            f"ERROR: no genlock-fifo audit data (>=2 lines/source) in {args.obs_log} — "
            "nothing to evaluate (NDI receive un-monitorable)",
            file=sys.stderr,
        )
        return 2

    # Echo the resolved thresholds on EVERY outcome so an OK/ALERT line is self-explaining (#266
    # finding :290, comprehensive-logging).
    thresholds = (
        f"fps_floor={args.fps_floor} idle_floor={args.idle_floor} "
        f"underrun_spike={args.underrun_spike} dantesync_core_pct={args.dantesync_core_pct}"
    )

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
        # Per-source diagnostic: fps + the counter deltas (consumed/underruns/overruns) so a quiet
        # OK run still records the full audit picture for later debugging (comprehensive-logging).
        def _diag(s: str, p: AuditSample, c: AuditSample) -> str:
            fps = received_fps(p, c)
            fps_s = f"{fps:.1f}fps" if fps is not None else "?fps"
            return (
                f"{s}={fps_s}(consumed+{max(0, c.consumed - p.consumed)} "
                f"underruns+{max(0, c.underruns - p.underruns)} "
                f"overruns+{max(0, c.overruns - p.overruns)})"
            )

        srcs = ", ".join(_diag(s, p, c) for s, (p, c) in sorted(per_source.items()))
        dante = f", dantesync={args.dantesync_cpu:.0f}%" if args.dantesync_cpu is not None else ""
        print(
            f"OK [{args.box}]: no stuck state — {srcs or 'no source samples'}{dante} [{thresholds}]"
        )
        return 0

    body = compose_alert_body(args.box, args.host, alerts)
    delivered = send_alert(body, args.airuleset, args.dry_run)
    print(f"ALERT [{args.box}]: {len(alerts)} condition(s) tripped [{thresholds}]", file=sys.stderr)
    if not delivered:
        print(
            f"ERROR [{args.box}]: alert could NOT be delivered — stuck state went UNREPORTED",
            file=sys.stderr,
        )
        return 3
    return 1


if __name__ == "__main__":
    sys.exit(main())
