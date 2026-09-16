#!/usr/bin/env python3
"""issue 1242 (task 1) -- DATA-FIRST attribution of the ~0.06% residual per-segment copy/gap churn.

WHAT: run over a set of FINISHED E2E run directories (`/tmp/recording-e2e-<RUN_ID>/`) and, for
EVERY residual copy/gap event in the fused verdict, align its `wall_clock_epoch_s` to that
cambox's OWN burn log and classify the SOURCE side in a +-window second window -- so the question
"does the residual churn belong to a per-GRABBER capture-cadence deficit, or to a DOWNSTREAM
(genlock-FIFO / 60->30 decimation-phase / optical-beat) survival phenomenon" is answered from data,
not asserted.

WHY the source-side burn log is decisive here: the strih genlock-FIFO time-series (`received=` /
`late_hold` / relock counters) is NOT captured in these run dirs (only aggregate per-source
head-skew jitter + the decode-progress log), so downstream is attributed by ELIMINATION -- an event
with NO anomalous source signal, on a rig whose painter is CLEAN
(`painter_pacing.attribution = downstream-of-painter`, cited, not re-derived here), is a downstream
residual. The three source signals mined per cambox burn log:
  * the 5-s `Streaming: <e> fps emitted / <c> fps captured (<S> sent, <M> captured, <D>
    capture-dropped, <R> corrupted)` line -- `S - M` is the capture DEFICIT (emit-fill material);
  * the 1-s `#707 emit-1s:[..] cap-1s:[..]` buckets -- finer `emit - cap` deficit;
  * the 5-s `(#889) dupe-preferring decimation: .. <L> late-dupe copies emitted .. <G> starvation
    last-frame repeats ..` line -- `L` = a duplicate the cambox ITSELF emitted into NDI (a genuine
    source-origin copy), `G` = the emit-fill / starvation repeat (the raw material a copy can
    survive from).

The DISCRIMINATOR (the physical hypothesis test the ticket asks for): a capture DEFICIT is a HIGH
base-rate background on the under-cadence grabbers (cam1/cam2 carry ~90-110 emit-fills EVERY run,
including the fully-clean 0/0 runs), so its mere presence at an event proves nothing. An event is
SOURCE-attributed ONLY when an ANOMALOUS source signal above that box's own steady background
coincides (a burst deficit >= ANOMALY_DEFICIT_FLOOR, a corrupted frame, or -- decisively -- a
`late-dupe copies emitted >= 1`). Otherwise the event is DOWNSTREAM. The tool also reports the
per-box counterfactual (emit-fills vs residuals, the survival ratio) so a reader sees the base rate.

Pure decision core (no I/O below the CLI, no ssh, no OBS, no rig) -- fixture-driven under Tier-0
#557, the window_gate_walkdown.py / arrival_floor_decompose.py / audio_lag_decision.py python-mirror
precedent. Wired into NO gate and drives NO rig: a SUPERVISOR mining instrument.

REUSES existing parsers (never a second regex for a line another script already parses):
  * `arrival_floor_decompose._strip_ansi` + `arrival_floor_decompose._STREAMING_RE` (the `Streaming:`
    line);
  * `window_gate_walkdown._genlock_sha` (rig-verified era segregation) + `.summarize_verdict`
    (per-run continuity counts).
The `#707 emit-1s/cap-1s` bucket line and the `(#889) dupe-preferring decimation` line have NO
existing python parser (only bash grep patterns in scripts/lib/leg-health-guard.sh), so their
regexes are defined here.
"""
import argparse
import datetime
import glob
import json
import os
import pathlib
import re
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parent
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

from arrival_floor_decompose import _STREAMING_RE, _strip_ansi  # noqa: E402  -- burn-log reuse
from window_gate_walkdown import _genlock_sha, summarize_verdict  # noqa: E402  -- verdict reuse

# --- burn-log lines with NO existing python parser (bash-only in leg-health-guard.sh) ------------
_TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.\d+)?Z")
_B707_RE = re.compile(r"#707 emit-1s:\s*\[([0-9,\s]+)\]\s*cap-1s:\s*\[([0-9,\s]+)\]")
_DECIM_RE = re.compile(
    r"dupe-preferring decimation:.*?(\d+)\s+late-dupe copies emitted"
    r".*?(\d+)\s+starvation last-frame repeats"
)

# A 5-s `sent - captured` deficit at/above this is ANOMALOUS vs the steady 1-2/5s emit-fill
# background the under-cadence grabbers carry run-wide. A `late-dupe copies emitted >= 1` or a
# `corrupted >= 1` is anomalous at any magnitude (a genuine on-box copy / capture corruption).
ANOMALY_DEFICIT_FLOOR = 3
DEFAULT_WINDOW_S = 10


# ======================================================================== pure core (no I/O) =====
def _epoch(iso):
    """Pure: 'YYYY-MM-DDTHH:MM:SS' (UTC, sub-second already trimmed) -> epoch seconds."""
    return int(
        datetime.datetime.strptime(iso, "%Y-%m-%dT%H:%M:%S")
        .replace(tzinfo=datetime.timezone.utc)
        .timestamp()
    )


def parse_burn_timeline(text):
    """Pure: parse ONE cambox burn-log text into a per-time source-signal timeline.

    Returns {stream, sec, decim} where
      stream = [(epoch, sent, captured, capture_dropped, corrupted)] from the 5-s `Streaming:` line
               (REUSES arrival_floor_decompose._STREAMING_RE + _strip_ansi),
      sec    = {epoch: (emit, cap)} from the 1-s `#707 emit-1s/cap-1s` buckets (oldest-first; each
               bucket i of n on a line stamped at t covers second `t-(n-1-i)`),
      decim  = [(epoch, late_dupe_copies, starvation_repeats)] from the `(#889) dupe-preferring
               decimation` line.
    An absent line kind yields an empty container (never a fabricated 0)."""
    stream, sec, decim = [], {}, []
    for raw in (text or "").splitlines():
        clean = _strip_ansi(raw)
        mt = _TS_RE.search(clean)
        if not mt:
            continue
        t = _epoch(mt.group(1))
        ms = _STREAMING_RE.search(clean)
        if ms:
            stream.append(
                (t, int(ms.group(3)), int(ms.group(4)), int(ms.group(5)), int(ms.group(6)))
            )
            continue
        mb = _B707_RE.search(clean)
        if mb:
            emit = [int(x) for x in mb.group(1).split(",") if x.strip()]
            cap = [int(x) for x in mb.group(2).split(",") if x.strip()]
            n = len(emit)
            for i in range(min(n, len(cap))):
                sec[t - (n - 1 - i)] = (emit[i], cap[i])
            continue
        md = _DECIM_RE.search(clean)
        if md:
            decim.append((t, int(md.group(1)), int(md.group(2))))
    return {"stream": stream, "sec": sec, "decim": decim}


def burn_baseline(timeline):
    """Pure: the run-wide per-box source base rate from a parsed timeline.

    The counterfactual denominator: how much emit-fill material a grabber produces run-wide (so a
    reader sees that a capture deficit is ubiquitous, not an event-specific fault)."""
    stream = timeline["stream"]
    n = len(stream)
    deficits = [max(0, s - c) for (_t, s, c, _d, _r) in stream]
    corrupts = [r for (_t, _s, _c, _d, r) in stream]
    return {
        "stream_lines": n,
        "deficit_lines": sum(1 for d in deficits if d >= 1),
        "total_emitfill": sum(deficits),
        "max_line_deficit": max(deficits, default=0),
        "mean_cap_fps": round(sum(c for (_t, _s, c, _d, _r) in stream) / n, 2) if n else None,
        # `corrupted` reads as a persistent per-box FLOOR here (e.g. cam7 shows exactly 4 on every
        # line), so the steady floor carries zero event information -- corruption is only anomalous
        # if it RISES above this floor within an event window.
        "corrupt_floor": min(corrupts, default=0),
        "max_corrupted": max(corrupts, default=0),
        "max_capture_dropped": max((d for (_t, _s, _c, d, _r) in stream), default=0),
        "total_starvation": sum(g for (_t, _l, g) in timeline["decim"]),
        "total_late_dupe": sum(l for (_t, l, _g) in timeline["decim"]),
        "sec_deficit_secs": sum(1 for (e, c) in timeline["sec"].values() if e - c >= 1),
        "sec_total": len(timeline["sec"]),
    }


def source_signal_at(timeline, t_event, window=DEFAULT_WINDOW_S):
    """Pure: the SOURCE-side signal on this box within +-window of an event epoch.

    Distinguishes the steady BACKGROUND deficit (`has_any_deficit`) from an ANOMALOUS signal
    (`has_anomalous`) that would attribute the event to the source."""
    sdef = [max(0, s - c) for (t, s, c, _d, _r) in timeline["stream"] if abs(t - t_event) <= window]
    scor = [r for (t, _s, _c, _d, r) in timeline["stream"] if abs(t - t_event) <= window]
    sdrp = [d for (t, _s, _c, d, _r) in timeline["stream"] if abs(t - t_event) <= window]
    secd = [e - c for (sec, (e, c)) in timeline["sec"].items() if abs(sec - t_event) <= window]
    ldup = [l for (t, l, _g) in timeline["decim"] if abs(t - t_event) <= window]
    starv = [g for (t, _l, g) in timeline["decim"] if abs(t - t_event) <= window]
    stream_deficit_max = max(sdef, default=0)
    corrupt_max = max(scor, default=0)
    late_dupe_max = max(ldup, default=0)
    sig = {
        "stream_deficit_max": stream_deficit_max,
        "stream_corrupt_max": corrupt_max,
        "stream_dropped_max": max(sdrp, default=0),
        "sec_deficit_max": max(secd, default=0),
        "late_dupe_max": late_dupe_max,
        "starvation_max": max(starv, default=0),
        "window_covered": bool(sdef or secd),
    }
    sig["has_any_deficit"] = stream_deficit_max >= 1 or sig["sec_deficit_max"] >= 1
    return sig


def classify_event(signal, baseline=None):
    """Pure: SOURCE iff an ANOMALOUS source signal coincides; else DOWNSTREAM.

    Anomaly is judged AGAINST the box's own run-wide baseline, never an absolute floor, because
    the under-cadence grabbers carry a steady emit-fill deficit AND (e.g. cam7) a steady persistent
    `corrupted` FLOOR run-wide -- both present in the fully-clean 0/0 runs too, so neither is
    event-specific. An event is SOURCE only when the source shows something the run's own background
    does NOT: a `late-dupe copies emitted` (baseline is always 0), corruption that ROSE above the
    box's steady floor, or a burst deficit >= ANOMALY_DEFICIT_FLOOR (the steady background is 1-2).
    Otherwise DOWNSTREAM. `window_covered=False` -> UNKNOWN (kept honest, never guessed)."""
    base = baseline or {}
    corrupt_floor = base.get("corrupt_floor", 0)
    if not signal["window_covered"]:
        return "UNKNOWN", "no source coverage at the event time (burn log absent/short)"
    if signal["late_dupe_max"] >= 1:
        return "SOURCE", "cambox emitted a late-dupe copy into NDI (#889/#1111)"
    if signal["stream_corrupt_max"] > corrupt_floor:
        return "SOURCE", "capture corruption ROSE above the box's steady floor (%d>%d)" % (
            signal["stream_corrupt_max"], corrupt_floor)
    if signal["stream_deficit_max"] >= ANOMALY_DEFICIT_FLOOR:
        return "SOURCE", "anomalous burst capture deficit >= %d in a 5-s bucket" % ANOMALY_DEFICIT_FLOOR
    if signal["has_any_deficit"]:
        return "DOWNSTREAM", "only the steady background emit-fill deficit at the event (covariate)"
    return "DOWNSTREAM", "source clean at the event (no deficit)"


def residual_events_from_verdict(verdict):
    """Pure: the de-duplicated residual copy/gap events from a parsed verdict dict.

    Unions the top-level `all_cambox_continuity.residual_events` with the per-segment
    `segments[].residual_events` (they mirror each other; the union is robust to either being
    absent), keyed on (cambox, kind, wall_clock_epoch_s, frame_index)."""
    c = verdict.get("all_cambox_continuity", {}) or {}
    seen, out = set(), []
    sources = list(c.get("residual_events", []) or [])
    for seg in c.get("segments", []) or []:
        sources.extend(seg.get("residual_events", []) or [])
    for e in sources:
        key = (
            (e.get("cambox") or "?").upper(),
            e.get("kind"),
            e.get("wall_clock_epoch_s"),
            e.get("frame_index"),
        )
        if key in seen:
            continue
        seen.add(key)
        out.append(
            {
                "cambox": key[0],
                "kind": e.get("kind"),
                "wall_clock_epoch_s": e.get("wall_clock_epoch_s"),
                "frame_index": e.get("frame_index"),
                "tick_before": e.get("tick_before"),
                "tick_after": e.get("tick_after"),
                "missing_slots": e.get("missing_slots"),
                "paired_with_catchup": e.get("paired_with_catchup"),
            }
        )
    return out


def _residual_counts(events):
    out = {}
    for e in events:
        d = out.setdefault(e["cambox"], {"copy": 0, "gap": 0})
        if e["kind"] in d:
            d[e["kind"]] += 1
    return out


def attribute_run(verdict, timelines_by_cam, window=DEFAULT_WINDOW_S):
    """Pure: classify every residual event of one run against per-cam parsed burn timelines.

    `timelines_by_cam` = {"CAM2": <parse_burn_timeline result>, ...}. Returns a per-run dict with
    the classified events, the per-box baseline + residual counts, and the run-level continuity
    summary (REUSES window_gate_walkdown.summarize_verdict)."""
    summ = summarize_verdict(verdict)
    events = residual_events_from_verdict(verdict)
    rescounts = _residual_counts(events)
    baselines = {cam: burn_baseline(tl) for cam, tl in timelines_by_cam.items()}
    classified = []
    for e in events:
        tl = timelines_by_cam.get(e["cambox"])
        t = e["wall_clock_epoch_s"]
        if tl is None or t is None:
            sig = {"window_covered": False, "has_any_deficit": False,
                   "stream_deficit_max": 0, "stream_corrupt_max": 0, "stream_dropped_max": 0,
                   "sec_deficit_max": 0, "late_dupe_max": 0, "starvation_max": 0}
        else:
            sig = source_signal_at(tl, t, window)
        attribution, reason = classify_event(sig, baselines.get(e["cambox"]))
        classified.append({**e, "signal": sig, "attribution": attribution, "reason": reason})
    per_box = {}
    for cam in sorted(timelines_by_cam):
        per_box[cam] = {
            "baseline": baselines[cam],
            "residuals": rescounts.get(cam, {"copy": 0, "gap": 0}),
        }
    return {
        "windows_failed_report_only": summ.get("w_fail_strict"),
        "windows_over_copies_gaps_tolerance": summ.get("w_over_tol"),
        "overall_pass": summ.get("overall_pass"),
        "worst_beat_unif": summ.get("worst_beat_unif"),
        "n_events": len(events),
        "events": classified,
        "per_box": per_box,
    }


def aggregate(run_results):
    """Pure: fold per-run attribution into the ticket's verdict (grabber vs FIFO vs mixed).

    The two decisive aggregate facts: (1) how the events split SOURCE vs DOWNSTREAM; (2) the
    counterfactual -- total source emit-fill material vs total residual copies (the survival
    ratio), and whether residual location tracks the per-box emit-fill ranking."""
    n_runs = len(run_results)
    events = [e for r in run_results for e in r["events"]]
    n_events = len(events)
    src = sum(1 for e in events if e["attribution"] == "SOURCE")
    dwn = sum(1 for e in events if e["attribution"] == "DOWNSTREAM")
    unk = sum(1 for e in events if e["attribution"] == "UNKNOWN")
    # counterfactual: total emit-fill material vs residual survivors, run-wide
    total_emitfill = 0
    total_copies = 0
    total_gaps = 0
    # the two decisive counter-signals against a per-grabber-cadence cause:
    #  (a) emit-fill material in the FULLY-CLEAN runs (0 residual events) -> if large with 0
    #      residuals, the deficit does not produce residuals;
    #  (b) residuals landing on a box that is NOT that run's worst-emit-fill grabber.
    clean_run_emitfill = 0
    clean_runs = 0
    nonworst = 0
    runs_with_residual = 0
    for r in run_results:
        per_box = r["per_box"]
        run_fill = {cam: per_box[cam]["baseline"]["total_emitfill"] for cam in per_box}
        for cam in per_box:
            total_emitfill += per_box[cam]["baseline"]["total_emitfill"]
            total_copies += per_box[cam]["residuals"]["copy"]
            total_gaps += per_box[cam]["residuals"]["gap"]
        res_boxes = {cam for cam in per_box
                     if per_box[cam]["residuals"]["copy"] or per_box[cam]["residuals"]["gap"]}
        if not res_boxes:
            clean_runs += 1
            clean_run_emitfill += sum(run_fill.values())
        else:
            runs_with_residual += 1
            if run_fill:
                worst_box = max(run_fill, key=lambda c: run_fill[c])
                if worst_box not in res_boxes or len(res_boxes) > 1:
                    # a residual box that is not the single worst grabber exists this run
                    if any(b != worst_box for b in res_boxes):
                        nonworst += 1
    survival_ratio = (total_copies / total_emitfill) if total_emitfill else None
    if n_events == 0:
        verdict = "no residual events in the sampled runs"
    elif src > dwn:
        verdict = "SOURCE / grabber-owned (majority of events carry an anomalous source signal)"
    elif dwn > 0 and src == 0:
        verdict = (
            "DOWNSTREAM (genlock-FIFO / 60->30 decimation-phase / optical-beat): NO event carries an "
            "anomalous source signal; the per-box capture cadence is a COVARIATE, not the cause"
        )
    else:
        verdict = "MIXED (source anomalies on some events, downstream on others)"
    return {
        "n_runs": n_runs,
        "n_events": n_events,
        "source_events": src,
        "downstream_events": dwn,
        "unknown_events": unk,
        "total_source_emitfill": total_emitfill,
        "total_residual_copies": total_copies,
        "total_residual_gaps": total_gaps,
        "survival_ratio": survival_ratio,
        "clean_runs": clean_runs,
        "clean_run_emitfill_with_zero_residuals": clean_run_emitfill,
        "residual_on_nonworst_grabber": "%d/%d runs with residuals" % (nonworst, runs_with_residual),
        "verdict": verdict,
    }


# ============================================================================ I/O + rendering =====
def _resolve_run_dir(arg):
    if os.path.isdir(arg):
        return arg
    cand = "/tmp/recording-e2e-" + str(arg)
    if os.path.isdir(cand):
        return cand
    return None


def _read(path):
    return pathlib.Path(path).read_text(errors="replace") if path and os.path.isfile(path) else None


def _burns_by_cam(run_dir):
    out = {}
    for p in sorted(glob.glob(os.path.join(run_dir, "cam*-cbox-burn-*.log"))):
        m = re.search(r"cam(\d+)-cbox-burn", os.path.basename(p))
        if m:
            out["CAM" + m.group(1)] = p
    return out


def mine_run_dir(run_dir, window=DEFAULT_WINDOW_S):
    vjs = sorted(glob.glob(os.path.join(run_dir, "verdict-*.json")))
    if not vjs:
        return None
    with open(vjs[0]) as fh:
        verdict = json.load(fh)
    run_id = os.path.basename(vjs[0])[len("verdict-"):-len(".json")]
    timelines = {}
    for cam, path in _burns_by_cam(run_dir).items():
        text = _read(path)
        if text is not None:
            timelines[cam] = parse_burn_timeline(text)
    res = attribute_run(verdict, timelines, window)
    res["run_id"] = run_id
    res["genlock_sha"] = (_genlock_sha(run_dir) or "?")[:9]
    return res


def render_markdown(run_results, agg):
    lines = []
    lines.append("## issue 1242 (task 1) -- residual copy/gap churn source-attribution\n")
    lines.append("### Per-event attribution\n")
    lines.append("| run | genlock | box | kind | src_deficit(bg/anom) | late-dupe | corrupt | attribution | reason |")
    lines.append("|---|---|---|---|---|---|---|---|---|")
    for r in run_results:
        if not r["events"]:
            lines.append("| %s | %s | -- | (none) | -- | -- | -- | -- | clean run |"
                         % (r["run_id"], r["genlock_sha"]))
        for e in r["events"]:
            s = e["signal"]
            bg = "yes" if s.get("has_any_deficit") else "no"
            an = "YES" if e["attribution"] == "SOURCE" else "no"
            lines.append(
                "| %s | %s | %s | %s | %s/%s (max5s=%s) | %s | %s | %s | %s |"
                % (r["run_id"], r["genlock_sha"], e["cambox"], e["kind"], bg, an,
                   s.get("stream_deficit_max"), s.get("late_dupe_max"), s.get("stream_corrupt_max"),
                   e["attribution"], e["reason"])
            )
    lines.append("\n### Per-box base rate (the counterfactual denominator)\n")
    lines.append("| run | box | mean_cap_fps | emit-fills (starvation) | deficit 5-s lines | residuals |")
    lines.append("|---|---|---|---|---|---|")
    for r in run_results:
        for cam in sorted(r["per_box"]):
            b = r["per_box"][cam]["baseline"]
            res = r["per_box"][cam]["residuals"]
            lines.append(
                "| %s | %s | %s | %d (%d) | %d/%d | %dc %dg |"
                % (r["run_id"], cam, b["mean_cap_fps"], b["total_emitfill"], b["total_starvation"],
                   b["deficit_lines"], b["stream_lines"], res["copy"], res["gap"])
            )
    lines.append("\n### Aggregate verdict\n")
    lines.append("- runs sampled: **%d**, residual events: **%d** (SOURCE %d / DOWNSTREAM %d / UNKNOWN %d)"
                 % (agg["n_runs"], agg["n_events"], agg["source_events"],
                    agg["downstream_events"], agg["unknown_events"]))
    sr = agg["survival_ratio"]
    lines.append("- source emit-fill material: **%d** frames -> residual copies: **%d**, gaps: **%d** (copy survival ratio **%s**)"
                 % (agg["total_source_emitfill"], agg["total_residual_copies"],
                    agg["total_residual_gaps"], ("%.4f" % sr) if sr is not None else "n/a"))
    lines.append("- fully-clean runs (0 residuals): **%d**, carrying **%d** emit-fill frames with ZERO residuals"
                 % (agg["clean_runs"], agg["clean_run_emitfill_with_zero_residuals"]))
    lines.append("- residual on a NON-worst-emit-fill grabber: **%s**"
                 % agg["residual_on_nonworst_grabber"])
    lines.append("- **verdict: %s**" % agg["verdict"])
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("runs", nargs="+", help="E2E run dirs or bare RUN_IDs (resolve /tmp/recording-e2e-<id>)")
    ap.add_argument("--window", type=int, default=DEFAULT_WINDOW_S, help="+-seconds around each event")
    ap.add_argument("--json", dest="json_out", help="write the full structured result to this path")
    ap.add_argument("--markdown", dest="md_out", help="write the markdown report to this path")
    args = ap.parse_args(argv)

    run_results = []
    for a in args.runs:
        d = _resolve_run_dir(a)
        if d is None:
            print("skip (no run dir): %s" % a, file=sys.stderr)
            continue
        res = mine_run_dir(d, args.window)
        if res is None:
            print("skip (no verdict): %s" % a, file=sys.stderr)
            continue
        run_results.append(res)
    if not run_results:
        print("no run dirs with a verdict", file=sys.stderr)
        return 1
    agg = aggregate(run_results)
    md = render_markdown(run_results, agg)
    if args.json_out:
        with open(args.json_out, "w") as fh:
            json.dump({"runs": run_results, "aggregate": agg}, fh, indent=2)
    if args.md_out:
        with open(args.md_out, "w") as fh:
            fh.write(md)
    print(md)
    return 0


if __name__ == "__main__":
    sys.exit(main())
