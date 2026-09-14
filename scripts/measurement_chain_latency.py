#!/usr/bin/env python3
"""#1312 — PURE measurement-chain-latency kernel for the "development" handover check's 14th item
(`avlatency`: meracia zvuková cesta — latencia oproti baseline).

WHY: after a production the mbc measurement-audio chain (cam2 painter QPSK marker → HDMI speaker →
measurement mic → mbc Ableton on 10.77.7.232 → Dante → stream OBS ASIO input `mbc`) can pick up a DC
LATENCY step and NOTHING between productions catches it — the #748 preflight only runs IN a full
~300 s E2E, and the #1310 watchdog only proves the marker is AUDIBLE, not ALIGNED. The one
always-available latency-ish signal, the stream dock `av_offset_recent_med_ms`, is PIN-RELATIVE (it
read +17 ms while the recording gate read −140 ms on 14.9.), so it can NOT be trusted as absolute.
This module is the pure kernel of a read-only PAIRED measurement that IS absolute: the cam2 marker
log's emit timestamps vs the stream `mbc` audio burst ONSETS (off the OBS-WS `InputVolumeMeters`
peak), median per-marker latency vs a persisted baseline, |now − baseline| > 90 ms → drift.

No I/O, no WS, no ssh — exhaustively unit-testable (pytest, Tier-0 #557 kills local cargo), the
`measurement_audio_decision` / `dantesync_clock_decision` python-mirror precedent. The thin I/O half is
`scripts/measurement_chain_latency_probe.py` (the WS sampler) + `scripts/measurement-chain-latency.sh`
(the standalone orchestrator: ssh marker read, run the sampler, call this CLI, `--baseline` write).

CLOCK — the #1312 STEP-0 finding, guarded here: the ticket assumed `emit_ts_ns` is on the
DanteSync-disciplined WALL clock, but the PERMANENT cam2 painter (`setup-device.sh`, no `--wall-clock`)
emits `start.elapsed()` MONOTONIC-since-painter-start ns. That is NOT comparable to the dev1 onset wall
clock, and it resets on every painter restart (which happens on every EVENT→TEST switch — exactly when
this check runs). So `measure` GUARDS on the emit-ts SHAPE (a wall-clock ns is ~1.7e18; a monotonic
elapsed value is orders smaller) and never pairs / never drifts when the emits are monotonic — the item
reads UNKNOWN (`neoverené`, reason `monotonic-emit`), never a false forgot. It goes green-capable the
moment the painter is switched to `--wall-clock` (a SAFE no-op for the A/V verdict path, which pairs by
index→frame_id and ignores `emit_ts`; surfaced as a supervisor follow-up).

THRESHOLD REUSE: the −60 dB burst-onset bar is the SAME `audio_preflight_default_threshold_db` (#748);
the orchestrator sources `scripts/lib/audio-presence-preflight.sh` and passes it in, so the literal is
NEVER retyped here (the CLI takes `--threshold-db` as REQUIRED). Onset uses `>=` — an inversion of
`audio_preflight_is_silent`'s strict `<` (exactly at the bar is PRESENT/onset, not silent).
"""
import argparse
import bisect
import json
import os
import sys

# --- verdict tokens (the handover `avlatency` item maps ALIGNED→OK, DRIFTED→FORGOT, else UNKNOWN) --
ALIGNED = "ALIGNED"
DRIFTED = "DRIFTED"
UNKNOWN = "UNKNOWN"
SKIP = "SKIP"
NO_BASELINE = "NO-BASELINE"
BASELINE_WRITTEN = "BASELINE-WRITTEN"

# tolerance = the E2E's own ±90 ms A/V gate; cadence-derived pairing window; onset floor.
DEFAULT_TOLERANCE_MS = 90.0
# half the ~5 s permanent-unit marker cadence (300 ticks @ 60 Hz) with margin — an onset pairs only to
# an emit within this window, so the pairing is unambiguous and any real chain latency (well under 2 s)
# still pairs. Configurable via --max-pair-ms.
DEFAULT_MAX_PAIR_MS = 2000.0
DEFAULT_MIN_PAIRED = 3  # < 3 paired markers → UNKNOWN (too few onsets to trust the median)
# a UNIX-epoch ns is ~1.7e18; a monotonic elapsed-since-start value is orders smaller (a painter up a
# full year ≈ 3.15e16). 1e18 ns ≈ 2001-09 — a clean floor separating wall-clock from monotonic emit_ts.
WALL_CLOCK_FLOOR_NS = 1_000_000_000_000_000_000


# --- pure parsers -------------------------------------------------------------------------------
def parse_marker_csv(text):
    """The emit timestamps (ns) from the cam2 marker log `index,frame_id,emit_ts_ns` (only the 3rd
    field matters here). Skips the `#`-params comment, the `index...` header, and any blank/malformed
    line — robust to a truncated tmpfs read (mirrors `qpsk_marker::parse_qpsk_marker_log`)."""
    out = []
    for line in (text or "").splitlines():
        s = line.strip()
        if not s or s.startswith("#") or s.startswith("index"):
            continue
        parts = s.split(",")
        if len(parts) < 3:
            continue
        try:
            out.append(int(parts[2].strip()))
        except (ValueError, TypeError):
            continue  # a malformed row is dropped, never a fabricated 0 (would be a false onset match)
    return out


def parse_meter_samples(text):
    """`(t_ns, db)` pairs from the WS sampler's `sample <t_ns> <db>` rows (the stream `mbc` peak in
    dBFS, stamped on dev1's wall clock at receipt). Non-sample lines and malformed rows are dropped."""
    out = []
    for line in (text or "").splitlines():
        s = line.strip()
        if not s.startswith("sample"):
            continue
        parts = s.split()
        if len(parts) < 3:
            continue
        try:
            out.append((int(parts[1]), float(parts[2])))
        except (ValueError, TypeError):
            continue
    return out


def looks_like_wall_clock_ns(ts):
    """True iff `ts` is a plausible UNIX-epoch ns (wall clock), False for a monotonic elapsed value."""
    return ts > WALL_CLOCK_FLOOR_NS


def emits_are_wall_clock(emits):
    """True iff there is at least one emit and EVERY emit looks wall-clock — the #1312 guard so a
    monotonic-emit painter (today's permanent unit) never produces a paired latency."""
    return bool(emits) and all(looks_like_wall_clock_ns(e) for e in emits)


# --- pure signal processing ---------------------------------------------------------------------
def detect_onsets(samples, threshold_db):
    """Burst ONSET times: a RISING edge where the `mbc` peak crosses from below the bar to at/above it.
    Starts DISarmed so a window that opens mid-burst never counts a partial first burst — the detector
    arms on the first below-bar sample, then the next at/above sample is an onset (and re-disarms until
    it drops below again). `>=` matches the −60 dB silence bar's exactly-at-bar-is-present convention."""
    onsets = []
    armed = False
    for t, db in samples:
        if db is None:
            continue
        if db >= threshold_db:
            if armed:
                onsets.append(t)
                armed = False
        else:
            armed = True
    return onsets


def pair_latencies(emit_ts_list, onset_ts_list, max_pair_ns):
    """Per-marker latency (ns): each onset pairs with the LATEST emit at or before it, provided the gap
    is within `max_pair_ns` (< half the cadence, so the pairing is unambiguous). Tolerant of a missed
    marker (an emit with no onset, or an onset with no prior emit, simply contributes nothing)."""
    emits = sorted(emit_ts_list)
    lat = []
    for onset in sorted(onset_ts_list):
        i = bisect.bisect_right(emits, onset) - 1  # latest emit <= onset
        if i >= 0:
            d = onset - emits[i]
            if 0 <= d <= max_pair_ns:
                lat.append(d)
    return lat


def median_ms(latencies_ns):
    """Median of the per-marker latencies in ms, or None when there are none."""
    if not latencies_ns:
        return None
    vals = sorted(latencies_ns)
    n = len(vals)
    mid = n // 2
    med = vals[mid] if n % 2 else (vals[mid - 1] + vals[mid]) / 2.0
    return med / 1e6


def measure(marker_text, meter_text, threshold_db, max_pair_ns):
    """Parse both captures, detect onsets, pair, take the median. Returns
    `{markers, onsets, paired, latency_ms, wall_clock_ok}`. Never pairs when the emits are not
    wall-clock (the #1312 monotonic-emit guard) — `latency_ms` stays None, `paired` 0."""
    emits = parse_marker_csv(marker_text)
    samples = parse_meter_samples(meter_text)
    wall_ok = emits_are_wall_clock(emits)
    onsets = detect_onsets(samples, threshold_db)
    lat = pair_latencies(emits, onsets, max_pair_ns) if wall_ok else []
    return {
        "markers": len(emits),
        "onsets": len(onsets),
        "paired": len(lat),
        "latency_ms": median_ms(lat),
        "wall_clock_ok": wall_ok,
    }


# --- the decision table -------------------------------------------------------------------------
def classify(latency_ms, baseline_ms, paired, box_reachable, markers, wall_clock_ok,
             tolerance_ms, min_paired):
    """One check's verdict token.

      box_reachable != 1        -> SKIP        (stream OBS down — a #1001/#732 page, never ours)
      markers <= 0              -> UNKNOWN     (no marker rows: cam2 down / the marker log is missing)
      not wall_clock_ok         -> UNKNOWN     (monotonic emit_ts — painter needs --wall-clock; #1312)
      baseline_ms is None       -> NO-BASELINE (not seeded yet -> handover UNKNOWN)
      paired < min_paired       -> UNKNOWN     (too few onsets to trust the median)
      latency_ms is None        -> UNKNOWN     (no median)
      |latency − baseline| > tol -> DRIFTED    (the chain latency stepped out of the ±90 ms gate)
      otherwise                 -> ALIGNED
    """
    if box_reachable != 1:
        return SKIP
    if markers <= 0:
        return UNKNOWN
    if not wall_clock_ok:
        return UNKNOWN
    if baseline_ms is None:
        return NO_BASELINE
    if paired < min_paired:
        return UNKNOWN
    if latency_ms is None:
        return UNKNOWN
    if abs(latency_ms - baseline_ms) > tolerance_ms:
        return DRIFTED
    return ALIGNED


def reason(verdict, markers, wall_clock_ok, paired, min_paired):
    """A short single-token reason for the CLI/log line (never contains a `verdict=` substring)."""
    if verdict == SKIP:
        return "stream-obs-unreachable"
    if verdict == UNKNOWN:
        if markers <= 0:
            return "no-markers"
        if not wall_clock_ok:
            return "monotonic-emit"
        if paired < min_paired:
            return "too-few-onsets"
        return "no-median"
    if verdict == NO_BASELINE:
        return "no-baseline"
    if verdict == DRIFTED:
        return "latency-step"
    if verdict == BASELINE_WRITTEN:
        return "baseline-written"
    return "aligned"


# --- baseline persistence -----------------------------------------------------------------------
def read_baseline(path):
    """The persisted baseline latency (ms) from `path`'s `{"baseline_ms": X}`, or None (missing /
    corrupt / non-numeric — never a fabricated number)."""
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return None
    val = data.get("baseline_ms") if isinstance(data, dict) else None
    try:
        return None if val is None else float(val)
    except (ValueError, TypeError):
        return None


def write_baseline(path, ms):
    """Persist `ms` as the baseline latency (supervisor runs `--baseline` right after a green E2E)."""
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump({"baseline_ms": ms, "written_by": "measurement-chain-latency.sh --baseline"}, fh)


# --- CLI ----------------------------------------------------------------------------------------
def _read_file(path):
    if not path:
        return ""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure measurement-chain-latency decision (#1312)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("classify",
                       help="measure marker-file vs meter-file and decide vs baseline (or --write-baseline)")
    c.add_argument("--marker-file", default="")
    c.add_argument("--meter-file", default="")
    c.add_argument("--box-reachable", type=int, required=True)
    # REQUIRED — no hardcoded -60 default; production sources audio-presence-preflight.sh and passes
    # audio_preflight_default_threshold_db so the #748 bar is single-source (never retyped here).
    c.add_argument("--threshold-db", type=float, required=True)
    c.add_argument("--max-pair-ms", type=float, default=DEFAULT_MAX_PAIR_MS)
    c.add_argument("--baseline-file", default="")
    c.add_argument("--tolerance-ms", type=float, default=DEFAULT_TOLERANCE_MS)
    c.add_argument("--min-paired", type=int, default=DEFAULT_MIN_PAIRED)
    c.add_argument("--write-baseline", action="store_true",
                   help="persist the measured latency as the new baseline (supervisor, after a green E2E)")

    ns = ap.parse_args(argv)
    if ns.cmd != "classify":
        return 2

    reachable = ns.box_reachable
    max_pair_ns = int(ns.max_pair_ms * 1e6)
    if reachable != 1:
        # a dead stream box needs no capture parsing — SKIP straight away (#1001/#732 territory).
        res = {"markers": 0, "onsets": 0, "paired": 0, "latency_ms": None, "wall_clock_ok": False}
    else:
        res = measure(_read_file(ns.marker_file), _read_file(ns.meter_file),
                      ns.threshold_db, max_pair_ns)

    baseline = read_baseline(ns.baseline_file) if ns.baseline_file else None
    verdict = classify(res["latency_ms"], baseline, res["paired"], reachable,
                       res["markers"], res["wall_clock_ok"], ns.tolerance_ms, ns.min_paired)

    if ns.write_baseline and reachable == 1 and res["wall_clock_ok"] \
            and res["paired"] >= ns.min_paired and res["latency_ms"] is not None:
        write_baseline(ns.baseline_file, res["latency_ms"])
        baseline = res["latency_ms"]
        verdict = BASELINE_WRITTEN

    delta = None
    if res["latency_ms"] is not None and baseline is not None:
        delta = abs(res["latency_ms"] - baseline)

    out = {
        "box_reachable": reachable,
        "markers": res["markers"],
        "onsets": res["onsets"],
        "paired": res["paired"],
        "latency_ms": res["latency_ms"],
        "baseline_ms": baseline,
        "tolerance_ms": ns.tolerance_ms,
        "delta_ms": delta,
        "reason": reason(verdict, res["markers"], res["wall_clock_ok"], res["paired"], ns.min_paired),
        "verdict": verdict,
    }
    for k in ("box_reachable", "markers", "onsets", "paired", "latency_ms", "baseline_ms",
              "tolerance_ms", "delta_ms", "reason", "verdict"):
        print(f"{k}={_fmt(out[k])}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
