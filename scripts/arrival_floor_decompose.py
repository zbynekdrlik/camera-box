#!/usr/bin/env python3
"""#1168 TASK 1 -- per-box arrival-floor STAGE decomposition mining tool.

WHAT: run over a FINISHED E2E run's collected logs and print ONE per-camera table that decomposes
each camera's arrival floor into three stages, so the supervisor can answer "which box/stage owns
the cross-camera presented-age offset" from data instead of hand-mining it. Pure decision core (no
I/O below the CLI, no ssh, no OBS, no rig) -- fixture-driven RED->GREEN under Tier-0 #557, the
#1199 strih-nic-selfheal / #1203 ndi-halving / #1226 audio-lag python-mirror precedent.

THE MODEL (algebraic, not fitted). Each camera's arrival floor is, exactly,
    floor = latency_ms + mean_head_skew_ms
-- the definition `qr_align_pins.arrival_floors_from_jitter` already uses (the strih FIFO pin plus
the signed mean present-age skew, the actual present age in the pin's OBS clock; #1253 drops a
samples<3 phantom floor). So the per-camera EXCESS over the FASTEST camera decomposes EXACTLY:
    excess = Delta latency_ms   (strih-config pin difference)
           + Delta mean_head_skew_ms   (everything UPSTREAM of the pin = NDI transport + cambox grabber)
The `recv-timing #797` cap_avg is the transport arrival CADENCE; when it is UNIFORM across cameras
(spread <= TRANSPORT_UNIFORM_SPREAD_MS) NDI transport is NOT the per-camera differentiator, so the
upstream excess is attributable to the CAMBOX GRABBER -- corroborated (never replaced) by the cambox
burn-log Streaming/#707 health. The strih genlock pin is already at latency_ms=3 for most inputs.

STAGE SOURCES (all standard E2E artefacts, REUSING the existing derivations -- no new skew regex):
  (a) grabber   <- cam*-cbox-burn-<RUN>.log  `Streaming:` + `#707 ... DEQUEUE STALL` lines
  (b) transport <- qr-align-strih-<RUN>.log   `recv-timing #797` via ndi_halving_decision.parse_recv_timing
  (c) skew/pin  <- qr-align-jitter-<RUN>.json  (= genlock-jitter-report --json) via
                   qr_align_pins.arrival_floors_from_jitter (total floor + #1253 guard) + raw fields

This tool is a supervisor MINING instrument only -- it is wired into NO gate and drives NO rig.
Tasks 2 (reduce the highest floor) and 3 (re-tighten [4i/8align]) are downstream of its output.
"""
import argparse
import glob
import json
import os
import pathlib
import re
import sys

_HERE = pathlib.Path(__file__).resolve().parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

from ndi_halving_decision import parse_recv_timing  # noqa: E402  -- stage (b) reuse
from qr_align_pins import arrival_floors_from_jitter  # noqa: E402  -- stage (c) total-floor reuse

# ---- attribution thresholds (ms) ----
TRANSPORT_UNIFORM_SPREAD_MS = 3.0  # cross-camera cap_avg spread at/below this => transport uniform
EXCESS_NOISE_MS = 2.0              # per-camera floor excess at/below this => within-noise / anchor
STRIH_CONFIG_MIN_MS = 1.0         # Delta latency_ms above this => a real strih-config pin difference
SOURCE_TEMPLATE = "NDI cam{n}"

# ---- multi-run aggregation (#1168 task 2) ----
# A run-POSITION (anchor=fastest / slowest) held by ONE camera in at least this fraction of the
# usable runs is a STABLE per-box property; below it, the run-to-run variance (transient grabber
# DQBUF stalls / load) dominates and NO single box owns the position. 0.6 = a clear majority, not a
# bare plurality -- so a 2-run disagreement (0.5) and a shuffled slowest correctly read UNSTABLE.
RANK_MODE_STABLE_FRAC = 0.6

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
_STREAMING_RE = re.compile(
    r"Streaming:\s*([0-9.]+)\s*fps\s*emitted\s*/\s*([0-9.]+)\s*fps\s*captured\s*"
    r"\((\d+)\s*sent,\s*(\d+)\s*captured,\s*(\d+)\s*capture-dropped,\s*(\d+)\s*corrupted\)"
)
_DQBUF_RE = re.compile(r"#707[^\n]*DEQUEUE STALL:\s*([0-9.]+)\s*ms")
_CAMNUM_RE = re.compile(r"cam(\d+)")


def _strip_ansi(text):
    return _ANSI_RE.sub("", text or "")


# --------------------------------------------------------------- stage (a): cambox grabber ----------
def parse_streaming(text):
    """Aggregate the cambox `Streaming: X fps emitted / Y fps captured (S sent, C captured, D
    capture-dropped, R corrupted)` ticks. Returns a dict of grabber-health signals; an empty/absent
    log yields lines=0 and None rates (never a fabricated 0)."""
    clean = _strip_ansi(text)
    emit, cap, dropped, corrupted, behind = [], [], [], [], 0
    for m in _STREAMING_RE.finditer(clean):
        e, c = float(m.group(1)), float(m.group(2))
        emit.append(e)
        cap.append(c)
        dropped.append(int(m.group(5)))
        corrupted.append(int(m.group(6)))
        if e < c - 0.5:  # emit falling behind capture => grabber/emit can't keep up
            behind += 1
    n = len(emit)
    return {
        "lines": n,
        "emit_fps_mean": (sum(emit) / n) if n else None,
        "cap_fps_mean": (sum(cap) / n) if n else None,
        "emit_behind_lines": behind,
        "max_capture_dropped": max(dropped) if dropped else 0,
        "max_corrupted": max(corrupted) if corrupted else 0,
    }


def parse_dqbuf_stalls(text):
    """`#707 ... DEQUEUE STALL: N.Nms` durations -> {count, max_ms, mean_ms}. A DQBUF stall is a
    grabber-side JITTER signal (the blocking VIDIOC_DQBUF wait), NOT a loss signal -- it is
    anti-correlated with real frame loss (#1198), so it corroborates a per-box floor, never gates."""
    vals = [float(m.group(1)) for m in _DQBUF_RE.finditer(_strip_ansi(text))]
    return {
        "count": len(vals),
        "max_ms": max(vals) if vals else None,
        "mean_ms": (sum(vals) / len(vals)) if vals else None,
    }


def grabber_health(text):
    """Combine the two cambox-side signals into one dict for a source's burn log."""
    g = parse_streaming(text)
    d = parse_dqbuf_stalls(text)
    g["dqbuf_stall_count"] = d["count"]
    g["dqbuf_stall_max_ms"] = d["max_ms"]
    return g


# --------------------------------------------------------------- stage (b): NDI transport -----------
def cap_avg_by_source(strih_text, sources):
    """{src: mean recv-timing #797 cap_avg (ms) or None}. REUSES parse_recv_timing (each line's own
    timestamp discipline, #797-safe -- never a wall-clock divisor). A source with no recv-timing
    lines yields None (honest absence, never fabricated)."""
    out = {}
    for s in sources:
        rows = parse_recv_timing(strih_text or "", s)
        caps = [cap for (_ts, _n, cap) in rows if cap is not None]
        out[s] = (sum(caps) / len(caps)) if caps else None
    return out


# --------------------------------------------------------------- stage (c): skew + FIFO pin --------
def floors_and_fields(jitter_json, sources):
    """{src: {floor_ms, latency_ms, mean_head_skew_ms, samples}} for the sources present in the
    jitter JSON. floor_ms REUSES qr_align_pins.arrival_floors_from_jitter (the canonical
    latency_ms + mean_head_skew_ms derivation WITH the #1253 samples-guard), so a phantom floor is
    dropped here exactly as the aligner drops it -- never re-derived."""
    floors = arrival_floors_from_jitter(jitter_json, sources)
    out = {}
    for s in sources:
        if s not in floors:
            continue
        entry = jitter_json.get(s, {}) if isinstance(jitter_json, dict) else {}
        out[s] = {
            "floor_ms": floors[s],
            "latency_ms": entry.get("latency_ms"),
            "mean_head_skew_ms": entry.get("mean_head_skew_ms"),
            "samples": entry.get("samples"),
        }
    return out


# --------------------------------------------------------------- CORE: decompose -------------------
def _grabber_note(grabber):
    if not grabber:
        return ""
    bits = []
    stall = grabber.get("dqbuf_stall_max_ms")
    if stall is not None and stall > 0:
        bits.append("DQBUF stall max %.1fms x%d" % (stall, grabber.get("dqbuf_stall_count", 0)))
    if grabber.get("emit_behind_lines"):
        bits.append("emit-behind %d ticks" % grabber["emit_behind_lines"])
    if grabber.get("max_capture_dropped"):
        bits.append("capture-dropped %d" % grabber["max_capture_dropped"])
    return (" [%s]" % "; ".join(bits)) if bits else " [grabber log clean -- steady per-box floor]"


def _attribute(is_anchor, excess, d_lat, d_skew, transport_uniform, grabber):
    if is_anchor:
        return "anchor (fastest)"
    if excess <= EXCESS_NOISE_MS:
        return "within-noise"
    parts = []
    if d_lat is not None and d_lat > STRIH_CONFIG_MIN_MS:
        parts.append("strih-config +%.0fms latency pin" % d_lat)
    if d_skew is not None and d_skew > EXCESS_NOISE_MS:
        if transport_uniform is True:
            parts.append("grabber/cambox +%.0fms%s" % (d_skew, _grabber_note(grabber)))
        elif transport_uniform is False:
            parts.append("transport +%.0fms (cap_avg NOT uniform) / grabber" % d_skew)
        else:
            parts.append("grabber/transport +%.0fms (transport cadence unknown)" % d_skew)
    if parts:
        return "; ".join(parts)
    # supra-noise excess but no single component cleared its own threshold: an honest mixed label,
    # never a "within-noise" that would contradict excess > EXCESS_NOISE_MS (review S1).
    return "mixed sub-threshold +%.1fms" % excess


def decompose(jitter_json, cap_avgs, grabber_by_src, sources):
    """Decompose each present camera's arrival floor into strih-config (Delta latency) + upstream
    (Delta skew), attribute the upstream term to grabber/transport via cap_avg uniformity, and
    return {rows, anchor_src, summary}. `sources` names the cameras to consider; a source absent or
    #1253-dropped from the jitter JSON is OMITTED from rows and listed in summary.omitted_sources."""
    cap_avgs = cap_avgs or {}
    grabber_by_src = grabber_by_src or {}
    floors = floors_and_fields(jitter_json, sources)
    present = list(floors.keys())
    omitted = [s for s in sources if s not in floors]

    if not present:
        return {
            "rows": [],
            "anchor_src": None,
            "summary": {
                "floor_spread_ms": None, "anchor_src": None, "anchor_floor_ms": None,
                "slowest_src": None, "slowest_floor_ms": None, "slowest_excess_ms": None,
                "slowest_owner": None, "transport_cap_avg_spread_ms": None,
                "transport_uniform": None, "omitted_sources": omitted,
            },
        }

    # anchor = fastest by floor; deterministic tie-break by source name.
    anchor_src = min(present, key=lambda s: (floors[s]["floor_ms"], s))
    a = floors[anchor_src]
    a_floor, a_lat, a_skew = a["floor_ms"], a["latency_ms"], a["mean_head_skew_ms"]

    cap_vals = [cap_avgs[s] for s in present if cap_avgs.get(s) is not None]
    if len(cap_vals) >= 2:
        transport_spread = max(cap_vals) - min(cap_vals)
        transport_uniform = transport_spread <= TRANSPORT_UNIFORM_SPREAD_MS
    elif len(cap_vals) == 1:
        # one camera can't establish uniformity OR a spread -> None, not a fabricated 0.0 (review S3)
        transport_spread, transport_uniform = None, None
    else:
        transport_spread, transport_uniform = None, None  # no transport data at all

    rows = []
    for s in sorted(present, key=lambda x: (floors[x]["floor_ms"], x)):
        f = floors[s]
        lat, skew = f["latency_ms"], f["mean_head_skew_ms"]
        excess = f["floor_ms"] - a_floor
        d_lat = (lat - a_lat) if (lat is not None and a_lat is not None) else None
        d_skew = (skew - a_skew) if (skew is not None and a_skew is not None) else None
        grab = grabber_by_src.get(s) or {}
        owner = _attribute(s == anchor_src, excess, d_lat, d_skew, transport_uniform, grab)
        rows.append({
            "src": s,
            "floor_ms": f["floor_ms"],
            "latency_ms": lat,
            "mean_head_skew_ms": skew,
            "samples": f["samples"],
            "cap_avg_ms": cap_avgs.get(s),
            "excess_ms": excess,
            "d_latency_ms": d_lat,
            "d_skew_ms": d_skew,
            "owner": owner,
            "grabber": grab,
        })

    slowest = max(rows, key=lambda r: r["floor_ms"])
    summary = {
        "floor_spread_ms": slowest["floor_ms"] - a_floor,
        "anchor_src": anchor_src,
        "anchor_floor_ms": a_floor,
        "slowest_src": slowest["src"],
        "slowest_floor_ms": slowest["floor_ms"],
        "slowest_excess_ms": slowest["excess_ms"],
        "slowest_owner": slowest["owner"],
        "transport_cap_avg_spread_ms": transport_spread,
        "transport_uniform": transport_uniform,
        "omitted_sources": omitted,
    }
    return {"rows": rows, "anchor_src": anchor_src, "summary": summary}


# --------------------------------------------------------------- MULTI-RUN: aggregate -------------
def _keep_run(result, only_uniform=False, min_cameras=0):
    """Stratification predicate for --multi. A run is kept only if it has >= min_cameras non-phantom
    rows AND, when only_uniform is set, its transport_uniform verdict is True (the clean ~50 ms
    constant-offset regime -- a transport-degraded / halving run is a DIFFERENT fault). An empty
    (all-#1253-phantom) run or a None result is NEVER kept. Pure predicate -- no I/O."""
    if not result or not result.get("rows"):
        return False
    if len(result["rows"]) < min_cameras:
        return False
    if only_uniform and (result.get("summary", {}) or {}).get("transport_uniform") is not True:
        return False
    return True


def _mode(counts):
    """(src, count) of the most common camera in a {src: n} tally; deterministic tie-break by src
    name (lowest sorts first); (None, 0) for an empty tally."""
    if not counts:
        return None, 0
    best = max(sorted(counts), key=lambda s: counts[s])
    return best, counts[best]


def aggregate(runs):
    """Fold a list of (run_id, decompose_result) into a cross-run summary so the target box is
    chosen from MANY runs, not one. Returns per-camera floor/excess distribution (median/min/max/
    pstdev), mean+median floor-RANK (1=fastest), the latency-pin set, anchor/slowest MODE counts,
    a median-floor ranking, and a STABILITY verdict (is one camera the anchor/slowest in >=
    RANK_MODE_STABLE_FRAC of usable runs?). Empty (all-phantom) runs are COUNTED (n_empty) but
    contribute no camera data. Pure -- consumes decompose() output, never re-parses a log."""
    import statistics as _st

    n_runs = len(runs)
    usable = [(rid, res) for (rid, res) in runs if res and res.get("rows")]
    n_usable = len(usable)
    n_empty = n_runs - n_usable

    percam = {}  # src -> {"floors": [], "excess": [], "ranks": [], "pins": set()}
    anchor_counts, slowest_counts = {}, {}
    for _rid, res in usable:
        s = res["summary"]
        a, sl = s.get("anchor_src"), s.get("slowest_src")
        if a is not None:
            anchor_counts[a] = anchor_counts.get(a, 0) + 1
        if sl is not None:
            slowest_counts[sl] = slowest_counts.get(sl, 0) + 1
        # rank 1 = fastest (lowest floor); deterministic tie-break by src, matching decompose().
        ordered = sorted(res["rows"], key=lambda r: (r["floor_ms"], r["src"]))
        for pos, r in enumerate(ordered, 1):
            d = percam.setdefault(r["src"], {"floors": [], "excess": [], "ranks": [], "pins": set()})
            d["floors"].append(r["floor_ms"])
            d["excess"].append(r["excess_ms"])
            d["ranks"].append(pos)
            if r.get("latency_ms") is not None:
                d["pins"].add(r["latency_ms"])

    per_camera = {}
    for c, d in percam.items():
        fl, ex, rk = d["floors"], d["excess"], d["ranks"]
        per_camera[c] = {
            "n": len(fl),
            "floor_median": _st.median(fl), "floor_min": min(fl), "floor_max": max(fl),
            "floor_pstdev": _st.pstdev(fl) if len(fl) > 1 else 0.0,
            "excess_median": _st.median(ex), "excess_min": min(ex), "excess_max": max(ex),
            "excess_pstdev": _st.pstdev(ex) if len(ex) > 1 else 0.0,
            "mean_floor_rank": _st.mean(rk), "median_floor_rank": _st.median(rk),
            "latency_pins": sorted(d["pins"]),
        }

    ranking = sorted(per_camera, key=lambda c: (per_camera[c]["floor_median"], c))

    def _stab(counts):
        src, cnt = _mode(counts)
        frac = (cnt / n_usable) if n_usable else 0.0
        return {"src": src, "count": cnt, "fraction": frac,
                "stable": bool(n_usable) and frac >= RANK_MODE_STABLE_FRAC}

    med_spread = None
    if per_camera:
        med_spread = per_camera[ranking[-1]]["floor_median"] - per_camera[ranking[0]]["floor_median"]

    return {
        "n_runs": n_runs, "n_usable": n_usable, "n_empty": n_empty,
        "per_camera": per_camera,
        "anchor_counts": anchor_counts, "slowest_counts": slowest_counts,
        "ranking_by_median_floor": ranking,
        "stability": {
            "anchor": _stab(anchor_counts), "slowest": _stab(slowest_counts),
            "median_floor_spread_ms": med_spread,
        },
    }


# --------------------------------------------------------------- CLI --------------------------------
def _read(path):
    return pathlib.Path(path).read_text(errors="replace") if path and os.path.isfile(path) else None


def _discover(run_dir):
    """Locate the three stage artefacts inside an E2E run dir by their standard names."""
    jit = sorted(glob.glob(os.path.join(run_dir, "qr-align-jitter-*.json")))
    strih = sorted(glob.glob(os.path.join(run_dir, "qr-align-strih-*.log")))
    burns = {}
    for p in sorted(glob.glob(os.path.join(run_dir, "cam*-cbox-burn-*.log"))):
        m = _CAMNUM_RE.search(os.path.basename(p))
        if m:
            burns[int(m.group(1))] = p
    return (jit[0] if jit else None), (strih[0] if strih else None), burns


def _sources_from_jitter(jitter_json, template):
    prefix = template.split("{")[0]  # "NDI cam"
    out = []
    for k in jitter_json:
        if not k.startswith(prefix):
            continue
        m = re.search(r"(\d+)$", k)
        if m and k == template.format(n=int(m.group(1))):
            out.append((int(m.group(1)), k))
    return [k for _n, k in sorted(out)]


def _decompose_artefacts(jitter_json, strih_path, burns, cameras=None):
    """Shared per-run glue: resolve sources, read strih cap_avgs + cambox grabber health, and
    decompose(). REUSES every existing derivation (arrival_floors_from_jitter via decompose,
    parse_recv_timing via cap_avg_by_source, grabber_health) -- no new skew regex. Both the
    single-run CLI path and mine_run_dir (the --multi miner) call this, so the two can never drift."""
    if cameras:
        sources = [SOURCE_TEMPLATE.format(n=n) for n in cameras]
    else:
        sources = _sources_from_jitter(jitter_json, SOURCE_TEMPLATE)
    strih_text = _read(strih_path)
    cap_avgs = cap_avg_by_source(strih_text, sources) if strih_text else {}
    grabber_by_src = {}
    for s in sources:
        m = re.search(r"(\d+)$", s)
        if not m:
            continue
        burn = burns.get(int(m.group(1))) if burns else None
        btext = _read(burn) if burn else None
        if btext:
            grabber_by_src[s] = grabber_health(btext)
    return decompose(jitter_json, cap_avgs, grabber_by_src, sources)


def mine_run_dir(run_dir, cameras=None):
    """Mine ONE E2E run dir -> its decompose() result, or None when no valid jitter JSON is present
    (honest absence -- a missing/empty/corrupt qr-align-jitter-*.json, never a crash). This is the
    per-run unit --multi folds; it reuses the SAME _discover + _decompose_artefacts the single-run
    CLI uses."""
    d_jit, d_strih, burns = _discover(run_dir)
    if not d_jit or not os.path.isfile(d_jit):
        return None
    try:
        jitter_json = json.loads(pathlib.Path(d_jit).read_text(errors="replace"))
    except json.JSONDecodeError:
        return None
    if not isinstance(jitter_json, dict):
        return None  # a well-formed but non-object artefact (bare 42/null/list) -> honest None,
        #              never a crash mid-sweep (the genlock-jitter-report shape is always an object)
    return _decompose_artefacts(jitter_json, d_strih, burns, cameras)


def _fmt(v, spec="%.1f"):
    return "-" if v is None else (spec % v)


def _render_table(result):
    rows = result["rows"]
    s = result["summary"]
    lines = []
    hdr = "%-9s %8s %8s %9s %8s %9s %9s  %s" % (
        "camera", "floor", "lat_ms", "skew_ms", "cap_avg", "d_lat", "d_skew", "owner")
    lines.append(hdr)
    lines.append("-" * len(hdr))
    for r in rows:
        cam = r["src"].replace("NDI ", "")
        lines.append("%-9s %8s %8s %9s %8s %9s %9s  %s" % (
            cam, _fmt(r["floor_ms"]), _fmt(r["latency_ms"], "%d"),
            _fmt(r["mean_head_skew_ms"]), _fmt(r["cap_avg_ms"], "%.2f"),
            _fmt(r["d_latency_ms"]), _fmt(r["d_skew_ms"]), r["owner"]))
    lines.append("")
    lines.append("SUMMARY: cross-camera floor spread = %s ms  (anchor %s @ %s ms -> slowest %s @ %s ms)"
                 % (_fmt(s["floor_spread_ms"]), s["anchor_src"], _fmt(s["anchor_floor_ms"]),
                    s["slowest_src"], _fmt(s["slowest_floor_ms"])))
    lines.append("  transport cap_avg spread = %s ms -> transport_uniform=%s%s"
                 % (_fmt(s["transport_cap_avg_spread_ms"]), s["transport_uniform"],
                    "  (uniform => NDI transport is NOT the per-camera differentiator)"
                    if s["transport_uniform"] else ""))
    lines.append("  slowest camera owner: %s" % s["slowest_owner"])
    if s["omitted_sources"]:
        lines.append("  omitted (absent or #1253 phantom floor): %s" % ", ".join(s["omitted_sources"]))
    return "\n".join(lines)


def _parse_cameras(args, ap):
    """--cameras "1,2,3,4" -> [1,2,3,4], or None. NOTE: SOURCE_TEMPLATE is fixed, NOT a CLI knob --
    the reused arrival_floors_from_jitter hardcodes the "NDI cam<N>" strih naming, so any other
    template would resolve zero floors (review W1)."""
    if not args.cameras:
        return None
    try:
        return [int(x) for x in args.cameras.replace(" ", "").split(",") if x]
    except ValueError:
        ap.error('--cameras must be comma-separated integers, e.g. "1,2,3,4"')


def _render_multi(agg, runs):
    """Human-readable --multi report: header, per-run digest, per-camera aggregate table (ordered
    by median floor), and the STABILITY verdict."""
    lines = []
    # honest accounting: scanned == usable + skipped + phantom + stratified-out. n_phantom_mined is
    # the CLI-tracked count (empty runs are dropped BEFORE aggregate(), so agg["n_empty"] is 0 here).
    lines.append(
        "MULTI-RUN arrival-floor aggregate: %d dir(s) scanned, %d usable, %d phantom, %d stratified-out"
        % (agg.get("n_dirs_scanned", len(runs)), agg["n_usable"],
           agg.get("n_phantom_mined", agg["n_empty"]), agg.get("n_stratified_out", 0)))
    if agg.get("n_skipped_no_data"):
        lines.append("  (%d dir(s) skipped: no valid qr-align-jitter-*.json)" % agg["n_skipped_no_data"])

    lines.append("")
    lines.append("%-13s %-8s %-8s %8s" % ("run", "anchor", "slowest", "spread"))
    for rid, res in runs:
        s = res["summary"]
        lines.append("%-13s %-8s %-8s %8s" % (
            rid, (s["anchor_src"] or "-").replace("NDI ", ""),
            (s["slowest_src"] or "-").replace("NDI ", ""), _fmt(s["floor_spread_ms"])))

    lines.append("")
    hdr = "%-8s %4s %8s %8s %8s %8s %9s %8s  %s" % (
        "camera", "n", "flr_med", "flr_min", "flr_max", "flr_std", "mean_rank", "ex_med", "lat_pins")
    lines.append(hdr)
    lines.append("-" * len(hdr))
    for c in agg["ranking_by_median_floor"]:
        d = agg["per_camera"][c]
        lines.append("%-8s %4d %8.1f %8.1f %8.1f %8.1f %9.2f %8.1f  %s" % (
            c.replace("NDI ", ""), d["n"], d["floor_median"], d["floor_min"], d["floor_max"],
            d["floor_pstdev"], d["mean_floor_rank"], d["excess_median"],
            ",".join(str(p) for p in d["latency_pins"])))

    lines.append("")
    st = agg["stability"]
    a, sl = st["anchor"], st["slowest"]
    lines.append("STABILITY VERDICT (position held by ONE camera in >= %.0f%% of usable runs = stable):"
                 % (100 * RANK_MODE_STABLE_FRAC))
    lines.append("  anchor (fastest): %s in %d/%d runs (%.0f%%) -> %s" % (
        (a["src"] or "-").replace("NDI ", ""), a["count"], agg["n_usable"], 100 * a["fraction"],
        "STABLE" if a["stable"] else "NOT stable (run-level variance dominates)"))
    lines.append("  slowest:          %s in %d/%d runs (%.0f%%) -> %s" % (
        (sl["src"] or "-").replace("NDI ", ""), sl["count"], agg["n_usable"], 100 * sl["fraction"],
        "STABLE" if sl["stable"] else "NOT stable (run-level variance dominates)"))
    lines.append("  median-floor spread across cameras = %s ms" % _fmt(st["median_floor_spread_ms"]))
    return "\n".join(lines)


def _run_multi(args, ap):
    """--multi: mine several run dirs (repeated --run-dir and/or --runs-glob), stratify with
    _keep_run, aggregate, and report. Reuses mine_run_dir (the SAME per-run path as single mode)."""
    dirs = list(args.run_dir or [])
    if args.runs_glob:
        dirs.extend(sorted(glob.glob(args.runs_glob)))
    seen, ordered = set(), []
    for d in dirs:
        if d not in seen:
            seen.add(d)
            ordered.append(d)
    if not ordered:
        ap.error("--multi needs run dirs: pass --run-dir <dir> (repeatable) and/or --runs-glob PATTERN")

    cam_nums = _parse_cameras(args, ap)
    runs, n_skipped, n_phantom, n_stratified = [], 0, 0, 0
    for d in ordered:
        res = mine_run_dir(d, cam_nums)
        if res is None:
            n_skipped += 1                    # no valid jitter JSON at all
            continue
        if not res.get("rows"):
            n_phantom += 1                    # mined, but every camera #1253-phantom
            continue
        if not _keep_run(res, only_uniform=args.only_uniform, min_cameras=args.min_cameras):
            n_stratified += 1                 # dropped by --only-uniform / --min-cameras
            continue
        rid = os.path.basename(d.rstrip("/")).replace("recording-e2e-", "")
        runs.append((rid, res))

    # accounting closes exactly: scanned == usable + skipped + phantom + stratified-out.
    agg = aggregate(runs)
    agg["n_dirs_scanned"] = len(ordered)
    agg["n_skipped_no_data"] = n_skipped
    agg["n_phantom_mined"] = n_phantom
    agg["n_stratified_out"] = n_stratified

    if args.as_json:
        print(json.dumps(agg, indent=2, sort_keys=True))
    else:
        print(_render_multi(agg, runs))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description="Per-box arrival-floor stage decomposition (#1168 tasks 1-2)")
    ap.add_argument("--run-dir", action="append",
                    help="an E2E run dir (/tmp/recording-e2e-<RUN>); repeat for --multi")
    ap.add_argument("--jitter-json", help="qr-align-jitter-<RUN>.json (overrides --run-dir discovery)")
    ap.add_argument("--strih-log", help="qr-align-strih-<RUN>.log (overrides --run-dir discovery)")
    ap.add_argument("--cameras", help='explicit camera numbers, e.g. "1,2,3,4"')
    ap.add_argument("--multi", action="store_true",
                    help="aggregate several run dirs (per-camera floor distribution + stability verdict)")
    ap.add_argument("--runs-glob", help="(--multi) glob of run dirs, e.g. '/tmp/recording-e2e-*'")
    ap.add_argument("--only-uniform", action="store_true",
                    help="(--multi) keep only transport-uniform runs (the clean ~50ms-offset regime)")
    ap.add_argument("--min-cameras", type=int, default=0,
                    help="(--multi) keep only runs with >= N non-phantom cameras")
    ap.add_argument("--json", action="store_true", dest="as_json", help="machine-parseable output")
    args = ap.parse_args(argv)

    if args.multi:
        return _run_multi(args, ap)

    burns = {}
    run_dir = None
    if args.run_dir:
        if len(args.run_dir) > 1:
            ap.error("multiple --run-dir given; use --multi to aggregate several run dirs")
        run_dir = args.run_dir[0]
    jitter_path, strih_path = args.jitter_json, args.strih_log
    if run_dir:
        d_jit, d_strih, burns = _discover(run_dir)
        jitter_path = jitter_path or d_jit
        strih_path = strih_path or d_strih
    if not jitter_path or not os.path.isfile(jitter_path):
        ap.error("no jitter JSON found (pass --jitter-json or a --run-dir containing qr-align-jitter-*.json)")

    try:
        jitter_json = json.loads(pathlib.Path(jitter_path).read_text(errors="replace"))
    except json.JSONDecodeError as e:
        ap.error("jitter JSON %s is not valid JSON: %s" % (jitter_path, e))

    result = _decompose_artefacts(jitter_json, strih_path, burns, _parse_cameras(args, ap))

    if args.as_json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(_render_table(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
