#!/usr/bin/env python3
"""issue 1367 -- the PURE decision core of the RESTART MATRIX (scripts/av-restart-matrix.sh).

Owner goal (24.9.2026, on the issue): after EVERY restart -- stream OBS, strih OBS (strih-lx), a
cambox service, dantesync, a power cycle -- the picture-to-sound latency on the stream output is
right by itself within a bounded settle time, and the cameras stay in sync with each other. The
acceptance: "for each restart kind, the stream output meets the A/V and spread bounds within the
settle time, with no manual step, 3/3 repeats".

The orchestrator restarts one component, waits until it reports healthy (bounded), waits the settle
time and measures ONE window with the soak itself (`scripts/av-soak.sh --run --hours 0`), so every
window's evidence is the soak's own CSV row (`av_soak_decision.row_from_verdict`: the measured
per-camera A/V offsets, the spreads, the gate's own per-window loss term, the continuity fold, the
hop burns). This module grades that row POINTWISE -- one window is one sample, so there is no slope
and no cadence here (the soak's `evaluate` needs a series) -- with the SAME rules and the SAME
bounds the soak uses:

- **A/V per camera**: the MEASURED `av_<cam>_ms` only; `|offset - av_expected_ms| <=
  AV_OFFSET_GATE_TOLERANCE_MS` (inclusive). A camera the verdict did not measure is UNKNOWN. An
  operator-excluded camera is not REQUIRED (no A/V, no loss sample needed), but a measured loss or
  camera-burn `false` still fails -- the soak's own rule.
- **spread**: every graded spread column (default `av_spread_ms`, the camera alignment at the stream
  output, ROZHODNUTÉ 5860604301) `<= SPREAD_THRESHOLD_MS`; never measured = UNKNOWN.
- **loss per camera**: the gate's own per-window term the soak recorded (`loss_<cam>_pass`: `false`
  fails, `true` / `report_only` pass, missing is UNKNOWN); the verdict's own continuity fold
  (`cont_overall_pass` false fails); a mirrored-term mismatch (`gate_term_disagrees`) is UNKNOWN.
- **hop burns**: `burn_strih_zero_loss` / `burn_stream_zero_loss` are required (never measured =
  UNKNOWN, `false` fails); a camera burn fails only when measured `false`.
- A breach always wins over missing evidence.

The two bounds are READ from their Rust sources through `av_soak_decision.load_gate_bounds` (never
retyped). The matrix verdict:

- one **step** per restart repeat (+ the baseline), from `matrix.tsv`: `measured` -> the window's
  grade; `not_healthy` (the component did not report healthy within the bound) and `restart_failed`
  (the restart command itself failed: the unit did not start again) -> FAIL; `not_performed` (the
  restart was never done: no unit, ssh unreachable, the supervisor never confirmed the stream OBS
  step), `window_stopped` (the soak ended the window without a measurement: the rig left TEST
  mode, a record volume ran low), `window_refused` / `window_aborted` / `window_error` -> UNKNOWN,
  with the reason. time-to-healthy = healthy_epoch - restart_epoch.
- the **receiver** column (context, NEVER graded): the restarted camera's strih main input, read
  from the strih OBS log right before a cambox / dantesync restart -- `parked` (connect-on-show:
  the input was not connected, so the restart was not seen by a live receiver; the window's own
  connect-on-show hold then connects it fresh), `connected`, `unread`, or `n/a` (the strih OBS /
  stream OBS kinds). The report counts it per kind, so a 3/3 PASS whose restarts all hit a parked
  receiver says so.
- a **kind**: FAIL when any repeat fails, PASS when every required repeat (`repeats`, default 3)
  passed, else UNKNOWN (`k/3 repeats passed`).
- the **matrix**: FAIL when the baseline or any kind fails; PASS only when the baseline and every
  kind pass; else UNKNOWN. Exit 0 PASS / 1 FAIL / 2 UNKNOWN / 3 usage or input error.

Files (the orchestrator writes them into the matrix run dir): `matrix.conf` (key=value: kinds,
repeats, settle_s, healthy_timeout_s, spread_columns, ...) and `matrix.tsv` (one row per step,
`STEP_FIELDS`). Subcommands: `record` (append one step), `grade-window --window-dir W` (one window's
verdict, for the orchestrator's baseline gate), `report --dir D [--json F]`.
"""
import argparse
import csv
import json
import os
import sys

import av_soak_decision as soak

PASS, FAIL, UNKNOWN = soak.PASS, soak.FAIL, soak.UNKNOWN
_EXIT = {PASS: 0, FAIL: 1, UNKNOWN: 2}
EXIT_USAGE = 3

# The restart kinds, in the order the orchestrator runs them (the one declaration; the bash lib
# lists the same four and the orchestrator test pins that --kinds rejects anything else).
KINDS = ("strih-obs", "cambox", "dantesync", "stream-obs")
BASELINE = "baseline"
DEFAULT_REPEATS = 3

STEP_FIELDS = ("step", "kind", "repeat", "target", "restart_epoch", "healthy_epoch", "healthy",
               "window_dir", "window_rc", "outcome", "receiver", "note")
MEASURED = "measured"
OUTCOMES = (MEASURED, "not_healthy", "restart_failed", "not_performed", "window_stopped",
            "window_refused", "window_aborted", "window_error")
RECEIVER_STATES = ("parked", "connected", "unread", "n/a", "")
_OUTCOME_FAIL = {"not_healthy", "restart_failed"}
_OUTCOME_TEXT = {
    "restart_failed": "the restart command failed (the unit did not start again)",
    "not_performed": "the restart was not performed",
    "window_stopped": "the soak ended the window without a measurement",
    "window_refused": "the window was refused before it touched the rig (soak exit 4)",
    "window_aborted": "the window was aborted (soak exit 5)",
    "window_error": "the window failed to run",
}


class InputError(Exception):
    """A matrix dir / conf / step table that cannot be graded (exit 3)."""


# --- one window, pointwise -----------------------------------------------------------------------


def read_window_row(window_dir):
    """The soak's own CSV of ONE window run -> (cams, the row), (None, None) when there is none.
    A `--hours 0` soak run writes exactly one row; the first row is the window."""
    path = os.path.join(window_dir or "", "soak.csv")
    if not window_dir or not os.path.isfile(path):
        return None, None
    fields, rows = soak.read_rows(path)
    if not rows:
        return None, None
    return soak.cams_from_fields(fields), rows[0]


def grade_window(row, cams, bounds, spread_columns=soak.DEFAULT_SPREAD_COLUMNS):
    """ONE window row -> {"verdict", "reasons", "fails", "unknowns"}. The soak's per-sample rules
    and bounds, one sample at a time (see the module doc)."""
    fails, unknowns = [], []
    if row is None:
        return _verdict([], ["no window row (the soak wrote no soak.csv)"])
    if row.get("outcome") != "ok":
        return _verdict([], [f"no verdict for the window ({row.get('outcome') or 'no outcome'})"])
    for c in spread_columns:
        if c not in soak.SPREAD_COLUMNS:
            raise ValueError(f"unknown spread column {c!r} (expected one of {soak.SPREAD_COLUMNS})")
    tol, thr = bounds["av_tolerance_ms"], bounds["spread_threshold_ms"]
    expected = soak._f(row.get("av_expected_ms"))
    judged = 0
    for c in cams:
        status = row.get(f"av_{c}_status") or "absent"
        excluded = status == "excluded"
        v = soak._f(row.get(f"av_{c}_ms"))
        if excluded:
            pass  # operator-excluded: no A/V sample required (a measured loss/burn still counts)
        elif status != "measured" or v is None:
            unknowns.append(f"av {c}: {status} (no measured A/V offset)")
        elif expected is None:
            unknowns.append(f"av {c}: {v:+.1f} ms but the verdict carries no expected offset")
        else:
            judged += 1
            if abs(v - expected) > tol:
                fails.append(f"av {c}: {v:+.1f} ms, |offset - expected {expected:+.1f}| = "
                             f"{abs(v - expected):.1f} ms > {tol:g} ms")
        loss = row.get(f"loss_{c}_pass")
        if loss == "false":
            fails.append(f"loss {c}: a loss window (copies={row.get(f'loss_{c}_copies') or '-'} "
                         f"gaps={row.get(f'loss_{c}_gaps') or '-'} "
                         f"undecodable={row.get(f'loss_{c}_undecodable') or '-'})")
        elif loss not in soak._LOSS_SAMPLE and not excluded:
            unknowns.append(f"loss {c}: not measured")
        if row.get(f"burn_{c}_zero_loss") == "false":
            fails.append(f"burn {c}: the camera burn lost frames "
                         f"(real_drops={row.get(f'burn_{c}_real_drops') or '-'})")
    if not judged and not fails:
        unknowns.append("no camera's A/V offset was graded")
    for col in spread_columns:
        v = soak._f(row.get(col))
        if v is None:
            unknowns.append(f"spread {col}: not measured")
        elif v > thr:
            fails.append(f"spread {col}: {v:.1f} ms > {thr:g} ms")
    if row.get("cont_overall_pass") == "false":
        fails.append("continuity gate: the verdict's own all_cambox_continuity.overall_pass is false")
    if soak.gate_term_disagrees(row, cams) is True:
        unknowns.append("the soak's mirrored loss terms disagree with the verdict's own continuity "
                        "fold (the Python copy of the Rust term may have drifted)")
    for node in soak.HOP_NODES:
        z = row.get(f"burn_{node}_zero_loss")
        if z == "false":
            fails.append(f"burn {node}: the {node} hop lost frames "
                         f"(real_drops={row.get(f'burn_{node}_real_drops') or '-'})")
        elif z != "true":
            unknowns.append(f"burn {node}: never measured (the OBS measurement burn is missing)")
    return _verdict(fails, unknowns)


def _verdict(fails, unknowns):
    verdict = FAIL if fails else (UNKNOWN if unknowns else PASS)
    return {"verdict": verdict, "reasons": list(fails) + list(unknowns), "fails": list(fails),
            "unknowns": list(unknowns)}


# --- the step table + conf -----------------------------------------------------------------------


def _clean(v):
    return " ".join(str("" if v is None else v).split())


def append_step(tsv_path, step):
    """Append ONE step row (tab-separated, header on the first row). Tabs/newlines in a value are
    collapsed to single spaces."""
    kind = step.get("kind")
    if kind not in KINDS + (BASELINE,):
        raise ValueError(f"unknown step kind {kind!r}")
    outcome = step.get("outcome")
    if outcome not in OUTCOMES:
        raise ValueError(f"unknown step outcome {outcome!r} (expected one of {OUTCOMES})")
    if (step.get("receiver") or "") not in RECEIVER_STATES:
        raise ValueError(f"unknown receiver state {step.get('receiver')!r} "
                         f"(expected one of {RECEIVER_STATES[:-1]})")
    exists = os.path.exists(tsv_path) and os.path.getsize(tsv_path) > 0
    with open(tsv_path, "a", newline="", encoding="utf-8") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        if not exists:
            w.writerow(STEP_FIELDS)
        w.writerow([_clean(step.get(k)) for k in STEP_FIELDS])


def read_steps(tsv_path):
    if not os.path.isfile(tsv_path):
        return []
    with open(tsv_path, newline="", encoding="utf-8") as f:
        r = csv.DictReader(f, delimiter="\t")
        if tuple(r.fieldnames or ()) != STEP_FIELDS:
            raise InputError(f"{tsv_path}: unexpected header {r.fieldnames}")
        return list(r)


def read_conf(path):
    if not os.path.isfile(path):
        raise InputError(f"no {path} (not a restart-matrix run dir)")
    conf = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                conf[k.strip()] = v.strip()
    kinds = tuple(conf.get("kinds", "").split())
    bad = [k for k in kinds if k not in KINDS]
    if bad:
        raise InputError(f"{path}: unknown restart kind(s) {bad} (expected {KINDS})")
    try:
        repeats = int(conf.get("repeats", DEFAULT_REPEATS))
    except ValueError as e:
        raise InputError(f"{path}: repeats is not an integer") from e
    if repeats < 1:
        raise InputError(f"{path}: repeats must be >= 1")
    conf["kinds_list"] = kinds
    conf["repeats_n"] = repeats
    return conf


# --- the matrix ----------------------------------------------------------------------------------


def grade_step(step, bounds, spread_columns, healthy_timeout_s=None):
    """One step row -> {"verdict", "reason", "time_to_healthy_s", "window"}."""
    out = {"step": step.get("step"), "kind": step.get("kind"), "repeat": step.get("repeat"),
           "target": step.get("target"), "outcome": step.get("outcome"),
           "window_dir": step.get("window_dir"), "receiver": step.get("receiver") or "",
           "time_to_healthy_s": None, "window": None}
    r0, h = soak._f(step.get("restart_epoch")), soak._f(step.get("healthy_epoch"))
    if r0 is not None and h is not None:
        out["time_to_healthy_s"] = int(round(h - r0))
    outcome = step.get("outcome")
    note = step.get("note") or ""
    if outcome == "not_healthy":
        bound = f"{healthy_timeout_s} s" if healthy_timeout_s else "the bound"
        out.update(verdict=FAIL, reason=f"the component did not report healthy within {bound} "
                                        f"after the restart{': ' + note if note else ''}")
        return out
    if outcome != MEASURED:
        text = _OUTCOME_TEXT.get(outcome, f"outcome {outcome}")
        verdict = FAIL if outcome in _OUTCOME_FAIL else UNKNOWN
        out.update(verdict=verdict, reason=text + (f": {note}" if note else ""))
        return out
    cams, row = read_window_row(step.get("window_dir"))
    g = grade_window(row, cams or [], bounds, spread_columns)
    out["window"] = g
    out.update(verdict=g["verdict"], reason="; ".join(g["reasons"]))
    return out


def evaluate(conf, steps, bounds, spread_columns=None):
    """The whole matrix -> the report dict."""
    if spread_columns is None:
        spread_columns = tuple(c for c in conf.get("spread_columns", "").split(",") if c) \
            or soak.DEFAULT_SPREAD_COLUMNS
    healthy_timeout_s = conf.get("healthy_timeout_s")
    kinds, repeats = conf["kinds_list"], conf["repeats_n"]
    rep = {"verdict": UNKNOWN, "repeats_required": repeats, "kinds_required": list(kinds),
           "settle_s": conf.get("settle_s"), "healthy_timeout_s": healthy_timeout_s,
           "bounds": {"av_tolerance_ms": bounds["av_tolerance_ms"],
                      "spread_threshold_ms": bounds["spread_threshold_ms"],
                      "sources": dict(bounds.get("sources", {}))},
           "spread_columns": list(spread_columns), "baseline": None, "kinds": {}, "reasons": []}
    base = [s for s in steps if s.get("kind") == BASELINE]
    if base:
        rep["baseline"] = grade_step(base[-1], bounds, spread_columns, healthy_timeout_s)
    else:
        rep["baseline"] = {"verdict": UNKNOWN, "reason": "no baseline window", "window": None}
    for kind in kinds:
        by_rep = {}
        for s in steps:
            if s.get("kind") != kind:
                continue
            try:
                r = int(s.get("repeat") or 0)
            except ValueError:
                continue
            if 1 <= r <= repeats:
                by_rep[r] = s
        graded = []
        for r in range(1, repeats + 1):
            if r in by_rep:
                g = grade_step(by_rep[r], bounds, spread_columns, healthy_timeout_s)
            else:
                g = {"repeat": str(r), "verdict": UNKNOWN, "reason": "not run",
                     "receiver": "", "time_to_healthy_s": None, "window": None}
            graded.append(g)
        passed = sum(1 for g in graded if g["verdict"] == PASS)
        tth = [g["time_to_healthy_s"] for g in graded if g.get("time_to_healthy_s") is not None]
        receivers = {}
        for g in graded:
            if g.get("receiver") not in ("", "n/a", None):
                receivers[g["receiver"]] = receivers.get(g["receiver"], 0) + 1
        k = {"verdict": UNKNOWN, "passed": passed, "repeats": graded, "receivers": receivers,
             "max_time_to_healthy_s": max(tth) if tth else None, "reason": ""}
        if any(g["verdict"] == FAIL for g in graded):
            k["verdict"] = FAIL
            k["reason"] = "a repeat failed"
        elif passed == repeats:
            k["verdict"] = PASS
        else:
            k["reason"] = f"{passed}/{repeats} repeats PASS"
        rep["kinds"][kind] = k
    b = rep["baseline"]
    if b["verdict"] != PASS:
        rep["reasons"].append(f"baseline: {b['verdict']} -- {b.get('reason') or ''}".rstrip(" -"))
    for kind, k in rep["kinds"].items():
        for g in k["repeats"]:
            if g["verdict"] != PASS:
                rep["reasons"].append(f"{kind} r{g.get('repeat')}: {g['verdict']} -- {g.get('reason')}")
    # a PASS whose restarts hit a parked (or unread) receiver never proved a CONNECTED strih
    # receiver survives the restart: say so next to the verdict, not only under the kind
    rep["caveats"] = []
    for kind, k in rep["kinds"].items():
        weak = [f"{st} {k['receivers'][st]}" for st in ("parked", "unread") if k["receivers"].get(st)]
        if k["verdict"] == PASS and weak:
            rep["caveats"].append(
                f"{kind} PASS: the restarted camera's strih receiver was {', '.join(weak)} of "
                f"{len(k['repeats'])} restart(s) -- a connected receiver surviving the restart is "
                f"not proven")
    verdicts = [b["verdict"]] + [k["verdict"] for k in rep["kinds"].values()]
    if FAIL in verdicts:
        rep["verdict"] = FAIL
    elif all(v == PASS for v in verdicts):
        rep["verdict"] = PASS
    else:
        rep["verdict"] = UNKNOWN
    return rep


def evaluate_dir(run_dir, bounds, spread_columns=None):
    conf = read_conf(os.path.join(run_dir, "matrix.conf"))
    steps = read_steps(os.path.join(run_dir, "matrix.tsv"))
    return evaluate(conf, steps, bounds, spread_columns)


def exit_code(report):
    return _EXIT.get(report.get("verdict"), 2)


# --- rendering -----------------------------------------------------------------------------------


def _tth(v):
    return "-" if v is None else f"{v} s"


def render_text(rep):
    b = rep["bounds"]
    src = b.get("sources", {})
    lines = [
        f"AV-RESTART-MATRIX: {len(rep['kinds'])} restart kind(s) x {rep['repeats_required']} "
        f"repeat(s) required; settle {rep.get('settle_s') or '-'} s after healthy, healthy bound "
        f"{rep.get('healthy_timeout_s') or '-'} s",
        f"  bounds: A/V |offset - expected| <= {b['av_tolerance_ms']:g} ms "
        f"({src.get('av_tolerance_ms', '?')}); spread <= {b['spread_threshold_ms']:g} ms "
        f"({src.get('spread_threshold_ms', '?')}); graded spread: {', '.join(rep['spread_columns'])}",
        f"  {'baseline':<12} {rep['baseline']['verdict']}"
        + (f" -- {rep['baseline'].get('reason')}" if rep["baseline"].get("reason") else ""),
    ]
    for kind, k in rep["kinds"].items():
        lines.append(f"  {kind:<12} {k['verdict']:<8} {k['passed']}/{len(k['repeats'])} PASS, "
                     f"max time to healthy {_tth(k['max_time_to_healthy_s'])}")
        parked = k["receivers"].get("parked", 0)
        unread = k["receivers"].get("unread", 0)
        if parked:
            lines.append(f"    NOTE: the restarted camera's strih input was parked during "
                         f"{parked}/{len(k['repeats'])} restart(s) -- no connected receiver saw "
                         f"them; the window's connect-on-show hold connected it fresh")
        if unread:
            lines.append(f"    NOTE: the restarted camera's strih receiver state was unread during "
                         f"{unread}/{len(k['repeats'])} restart(s)")
        for g in k["repeats"]:
            rcv = f", receiver {g['receiver']}" if g.get("receiver") not in ("", None) else ""
            lines.append(f"    r{g.get('repeat')}: {g['verdict']:<8} healthy after "
                         f"{_tth(g.get('time_to_healthy_s'))}{rcv}"
                         + (f" -- {g['reason']}" if g.get("reason") else ""))
    lines.append(f"  VERDICT: {rep['verdict']}")
    for c in rep.get("caveats", []):
        lines.append(f"  CAVEAT: {c}")
    for r in rep["reasons"]:
        lines.append(f"    - {r}")
    return "\n".join(lines)


# --- CLI -----------------------------------------------------------------------------------------


def _cmd_record(a):
    append_step(a.tsv, {"step": a.step, "kind": a.kind, "repeat": a.repeat, "target": a.target,
                        "restart_epoch": a.restart_epoch, "healthy_epoch": a.healthy_epoch,
                        "healthy": a.healthy, "window_dir": a.window_dir, "window_rc": a.window_rc,
                        "outcome": a.outcome, "receiver": a.receiver, "note": a.note})
    return 0


def _cmd_grade_window(a):
    cams, row = read_window_row(a.window_dir)
    spread = tuple(c for c in a.spread_columns.split(",") if c)
    g = grade_window(row, cams or [], soak.load_gate_bounds(a.repo_root), spread)
    print(f"verdict={g['verdict']}")
    for r in g["reasons"]:
        print(f"reason={r}")
    return exit_code(g)


def _cmd_report(a):
    spread = tuple(c for c in a.spread_columns.split(",") if c) if a.spread_columns else None
    rep = evaluate_dir(a.dir, soak.load_gate_bounds(a.repo_root), spread)
    print(render_text(rep))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump(rep, f, indent=2, sort_keys=True)
    return exit_code(rep)


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1367 restart-matrix decision core")
    ap.add_argument("--repo-root", default=None, help="repo root holding src/*.rs (default: ..)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("record", help="append ONE step to matrix.tsv")
    r.add_argument("--tsv", required=True)
    r.add_argument("--step", required=True)
    r.add_argument("--kind", required=True)
    r.add_argument("--repeat", default="0")
    r.add_argument("--target", default="")
    r.add_argument("--restart-epoch", default="")
    r.add_argument("--healthy-epoch", default="")
    r.add_argument("--healthy", default="")
    r.add_argument("--window-dir", default="")
    r.add_argument("--window-rc", default="")
    r.add_argument("--outcome", required=True)
    r.add_argument("--receiver", default="")
    r.add_argument("--note", default="")
    g = sub.add_parser("grade-window", help="grade ONE soak window dir (prints verdict=...)")
    g.add_argument("--window-dir", required=True)
    g.add_argument("--spread-columns", default=",".join(soak.DEFAULT_SPREAD_COLUMNS))
    p = sub.add_parser("report", help="grade a matrix run dir")
    p.add_argument("--dir", required=True)
    p.add_argument("--json", default="")
    p.add_argument("--spread-columns", default="",
                   help="default: the run's matrix.conf spread_columns, else av_spread_ms")
    a = ap.parse_args(argv)
    try:
        return {"record": _cmd_record, "grade-window": _cmd_grade_window,
                "report": _cmd_report}[a.cmd](a)
    except (soak.BoundsError, InputError, ValueError, OSError) as e:
        print(f"av-restart-matrix {a.cmd}: ERROR: {e}", file=sys.stderr)
        return EXIT_USAGE


if __name__ == "__main__":
    sys.exit(main())
