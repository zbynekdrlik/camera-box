#!/usr/bin/env python3
"""#757 -- imag ALWAYS runs minimum genlock latency on every NDI input, no exceptions.

**Binding user directive (2026-07-15, in response to the #757 pre-record auto-pin work):**
imag is the LOW-LATENCY IMAG projection -- per-camera pin equalization (the strih concept: hold
the fastest camera back so all cameras present at the SAME instant) is a STRIH-ONLY idea. imag
must never carry it: every one of its NDI inputs stays pinned at the 3ms floor, always. The user
verbatim: "imag ma MINIMUM latenciu VSADE... uz som nastavil live vsetkych 16 imag NDI inputov na
3ms (read-back verified)". This script is what makes that state SELF-HEALING -- a stray future
change (a fat-fingered WS call, an OBS scene-collection import, manual tuning meant for a
different box) gets caught and corrected every run, loudly, instead of silently drifting.

Unlike `phase_sync_calibrate.py` / `av_sync_calibrate.py` (which compute a RELATIVE per-source
offset from a measurement), this script enforces ONE fixed absolute value across an entire
HOST's NDI inputs -- a different, simpler operation that doesn't belong bolted onto either of
those scripts.

Discovery is LIVE (`GetInputList` filtered by `inputKind` containing "ndi"), never a hardcoded
cam1..7 list -- imag's real input set includes non-camera NDI sources too (confirmed live,
2026-07-15: 16 inputs = NDI CAM1..7 + MV CAM1..7 + "NDI resolume imag" + "MW imag resolume"), so
a hardcoded camera-only list would silently leave 2 inputs unenforced.

Pure functions (`is_ndi_kind`, `list_ndi_inputs`) do NO I/O and are unit tested with NO rig
(`tests/python/test_imag_latency_enforce.py`). The live apply+verify function
(`enforce_fixed_latency`) mirrors `phase_sync_calibrate.apply_latency`'s read-back safety shape
(SET, then GetInputSettings read-back MUST match, never left half-set) but has no rollback
concept -- there is only ONE correct target value (the floor), never a "previous" value worth
restoring.

Usage:
    imag_latency_enforce.py --host 10.77.9.182 [--password P] [--target-ms 3]
"""
from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from obs_phase2 import _conn, _rpc  # noqa: E402

GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"
IMAG_FIXED_LATENCY_MS = 3


def is_ndi_kind(input_kind) -> bool:
    """Pure: does this OBS `inputKind` string identify an NDI source? Case-insensitive
    substring match (mirrors the live-verified `ndi_source` kind) -- never a fabricated True
    for a non-string/None kind."""
    return isinstance(input_kind, str) and "ndi" in input_kind.lower()


def list_ndi_inputs(inputs: list) -> list:
    """Pure: from `GetInputList`'s raw `inputs` array (`[{"inputName": ..., "inputKind": ...},
    ...]`), return the NDI input NAMES, in the SAME order they were given (no re-sorting -- a
    caller wanting a stable order sorts it themselves). Never guesses at a name/kind that isn't
    actually a string -- a malformed entry is silently skipped, never crashes the whole scan."""
    out = []
    for inp in inputs:
        if not isinstance(inp, dict):
            continue
        name = inp.get("inputName")
        if not isinstance(name, str):
            continue
        if is_ndi_kind(inp.get("inputKind")):
            out.append(name)
    return out


def enforce_fixed_latency(ws, names: list, target_ms: int = IMAG_FIXED_LATENCY_MS) -> list:
    """Live: for each source in `names`, read its CURRENT genlock_latency_ms_src; if it already
    equals `target_ms`, do nothing (no needless WS churn). Otherwise SET it and verify via
    read-back -- on a mismatch, FAIL LOUD (raises SystemExit) rather than silently leaving a
    half-set source, mirroring phase_sync_calibrate.apply_latency's safety shape.

    Returns a list of `{"source": name, "before_ms": ..., "after_ms": ..., "corrected": bool}`
    dicts (one per name, in order) -- a caller uses `corrected` to loudly log which inputs had
    actually DRIFTED and needed correction this run, vs. which were already compliant.
    """
    results = []
    for name in names:
        before = _rpc(ws, "GetInputSettings", {"inputName": name}).get("inputSettings", {})
        before_ms = before.get(GENLOCK_SRC_LATENCY_KEY)
        if before_ms == target_ms:
            results.append(
                {"source": name, "before_ms": before_ms, "after_ms": before_ms, "corrected": False}
            )
            continue
        _rpc(ws, "SetInputSettings", {
            "inputName": name,
            "inputSettings": {GENLOCK_SRC_LATENCY_KEY: target_ms},
            "overlay": True,
        })
        after = _rpc(ws, "GetInputSettings", {"inputName": name}).get("inputSettings", {})
        after_ms = after.get(GENLOCK_SRC_LATENCY_KEY)
        if after_ms != target_ms:
            raise SystemExit(
                f"[imag-latency-enforce] FAILED to set {GENLOCK_SRC_LATENCY_KEY}={target_ms} on "
                f"'{name}' (was {before_ms!r}, read-back={after_ms!r}) -- source may be half-set, "
                "manual check required"
            )
        results.append(
            {"source": name, "before_ms": before_ms, "after_ms": after_ms, "corrected": True}
        )
    return results


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--target-ms", type=int, default=IMAG_FIXED_LATENCY_MS)
    args = ap.parse_args(argv)
    password = os.environ.get("OBS_PASSWORD", args.password)

    ws = _conn(args.host, password)
    try:
        inputs = _rpc(ws, "GetInputList").get("inputs", [])
        names = list_ndi_inputs(inputs)
        if not names:
            print(
                f"WARNING: [imag-latency-enforce] no NDI inputs found on {args.host} -- "
                "nothing to enforce (unexpected on a real imag box)",
                file=sys.stderr,
            )
            return 1
        results = enforce_fixed_latency(ws, names, args.target_ms)
    finally:
        ws.close()

    corrected = [r for r in results if r["corrected"]]
    print(
        f"[imag-latency-enforce] {len(results)} NDI input(s) checked on {args.host}, "
        f"target={args.target_ms}ms, {len(corrected)} corrected"
    )
    for r in corrected:
        print(
            f"[imag-latency-enforce] DRIFT CORRECTED: '{r['source']}' was "
            f"{r['before_ms']!r}ms -> now {r['after_ms']}ms"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
