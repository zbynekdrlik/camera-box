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
- **loss per camera** (`loss <cam>`): the term the gate itself folds for each of the camera's windows
  in `all_cambox_continuity.segments` (`gate_window_term`): src/window_gate.rs
  `decide_with_tolerance(...).overall_pass_term` with the issue-1367 multi-source scope
  (src/multi_source_window.rs `scoped_continuity_term`: a `multi_source` window's copies/gaps are
  dropped, its presence + optical floor still gate), evaluated with the verdict's OWN serialized
  seam flags (`copies_gaps_tolerance_gates_overall_pass`, the singleton allowance, the per-window
  undecodable floor + `undecodable_floor_gates_overall_pass`). On real runs the AND of these terms is
  exactly the verdict's `overall_pass` (tests/python/fixtures/av_soak). An older verdict without the
  flags falls back to `relaxed_pass`, then `pass` (a multi-source window there is `report_only`: a
  sample, never a breach; an all-`report_only` series is REPORTED, never a PASS). The strict `pass`
  and the raw copies/gaps/undecodable are recorded and reported.
- **continuity gate**: the verdict's OWN `all_cambox_continuity.overall_pass` per window. It adds
  what the per-window term cannot see: the run-wide undecodable sum over `RUN_UNDECODABLE_FLOOR`
  (src/probe/recording_segments.rs `overall_pass &= run_wide_undecodable_within_floor ||
  !optical_floor::gates_overall_pass()`, recorded as `loss_run_wide_pass`) and an empty schedule.
  A `false` window fails; a verdict without the field leaves the series NOT_MEASURED. A window
  where the soak's mirrored terms (every camera's loss term AND the run-wide term) disagree with
  the fold is counted (`gate_term_mismatch_windows`) and named: the Python copy of the Rust gate
  term drifted. A mismatch makes the run at least UNKNOWN (a FAIL still wins).
- **burn loss** (`burn <node>`): `full_chain.loss.<node>.zero_loss` for the `strih` + `stream` hops
  (the OBS measurement burns the soak turns on and requires -- never measured is UNKNOWN) and for
  every camera (only with the capture burns: never measured is not required). A measured `false`
  fails; once measured, a gap counts.
- **slope** of every value series: least squares over (hours, value) with its standard error and
  the two-sided 95 % Student-t quantile for its n - 2 degrees of freedom (`t95`): FAIL only when
  `|slope| - t*SE > 2 ms/h` (`SLOPE_BOUND_MS_PER_H`, the acceptance on issue 1367), PASS only when
  `|slope| + t*SE <= 2 ms/h`, UNKNOWN when the interval straddles the bound (per-window noise alone
  over 1 h cannot prove either). Needs >= 3 samples over >= 30 min.
- **cadence**: the acceptance asks for a sample at least every slot (10 min), so every required
  series must have no gap longer than the slot + 60 s start-jitter allowance (the slot is read from
  the CSV's `slot_s`; one missed slot is a 2-slot gap), counting the gap from the run's first window
  to the series' first sample and from its last sample to the last window.
- **duration**: the run's window starts must span `min_duration - 60 s`.
- **a camera graded**: a run where no camera's A/V series was graded (every camera
  operator-excluded) is UNKNOWN -- the verdict's own zero-judged-cameras floor.
- The windows the E2E gate itself failed (`verdict_rc` != 0 on a decoded window) are counted and
  printed; informational, the soak grades its own series.

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
prints the 1 h partial once the CSV covers the first hour, and the full run; `--json` writes both).
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
# The slope is judged with its uncertainty: the two-sided 95 % Student-t quantile for n - 2 degrees
# of freedom (2 SE would over-call a slope from 3-4 samples). Table values; a dof between two rows
# takes the smaller dof's (larger, conservative) quantile.
_T95 = ((1, 12.706), (2, 4.303), (3, 3.182), (4, 2.776), (5, 2.571), (6, 2.447), (7, 2.365),
        (8, 2.306), (9, 2.262), (10, 2.228), (12, 2.179), (15, 2.131), (20, 2.086), (25, 2.060),
        (30, 2.042), (40, 2.021), (60, 2.000), (120, 1.980))
_T95_INF = 1.960
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

# The two OBS hops whose own measurement burns (strih 911002 / stream 911004) the soak turns on.
HOP_NODES = ("strih", "stream")

_CAM_RE = re.compile(r"^cam[0-9]+$")
_BASE_FIELDS = (
    "ts_utc", "epoch_s", "slot", "slot_s", "window_s", "outcome", "verdict_rc", "verdict_path",
    "painter_run_id", "av_expected_ms", "av_judged_cameras",
    "source_spread_ms", "delivery_spread_ms", "av_spread_ms", "av_spread_cams",
    "cont_overall_pass", "loss_run_wide_pass", "loss_run_wide_undecodable",
) + tuple(f"burn_{n}_{k}" for n in HOP_NODES for k in ("zero_loss", "real_drops"))
_CAM_FIELDS = (
    "av_{c}_ms", "av_{c}_status", "av_{c}_gate_pass",
    "loss_{c}_frames", "loss_{c}_copies", "loss_{c}_gaps", "loss_{c}_undecodable", "loss_{c}_pass",
    "loss_{c}_strict_pass", "loss_{c}_multi_source",
    "burn_{c}_zero_loss", "burn_{c}_real_drops",
)
_LOSS_SAMPLE = ("true", "false", "report_only")
# The verdict's own seam flags the per-window gate term needs (all_cambox_continuity).
_GATE_TERM_KEYS = ("copies_gaps_tolerance_gates_overall_pass", "undecodable_floor_gates_overall_pass",
                   "per_window_undecodable_floor")


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
    for k in ("slot", "slot_s", "window_s", "verdict_rc"):
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

    cont = _get(verdict, "all_cambox_continuity")
    cont = cont if isinstance(cont, dict) else {}
    if "overall_pass" in cont:
        row["cont_overall_pass"] = _fmt_bool(cont.get("overall_pass"))
    row["loss_run_wide_pass"] = _fmt_bool(run_wide_term(cont))
    if isinstance(cont.get("segments"), list):
        row["loss_run_wide_undecodable"] = str(sum(_int(g.get("undecodable"))
                                                   for g in cont["segments"] if isinstance(g, dict)))
    for c, a in _loss_by_camera(cont.get("segments"), cams, cont).items():
        for k in ("frames", "copies", "gaps", "undecodable"):
            row[f"loss_{c}_{k}"] = str(a[k])
        row[f"loss_{c}_strict_pass"] = _fmt_bool(a["strict"])
        row[f"loss_{c}_multi_source"] = _fmt_bool(a["multi"])
        if a["graded"] is not None:
            row[f"loss_{c}_pass"] = _fmt_bool(a["graded"])
        elif a["multi"]:
            row[f"loss_{c}_pass"] = "report_only"

    loss = _get(verdict, "full_chain", "loss")
    for node_name in list(cams) + list(HOP_NODES):
        node = loss.get(node_name) if isinstance(loss, dict) else None
        if isinstance(node, dict):
            row[f"burn_{node_name}_zero_loss"] = _fmt_bool(node.get("zero_loss"))
            row[f"burn_{node_name}_real_drops"] = _fmt_int(node.get("real_drops"))
    return row


def _int(v):
    return int(v) if _num(v) else 0


def gate_window_term(seg, cont):
    """The per-window term the gate folds into `all_cambox_continuity.overall_pass`, from the
    verdict's OWN serialized seam flags: src/window_gate.rs `decide_with_tolerance(frames,
    undecodable, copies, gaps, tolerance).overall_pass_term`, with src/multi_source_window.rs
    `scoped_continuity_term` dropping a multi-source window's copies/gaps. None when the verdict
    does not carry the flags (an older verdict)."""
    if not isinstance(seg, dict) or not isinstance(cont, dict):
        return None
    if not all(k in cont for k in _GATE_TERM_KEYS):
        return None
    frames = _int(seg.get("frames"))
    undecodable = _int(seg.get("undecodable"))
    copies, gaps = _int(seg.get("copies")), _int(seg.get("gaps"))
    if seg.get("multi_source"):
        copies = gaps = 0
    floor_ok = (cont["undecodable_floor_gates_overall_pass"] is not True
                or (frames > 0 and undecodable <= _int(cont["per_window_undecodable_floor"])))
    if cont["copies_gaps_tolerance_gates_overall_pass"] is True:
        tol = seg.get("copies_gaps_tolerance", cont.get("copies_gaps_tolerance"))
        copies_gaps_ok = _num(tol) and copies <= tol and gaps <= tol
    elif cont.get("segment_singleton_allowance_gates_overall_pass") is True:
        copies_gaps_ok = (copies <= _int(cont.get("segment_singleton_copies_allowance"))
                          and gaps <= _int(cont.get("segment_singleton_gaps_allowance")))
    else:
        copies_gaps_ok = copies == 0 and gaps == 0
    return frames > 0 and floor_ok and bool(copies_gaps_ok)


def run_wide_term(cont):
    """The run-wide half of the gate's continuity fold: src/probe/recording_segments.rs
    `overall_pass &= run_wide_undecodable_within_floor || !optical_floor::gates_overall_pass()`
    (the verdict serializes the latter as `undecodable_floor_gates_overall_pass`). None when the
    verdict does not carry both flags."""
    if not isinstance(cont, dict):
        return None
    if "run_wide_undecodable_within_floor" not in cont \
            or "undecodable_floor_gates_overall_pass" not in cont:
        return None
    return (cont["run_wide_undecodable_within_floor"] is True
            or cont["undecodable_floor_gates_overall_pass"] is not True)


def gate_term_disagrees(row, cams):
    """True when the soak's mirrored terms for one window (every camera's loss term AND the
    run-wide term) differ from the verdict's own `cont_overall_pass`; False when they agree; None
    when the row cannot be compared (no fold recorded, or a camera without a loss term)."""
    fold = row.get("cont_overall_pass")
    if fold not in ("true", "false"):
        return None
    terms = [row.get(f"loss_{c}_pass") for c in cams]
    if any(t not in _LOSS_SAMPLE for t in terms):
        return None
    mine = all(t != "false" for t in terms) and row.get("loss_run_wide_pass") != "false"
    return mine != (fold == "true")


def _loss_by_camera(segments, cams, cont=None):
    """Aggregate one window's `all_cambox_continuity.segments` per camera (a camera may own several
    cycled segments). `graded` = the AND of the gate's own per-window term (`gate_window_term`) over
    the camera's segments; on an older verdict without the seam flags, the AND of `relaxed_pass`
    (else `pass`) over its non-multi-source segments, None when every segment was multi-source.
    `strict` = the AND of the strict `pass` (reported only). Unlike the Discord report's strict
    aggregation, this grades the gate's own per-window term."""
    agg = {}
    for seg in segments if isinstance(segments, list) else []:
        if not isinstance(seg, dict):
            continue
        cam = str(seg.get("cambox", "")).lower()
        if cam not in cams:
            continue
        a = agg.setdefault(cam, {"frames": 0, "copies": 0, "gaps": 0, "undecodable": 0,
                                 "strict": True, "graded": None, "multi": False})
        for k in ("frames", "copies", "gaps", "undecodable"):
            v = seg.get(k)
            a[k] += int(v) if _num(v) else 0
        a["strict"] = a["strict"] and seg.get("pass") is True
        if seg.get("multi_source"):
            a["multi"] = True
        term = gate_window_term(seg, cont)
        if term is None:
            if seg.get("multi_source"):
                continue
            term = seg.get("relaxed_pass") if "relaxed_pass" in seg else seg.get("pass")
        a["graded"] = (True if a["graded"] is None else a["graded"]) and term is True
    return agg


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


def fit_slope_with_se(points):
    """Least-squares slope of [(epoch_s, value_ms)] in ms per hour + its standard error.
    Returns (slope, se); slope None when degenerate (< 2 points or no time spread); se None with
    fewer than 3 points (no residual degree of freedom)."""
    pts = [(float(t), float(v)) for t, v in points]
    if len(pts) < 2:
        return None, None
    t0 = pts[0][0]
    xs = [(t - t0) / 3600.0 for t, _ in pts]
    ys = [v for _, v in pts]
    mx = sum(xs) / len(xs)
    my = sum(ys) / len(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx <= 0:
        return None, None
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    if len(pts) < 3:
        return slope, None
    intercept = my - slope * mx
    rss = sum((y - (intercept + slope * x)) ** 2 for x, y in zip(xs, ys))
    return slope, math.sqrt(max(rss, 0.0) / (len(pts) - 2) / sxx)


def t95(dof):
    """Two-sided 95 % Student-t quantile for `dof` degrees of freedom (conservative table lookup)."""
    if dof < 1:
        return math.inf
    if dof >= 1000:
        return _T95_INF
    for d, v in reversed(_T95):
        if dof >= d:
            return v
    return _T95[0][1]


def fit_slope_ms_per_h(points):
    """Least-squares slope of [(epoch_s, value_ms)] in ms per hour, or None when degenerate."""
    return fit_slope_with_se(points)[0]


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
    slope, se = None, None
    if len(points) >= MIN_SLOPE_SAMPLES and span >= MIN_SLOPE_SPAN_S:
        slope, se = fit_slope_with_se(points)
    band = None if se is None else t95(len(points) - 2) * se
    gap = _max_gap([t for t, _ in points], run_start, run_end)
    s = {
        "name": name, "graded": True, "n": len(points),
        "min": min(vals) if vals else None, "max": max(vals) if vals else None,
        "last": vals[-1] if vals else None, "worst_residual": worst, "bound": bound,
        "bound_label": bound_label, "breaches": breaches, "ungradable": ungradable,
        "slope_ms_per_h": slope, "slope_se_ms_per_h": se, "slope_band_ms_per_h": band,
        "max_gap_s": gap,
    }
    if not points:
        s["verdict"], s["reason"] = UNKNOWN, "no samples"
    elif breaches:
        s["verdict"] = FAIL
        s["reason"] = f"{breaches} sample(s) outside {bound_label} {bound:g} ms"
    elif slope is not None and band is not None and abs(slope) - band > slope_bound:
        s["verdict"] = FAIL
        s["reason"] = (f"slope {slope:+.2f} +/- {band:.2f} ms/h (95 % t band) is outside "
                       f"+/-{slope_bound:g} ms/h")
    elif ungradable:
        s["verdict"] = UNKNOWN
        s["reason"] = f"{ungradable} sample(s) without an expected offset"
    elif slope is None or band is None:
        s["verdict"] = UNKNOWN
        s["reason"] = (f"slope needs >= {MIN_SLOPE_SAMPLES} samples over >= "
                       f"{MIN_SLOPE_SPAN_S // 60} min (have {len(points)} over {span / 60:.0f} min)")
    elif abs(slope) + band > slope_bound:
        s["verdict"] = UNKNOWN
        s["reason"] = (f"slope {slope:+.2f} +/- {band:.2f} ms/h (95 % t band) straddles the "
                       f"+/-{slope_bound:g} ms/h bound -- not enough evidence either way")
    elif gap is not None and gap > max_gap_s:
        s["verdict"] = UNKNOWN
        s["reason"] = f"sample gap {gap:.0f} s > {max_gap_s:g} s"
    else:
        s["verdict"], s["reason"] = PASS, ""
    return s


def _state_series(name, points, run_start, run_end, max_gap_s, required, extra=None,
                  never_reason="no samples"):
    """Grade a per-window state series (loss / burn loss). points = [(epoch, state)] with state
    "true" (clean), "false" (a loss window) or "report_only" (measured, not gating)."""
    fails = sum(1 for _, st in points if st == "false")
    report_only = sum(1 for _, st in points if st == "report_only")
    gap = _max_gap([t for t, _ in points], run_start, run_end)
    s = {"name": name, "graded": True, "n": len(points), "breaches": fails,
         "report_only": report_only, "max_gap_s": gap, "slope_ms_per_h": None,
         "slope_se_ms_per_h": None, "slope_band_ms_per_h": None}
    s.update(extra or {})
    if fails:
        s["verdict"], s["reason"] = FAIL, f"{fails} window(s) with loss"
    elif points and report_only == len(points):
        s["graded"] = False
        s["verdict"], s["reason"] = REPORTED, "every window report-only (multi-source)"
    elif not points:
        s["verdict"] = UNKNOWN if required else NOT_MEASURED
        s["reason"] = never_reason
    elif gap is not None and gap > max_gap_s:
        s["verdict"], s["reason"] = UNKNOWN, f"sample gap {gap:.0f} s > {max_gap_s:g} s"
    else:
        s["verdict"], s["reason"] = PASS, ""
    return s


# --- the verdict --------------------------------------------------------------------------------


def max_gap_from_rows(rows):
    """The allowed sample gap for these rows: the largest `slot_s` written into the CSV + the start
    jitter allowance; the 600 s default slot when the CSV carries none."""
    slots = [v for v in (_f(r.get("slot_s")) for r in rows) if v is not None and v > 0]
    return (max(slots) if slots else DEFAULT_SLOT_S) + START_JITTER_ALLOWANCE_S


def evaluate(rows, cams, *, bounds, min_duration_s, max_gap_s=None,
             slope_bound=SLOPE_BOUND_MS_PER_H, spread_columns=DEFAULT_SPREAD_COLUMNS,
             scope="full"):
    """Grade the rows (CSV dicts, string values). Returns the report dict for ONE scope."""
    for c in spread_columns:
        if c not in SPREAD_COLUMNS:
            raise ValueError(f"unknown spread column {c!r} (expected one of {SPREAD_COLUMNS})")
    rows = sorted((r for r in rows if _f(r.get("epoch_s")) is not None),
                  key=lambda r: _f(r["epoch_s"]))
    if max_gap_s is None:
        max_gap_s = max_gap_from_rows(rows)
    tol = bounds["av_tolerance_ms"]
    thr = bounds["spread_threshold_ms"]
    ok_rows = [r for r in rows if r.get("outcome") == "ok"]
    rep = {"scope": scope, "windows": len(rows), "windows_ok": len(ok_rows),
           "gate_failed_windows": sum(1 for r in ok_rows
                                      if r.get("verdict_rc") not in (None, "", "0")),
           "bounds": {"av_tolerance_ms": tol, "spread_threshold_ms": thr,
                      "slope_bound_ms_per_h": slope_bound, "slope_band": "95 % t",
                      "max_gap_s": max_gap_s, "sources": dict(bounds.get("sources", {}))},
           "spread_columns": list(spread_columns), "min_duration_s": min_duration_s,
           "gate_term_mismatch_windows": 0, "series": {}, "reasons": []}
    if not rows:
        rep.update(verdict=UNKNOWN, first_ts=None, last_ts=None, span_s=0)
        rep["reasons"].append("no windows in the CSV")
        return rep
    start = _f(rows[0]["epoch_s"])
    end = _f(rows[-1]["epoch_s"])
    rep.update(first_ts=rows[0].get("ts_utc"), last_ts=rows[-1].get("ts_utc"), span_s=end - start)

    series = rep["series"]
    for c in cams:
        statuses = [r.get(f"av_{c}_status", "") for r in rows if r.get("outcome") == "ok"]
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
            if p in _LOSS_SAMPLE:
                lpts.append((_f(r["epoch_s"]), p))
                for k in tot:
                    tot[k] += int(_f(r.get(f"loss_{c}_{k}")) or 0)
        ls = _state_series(f"loss {c}", lpts, start, end, max_gap_s, required=not excluded,
                           extra=tot)
        if excluded and not lpts:
            ls.update(verdict=EXCLUDED, graded=False, reason="operator-excluded in every window")
        series[ls["name"]] = ls

    cpts = [(_f(r["epoch_s"]), r.get("cont_overall_pass")) for r in rows
            if r.get("cont_overall_pass") in ("true", "false")]
    series["continuity gate"] = _state_series(
        "continuity gate", cpts, start, end, max_gap_s, required=False,
        extra={"run_wide_breaches": sum(1 for r in rows if r.get("loss_run_wide_pass") == "false"),
               "undecodable": sum(int(_f(r.get("loss_run_wide_undecodable")) or 0) for r in rows)},
        never_reason="never measured (a verdict without all_cambox_continuity.overall_pass)")
    mismatch = sum(1 for r in ok_rows if gate_term_disagrees(r, cams) is True)
    rep["gate_term_mismatch_windows"] = mismatch
    if mismatch:
        rep["reasons"].append(
            f"the soak's mirrored loss terms disagree with the verdict's own "
            f"all_cambox_continuity.overall_pass in {mismatch} window(s) -- the Python copy of the "
            f"Rust gate term may have drifted (src/window_gate.rs, src/probe/recording_segments.rs)")

    for node in list(cams) + list(HOP_NODES):
        bpts = [(_f(r["epoch_s"]), r.get(f"burn_{node}_zero_loss")) for r in rows
                if r.get(f"burn_{node}_zero_loss") in ("true", "false")]
        drops = sum(int(_f(r.get(f"burn_{node}_real_drops")) or 0) for r in rows)
        why = ("never measured (needs the camera's capture burn)" if node in cams
               else "never measured (the OBS measurement burn the soak turned on is missing)")
        series[f"burn {node}"] = _state_series(f"burn {node}", bpts, start, end, max_gap_s,
                                               required=node in HOP_NODES,
                                               extra={"real_drops": drops}, never_reason=why)

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
    judged = [s for s in series.values() if s["name"].startswith("av ") and s["graded"]]
    if not judged:
        rep["reasons"].append("no camera was graded (every camera operator-excluded)")
    rep["reasons"].extend(f"{s['name']}: {s['reason']}" for s in unknowns)
    if fails:
        rep["verdict"] = FAIL
    elif short or unknowns or not judged or mismatch:
        # a mismatch without a failing series (only the run-wide mirror disagrees): the grading
        # itself cannot be trusted -- UNKNOWN, never a quiet PASS
        rep["verdict"] = UNKNOWN
    else:
        rep["verdict"] = PASS
    return rep


def evaluate_scopes(rows, cams, *, bounds, partial_h=DEFAULT_PARTIAL_H,
                    min_duration_h=DEFAULT_MIN_DURATION_H, max_gap_s=None,
                    slope_bound=SLOPE_BOUND_MS_PER_H, spread_columns=DEFAULT_SPREAD_COLUMNS):
    """The partial (the first `partial_h`, its own verdict graded as a `partial_h` run, present as
    soon as the CSV covers it) + the full run."""
    kw = dict(bounds=bounds, max_gap_s=max_gap_s, slope_bound=slope_bound,
              spread_columns=spread_columns)
    full = evaluate(rows, cams, min_duration_s=min_duration_h * 3600, scope="full", **kw)
    out = {"full": full, "partial": None}
    epochs = [e for e in (_f(r.get("epoch_s")) for r in rows) if e is not None]
    if epochs and partial_h:
        first = min(epochs)
        if max(epochs) - first >= partial_h * 3600 - START_JITTER_ALLOWANCE_S:
            cut = first + partial_h * 3600 + START_JITTER_ALLOWANCE_S
            prows = [r for r in rows if (_f(r.get("epoch_s")) or math.inf) <= cut]
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
        f"({src.get('spread_threshold_ms', '?')}); |slope| <= {b['slope_bound_ms_per_h']:g} ms/h "
        f"(judged +/- the 95 % t band); sample gap <= {b['max_gap_s']:g} s; "
        f"graded spread: {', '.join(rep['spread_columns']) or 'none'}",
        f"  the E2E gate itself failed {rep.get('gate_failed_windows', 0)} of {rep['windows_ok']} "
        f"decoded window(s) (its own verdict; informational)",
        f"  {'series':<30} {'n':>3} {'min':>9} {'max':>9} {'last':>9} {'slope/h':>9} {'band':>6} "
        f"{'gap s':>6}  verdict",
    ]
    for s in rep["series"].values():
        if s["name"].startswith(("loss ", "burn ", "continuity ")):
            if s["name"].startswith("loss "):
                detail = (f"copies={s.get('copies', '-')} gaps={s.get('gaps', '-')} "
                          f"undecodable={s.get('undecodable', '-')} "
                          f"report_only={s.get('report_only', 0)}")
            elif s["name"].startswith("continuity "):
                detail = (f"run_wide_breaches={s.get('run_wide_breaches', 0)} "
                          f"undecodable={s.get('undecodable', 0)}")
            else:
                detail = f"real_drops={s.get('real_drops', 0)}"
            lines.append(f"  {s['name']:<30} {s['n']:>3} {detail:<46} "
                         f"{_n(s['max_gap_s'], '{:.0f}'):>6}  {s['verdict']}"
                         + (f" -- {s['reason']}" if s.get("reason") else ""))
            continue
        band = s.get("slope_band_ms_per_h")
        lines.append(
            f"  {s['name']:<30} {s['n']:>3} {_n(s['min'], '{:+.1f}'):>9} "
            f"{_n(s['max'], '{:+.1f}'):>9} {_n(s['last'], '{:+.1f}'):>9} "
            f"{_n(s['slope_ms_per_h']):>9} {_n(band, '{:.2f}'):>6} "
            f"{_n(s['max_gap_s'], '{:.0f}'):>6}  {s['verdict']}"
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
          f"slope_bound_ms_per_h={SLOPE_BOUND_MS_PER_H} slope_band=t95(n-2)*SE "
          f"max_gap_s=slot+{START_JITTER_ALLOWANCE_S}")
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
    meta = {"epoch_s": a.epoch_s, "slot": a.slot, "slot_s": a.slot_s, "window_s": a.window_s,
            "outcome": outcome,
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
    r.add_argument("--slot-s", type=int, default=None, help="the slot length (sets the gap bound)")
    r.add_argument("--outcome", default="ok")
    r.add_argument("--verdict-rc", type=int, default=None)
    r.add_argument("--verdict-path", default="")
    r.add_argument("--painter-run-id", default="")

    p = sub.add_parser("report", help="grade the CSV (1 h partial + full)")
    p.add_argument("--csv", required=True)
    p.add_argument("--json", default="")
    p.add_argument("--min-duration-h", type=float, default=DEFAULT_MIN_DURATION_H)
    p.add_argument("--partial-h", type=float, default=DEFAULT_PARTIAL_H)
    p.add_argument("--max-gap-s", type=float, default=None,
                   help="default: the CSV's slot_s + the start-jitter allowance")
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
