#!/usr/bin/env python3
"""#1061 -- latency-pin verify-at-start: REPORT-only drift check (issue 866 latency half).

Issue 866's binding ask was to verify the WHOLE runtime OBS state at OBS start against expected --
"burny VYPNUTE, piny na dohodnutych hodnotach" -- and report drift LOUDLY. Issue 1057 delivered
the BURN half (force OFF: a measurement burn is never legitimate operator state). This is the
LATENCY half, which is genuinely different in kind:

  * Per-source `genlock_latency_ms_src` persists to the OBS scene-collection and RELOADS at OBS
    start (the same persist-to-disk mechanism as the burn -- issue 866 evidence, #707).
  * BUT per-source latency is the operator's A/V-align domain (repo memory "latency is user's
    A/V-align domain"), so the start path may only **REPORT** drift, NEVER force-overwrite --
    forcing would fight the operator, the opposite of the burn case.
  * "Against expected" needs a canonical **agreed pins** baseline -- scripts/latency-pins-baseline.json
    (committed source of truth; the operator records a legitimate re-tune by updating it in a PR).

This script reads the LIVE `genlock_latency_ms_src` for the box's baseline inputs over OBS
WebSocket (read-only -- never SET), diffs against the committed baseline, and reports every drift
LOUD naming box+input+got+want. Exit 0 = every pin on-baseline; exit 1 = drift (a loud ADVISORY
signal, not a launch abort -- OBS is already up); exit 2 = a connect/read failure (fail-closed).

It REUSES the repo's existing framework rather than re-implementing WS plumbing: obs_phase2's
`_conn`/`_rpc`, imag_latency_enforce's `is_ndi_kind` (live NDI enumeration for the imag floor), and
the honest-None `genlock_latency_ms_src` read convention latency_pins_snapshot.read_pin established.
A verify is a different, simpler operation than a gather, so it is its own script (the same
separation imag_latency_enforce.py keeps from phase_sync_calibrate.py).

Usage:
    latency_pins_verify.py --box strih  --host 10.77.9.202 [--password P] [--baseline PATH]
    latency_pins_verify.py --box stream --host 10.77.9.204
    latency_pins_verify.py --box imag   --host 10.77.9.182
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from obs_phase2 import _conn, _rpc  # noqa: E402
from imag_latency_enforce import is_ndi_kind  # noqa: E402

GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"
FLOOR_KEY = "_all_ndi_inputs_ms"
DEFAULT_BASELINE = os.path.join(_HERE, "latency-pins-baseline.json")


# ---------------------------------------------------------------------------
# Pure diff logic (unit-tested with NO rig, tests/python/test_latency_pins_verify.py)
# ---------------------------------------------------------------------------
def normalize_spec(spec) -> tuple:
    """Normalize a baseline entry to (want_ms:int, tolerance_ms:int).

    A bare int is an EXACT pin (zero tolerance); a dict is {"want_ms": N[, "tolerance_ms": T]}.
    Raises ValueError on anything malformed (a bool -- int subclass -- a string, a missing want,
    or a negative tolerance) rather than silently coercing it to a wrong number."""
    if isinstance(spec, bool):
        raise ValueError(f"pin spec must not be a bool: {spec!r}")
    if isinstance(spec, int):
        return (spec, 0)
    if isinstance(spec, dict):
        want = spec.get("want_ms")
        tol = spec.get("tolerance_ms", 0)
        if isinstance(want, bool) or not isinstance(want, int):
            raise ValueError(f"pin spec 'want_ms' must be an int: {spec!r}")
        if isinstance(tol, bool) or not isinstance(tol, int) or tol < 0:
            raise ValueError(f"pin spec 'tolerance_ms' must be a non-negative int: {spec!r}")
        return (want, tol)
    raise ValueError(f"pin spec must be an int or a dict: {spec!r}")


def diff_pin(name: str, got, spec) -> "str | None":
    """Return None when `got` (an int, or None for an honest N/A live read) is within the
    baseline `spec`'s tolerance band, else a LOUD drift message naming input+got+want+tol."""
    want, tol = normalize_spec(spec)
    if got is None:
        return f'LATENCY-PIN DRIFT input="{name}" got=N/A want={want}ms (tol +/-{tol}ms)'
    if abs(int(got) - want) <= tol:
        return None
    return f'LATENCY-PIN DRIFT input="{name}" got={got}ms want={want}ms (tol +/-{tol}ms)'


def verify_box(box: str, baseline_box: dict, live_pins: dict) -> list:
    """Pure: diff every baseline pin for `box` against `live_pins` ({name: int|None}).

    Two baseline shapes: a FLOOR sentinel (`_all_ndi_inputs_ms`) means every LIVE NDI input must
    equal the floor (imag -- its input set is dynamic); explicit `{name: spec}` entries pin those
    named inputs. Underscore-prefixed keys (comments, the sentinel) are never treated as named
    pins. Returns a list of `box=<box> <drift msg>` strings (empty = every pin on-baseline)."""
    drifts = []
    floor = baseline_box.get(FLOOR_KEY)
    if floor is not None:
        for name in sorted(live_pins):
            msg = diff_pin(name, live_pins[name], floor)
            if msg:
                drifts.append(f"box={box} " + msg)
    for name in sorted(baseline_box):
        if name.startswith("_"):
            continue
        msg = diff_pin(name, live_pins.get(name), baseline_box[name])
        if msg:
            drifts.append(f"box={box} " + msg)
    return drifts


def baseline_names(baseline_box: dict) -> "list | None":
    """The live inputs to read for a box: None (enumerate every live NDI input) when the box is a
    floor box, else the explicit named pins (underscore keys excluded)."""
    if FLOOR_KEY in baseline_box:
        return None
    return sorted(n for n in baseline_box if not n.startswith("_"))


# ---------------------------------------------------------------------------
# WS reader (module-local _rpc so tests monkeypatch ONE point -- imag test convention)
# ---------------------------------------------------------------------------
def _read_one_pin(ws, name: str) -> "int | None":
    """genlock_latency_ms_src for `name` over an already-connected ws -- None if the source or the
    key is missing (honest N/A, mirrors latency_pins_snapshot.read_pin). Never aborts the sweep."""
    try:
        settings = _rpc(ws, "GetInputSettings", {"inputName": name}, ignore_err=True)
    except Exception as e:  # noqa: BLE001 -- any RPC/network failure -> honest N/A, logged not swallowed
        print(f"WARNING: latency_pins_verify: GetInputSettings({name!r}) failed: {e}", file=sys.stderr)
        return None
    if not isinstance(settings, dict):
        return None
    inp = settings.get("inputSettings", {})
    v = inp.get(GENLOCK_SRC_LATENCY_KEY) if isinstance(inp, dict) else None
    return int(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def read_pins_over_ws(ws, names) -> dict:
    """Read genlock pins over `ws`. `names` = an explicit list, OR None to enumerate every live
    NDI-kind input. Returns {name: int|None}."""
    if names is None:
        il = _rpc(ws, "GetInputList", {}, ignore_err=True) or {}
        names = [
            i.get("inputName")
            for i in il.get("inputs", [])
            if is_ndi_kind(i.get("inputKind", ""))
        ]
    return {name: _read_one_pin(ws, name) for name in names}


def read_live_pins(host: str, password: str, names) -> dict:
    """Connect to `host` and read pins (raises on a connect failure -> caller fail-closes)."""
    ws = _conn(host, password)
    try:
        return read_pins_over_ws(ws, names)
    finally:
        ws.close()


def load_baseline(path: str) -> dict:
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, dict):
        raise ValueError(f"baseline {path} is not a JSON object")
    return data


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--box", required=True, choices=["strih", "stream", "imag"])
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("--baseline", default=DEFAULT_BASELINE)
    args = ap.parse_args(argv)

    baseline = load_baseline(args.baseline)
    baseline_box = baseline.get(args.box)
    if not isinstance(baseline_box, dict):
        print(f"ERROR: latency_pins_verify: baseline has no '{args.box}' box (baseline={args.baseline})", file=sys.stderr)
        return 2

    names = baseline_names(baseline_box)
    try:
        live_pins = read_live_pins(args.host, args.password, names)
    except Exception as e:  # noqa: BLE001 -- fail-CLOSED: an unreachable box is loud, never a silent clean
        print(
            f"ERROR: latency_pins_verify: could not read {args.box} pins over WS at {args.host}: {e}\n"
            f"       FAIL-CLOSED -- cannot confirm the agreed latency pins; do NOT trust this box's pins.",
            file=sys.stderr,
        )
        return 2

    drifts = verify_box(args.box, baseline_box, live_pins)
    if drifts:
        print(
            f"LATENCY-PIN DRIFT on {args.box} ({args.host}) -- {len(drifts)} input(s) off the agreed "
            f"baseline ({os.path.basename(args.baseline)}). REPORT-ONLY (per-source latency is the "
            f"operator's A/V-align domain -- NOT overwritten):",
            file=sys.stderr,
        )
        for d in drifts:
            print(f"  {d}", file=sys.stderr)
        print(
            "  -> if these are a legitimate re-tune, update scripts/latency-pins-baseline.json in a PR; "
            "if they are an unjustified restart revert (#866/#707), re-apply the agreed pins.",
            file=sys.stderr,
        )
        return 1

    print(f"latency_pins_verify: {args.box} ({args.host}) -- all agreed latency pins on baseline. OK.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
