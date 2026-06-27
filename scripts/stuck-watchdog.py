#!/usr/bin/env python3
"""#266 — DETECT + ALERT the strih/stream NDI-receive stuck state before it bites a live event.

THE STUCK STATE (user-confirmed, intermittent — see #265): after long uptime the cam->OBS NDI
receive on a broadcast box collapses to ~7-10 fps (the genlock FIFO is STARVED, underruns climbing
on a drained queue) while `dantesync.exe` runs away pegging ~1 of 16 cores. Restarting the dantesync
service alone does NOT fix it; only a full PC reboot does (2026-06-26: cam1->strih ~10fps->30.2fps,
underruns 290k->0 after reboot). The operator must catch it BEFORE a show, not discover a stuttering
camera mid-air.

This is a LEAN, standalone DETECTION + ALERT check (NOT Prometheus, NOT #138-143 observability).
Recovery is the documented PC reboot (#265) — the watchdog only detects and alerts.

THE HARD PART — discriminate a genuine STUCK from a benign IDLE source (the redesign mandate; the
first cut false-negatived a 0-fps frozen cam AND false-positived an idle probe input). The signal is
NOT a single underruns reading — it is the TRAJECTORY across a WINDOW of recent audit samples per
source:

  * STUCK (alert) — the source HAS delivered (its cumulative `received` is non-trivial, and/or it was
    advancing earlier in the window) and is NOW collapsed below the fps floor (down to a full freeze
    at 0 fps) AND STARVED: underruns climbing, the FIFO depth drained to ~0, and NOT overrun-dominant.
    This is the #265 collapse. A 0-fps source that HAD been delivering is the WORST case and MUST
    alert even when it is not named (default scan-all mode) — including a freeze that has lasted
    LONGER than the observed window (the cumulative `received` is what proves prior delivery once the
    delivering samples scroll out of the window; verified against real strih log data where `received`
    sat frozen at 41421 for ~30 audit samples while underruns climbed ~30/s).
  * benign IDLE (no alert) — a source that NEVER delivered (cumulative `received` ~0 and flat across
    the window: a probe/monitor input parked off-program, never on the wire), OR a source whose churn
    is OVERRUNS-dominant (frames arriving faster than consumed → the FIFO overflows → holding, not
    starved). overruns is the holding-vs-starved discriminator.

Reading the WINDOW (not just the last two samples) is what lets us tell "was advancing then froze"
(STUCK) from "never advanced" (IDLE) — both show a flat `received` in the most recent pair.

STALENESS (the live-rig safeguard against a false alert) — a genlock-fifo audit line is written
(every ~5 s) only while the source is on an actively-rendered scene. When a source leaves program OBS
STOPS logging it, so its newest sample goes OLD. A source whose newest sample is older than
--max-sample-age (vs `now`, the box clock) is no longer being rendered → NOT stuck, just not in use →
SKIPPED (a NAMED/required one alerts as `vanished`). This stops the "received frozen at 41421 from
hours ago" tail of a stopped log from false-alarming a healthy box, while a GENUINELY stuck source
(still on program → still logging fresh frozen+starving samples every 5 s) alerts. Staleness is OFF
when no `now` is supplied — the pure classification logic is unit-tested in isolation that way; the
poller passes `--now` = the box's current HH:MM:SS (read via the same win-* MCP as the log).

  (a) per-source NDI received-fps + the starvation signature — from the genlock-fifo audit line the
      genlock OBS build emits every ~5 s (vendor/obs-studio/libobs/obs-source.c::genlock_audit_log):
        `... genlock-fifo audit 'NDI cam5': received=N consumed=N underruns=N holds=N overruns=N
         depth=N peak=N latency_ms=...`
      (NB the `received`/`underruns`/`overruns` counters are CUMULATIVE per OBS process — only the
      DELTA across the window matters; an absolute underruns of 290k can be idle accumulation.)
  (b) dantesync CPU — the percent of ONE core dantesync.exe is burning (a runaway pegs ~100% of a
      single core). Provided by the poller via --dantesync-cpu. Checked INDEPENDENTLY of the NDI
      audit: a runaway alerts even when there is no audit data at all.

ACQUISITION (read-only — no rig deploy, no Windows SSH):
  The deterministic parse/classify/alert core lives here and is unit-tested
  (tests/python/test_stuck_watchdog.py). The two raw inputs are gathered per box with the win-*
  MCP (or SMB) and handed to this script:
    - OBS log: `win-strih` / `win-stream-snv` FileRead the live OBS log
      (`%APPDATA%\\obs-studio\\logs\\<latest>.txt`), pass --obs-log.
    - dantesync CPU: `win-strih` ListProcesses / GetSystemInfo for dantesync.exe CPU%-of-one-core,
      pass --dantesync-cpu. (Optional; omit to check only the NDI receive.)
  Run once per box (e.g. cron / the win-MCP recipe), repeatable for strih + stream.

EXIT CODES (the full table — the redesign requires every one documented + distinct):
  0 = healthy        — nothing tripped (the NDI receive and/or dantesync we COULD read are fine).
  1 = alert tripped AND DELIVERED — a stuck/vanished/runaway condition fired and the Discord alert
                       was sent successfully (or printed under --dry-run).
  2 = no signal      — nothing to judge at all: no usable NDI audit data AND no declared --source
                       AND no --dantesync-cpu reading. (A dantesync reading, even a healthy one, is
                       a signal → never exit 2; a runaway with no audit alerts, never exit 2.)
  3 = alert tripped but DELIVERY FAILED — a condition fired but `airuleset notify` could not deliver
                       the Discord alert (non-zero exit / could not spawn). DISTINCT from exit 1 so an
                       un-delivered stuck state is itself loud (script-failure-policy / fail-loudly),
                       never silently read as "handled".
"""
import argparse
import datetime
import os
import re
import statistics
import subprocess
import sys
from dataclasses import dataclass
from typing import Dict, List, Optional

# The genlock build logs the audit line every ~5 s (GENLOCK_AUDIT_LOG_INTERVAL_NS in
# vendor/obs-studio/libobs/obs-source.c). Used as the cadence when no audit line in the window
# carries a parseable timestamp (the per-window cadence is derived from real timestamps first).
AUDIT_INTERVAL_SECS = 5.0

# How many recent audit samples per source to retain — the WINDOW. At the ~5 s cadence this is
# ~60 s of history, enough to see "was delivering then froze" vs "never delivered".
DEFAULT_WINDOW = 12
# The most-recent sub-window (in samples) the current fps / starvation rates are measured over.
# 3 samples == 2 pairs == ~10 s — long enough to be steady, short enough to catch a fresh freeze.
RECENT_SAMPLES = 3

# A pair_dt floor: two audit lines logged sub-second apart (a double-log / re-arm) carry a tiny dt
# that would over-amplify a per-second rate (a 5-underrun tick over 0.1 s reads as 50/s). Floor the
# interval so a near-zero dt can never manufacture a wild fps/underrun rate (#266 dt-floor).
MIN_PAIR_DT = 1.0
# A real midnight rollover sends the log timestamp BACKWARDS by nearly a full day; anything less
# negative is clock jitter or two same-second samples, NOT a wrap (#266 midnight-wrap-only-on-large).
MIDNIGHT_WRAP_THRESHOLD_SECS = 43200.0  # half a day

# Default thresholds (overridable via CLI):
DEFAULT_FPS_FLOOR = 25.0  # a broadcast input below this (norm 30) == collapsing/stuck
DEFAULT_IDLE_FLOOR = 1.0  # below this the source is "frozen/near-0" (ambiguous → use the trajectory)
DEFAULT_UNDERRUN_RATE = 10.0  # underruns added PER SECOND above this == the FIFO starving (#265 ~25/s)
DEFAULT_DEPTH_FLOOR = 2.0  # recent FIFO depth at/below this == drained (starved); #265 ran depth 0-1
DEFAULT_DELIVERED_FRAMES = 30  # cumulative received above this (~1 s @30fps) proves the source HAS
#                               delivered — it is a real feed, not a never-delivered idle probe.
DEFAULT_MAX_SAMPLE_AGE_SECS = 60.0  # a source whose newest audit sample is older than this (vs `now`)
#                                     stopped logging → left the rendered scene → not currently active.
DEFAULT_DANTESYNC_CORE_PCT = 85.0  # dantesync burning > this % of ONE core == runaway

# The OBS log-line timestamp prefix at the START of the line (`HH:MM:SS.mmm`), extracted
# independently of the message so an intervening token never hides it.
_TS_PREFIX_RE = re.compile(r"^\s*(\d{2}):(\d{2}):(\d{2})\.(\d{3})\b")
# The quoted source name in the audit line.
_SOURCE_RE = re.compile(r"genlock-fifo audit '(?P<source>[^']*)':")


@dataclass
class AuditSample:
    """One parsed genlock-fifo audit line for a source (cumulative counters + optional FIFO depth
    + optional log ts)."""

    source: str
    received: int
    consumed: int
    underruns: int
    overruns: int
    depth: Optional[int] = None
    ts_secs: Optional[float] = None


@dataclass
class SourceStat:
    """The window-derived view of ONE source: the recent rates + the trajectory verdicts the
    classifier reads. Explicit + inspectable on purpose — this is the tool that earned distrust."""

    source: str
    n_samples: int
    recent_fps: Optional[float]  # received-fps over the recent sub-window (None = reset/unknown)
    peak_fps: Optional[float]  # best received-fps across the WHOLE window (the "was delivering" peak)
    underrun_rate: float  # underruns added / s over the recent sub-window
    overrun_rate: float  # overruns added / s over the recent sub-window
    recent_depth: Optional[float]  # median FIFO depth over the recent sub-window
    collapsed: bool  # recent_fps < fps_floor
    starved: bool  # underruns climbing + FIFO drained + NOT overrun-dominant
    was_delivering: bool  # currently delivering some, OR delivered healthily earlier in the window


@dataclass
class Alert:
    """One tripped condition, ready to render into the Discord body."""

    box: str
    kind: str  # "stuck" | "vanished" | "dantesync_runaway"
    detail: str


def _field(line: str, key: str) -> Optional[int]:
    """Extract an integer `key=N` token from an audit line (field-by-field so an inserted field —
    e.g. `holds=` added by #148 between `underruns=` and `overruns=` — never breaks the parse)."""
    m = re.search(rf"\b{re.escape(key)}=(\d+)", line)
    return int(m.group(1)) if m else None


def _ts_secs(line: str) -> Optional[float]:
    """`HH:MM:SS.mmm` log prefix → seconds since midnight (float). None if no prefix."""
    m = _TS_PREFIX_RE.match(line)
    if not m:
        return None
    h, mi, s, ms = (int(x) for x in m.groups())
    return h * 3600 + mi * 60 + s + ms / 1000.0


def parse_audit_line(line: str) -> Optional[AuditSample]:
    """Parse ONE genlock-fifo audit log line. Returns None for any non-matching line, or any audit
    line missing a required counter (received/consumed/underruns/overruns). `depth` is optional (an
    older build without the field still parses)."""
    sm = _SOURCE_RE.search(line)
    if not sm:
        return None
    received = _field(line, "received")
    consumed = _field(line, "consumed")
    underruns = _field(line, "underruns")
    overruns = _field(line, "overruns")
    if None in (received, consumed, underruns, overruns):
        return None
    return AuditSample(
        source=sm.group("source"),
        received=received,
        consumed=consumed,
        underruns=underruns,
        overruns=overruns,
        depth=_field(line, "depth"),
        ts_secs=_ts_secs(line),
    )


def window_per_source(lines: List[str], window: int = DEFAULT_WINDOW) -> Dict[str, List[AuditSample]]:
    """Scan log lines; keep the LAST `window` audit samples per source, in chronological order.

    Reading a WINDOW (not just the last two) is the core of the redesign: it is what lets the
    classifier tell "was advancing then froze" (STUCK) from "never advanced" (IDLE)."""
    per: Dict[str, List[AuditSample]] = {}
    for line in lines:
        s = parse_audit_line(line)
        if s is None:
            continue
        buf = per.setdefault(s.source, [])
        buf.append(s)
        if len(buf) > window:
            buf.pop(0)
    return per


def pair_dt(prev: AuditSample, curr: AuditSample, cadence: float = AUDIT_INTERVAL_SECS) -> float:
    """Elapsed seconds between two consecutive samples.

    Uses the parsed OBS log timestamps when BOTH are present — handling a GENUINE midnight wrap (a
    backwards jump of MORE than half a day → +86400) and flooring a sub-second positive dt to
    MIN_PAIR_DT so a double-log can't over-amplify a rate. A 0 / tiny / small-negative delta is two
    same-second samples or clock jitter (NOT a wrap), and a missing timestamp carries no usable
    interval → both fall back to the per-window `cadence`."""
    if prev.ts_secs is not None and curr.ts_secs is not None:
        diff = curr.ts_secs - prev.ts_secs
        if diff < -MIDNIGHT_WRAP_THRESHOLD_SECS:
            diff += 86400.0  # genuine day rollover
        if diff > 0:
            return max(diff, MIN_PAIR_DT)
        # diff <= 0 → same-second samples / clock jitter → no usable interval; use the cadence.
    return cadence


def received_fps(
    prev: AuditSample, curr: AuditSample, cadence: float = AUDIT_INTERVAL_SECS
) -> Optional[float]:
    """Received-fps between two consecutive samples = delta(received) / pair_dt. A backwards
    `received` (counter reset / OBS restart) yields None — no meaningful rate across a reset."""
    if curr.received < prev.received:
        return None
    dt = pair_dt(prev, curr, cadence)
    if dt <= 0:
        return None
    return (curr.received - prev.received) / dt


def estimate_cadence(samples: List[AuditSample]) -> float:
    """The real audit cadence for THIS source, derived from its actual timestamped samples (median
    of the wrap/floor-corrected ts deltas). Falls back to AUDIT_INTERVAL_SECS when no pair carries
    two timestamps — so a missing HH:MM:SS prefix uses the real measured cadence, never a fabricated
    dt (#266 missing-ts robustness)."""
    deltas: List[float] = []
    for prev, curr in zip(samples, samples[1:]):
        if prev.ts_secs is None or curr.ts_secs is None:
            continue
        diff = curr.ts_secs - prev.ts_secs
        if diff < -MIDNIGHT_WRAP_THRESHOLD_SECS:
            diff += 86400.0
        if diff > 0:
            deltas.append(max(diff, MIN_PAIR_DT))
    return statistics.median(deltas) if deltas else AUDIT_INTERVAL_SECS


def _slice_received_fps(samples: List[AuditSample], cadence: float) -> Optional[float]:
    """Aggregate received-fps over a contiguous slice. None if the slice has a counter reset (don't
    judge a rate across an OBS restart) or carries no usable interval."""
    total_recv = 0
    total_dt = 0.0
    for prev, curr in zip(samples, samples[1:]):
        if curr.received < prev.received:
            return None  # reset inside the slice → can't judge the rate across it
        total_recv += curr.received - prev.received
        total_dt += pair_dt(prev, curr, cadence)
    if total_dt <= 0:
        return None
    return total_recv / total_dt


def _slice_counter_rate(samples: List[AuditSample], attr: str, cadence: float) -> float:
    """Aggregate per-second rate of a cumulative counter over a slice. A reset (negative delta) on a
    pair contributes 0 for that pair (counters only climb; a reset is an OBS restart, not churn)."""
    total = 0
    total_dt = 0.0
    for prev, curr in zip(samples, samples[1:]):
        total += max(0, getattr(curr, attr) - getattr(prev, attr))
        total_dt += pair_dt(prev, curr, cadence)
    if total_dt <= 0:
        return 0.0
    return total / total_dt


def _peak_received_fps(samples: List[AuditSample], cadence: float) -> Optional[float]:
    """The best received-fps across any consecutive pair in the WHOLE window — the evidence the
    source WAS delivering at some point (reset pairs skipped)."""
    best: Optional[float] = None
    for prev, curr in zip(samples, samples[1:]):
        fps = received_fps(prev, curr, cadence)
        if fps is None:
            continue
        best = fps if best is None else max(best, fps)
    return best


def sample_age_secs(newest_ts: Optional[float], now_secs: Optional[float]) -> Optional[float]:
    """Seconds between a source's newest audit sample and `now` (both seconds-of-day), correcting a
    single midnight rollover (newest late yesterday vs now early today → a large negative → +86400).
    None when either timestamp is absent (age unknowable). The caller compares abs(age): a small
    value (positive OR negative clock-skew) is fresh, a large one is stale — abs handles the
    >12 h-but-<24 h ambiguity where the seconds-of-day wrap heuristic cannot."""
    if newest_ts is None or now_secs is None:
        return None
    age = now_secs - newest_ts
    if age < -MIDNIGHT_WRAP_THRESHOLD_SECS:
        age += 86400.0  # newest sample was late yesterday, now is early today
    return age


def is_stale(
    samples: List[AuditSample], now_secs: Optional[float], max_age: float
) -> bool:
    """True when the source's newest audit sample is older than `max_age` vs `now` — i.e. OBS stopped
    logging it (it left the rendered scene), so it is not currently active. False when `now` is not
    supplied (staleness off) or the newest sample has no timestamp (age unknowable → treat as fresh)."""
    if now_secs is None or not samples:
        return False
    age = sample_age_secs(samples[-1].ts_secs, now_secs)
    if age is None:
        return False
    return abs(age) > max_age


def analyze_source(
    samples: List[AuditSample],
    *,
    fps_floor: float = DEFAULT_FPS_FLOOR,
    idle_floor: float = DEFAULT_IDLE_FLOOR,
    underrun_rate: float = DEFAULT_UNDERRUN_RATE,
    depth_floor: float = DEFAULT_DEPTH_FLOOR,
    delivered_frames: int = DEFAULT_DELIVERED_FRAMES,
) -> Optional[SourceStat]:
    """Reduce a source's window of samples to the trajectory + starvation verdicts. None when there
    are < 2 samples (no pair → no rate to judge)."""
    if len(samples) < 2:
        return None
    cadence = estimate_cadence(samples)
    recent = samples[-RECENT_SAMPLES:]
    if len(recent) < 2:
        recent = samples[-2:]

    recent_fps = _slice_received_fps(recent, cadence)
    u_rate = _slice_counter_rate(recent, "underruns", cadence)
    o_rate = _slice_counter_rate(recent, "overruns", cadence)
    depths = [float(s.depth) for s in recent if s.depth is not None]
    recent_depth = statistics.median(depths) if depths else None
    peak_fps = _peak_received_fps(samples, cadence)

    collapsed = recent_fps is not None and recent_fps < fps_floor
    depth_drained = recent_depth is None or recent_depth <= depth_floor
    # STARVED: underruns climbing on a drained FIFO, and NOT overrun-dominant (overruns climbing as
    # fast or faster == the FIFO is overflowing, i.e. holding/parked, NOT starved).
    starved = u_rate > underrun_rate and o_rate < u_rate and depth_drained
    # WAS DELIVERING: the source is a real feed that HAS delivered frames — proven by ANY of:
    #   - currently delivering SOME (recent_fps >= idle_floor: an active but degraded broadcast input),
    #   - it delivered healthily (>= fps_floor) earlier IN the window (the in-window trajectory), OR
    #   - its cumulative `received` is non-trivial (it delivered at some point, even before the window
    #     started — this is what catches a freeze that has lasted LONGER than the window, where the
    #     delivering samples have already scrolled out and peak_fps has decayed to ~0).
    # It separates a frozen-but-real-feed (STUCK) from a never-delivered idle probe (received ~0).
    was_delivering = (
        (recent_fps is not None and recent_fps >= idle_floor)
        or (peak_fps is not None and peak_fps >= fps_floor)
        or samples[-1].received > delivered_frames
    )
    return SourceStat(
        source=samples[-1].source,
        n_samples=len(samples),
        recent_fps=recent_fps,
        peak_fps=peak_fps,
        underrun_rate=u_rate,
        overrun_rate=o_rate,
        recent_depth=recent_depth,
        collapsed=collapsed,
        starved=starved,
        was_delivering=was_delivering,
    )


def _fmt(x: Optional[float], suffix: str = "") -> str:
    return f"{x:.1f}{suffix}" if x is not None else "?"


def evaluate(
    box: str,
    window: Dict[str, List[AuditSample]],
    dantesync_cpu: Optional[float],
    *,
    monitored_sources: Optional[List[str]] = None,
    fps_floor: float = DEFAULT_FPS_FLOOR,
    idle_floor: float = DEFAULT_IDLE_FLOOR,
    underrun_rate: float = DEFAULT_UNDERRUN_RATE,
    depth_floor: float = DEFAULT_DEPTH_FLOOR,
    delivered_frames: int = DEFAULT_DELIVERED_FRAMES,
    dantesync_core_pct: float = DEFAULT_DANTESYNC_CORE_PCT,
    now_secs: Optional[float] = None,
    max_sample_age: float = DEFAULT_MAX_SAMPLE_AGE_SECS,
) -> List[Alert]:
    """Apply the stuck-state classification. One Alert per tripped condition (empty == healthy).

    - default (scan-all) mode: a source alerts as `stuck` ONLY on the unambiguous starved-collapse
      signature WITH a delivering history (so a frozen real feed alerts — even past the window — but a
      never-delivered probe / an overrun-dominant parked source stays quiet).
    - a NAMED/required broadcast input is held to a stricter bar: it alerts whenever its receive
      drops below the fps floor at all (a required cam at ~0 fps is bad whether or not it is the
      classic starved signature), and if it produced no usable audit pair OR went stale it alerts as
      `vanished`.
    - STALENESS: when `now_secs` is supplied, a source whose newest sample is older than
      `max_sample_age` stopped logging (left program) → it is SKIPPED in scan-all (not stuck, just not
      in use), and counts as `vanished` for a NAMED input. (now_secs None → staleness off.)
    - dantesync runaway is checked INDEPENDENTLY of any NDI audit data."""
    alerts: List[Alert] = []
    named = set(monitored_sources or [])

    for source in sorted(window):
        if is_stale(window[source], now_secs, max_sample_age):
            continue  # stopped logging → off-program → not currently active (named → vanished below)
        stat = analyze_source(
            window[source],
            fps_floor=fps_floor,
            idle_floor=idle_floor,
            underrun_rate=underrun_rate,
            depth_floor=depth_floor,
            delivered_frames=delivered_frames,
        )
        if stat is None:
            continue  # < 2 samples; a named one is caught by the vanished pass below
        is_named = source in named
        if is_named:
            trip = stat.recent_fps is not None and stat.recent_fps < fps_floor
        else:
            trip = stat.collapsed and stat.starved and stat.was_delivering
        if trip:
            alerts.append(
                Alert(
                    box=box,
                    kind="stuck",
                    detail=(
                        f"NDI príjem '{source}' = {_fmt(stat.recent_fps, ' fps')} (norma 30); "
                        f"underruns +{stat.underrun_rate:.0f}/s, depth~{_fmt(stat.recent_depth)} "
                        f"— genlock hladuje (#265)"
                    ),
                )
            )

    # A DECLARED (named/required) input that produced no usable audit pair, OR whose logging went
    # stale (NDI dropped / the fifo stopped / it left program), has VANISHED — a required input we can
    # no longer confirm is alive. It MUST still alert, never silently disappear.
    for source in sorted(named):
        samples = window.get(source, [])
        if len(samples) < 2 or is_stale(samples, now_secs, max_sample_age):
            alerts.append(
                Alert(
                    box=box,
                    kind="vanished",
                    detail=f"deklarovaný vstup '{source}' zmizol — žiadne čerstvé NDI audit dáta",
                )
            )

    # dantesync runaway — INDEPENDENT of the NDI audit (alerts even with no audit data at all).
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
    """Fire the Discord alert via `airuleset notify` (or just print it on --dry-run).

    Returns True when the alert was delivered (or printed on --dry-run), False when delivery FAILED —
    airuleset notify exited non-zero (broken Discord webhook / missing env / network), or could not be
    spawned at all. main() turns a False here into a DISTINCT loud exit code (3) so an un-delivered
    stuck-state is itself surfaced (script-failure-policy / fail-loudly), never read as "handled"."""
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


def read_recent_lines(path: str, max_bytes: int = 4_000_000) -> List[str]:
    """Read only the TAIL of the (possibly multi-GB) OBS log.

    Sized generously so the per-source WINDOW is never clipped: at the ~5 s audit cadence, ~4 MB of
    OBS log holds many minutes — far more than DEFAULT_WINDOW samples for every active source — so a
    healthy still-logging source is never false-"vanished" by a too-small tail (#266 robust-tail). A
    source whose lines genuinely fell off the tail stopped logging → it has vanished, and a NAMED one
    alerts via the vanished check."""
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
        help="path to the box's OBS log (read-only; fetched via win-* MCP or SMB). Optional — omit "
        "to run a dantesync-only check.",
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
    ap.add_argument("--window", type=int, default=DEFAULT_WINDOW, help="audit samples per source")
    ap.add_argument("--fps-floor", type=float, default=DEFAULT_FPS_FLOOR)
    ap.add_argument("--idle-floor", type=float, default=DEFAULT_IDLE_FLOOR)
    ap.add_argument("--underrun-rate", type=float, default=DEFAULT_UNDERRUN_RATE)
    ap.add_argument("--depth-floor", type=float, default=DEFAULT_DEPTH_FLOOR)
    ap.add_argument("--delivered-frames", type=int, default=DEFAULT_DELIVERED_FRAMES)
    ap.add_argument("--dantesync-core-pct", type=float, default=DEFAULT_DANTESYNC_CORE_PCT)
    ap.add_argument(
        "--now",
        help="the box's current time HH:MM:SS for staleness (poller reads it via the same MCP as the "
        "log). Default = this host's local clock; pass the BOX clock if it differs (timezone).",
    )
    ap.add_argument(
        "--max-sample-age",
        type=float,
        default=DEFAULT_MAX_SAMPLE_AGE_SECS,
        help="a source whose newest audit sample is older than this (vs --now) is no longer rendered "
        "→ skipped (named → vanished). Set 0 to disable the staleness filter.",
    )
    ap.add_argument(
        "--airuleset",
        default="~/devel/airuleset/airuleset.py",
        help="path to airuleset.py for the Discord alert",
    )
    ap.add_argument("--dry-run", action="store_true", help="print the alert instead of sending it")
    args = ap.parse_args(argv)

    # Resolve `now` (seconds-of-day) for the staleness filter: explicit --now, else this host's clock.
    # --max-sample-age 0 disables staleness (now_secs=None).
    now_secs: Optional[float] = None
    if args.max_sample_age > 0:
        if args.now:
            nm = re.fullmatch(r"(\d{1,2}):(\d{2}):(\d{2})(?:\.\d+)?", args.now.strip())
            if not nm:
                ap.error(f"--now must be HH:MM:SS, got {args.now!r}")
            now_secs = int(nm.group(1)) * 3600 + int(nm.group(2)) * 60 + int(nm.group(3))
        else:
            n = datetime.datetime.now()
            now_secs = n.hour * 3600 + n.minute * 60 + n.second + n.microsecond / 1e6

    lines: List[str] = []
    if args.obs_log:
        try:
            lines = read_recent_lines(args.obs_log)
        except OSError as e:
            # A read failure must NOT suppress the dantesync check (#266) — log loudly, keep going.
            print(f"ERROR: cannot read OBS log {args.obs_log}: {e}", file=sys.stderr)

    window = window_per_source(lines, args.window)
    audit_present = any(len(s) >= 2 for s in window.values())
    dantesync_present = args.dantesync_cpu is not None

    # Resolved thresholds echoed on EVERY outcome so an OK/ALERT line is self-explaining
    # (comprehensive-logging).
    thresholds = (
        f"window={args.window} fps_floor={args.fps_floor} idle_floor={args.idle_floor} "
        f"underrun_rate={args.underrun_rate}/s depth_floor={args.depth_floor} "
        f"delivered_frames={args.delivered_frames} dantesync_core_pct={args.dantesync_core_pct} "
        f"max_sample_age={args.max_sample_age}s now={'auto' if now_secs is not None and not args.now else (args.now or 'off')}"
    )

    # Exit 2 ONLY when there is NO signal of ANY kind: no usable audit data AND no declared source
    # AND no dantesync reading. A dantesync reading (even healthy) is a signal → evaluate it.
    if not audit_present and not args.source and not dantesync_present:
        print(
            f"ERROR [{args.box}]: no signal to judge — no genlock-fifo audit data "
            f"(>=2 lines/source){' in ' + args.obs_log if args.obs_log else ''}, no --source, "
            f"no --dantesync-cpu [{thresholds}]",
            file=sys.stderr,
        )
        return 2

    alerts = evaluate(
        args.box,
        window,
        args.dantesync_cpu,
        monitored_sources=args.source,
        fps_floor=args.fps_floor,
        idle_floor=args.idle_floor,
        underrun_rate=args.underrun_rate,
        depth_floor=args.depth_floor,
        delivered_frames=args.delivered_frames,
        dantesync_core_pct=args.dantesync_core_pct,
        now_secs=now_secs,
        max_sample_age=args.max_sample_age,
    )

    if not alerts:
        # Per-source diagnostic on a quiet OK run so the full audit picture is still recorded for
        # later debugging (comprehensive-logging). Stale (off-program) sources are annotated, not
        # judged — they are why the run is OK despite a frozen tail in the log.
        diags = []
        for source in sorted(window):
            if is_stale(window[source], now_secs, args.max_sample_age):
                age = sample_age_secs(window[source][-1].ts_secs, now_secs)
                diags.append(f"{source}=stale({age:.0f}s, off-program)" if age is not None else f"{source}=stale")
                continue
            stat = analyze_source(
                window[source],
                fps_floor=args.fps_floor,
                idle_floor=args.idle_floor,
                underrun_rate=args.underrun_rate,
                depth_floor=args.depth_floor,
                delivered_frames=args.delivered_frames,
            )
            if stat is None:
                diags.append(f"{source}=<{len(window[source])} sample>")
                continue
            diags.append(
                f"{source}={_fmt(stat.recent_fps, 'fps')}"
                f"(peak={_fmt(stat.peak_fps, 'fps')} underruns+{stat.underrun_rate:.0f}/s "
                f"overruns+{stat.overrun_rate:.0f}/s depth~{_fmt(stat.recent_depth)})"
            )
        dante = f", dantesync={args.dantesync_cpu:.0f}%" if dantesync_present else ""
        srcs = ", ".join(diags) if diags else "no source samples"
        print(f"OK [{args.box}]: no stuck state — {srcs}{dante} [{thresholds}]")
        return 0

    body = compose_alert_body(args.box, args.host, alerts)
    delivered = send_alert(body, args.airuleset, args.dry_run)
    print(
        f"ALERT [{args.box}]: {len(alerts)} condition(s) tripped: "
        f"{', '.join(a.kind for a in alerts)} [{thresholds}]",
        file=sys.stderr,
    )
    if not delivered:
        print(
            f"ERROR [{args.box}]: alert could NOT be delivered — stuck state went UNREPORTED",
            file=sys.stderr,
        )
        return 3
    return 1


if __name__ == "__main__":
    sys.exit(main())
