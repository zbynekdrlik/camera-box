#!/usr/bin/env python3
"""issue 1367 -- the PURE decision core of the 8 h stream-output A/V soak (scripts/av-soak.sh).

Owner goal (24.9.2026, verbatim on the issue): on the stream OBS output the picture/sound latency
must be right, with no dropouts, and the sync must not drift apart for 8 hours; a real drift is
already visible after about 1 h; and the cameras must stay in sync with each other.

The orchestrator (scripts/av-soak.sh) records ONE short window per slot (default every 10 min),
decodes it with the SAME recording-verdict the full-path E2E uses, and calls `row` here to append
ONE CSV row per window. `report` grades the CSV. This module does NO rig I/O at all: it reads a
verdict JSON / the CSV and two Rust source files, so it is exhaustively pytest-able under Tier-0
(tests/python/test_av_soak_decision_1367.py).

## What is graded, against what

- **A/V per camera** (`av <cam>`): the verdict's own MEASURED `all_cambox_av_sync.<cam>.av_offset_ms`
  (a `derived` / `unknown` value is never a sample). Each sample must satisfy
  `|offset - expected_ms| <= AV_OFFSET_GATE_TOLERANCE_MS` (inclusive, the verdict's own rule).
- **cross-camera spread** (`spread <column>`): each value `<= SPREAD_THRESHOLD_MS` (inclusive, the
  `switch_latency::spread_verdict` boundary). Which spread columns are GRADED is a parameter
  (`--spread-columns`); the others are only REPORTED. The CSV carries three:
    * `source_spread_ms` / `delivery_spread_ms` -- the verdict's own two gate spreads. Both need the
      per-camera capture burn (the probe-featured camera-box the E2E deploys); a passive TEST-mode
      rig has none, so they are empty there and a graded empty column is UNKNOWN, never a pass.
    * `av_spread_ms` -- `max - min` of the measured per-camera A/V offsets, cam2 excluded (cam2's
      number pools the whole recording, whatever camera is on program). The audio reference is
      common, so this is the camera-to-camera alignment AT THE STREAM OUTPUT.
  Which one the soak grades is an open design question on issue 1367 (comment 5858491691); the
  default is the design as written (the gate's two spreads).
- **loss per camera** (`loss <cam>`): the verdict's own per-segment `pass` of the camera's windows in
  `all_cambox_continuity.segments` (that is the existing zero-loss bar, with whatever tolerance the
  verdict applies -- never re-derived here). Raw copies/gaps/undecodable are summed and reported.
- **burn loss per camera** (`burn <cam>`): `full_chain.loss.<cam>.zero_loss` when the verdict has it
  (only with the capture burns). Optional: never measured = not required; a measured `false` fails.
- **slope** of every value series: least squares over (hours, value), `|slope| <= 2 ms/h`
  (`SLOPE_BOUND_MS_PER_H`, the acceptance on issue 1367). Needs >= 3 samples over >= 30 min.
- **cadence**: the acceptance asks for a sample at least every 10 min, so every required series
  must have no gap longer than `DEFAULT_MAX_GAP_S` (the 600 s slot + 60 s allowance for the
  per-slot pre-record steps' start jitter; one missed slot is a 1200 s gap), counting the gap from
  the run's first window to the series' first sample and from its last sample to the last window.
- **duration**: the run's window starts must span `min_duration - 60 s`.

The two gate bounds are READ from their single sources (`src/av_window.rs`
`AV_OFFSET_GATE_TOLERANCE_MS`, `src/switch_latency.rs` `SPREAD_THRESHOLD_MS`); a missing constant
fails closed (`BoundsError`, exit 3). They are never retyped here.

## Verdicts and exit codes

`PASS` (0): every required series passes. `FAIL` (1): any sample out of bound, any slope over the
bound, any loss or burn-loss window. `UNKNOWN` (2): no windows, a run shorter than required, a
series with no samples / not enough for a slope / a cadence gap. A breach always wins over missing
evidence. `3`: a usage / input error (unreadable CSV, a missing bound constant, an unknown column).
`EXCLUDED`: a camera the verdict reported as operator-excluded in every window; it is not required.
`REPORTED` / `NOT_MEASURED`: a non-graded spread column / a burn column never measured.

Subcommands: `bounds` (print the loaded bounds), `row` (append one window's row), `report` (grade;
prints the 1 h partial when the CSV extends past it, and the full run; `--json` writes both).
"""
import argparse
import csv
import json
import math
import os
import re
import sys
import time

PASS, FAIL, UNKNOWN = "PASS", "FAIL", "UNKNOWN"
EXCLUDED, REPORTED, NOT_MEASURED = "EXCLUDED", "REPORTED", "NOT_MEASURED"
_EXIT = {PASS: 0, FAIL: 1, UNKNOWN: 2}
EXIT_USAGE = 3

# The acceptance on issue 1367: "the fitted drift (slope) of each series stays below 2 ms/h".
SLOPE_BOUND_MS_PER_H = 2.0
# A slope from fewer points or a shorter span is noise, not a trend: UNKNOWN instead.
MIN_SLOPE_SAMPLES = 3
MIN_SLOPE_SPAN_S = 1800
# "at least one every 10 min" on a fixed 10-min slot grid; the allowance absorbs the per-slot
# pre-record steps' start jitter (never a missed slot, which is a 1200 s gap).
DEFAULT_SLOT_S = 600
START_JITTER_ALLOWANCE_S = 60
DEFAULT_MAX_GAP_S = DEFAULT_SLOT_S + START_JITTER_ALLOWANCE_S
DEFAULT_PARTIAL_H = 1.0
DEFAULT_MIN_DURATION_H = 8.0

SPREAD_COLUMNS = ("source_spread_ms", "delivery_spread_ms", "av_spread_ms")
# The design as written grades the gate's own two spreads (issue 1367 design question, B = av).
DEFAULT_SPREAD_COLUMNS = ("source_spread_ms", "delivery_spread_ms")
# cam2's A/V number pools the WHOLE recording (it is the painter's own emitter, no window of its own
# shows the QR), so it is not a camera path and is left out of the stream-output spread.
AV_SPREAD_EXCLUDED_CAMS = ("cam2",)

# (file under the repo root, constant name) -- the ONE place each gate bound lives.
_BOUND_SOURCES = {
    "av_tolerance_ms": ("src/av_window.rs", "AV_OFFSET_GATE_TOLERANCE_MS"),
    "spread_threshold_ms": ("src/switch_latency.rs", "SPREAD_THRESHOLD_MS"),
}

_CAM_RE = re.compile(r"^cam[0-9]+$")
_BASE_FIELDS = (
    "ts_utc", "epoch_s", "slot", "window_s", "outcome", "verdict_rc", "verdict_path",
    "painter_run_id", "av_expected_ms", "av_judged_cameras",
    "source_spread_ms", "delivery_spread_ms", "av_spread_ms", "av_spread_cams",
)
_CAM_FIELDS = (
    "av_{c}_ms", "av_{c}_status", "av_{c}_gate_pass",
    "loss_{c}_frames", "loss_{c}_copies", "loss_{c}_gaps", "loss_{c}_undecodable", "loss_{c}_pass",
    "burn_{c}_zero_loss", "burn_{c}_real_drops",
)


class BoundsError(Exception):
    """A gate bound could not be read from its single source (fail closed)."""


class CsvSchemaError(Exception):
    """The CSV header does not match the row being appended (a mid-run camera-set change)."""


# --- bounds -------------------------------------------------------------------------------------


def parse_rust_f64_const(text, name):
    """`pub const NAME: f64 = <number>;` -> float, or None when absent. Exact-name match."""
    m = re.search(r"\bpub\s+const\s+" + re.escape(name) + r"\s*:\s*f64\s*=\s*([-+0-9._eE]+)\s*;",
                  text or "")
    if not m:
        return None
    try:
        return float(m.group(1).replace("_", ""))
    except ValueError:
        return None


def default_repo_root():
    return os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))


def load_gate_bounds(repo_root=None):
    """Read both gate bounds from their Rust sources. Raises BoundsError on any miss."""
    root = repo_root or default_repo_root()
    out = {"sources": {}}
    for key, (rel, name) in _BOUND_SOURCES.items():
        path = os.path.join(root, rel)
        try:
            with open(path, encoding="utf-8") as f:
                text = f.read()
        except OSError as e:
            raise BoundsError(f"cannot read {path}: {e}") from e
        val = parse_rust_f64_const(text, name)
        if val is None or not math.isfinite(val) or val <= 0:
            raise BoundsError(f"{name} not found as a positive `pub const {name}: f64` in {path}")
        out[key] = val
        out["sources"][key] = f"{rel} {name}"
    return out


# --- one CSV row per window ---------------------------------------------------------------------


def utc_iso(epoch_s):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(float(epoch_s)))


def _validate_cams(cams):
    cams = list(cams)
    if not cams:
        raise ValueError("at least one camera is required")
    for c in cams:
        if not _CAM_RE.match(str(c)):
            raise ValueError(f"invalid camera name {c!r} (expected camN)")
    if len(set(cams)) != len(cams):
        raise ValueError(f"duplicate camera in {cams}")
    return cams


def csv_fields(cams):
    cams = _validate_cams(cams)
    fields = list(_BASE_FIELDS)
    for c in cams:
        fields.extend(t.format(c=c) for t in _CAM_FIELDS)
    return fields


def cams_from_fields(fields):
    out = []
    for f in fields:
        m = re.match(r"^av_(cam[0-9]+)_ms$", f)
        if m:
            out.append(m.group(1))
    return out


def _num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def _fmt_ms(v):
    return format(float(v), ".3f") if _num(v) else ""


def _fmt_int(v):
    return str(int(v)) if _num(v) else ""


def _fmt_bool(v):
    return "true" if v is True else ("false" if v is False else "")


def _get(d, *path):
    for k in path:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def row_from_verdict(verdict, cams, meta):
    """ONE window -> ONE row (every value a string). `verdict` None = the window produced no
    verdict (decode/merge failed): the row keeps the window (so the cadence check sees it) with
    every measurement empty. Only a MEASURED A/V offset is a sample."""
    fields = csv_fields(cams)
    row = {f: "" for f in fields}
    epoch = meta.get("epoch_s")
    row["epoch_s"] = _fmt_int(epoch) if _num(epoch) else str(epoch or "")
    row["ts_utc"] = meta.get("ts_utc") or (utc_iso(epoch) if _num(epoch) else "")
    for k in ("slot", "window_s", "verdict_rc"):
        v = meta.get(k)
        row[k] = _fmt_int(v) if _num(v) else ("" if v is None else str(v))
    for k in ("outcome", "verdict_path", "painter_run_id"):
        v = meta.get(k)
        row[k] = "" if v is None else str(v)
    for c in cams:
        row[f"av_{c}_status"] = "absent"
    if not isinstance(verdict, dict):
        return row

    av = verdict.get("all_cambox_av_sync")
    av = av if isinstance(av, dict) else {}
    row["av_expected_ms"] = _fmt_ms(av.get("expected_ms"))
    row["av_judged_cameras"] = _fmt_int(av.get("judged_cameras"))
    spread_vals = []
    for c in cams:
        node = av.get(c)
        if not isinstance(node, dict):
            continue
        status = str(node.get("verdict") or "absent")
        row[f"av_{c}_status"] = status
        row[f"av_{c}_gate_pass"] = _fmt_bool(node.get("gate_pass"))
        off = node.get("av_offset_ms")
        if status == "measured" and _num(off):
            row[f"av_{c}_ms"] = _fmt_ms(off)
            if c not in AV_SPREAD_EXCLUDED_CAMS:
                spread_vals.append(float(off))
    row["av_spread_cams"] = str(len(spread_vals))
    if len(spread_vals) >= 2:
        row["av_spread_ms"] = _fmt_ms(max(spread_vals) - min(spread_vals))
    row["source_spread_ms"] = _fmt_ms(_get(verdict, "all_cambox_latency", "cross_camera_spread_ms"))
    row["delivery_spread_ms"] = _fmt_ms(
        _get(verdict, "all_cambox_delivery_latency", "cross_camera_spread_ms"))

    segs = _get(verdict, "all_cambox_continuity", "segments")
    agg = {}
    for seg in segs if isinstance(segs, list) else []:
        if not isinstance(seg, dict):
            continue
        cam = str(seg.get("cambox", "")).lower()
        if cam not in cams:
            continue
        a = agg.setdefault(cam, {"frames": 0, "copies": 0, "gaps": 0, "undecodable": 0,
                                 "pass": True})
        for k in ("frames", "copies", "gaps", "undecodable"):
            v = seg.get(k)
            a[k] += int(v) if _num(v) else 0
        a["pass"] = a["pass"] and seg.get("pass") is True
    for c, a in agg.items():
        for k in ("frames", "copies", "gaps", "undecodable"):
            row[f"loss_{c}_{k}"] = str(a[k])
        row[f"loss_{c}_pass"] = _fmt_bool(a["pass"])

    loss = _get(verdict, "full_chain", "loss")
    for c in cams:
        node = loss.get(c) if isinstance(loss, dict) else None
        if isinstance(node, dict):
            row[f"burn_{c}_zero_loss"] = _fmt_bool(node.get("zero_loss"))
            row[f"burn_{c}_real_drops"] = _fmt_int(node.get("real_drops"))
    return row


def append_row(csv_path, row, cams):
    """Append ONE row; write the header on the first row; refuse a header mismatch."""
    fields = csv_fields(cams)
    exists = os.path.exists(csv_path) and os.path.getsize(csv_path) > 0
    if exists:
        with open(csv_path, newline="", encoding="utf-8") as f:
            header = next(csv.reader(f), [])
        if header != fields:
            raise CsvSchemaError(
                f"{csv_path}: header has {len(header)} columns, this row has {len(fields)} "
                f"(cameras {cams}) -- a mid-run camera-set change; start a new run dir")
    with open(csv_path, "a", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        if not exists:
            w.writeheader()
        w.writerow({k: row.get(k, "") for k in fields})


def read_rows(csv_path):
    with open(csv_path, newline="", encoding="utf-8") as f:
        r = csv.DictReader(f)
        rows = list(r)
        return list(r.fieldnames or []), rows


# --- statistics ---------------------------------------------------------------------------------


def _f(s):
    try:
        v = float(s)
    except (TypeError, ValueError):
        return None
    return v if math.isfinite(v) else None


def fit_slope_ms_per_h(points):
    """Least-squares slope of [(epoch_s, value_ms)] in ms per hour, or None when degenerate."""
    pts = [(float(t), float(v)) for t, v in points]
    if len(pts) < 2:
        return None
    t0 = pts[0][0]
    xs = [(t - t0) / 3600.0 for t, _ in pts]
    ys = [v for _, v in pts]
    mx = sum(xs) / len(xs)
    my = sum(ys) / len(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx <= 0:
        return None
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx


def _max_gap(epochs, run_start, run_end):
    """Largest gap between consecutive sample epochs, including the run edges."""
    if not epochs:
        return None
    pts = sorted(epochs)
    gaps = [pts[0] - run_start, run_end - pts[-1]]
    gaps += [b - a for a, b in zip(pts, pts[1:])]
    return max(gaps)


def _value_series(name, points, bound, bound_label, run_start, run_end, max_gap_s, slope_bound,
                  residual=None):
    """Grade one value series. `points` = [(epoch, value)]; `residual(epoch, value)` -> the value
    compared with `bound` (defaults to the value itself); None residual = an ungradable sample."""
    residual = residual or (lambda _t, v: v)
    breaches = 0
    ungradable = 0
    worst = None
    for t, v in points:
        r = residual(t, v)
        if r is None:
            ungradable += 1
            continue
        if worst is None or abs(r) > abs(worst):
            worst = r
        if abs(r) > bound:
            breaches += 1
    vals = [v for _, v in points]
    span = (points[-1][0] - points[0][0]) if points else 0
    slope = None
    if len(points) >= MIN_SLOPE_SAMPLES and span >= MIN_SLOPE_SPAN_S:
        slope = fit_slope_ms_per_h(points)
    gap = _max_gap([t for t, _ in points], run_start, run_end)
    s = {
        "name": name, "graded": True, "n": len(points),
        "min": min(vals) if vals else None, "max": max(vals) if vals else None,
        "last": vals[-1] if vals else None, "worst_residual": worst, "bound": bound,
        "bound_label": bound_label, "breaches": breaches, "ungradable": ungradable,
        "slope_ms_per_h": slope, "max_gap_s": gap,
    }
    if not points:
        s["verdict"], s["reason"] = UNKNOWN, "no samples"
    elif breaches:
        s["verdict"] = FAIL
        s["reason"] = f"{breaches} sample(s) outside {bound_label} {bound:g} ms"
    elif slope is not None and abs(slope) > slope_bound:
        s["verdict"] = FAIL
        s["reason"] = f"slope {slope:+.2f} ms/h outside +/-{slope_bound:g} ms/h"
    elif ungradable:
        s["verdict"] = UNKNOWN
        s["reason"] = f"{ungradable} sample(s) without an expected offset"
    elif slope is None:
        s["verdict"] = UNKNOWN
        s["reason"] = (f"slope needs >= {MIN_SLOPE_SAMPLES} samples over >= "
                       f"{MIN_SLOPE_SPAN_S // 60} min (have {len(points)} over {span / 60:.0f} min)")
    elif gap is not None and gap > max_gap_s:
        s["verdict"] = UNKNOWN
        s["reason"] = f"sample gap {gap:.0f} s > {max_gap_s:g} s"
    else:
        s["verdict"], s["reason"] = PASS, ""
    return s


def _bool_series(name, points, run_start, run_end, max_gap_s, required, extra=None):
    """Grade a per-window boolean series (loss / burn loss). points = [(epoch, bool)]."""
    fails = sum(1 for _, ok in points if not ok)
    gap = _max_gap([t for t, _ in points], run_start, run_end)
    s = {"name": name, "graded": True, "n": len(points), "breaches": fails, "max_gap_s": gap,
         "slope_ms_per_h": None}
    s.update(extra or {})
    if fails:
        s["verdict"], s["reason"] = FAIL, f"{fails} window(s) with loss"
    elif not points:
        s["verdict"] = UNKNOWN if required else NOT_MEASURED
        s["reason"] = "no samples" if required else "never measured (needs the capture burns)"
    elif required and gap is not None and gap > max_gap_s:
        s["verdict"], s["reason"] = UNKNOWN, f"sample gap {gap:.0f} s > {max_gap_s:g} s"
    else:
        s["verdict"], s["reason"] = PASS, ""
    return s


# --- the verdict --------------------------------------------------------------------------------


def evaluate(rows, cams, *, bounds, min_duration_s, max_gap_s=DEFAULT_MAX_GAP_S,
             slope_bound=SLOPE_BOUND_MS_PER_H, spread_columns=DEFAULT_SPREAD_COLUMNS,
             scope="full"):
    """Grade the rows (CSV dicts, string values). Returns the report dict for ONE scope."""
    for c in spread_columns:
        if c not in SPREAD_COLUMNS:
            raise ValueError(f"unknown spread column {c!r} (expected one of {SPREAD_COLUMNS})")
    rows = sorted((r for r in rows if _f(r.get("epoch_s")) is not None),
                  key=lambda r: _f(r["epoch_s"]))
    tol = bounds["av_tolerance_ms"]
    thr = bounds["spread_threshold_ms"]
    rep = {"scope": scope, "windows": len(rows),
           "windows_ok": sum(1 for r in rows if r.get("outcome") == "ok"),
           "bounds": {"av_tolerance_ms": tol, "spread_threshold_ms": thr,
                      "slope_bound_ms_per_h": slope_bound, "max_gap_s": max_gap_s,
                      "sources": dict(bounds.get("sources", {}))},
           "spread_columns": list(spread_columns), "min_duration_s": min_duration_s,
           "series": {}, "reasons": []}
    if not rows:
        rep.update(verdict=UNKNOWN, first_ts=None, last_ts=None, span_s=0)
        rep["reasons"].append("no windows in the CSV")
        return rep
    start = _f(rows[0]["epoch_s"])
    end = _f(rows[-1]["epoch_s"])
    rep.update(first_ts=rows[0].get("ts_utc"), last_ts=rows[-1].get("ts_utc"), span_s=end - start)

    series = rep["series"]
    for c in cams:
        statuses = [r.get(f"av_{c}_status", "") for r in rows]
        excluded = bool(statuses) and all(s == "excluded" for s in statuses)
        pts = []
        expected = {}
        for r in rows:
            v = _f(r.get(f"av_{c}_ms"))
            if r.get(f"av_{c}_status") == "measured" and v is not None:
                t = _f(r["epoch_s"])
                pts.append((t, v))
                expected[t] = _f(r.get("av_expected_ms"))

        def av_residual(t, v, _exp=expected):
            e = _exp.get(t)
            return None if e is None else v - e

        s = _value_series(f"av {c}", pts, tol, "A/V |offset - expected|", start, end, max_gap_s,
                          slope_bound, residual=av_residual)
        if excluded and not pts:
            s.update(verdict=EXCLUDED, graded=False, reason="operator-excluded in every window")
        series[s["name"]] = s

        lpts = []
        tot = {"copies": 0, "gaps": 0, "undecodable": 0, "frames": 0}
        for r in rows:
            p = r.get(f"loss_{c}_pass")
            if p in ("true", "false"):
                lpts.append((_f(r["epoch_s"]), p == "true"))
                for k in tot:
                    tot[k] += int(_f(r.get(f"loss_{c}_{k}")) or 0)
        ls = _bool_series(f"loss {c}", lpts, start, end, max_gap_s, required=not excluded,
                          extra=tot)
        if excluded and not lpts:
            ls.update(verdict=EXCLUDED, graded=False, reason="operator-excluded in every window")
        series[ls["name"]] = ls

        bpts = [(_f(r["epoch_s"]), r.get(f"burn_{c}_zero_loss") == "true") for r in rows
                if r.get(f"burn_{c}_zero_loss") in ("true", "false")]
        drops = sum(int(_f(r.get(f"burn_{c}_real_drops")) or 0) for r in rows)
        series[f"burn {c}"] = _bool_series(f"burn {c}", bpts, start, end, max_gap_s,
                                           required=False, extra={"real_drops": drops})

    for col in SPREAD_COLUMNS:
        pts = [(_f(r["epoch_s"]), _f(r.get(col))) for r in rows if _f(r.get(col)) is not None]
        s = _value_series(f"spread {col}", pts, thr, "cross-camera spread", start, end,
                          max_gap_s, slope_bound)
        if col not in spread_columns:
            s.update(graded=False, verdict=REPORTED,
                     reason=f"reported only ({s['reason'] or 'within bound'})")
        series[s["name"]] = s

    graded = [s for s in series.values() if s["verdict"] in (PASS, FAIL, UNKNOWN)]
    fails = [s for s in graded if s["verdict"] == FAIL]
    unknowns = [s for s in graded if s["verdict"] == UNKNOWN]
    rep["reasons"].extend(f"{s['name']}: {s['reason']}" for s in fails)
    short = rep["span_s"] < min_duration_s - START_JITTER_ALLOWANCE_S
    if short:
        rep["reasons"].append(
            f"run shorter than required: window starts span {rep['span_s'] / 3600:.2f} h < "
            f"{min_duration_s / 3600:.2f} h")
    rep["reasons"].extend(f"{s['name']}: {s['reason']}" for s in unknowns)
    if fails:
        rep["verdict"] = FAIL
    elif short or unknowns:
        rep["verdict"] = UNKNOWN
    else:
        rep["verdict"] = PASS
    return rep


def evaluate_scopes(rows, cams, *, bounds, partial_h=DEFAULT_PARTIAL_H,
                    min_duration_h=DEFAULT_MIN_DURATION_H, max_gap_s=DEFAULT_MAX_GAP_S,
                    slope_bound=SLOPE_BOUND_MS_PER_H, spread_columns=DEFAULT_SPREAD_COLUMNS):
    """The 1 h partial (its own verdict, graded as a 1 h run) + the full run."""
    kw = dict(bounds=bounds, max_gap_s=max_gap_s, slope_bound=slope_bound,
              spread_columns=spread_columns)
    full = evaluate(rows, cams, min_duration_s=min_duration_h * 3600, scope="full", **kw)
    out = {"full": full, "partial": None}
    epochs = [e for e in (_f(r.get("epoch_s")) for r in rows) if e is not None]
    if epochs and partial_h:
        cut = min(epochs) + partial_h * 3600 + START_JITTER_ALLOWANCE_S
        prows = [r for r in rows if (_f(r.get("epoch_s")) or math.inf) <= cut]
        if len(prows) < len(epochs):
            out["partial"] = evaluate(prows, cams, min_duration_s=partial_h * 3600,
                                      scope=f"partial (first {partial_h:g} h)", **kw)
    return out


def exit_code(report):
    return _EXIT.get(report.get("verdict"), 2)


# --- rendering ----------------------------------------------------------------------------------


def _n(v, fmt="{:+.2f}"):
    return "-" if v is None else fmt.format(v)


def _render_one(rep):
    b = rep["bounds"]
    src = b.get("sources", {})
    lines = [
        f"AV-SOAK {rep['scope'].upper()}: {rep['windows']} window(s) ({rep['windows_ok']} decoded), "
        f"{rep.get('first_ts') or '-'} .. {rep.get('last_ts') or '-'} "
        f"(span {rep.get('span_s', 0) / 3600:.2f} h, required {rep['min_duration_s'] / 3600:.2f} h)",
        f"  bounds: A/V |offset - expected| <= {b['av_tolerance_ms']:g} ms "
        f"({src.get('av_tolerance_ms', '?')}); spread <= {b['spread_threshold_ms']:g} ms "
        f"({src.get('spread_threshold_ms', '?')}); |slope| <= {b['slope_bound_ms_per_h']:g} ms/h; "
        f"sample gap <= {b['max_gap_s']:g} s; graded spread: {', '.join(rep['spread_columns'])}",
        f"  {'series':<30} {'n':>3} {'min':>9} {'max':>9} {'last':>9} {'slope/h':>9} "
        f"{'gap s':>6}  verdict",
    ]
    for s in rep["series"].values():
        if s["name"].startswith(("loss ", "burn ")):
            detail = (f"copies={s.get('copies', '-')} gaps={s.get('gaps', '-')} "
                      f"undecodable={s.get('undecodable', '-')}" if s["name"].startswith("loss ")
                      else f"real_drops={s.get('real_drops', 0)}")
            lines.append(f"  {s['name']:<30} {s['n']:>3} {detail:<39} "
                         f"{_n(s['max_gap_s'], '{:.0f}'):>6}  {s['verdict']}"
                         + (f" -- {s['reason']}" if s.get("reason") else ""))
            continue
        lines.append(
            f"  {s['name']:<30} {s['n']:>3} {_n(s['min'], '{:+.1f}'):>9} "
            f"{_n(s['max'], '{:+.1f}'):>9} {_n(s['last'], '{:+.1f}'):>9} "
            f"{_n(s['slope_ms_per_h']):>9} {_n(s['max_gap_s'], '{:.0f}'):>6}  {s['verdict']}"
            + (f" -- {s['reason']}" if s.get("reason") else ""))
    lines.append(f"  VERDICT: {rep['verdict']}")
    for r in rep["reasons"]:
        lines.append(f"    - {r}")
    return "\n".join(lines)


def render_text(scopes):
    parts = []
    if scopes.get("partial"):
        parts.append(_render_one(scopes["partial"]))
    parts.append(_render_one(scopes["full"]))
    return "\n\n".join(parts)


# --- CLI ----------------------------------------------------------------------------------------


def _cmd_bounds(a):
    b = load_gate_bounds(a.repo_root)
    print(f"av_tolerance_ms={b['av_tolerance_ms']} spread_threshold_ms={b['spread_threshold_ms']} "
          f"slope_bound_ms_per_h={SLOPE_BOUND_MS_PER_H} max_gap_s={DEFAULT_MAX_GAP_S}")
    for k, v in b["sources"].items():
        print(f"  {k} <- {v}")
    return 0


def _cmd_row(a):
    cams = a.cams.split()
    verdict = None
    outcome = a.outcome
    if a.verdict_json:
        try:
            with open(a.verdict_json, encoding="utf-8") as f:
                verdict = json.load(f)
        except (OSError, ValueError) as e:
            print(f"av-soak row: verdict JSON unreadable ({e}) -- recording the window without "
                  "measurements", file=sys.stderr)
            verdict = None
            if outcome == "ok":
                outcome = "no_verdict:unreadable_json"
    meta = {"epoch_s": a.epoch_s, "slot": a.slot, "window_s": a.window_s, "outcome": outcome,
            "verdict_rc": a.verdict_rc, "verdict_path": a.verdict_path or a.verdict_json or "",
            "painter_run_id": a.painter_run_id}
    row = row_from_verdict(verdict, cams, meta)
    append_row(a.csv, row, cams)
    avs = " ".join(f"{c}={row[f'av_{c}_ms'] or row[f'av_{c}_status']}" for c in cams)
    print(f"av-soak row slot={row['slot']} {row['ts_utc']} outcome={row['outcome']} A/V[{avs}] "
          f"spread source={row['source_spread_ms'] or '-'} delivery={row['delivery_spread_ms'] or '-'}"
          f" av={row['av_spread_ms'] or '-'}")
    return 0


def _cmd_report(a):
    fields, rows = read_rows(a.csv)
    cams = cams_from_fields(fields)
    if not cams:
        raise ValueError(f"{a.csv}: no av_<cam>_ms columns")
    spread_columns = tuple(c for c in a.spread_columns.split(",") if c)
    scopes = evaluate_scopes(rows, cams, bounds=load_gate_bounds(a.repo_root),
                             partial_h=a.partial_h, min_duration_h=a.min_duration_h,
                             max_gap_s=a.max_gap_s, slope_bound=a.slope_bound,
                             spread_columns=spread_columns)
    print(render_text(scopes))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump(scopes, f, indent=2, sort_keys=True)
    return exit_code(scopes["full"])


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1367 A/V soak decision core")
    ap.add_argument("--repo-root", default=None, help="repo root holding src/*.rs (default: ..)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("bounds", help="print the gate bounds read from their Rust sources")

    r = sub.add_parser("row", help="append ONE window's row to the CSV")
    r.add_argument("--csv", required=True)
    r.add_argument("--cams", required=True, help='space-separated, e.g. "cam1 cam2 cam3"')
    g = r.add_mutually_exclusive_group(required=True)
    g.add_argument("--verdict-json")
    g.add_argument("--no-verdict", action="store_true")
    r.add_argument("--epoch-s", type=float, required=True)
    r.add_argument("--slot", type=int, required=True)
    r.add_argument("--window-s", type=int, required=True)
    r.add_argument("--outcome", default="ok")
    r.add_argument("--verdict-rc", type=int, default=None)
    r.add_argument("--verdict-path", default="")
    r.add_argument("--painter-run-id", default="")

    p = sub.add_parser("report", help="grade the CSV (1 h partial + full)")
    p.add_argument("--csv", required=True)
    p.add_argument("--json", default="")
    p.add_argument("--min-duration-h", type=float, default=DEFAULT_MIN_DURATION_H)
    p.add_argument("--partial-h", type=float, default=DEFAULT_PARTIAL_H)
    p.add_argument("--max-gap-s", type=float, default=DEFAULT_MAX_GAP_S)
    p.add_argument("--slope-bound", type=float, default=SLOPE_BOUND_MS_PER_H)
    p.add_argument("--spread-columns", default=",".join(DEFAULT_SPREAD_COLUMNS))
    a = ap.parse_args(argv)
    try:
        return {"bounds": _cmd_bounds, "row": _cmd_row, "report": _cmd_report}[a.cmd](a)
    except (BoundsError, CsvSchemaError, ValueError, OSError) as e:
        print(f"av-soak {a.cmd}: ERROR: {e}", file=sys.stderr)
        return EXIT_USAGE


if __name__ == "__main__":
    sys.exit(main())
