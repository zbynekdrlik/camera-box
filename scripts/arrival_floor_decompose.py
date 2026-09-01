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
    return "; ".join(parts) if parts else "within-noise"


def decompose(jitter_json, cap_avgs, grabber_by_src, sources, *, source_template=SOURCE_TEMPLATE):
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
        transport_spread, transport_uniform = 0.0, None  # one camera can't establish uniformity
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
            cam, _fmt(r["floor_ms"]), _fmt(r["latency_ms"], "%d") if r["latency_ms"] is not None else "-",
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


def main(argv=None):
    ap = argparse.ArgumentParser(description="Per-box arrival-floor stage decomposition (#1168 task 1)")
    ap.add_argument("--run-dir", help="an E2E run dir (/tmp/recording-e2e-<RUN>) to auto-discover from")
    ap.add_argument("--jitter-json", help="qr-align-jitter-<RUN>.json (overrides --run-dir discovery)")
    ap.add_argument("--strih-log", help="qr-align-strih-<RUN>.log (overrides --run-dir discovery)")
    ap.add_argument("--source-template", default=SOURCE_TEMPLATE)
    ap.add_argument("--cameras", help='explicit camera numbers, e.g. "1,2,3,4"')
    ap.add_argument("--json", action="store_true", dest="as_json", help="machine-parseable output")
    args = ap.parse_args(argv)

    burns = {}
    jitter_path, strih_path = args.jitter_json, args.strih_log
    if args.run_dir:
        d_jit, d_strih, burns = _discover(args.run_dir)
        jitter_path = jitter_path or d_jit
        strih_path = strih_path or d_strih
    if not jitter_path or not os.path.isfile(jitter_path):
        ap.error("no jitter JSON found (pass --jitter-json or a --run-dir containing qr-align-jitter-*.json)")

    jitter_json = json.loads(pathlib.Path(jitter_path).read_text(errors="replace"))

    if args.cameras:
        nums = [int(x) for x in args.cameras.replace(" ", "").split(",") if x]
        sources = [args.source_template.format(n=n) for n in nums]
    else:
        sources = _sources_from_jitter(jitter_json, args.source_template)

    strih_text = _read(strih_path)
    cap_avgs = cap_avg_by_source(strih_text, sources) if strih_text else {}

    grabber_by_src = {}
    for s in sources:
        m = re.search(r"(\d+)$", s)
        if not m:
            continue
        burn = burns.get(int(m.group(1)))
        btext = _read(burn) if burn else None
        if btext:
            grabber_by_src[s] = grabber_health(btext)

    result = decompose(jitter_json, cap_avgs, grabber_by_src, sources,
                       source_template=args.source_template)

    if args.as_json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(_render_table(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
