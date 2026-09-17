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

ONSET DETECTION (#1312): the −60 dB bar (`audio_preflight_default_threshold_db`, #748; sourced from
`scripts/lib/audio-presence-preflight.sh` by the orchestrator and passed in as REQUIRED `--threshold-db`,
never retyped here) is the SILENT-CHAIN guard ONLY — `chain_is_silent` reads chain-silent (UNKNOWN)
when every sample is below it. It is NOT the onset criterion: on the rig the `mbc` peak sits
continuously at ~−44 dB (room/PA floor through the measurement mic), always above −60, so an absolute
onset bar never armed (`markers=145 onsets=0` on a working chain — the #1312 defect). Onsets are
instead detected RELATIVE to a rolling floor (`detect_onsets`: a burst rises ≥ `ONSET_DELTA_DB` above
the trailing-window median, re-arming with hysteresis), calibrated from a live capture.
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
# --- onset detection (#1312: RELATIVE, not the absolute −60 bar) ---------------------------------
# the rig room/PA floor through the measurement mic sits CONTINUOUSLY at ~−44.5 dB (a 45 s live
# capture: p99 rise 3.8 dB above a trailing-20 median; the ~5 s cadence QPSK bursts rise +7…+25 dB).
# The absolute −60 dB bar never armed on that floor (markers=145 onsets=0 on a working chain), so
# onsets are detected RELATIVE to a rolling floor. The −60 bar is kept only as the SILENT-CHAIN guard.
ROLLING_FLOOR_WINDOW = 20   # trailing samples for the rolling floor ≈ 1 s at the ~50 ms WS cadence
# a burst must rise this far above the rolling floor to be an onset. Calibrated from the live capture:
# noise stays ≤ 3.8 dB above the floor (p99), the weakest real burst rises +7.0 dB → 6 dB sits 2.2 dB
# above the noise and catches every real burst. Re-arm hysteresis is ONSET_DELTA_DB / 2.
ONSET_DELTA_DB = 6.0


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
def _median_float(vals):
    """Median of a non-empty list of floats (a small local helper — `median_ms` is ns→ms scaled)."""
    xs = sorted(vals)
    n = len(xs)
    mid = n // 2
    return xs[mid] if n % 2 else (xs[mid - 1] + xs[mid]) / 2.0


def chain_is_silent(samples, threshold_db):
    """True iff there are samples and the LOUDEST one never reached the absolute bar — the mbc chain
    is silent (a #1310-class dead-audio problem, not a latency drift). An EMPTY capture is NOT "silent"
    (it is a different UNKNOWN — no meter data), so this returns False for no samples."""
    dbs = [db for _, db in samples if db is not None]
    return bool(dbs) and max(dbs) < threshold_db


def detect_onsets(samples, floor_window=ROLLING_FLOOR_WINDOW, delta_db=ONSET_DELTA_DB):
    """Burst ONSET times, detected RELATIVE to a rolling floor (#1312). The rolling floor is the median
    of the trailing `floor_window` PRIOR samples; an onset fires when a sample rises ≥ `delta_db` above
    that floor while armed, then re-arms (hysteresis) once the level falls back within `delta_db`/2 of
    the floor. Starts DISarmed and arms on the first at-floor sample, so a window opening mid-burst
    never counts a partial first burst, and a single loud outlier in the window never moves the median.
    This replaces the old absolute −60 dB onset bar, which never armed on the rig's ~−44 dB room floor;
    the −60 bar is now only `chain_is_silent`'s guard."""
    seq = [(t, db) for t, db in samples if db is not None]
    vals = [db for _, db in seq]
    onsets = []
    armed = False
    rearm = delta_db / 2.0
    for i, (t, db) in enumerate(seq):
        window = vals[max(0, i - floor_window):i]
        floor = _median_float(window) if window else db
        rise = db - floor
        if rise >= delta_db:
            if armed:
                onsets.append(t)
                armed = False
        elif rise <= rearm:
            armed = True
    return onsets


def emit_cadence_ns(emit_ts_list):
    """Median inter-emit gap (ns), or None for fewer than two emits — the marker cadence derived from
    the log ITSELF (#1332), never a hardcoded constant. Tells an unambiguous ~5 s cadence (300 painter
    ticks) apart from the ~0.5 s cadence (30 ticks, live since issue 1318) that puts several candidate
    emits inside the pairing window."""
    emits = sorted(emit_ts_list)
    if len(emits) < 2:
        return None
    gaps = [emits[i + 1] - emits[i] for i in range(len(emits) - 1)]
    return _median_float(gaps)


def pairing_is_ambiguous(cadence_ns, max_pair_ns):
    """True iff the emit cadence is dense enough that more than one candidate emit can fall inside the
    `[onset − max_pair_ns, onset]` window — i.e. `cadence_ns < max_pair_ns` (#1332). With such a cadence
    the "latest prior emit" pairing aliases the real latency (1132 → 132 ms at a 0.5 s cadence, 2 s
    window), so a prior is required to disambiguate. A None cadence (< 2 emits) is never ambiguous."""
    return cadence_ns is not None and cadence_ns < max_pair_ns


def pair_latencies(emit_ts_list, onset_ts_list, max_pair_ns, prior_ns=None):
    """Per-marker latency (ns). The candidates for each onset are every emit in
    `[onset − max_pair_ns, onset]`. With `prior_ns` (the baseline as a prior, #1332) the candidate whose
    latency is CLOSEST to the prior is chosen — this resolves the 0.5 s-cadence alias. Without a prior
    the LATEST prior emit is chosen (the legacy behaviour, correct for an unambiguous ≥-window cadence).
    Tolerant of a missed marker (an emit with no onset, or an onset with no prior emit in window,
    simply contributes nothing)."""
    emits = sorted(emit_ts_list)
    lat = []
    for onset in sorted(onset_ts_list):
        hi = bisect.bisect_right(emits, onset) - 1  # latest emit <= onset
        if hi < 0:
            continue
        if prior_ns is None:
            d = onset - emits[hi]
            if 0 <= d <= max_pair_ns:
                lat.append(d)
            continue
        lo = bisect.bisect_left(emits, onset - max_pair_ns)  # first emit >= onset − window
        best = None
        for i in range(lo, hi + 1):
            d = onset - emits[i]
            if 0 <= d <= max_pair_ns and (best is None or abs(d - prior_ns) < abs(best - prior_ns)):
                best = d
        if best is not None:
            lat.append(best)
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


def measure(marker_text, meter_text, threshold_db, max_pair_ns, prior_ns=None):
    """Parse both captures, detect onsets, pair, take the median. Returns
    `{markers, onsets, paired, latency_ms, wall_clock_ok, chain_silent, ambiguous}`. Never pairs when
    the emits are not wall-clock (the #1312 monotonic-emit guard) — `latency_ms` stays None, `paired` 0.
    When the emit cadence is ambiguous (< the pairing window) and no `prior_ns` is available to
    disambiguate (#1332), does NOT pair and surfaces `ambiguous=True` — the caller reads UNKNOWN reason
    `ambiguous-cadence`, never a false drift. A `prior_ns` (the baseline) resolves the ambiguity and
    pairing proceeds via the closest-to-prior candidate."""
    emits = parse_marker_csv(marker_text)
    samples = parse_meter_samples(meter_text)
    wall_ok = emits_are_wall_clock(emits)
    silent = chain_is_silent(samples, threshold_db)
    # a silent chain has no real bursts to detect — force 0 onsets so the reason is chain-silent, not a
    # spurious relative onset off the noise floor.
    onsets = [] if silent else detect_onsets(samples)
    cadence = emit_cadence_ns(emits)
    # #1332: ambiguity only matters when we WOULD pair (wall-clock emits, a non-silent chain with real
    # onsets) and have no prior to pick the right alias. A dead/too-quiet chain keeps its own reason.
    ambiguous = (prior_ns is None and wall_ok and not silent and len(onsets) > 0
                 and pairing_is_ambiguous(cadence, max_pair_ns))
    lat = pair_latencies(emits, onsets, max_pair_ns, prior_ns=prior_ns) \
        if (wall_ok and not ambiguous) else []
    return {
        "markers": len(emits),
        "onsets": len(onsets),
        "paired": len(lat),
        "latency_ms": median_ms(lat),
        "wall_clock_ok": wall_ok,
        "chain_silent": silent,
        "ambiguous": ambiguous,
    }


# --- the decision table -------------------------------------------------------------------------
def classify(latency_ms, baseline_ms, paired, box_reachable, markers, wall_clock_ok,
             tolerance_ms, min_paired, ambiguous=False):
    """One check's verdict token.

      box_reachable != 1        -> SKIP        (stream OBS down — a #1001/#732 page, never ours)
      markers <= 0              -> UNKNOWN     (no marker rows: cam2 down / the marker log is missing)
      not wall_clock_ok         -> UNKNOWN     (monotonic emit_ts — painter needs --wall-clock; #1312)
      ambiguous                 -> UNKNOWN     (0.5 s cadence, no prior to disambiguate; #1332 —
                                                NEVER a false DRIFTED, and beats NO-BASELINE)
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
    if ambiguous:
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


def reason(verdict, markers, wall_clock_ok, paired, min_paired, chain_silent=False, ambiguous=False):
    """A short single-token reason for the CLI/log line (never contains a `verdict=` substring)."""
    if verdict == SKIP:
        return "stream-obs-unreachable"
    if verdict == UNKNOWN:
        if markers <= 0:
            return "no-markers"
        if not wall_clock_ok:
            return "monotonic-emit"
        if chain_silent:
            return "chain-silent"       # #1312: every mbc sample below the −60 bar (dead audio chain)
        if ambiguous:
            return "ambiguous-cadence"  # #1332: dense marker cadence, no prior to pick the right alias
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
    ap = argparse.ArgumentParser(
        description="pure measurement-chain-latency decision (#1312, alias-aware pairing #1332)")
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
    # #1332: the pairing PRIOR (ms). An explicit --expected-ms wins over the persisted baseline; it is
    # REQUIRED to seed a baseline on an ambiguous (dense) marker cadence, else --write-baseline refuses.
    c.add_argument("--expected-ms", type=float, default=None,
                   help="known chain latency (ms) used as the pairing prior to resolve a 0.5 s-cadence "
                        "alias; required with --write-baseline on an ambiguous cadence (#1332)")
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
    # #1332: the pairing prior — an explicit --expected-ms wins (seeding), else the persisted baseline.
    # Read the baseline BEFORE measure so it can flow into pairing (this was measure-then-read before).
    baseline = read_baseline(ns.baseline_file) if ns.baseline_file else None
    prior_ms = ns.expected_ms if ns.expected_ms is not None else baseline
    prior_ns = int(prior_ms * 1e6) if prior_ms is not None else None

    if reachable != 1:
        # a dead stream box needs no capture parsing — SKIP straight away (#1001/#732 territory).
        res = {"markers": 0, "onsets": 0, "paired": 0, "latency_ms": None, "wall_clock_ok": False,
               "chain_silent": False, "ambiguous": False}
    else:
        res = measure(_read_file(ns.marker_file), _read_file(ns.meter_file),
                      ns.threshold_db, max_pair_ns, prior_ns=prior_ns)

    # #1332: --write-baseline must NEVER persist an aliased latency. On an ambiguous (dense) cadence
    # with no prior (no baseline, no --expected-ms) FAIL LOUD (non-zero) rather than seed garbage.
    if ns.write_baseline and reachable == 1 and res["ambiguous"]:
        print("measurement-chain-latency: --write-baseline refused: the marker cadence is ambiguous "
              "(emit spacing < the pair window) and no prior was available to disambiguate. Pass "
              "--expected-ms <ms> (the known chain latency) so the correct alias is selected; "
              "otherwise an aliased latency would be persisted as the baseline.", file=sys.stderr)
        return 3

    verdict = classify(res["latency_ms"], baseline, res["paired"], reachable,
                       res["markers"], res["wall_clock_ok"], ns.tolerance_ms, ns.min_paired,
                       ambiguous=res["ambiguous"])

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
        "reason": reason(verdict, res["markers"], res["wall_clock_ok"], res["paired"], ns.min_paired,
                         chain_silent=res["chain_silent"], ambiguous=res["ambiguous"]),
        "verdict": verdict,
    }
    for k in ("box_reachable", "markers", "onsets", "paired", "latency_ms", "baseline_ms",
              "tolerance_ms", "delta_ms", "reason", "verdict"):
        print(f"{k}={_fmt(out[k])}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
