#!/usr/bin/env python3
"""#1003 -- apply the agreed per-source genlock latency pins baseline to a live box (strih/stream).

The drift-guard baseline (scripts/latency-pins-baseline.json) is the committed SOURCE OF TRUTH that
`latency_pins_verify.py` reads REPORT-ONLY at every OBS start (it names drift LOUD but NEVER writes
-- per-source latency is the operator's A/V-align domain, latency-pins-verify.md). This is the
deliberate WRITER counterpart: it PUSHES that agreed baseline onto a live box over OBS WebSocket
using `genlock_latency_ms_src` (the authoritative pin key), so a PR-recorded re-tune actually takes
effect on the rig.

The uses (#1003 owner rework, 2026-08-20): the deep DELIVERY-equalized promotion (90/160/184 + 791)
was REJECTED + REVERTED -- deep absolute pins add ~180 ms of needless chain latency. Production
alignment is now the per-run FLOOR-3 auto-align (scripts/qr_align_pins.py), which reuses this
module's `apply_pins` directly to push its computed floor-3 set. This tool's own CLI stays useful
in two roles: (1) the manual RUNBOOK path to push a computed set with `--pins '{...}'`, and (2) the
baseline path (`--box`) to push the committed drift-guard REFERENCE. Both go through the same
DRY-RUN-default, read-back-verified, fail-loud writer.

Design guardrails (mirrors the latency_pins_* family):
  * DRY-RUN BY DEFAULT -- prints the plan (live -> want per source) and writes NOTHING. `--execute`
    is the only path that writes. This keeps the write DELIBERATE, honoring the operator domain:
    the passive verify-at-start reports, this active tool is run by hand in a maintenance window to
    push a newly-agreed baseline.
  * Idempotent -- a source already on-baseline is a no-op (no needless WS churn), like
    imag_latency_enforce.enforce_fixed_latency.
  * Read-back verified -- every write is confirmed by re-reading genlock_latency_ms_src; a mismatch
    is FAIL LOUD (SystemExit), never a silently half-set source.
  * imag is REFUSED -- imag's pins are the 3ms floor mandate (imag-min-latency-3ms-always), owned
    by imag_latency_enforce.py; this promotion tool never touches them.

Pure functions (`explicit_pins_for_box`, `plan_pin_changes`) do NO I/O and are unit-tested with NO
rig (tests/python/test_apply_latency_pins_1003.py); the live apply uses this module's OWN `_rpc`
(imported from obs_phase2) so a test monkeypatches ONE point (the latency_pins_verify convention).

Usage:
    apply_latency_pins.py --box strih  --host 10.77.9.202                # DRY-RUN plan (default)
    apply_latency_pins.py --box strih  --host 10.77.9.202 --execute      # APPLY
    apply_latency_pins.py --box stream --host 10.77.9.204 --execute
    (OBS_PASSWORD env, or --password; --baseline overrides the committed baseline path)
"""
from __future__ import annotations

import argparse
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from obs_phase2 import _conn, _rpc  # noqa: E402
from latency_pins_verify import normalize_spec  # noqa: E402

GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"
FLOOR_KEY = "_all_ndi_inputs_ms"
DEFAULT_BASELINE = os.path.join(_HERE, "latency-pins-baseline.json")


# ---------------------------------------------------------------------------
# Pure logic (unit-tested with NO rig)
# ---------------------------------------------------------------------------
def explicit_pins_for_box(box: str, baseline_box: dict) -> dict:
    """Pure: the {source: want_ms(int)} pins this tool will PUSH for `box`.

    A FLOOR-sentinel box (imag, `_all_ndi_inputs_ms`) is REFUSED (SystemExit) -- imag's 3ms floor
    is imag_latency_enforce.py's domain and is never promoted here (imag-min-latency-3ms-always).
    For an explicit-pin box, every non-underscore entry is normalized to its want_ms (a bare int is
    exact; a `{want_ms, tolerance_ms}` band contributes its want_ms -- the tolerance is a verify
    band, not an apply target). An empty result (only comments) is a config error, not a vacuous
    no-op apply, so it also fails loud."""
    if FLOOR_KEY in baseline_box:
        raise SystemExit(
            f"[apply-latency-pins] box {box!r} is the imag floor sentinel ({FLOOR_KEY}) -- "
            "this tool never promotes the imag 3ms floor (imag-min-latency-3ms-always); use "
            "scripts/imag_latency_enforce.py for imag.")
    pins = {}
    for name, spec in baseline_box.items():
        if name.startswith("_"):
            continue
        want, _tol = normalize_spec(spec)
        pins[name] = want
    if not pins:
        raise SystemExit(
            f"[apply-latency-pins] box {box!r} has no explicit pins in the baseline -- nothing to "
            "apply (a box with only comments is a config error, never a silent no-op).")
    return pins


def pins_from_arg(pins_arg: str) -> dict:
    """Parse --pins: a JSON object {source: ms(int)} either inline or as '@path'. This is the
    COMPUTED-set entry point (the #1003 floor-3 aligner pushes its per-run plan here instead of the
    committed baseline). Validates non-empty and int-valued; a bool / non-number / empty is a
    config error, never a vacuous apply. The floor sentinel (imag) has no place here -- the caller
    only ever passes the strih inputs it aligned, so an imag key would be a caller bug."""
    raw = pins_arg
    if pins_arg.startswith("@"):
        with open(pins_arg[1:], encoding="utf-8") as fh:
            raw = fh.read()
    try:
        obj = json.loads(raw)
    except ValueError as exc:
        raise SystemExit(f"[apply-latency-pins] --pins is not valid JSON: {exc}")
    if not isinstance(obj, dict) or not obj:
        raise SystemExit(
            "[apply-latency-pins] --pins must be a non-empty JSON object {source: ms}")
    pins = {}
    for src, ms in obj.items():
        if src.startswith("_"):
            # Refuse the imag floor sentinel (_all_ndi_inputs_ms) and any underscore/comment key --
            # this writer only ever pushes NAMED strih inputs; an imag floor pin is never applied
            # here (imag-min-latency-3ms-always is imag_latency_enforce.py's domain), #1003 review.
            raise SystemExit(
                f"[apply-latency-pins] --pins key {src!r} is a sentinel/comment key -- this tool "
                "only pushes named source pins, never the imag floor sentinel.")
        if isinstance(ms, bool) or not isinstance(ms, (int, float)):
            raise SystemExit(
                f"[apply-latency-pins] --pins value for {src!r} must be a number, got {ms!r}")
        pins[src] = int(ms)
    return pins


def plan_pin_changes(want_pins: dict, live_pins: dict) -> list:
    """Pure: per-source apply plan given the target pins and the live-read values ({source:
    int|None}). action='noop' when the live value already equals the target; else 'set'. A missing
    or None (unreadable / honest N/A) live value is a 'set' -- it must be written to reach the
    target, and the live apply's read-back is what proves it took."""
    plan = []
    for source in want_pins:
        want = want_pins[source]
        live = live_pins.get(source)
        plan.append({
            "source": source,
            "live_ms": live,
            "want_ms": want,
            "action": "noop" if live == want else "set",
        })
    return plan


# ---------------------------------------------------------------------------
# Live WS apply (module-local _rpc so a test monkeypatches ONE point)
# ---------------------------------------------------------------------------
def _read_pin(ws, source: str) -> "int | None":
    """Read the CURRENT genlock_latency_ms_src on `source`, or None when the source/key is absent
    (honest N/A -- never a fabricated default). A WS/RPC error raises (a deliberate apply fails
    loud on an unreadable box, unlike the report-only verify path)."""
    settings = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    v = settings.get(GENLOCK_SRC_LATENCY_KEY)
    return int(v) if isinstance(v, (int, float)) else None


def apply_pins(ws, want_pins: dict, execute: bool) -> list:
    """Live: read every current pin ONCE, derive the noop/set plan with the PURE (unit-tested)
    `plan_pin_changes`, then act on it -- a 'noop' source is left untouched; a 'set' source under
    DRY-RUN (execute=False) records 'planned' and writes NOTHING; a 'set' source under --execute is
    SET, re-read, and requires the read-back to match -> 'applied' (else FAIL LOUD via SystemExit,
    never leaving a half-set source). Returns one result dict per source, in order. Routing through
    `plan_pin_changes` keeps the pure planner (not a re-implemented inline copy) load-bearing."""
    live_pins = {source: _read_pin(ws, source) for source in want_pins}
    results = []
    for entry in plan_pin_changes(want_pins, live_pins):
        source, want, before = entry["source"], entry["want_ms"], entry["live_ms"]
        if entry["action"] == "noop":
            results.append({"source": source, "want_ms": want, "before_ms": before,
                            "after_ms": before, "action": "noop"})
            continue
        if not execute:
            results.append({"source": source, "want_ms": want, "before_ms": before,
                            "after_ms": None, "action": "planned"})
            continue
        _rpc(ws, "SetInputSettings", {
            "inputName": source,
            "inputSettings": {GENLOCK_SRC_LATENCY_KEY: want},
            "overlay": True,
        })
        after = _read_pin(ws, source)
        if after != want:
            raise SystemExit(
                f"[apply-latency-pins] FAILED to set {GENLOCK_SRC_LATENCY_KEY}={want} on "
                f"'{source}' (was {before!r}, read-back={after!r}) -- source may be half-set, "
                "manual check required.")
        results.append({"source": source, "want_ms": want, "before_ms": before,
                        "after_ms": after, "action": "applied"})
    return results


def _print_plan(box: str, host: str, results: list, execute: bool) -> None:
    mode = "APPLY (--execute)" if execute else "DRY-RUN (no --execute; nothing written)"
    print(f"[apply-latency-pins] box={box} host={host} -- {mode}")
    for r in results:
        before = r["before_ms"] if r["before_ms"] is not None else "N/A"
        if r["action"] == "noop":
            print(f"  = '{r['source']}': already {r['want_ms']}ms (on-baseline, no write)")
        elif r["action"] == "planned":
            print(f"  ~ '{r['source']}': {before}ms -> {r['want_ms']}ms WOULD SET (run with --execute)")
        elif r["action"] == "applied":
            print(f"  + '{r['source']}': {before}ms -> {r['after_ms']}ms (applied, read-back verified)")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--box", required=True,
                    help="box key: reads scripts/latency-pins-baseline.json[box] unless --pins is "
                         "given, in which case --box is just the print/host label")
    ap.add_argument("--host", required=True)
    # explicit --password wins over the OBS_PASSWORD env var (the flag's default IS the env value),
    # matching the sibling latency_pins_verify.py precedence.
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("--baseline", default=DEFAULT_BASELINE)
    ap.add_argument("--pins",
                    help="push a COMPUTED {source: ms} set (JSON inline or @file) instead of the "
                         "committed baseline -- the #1003 floor-3 aligner's per-run plan path")
    ap.add_argument("--execute", action="store_true",
                    help="WRITE the pins (default: DRY-RUN, prints the plan, writes nothing)")
    args = ap.parse_args(argv)

    if args.pins:
        # COMPUTED set (e.g. the floor-3 aligner) -- bypass the committed baseline entirely.
        want_pins = pins_from_arg(args.pins)
    else:
        with open(args.baseline, encoding="utf-8") as fh:
            baseline = json.load(fh)
        if args.box not in baseline:
            raise SystemExit(
                f"[apply-latency-pins] box {args.box!r} not in baseline {args.baseline} "
                f"(have: {', '.join(sorted(baseline))})")
        want_pins = explicit_pins_for_box(args.box, baseline[args.box])

    ws = _conn(args.host, args.password)
    try:
        results = apply_pins(ws, want_pins, args.execute)
    finally:
        ws.close()

    _print_plan(args.box, args.host, results, args.execute)

    changed = [r for r in results if r["action"] in ("planned", "applied")]
    if not args.execute and changed:
        print(f"[apply-latency-pins] {len(changed)} source(s) would change -- re-run with "
              "--execute to apply (in a NO-E2E maintenance window).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
