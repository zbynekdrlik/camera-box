#!/usr/bin/env python3
"""#757 -- pre-record phase auto-pin: reconstruct each camera's absolute cam->strih transit
latency from a LIVE strih OBS log segment (`genlock-jitter-report --json` output), WITHOUT a
full recording+decode cycle, and re-key it under strih/imag's own per-host source-name
templates so `scripts/phase_sync_calibrate.py` can apply the corrected pins via its EXISTING
`--measured-json --apply` flow (unchanged -- this module never re-implements the offset math).

Why this exists (#757): `scripts/recording-e2e.sh`'s `[2/8]`/`[2b/8]` deploy restarts EVERY
camera-box (`systemctl stop`/`start`) on EVERY run, and each USB capture card's own internal
clock free-runs from a phase relative to strih's presentation grid that is effectively
RE-RANDOMIZED by that restart -- so a camera's delivery p50, measured by one run's full
recording, swings by up to ~one frame period (~16.7-33ms) run-to-run (confirmed across 10
consecutive fused-gate runs the night of 2026-07-14/15, see issue #757's own comments). A
STATIC pin set -- however well-calibrated on a PAST run -- cannot track this: it only ever
removes the FIXED per-camera baseline (hardware/network differences), leaving the full
per-restart random re-phase as cross-camera SPREAD error. Only a measurement taken THIS run,
before the scored recording starts, can correct it.

The measurement (`scripts/recording-e2e.sh`'s `[4f/8]` step): cut strih's PROGRAM through each
camera briefly, read strih's LIVE `genlock-fifo audit` log for that window
(`genlock-jitter-report --json`), and reconstruct each camera's TRUE absolute transit time as

    measured_latency_ms[cam] = latency_ms[cam] + mean_head_skew_ms[cam]

-- the genlock pin ACTIVE during the window, corrected by the SIGNED mean deviation of the
actual arrival from that pin's own release schedule (`camera_box::jitter_audit`'s
`mean_head_skew_ms`, #757 -- see that field's own doc for the derivation). Feeding this into
the EXISTING #286 `compute_phase_sync_offsets` kernel (via `phase_sync_calibrate.py`, never
touched here) produces the SAME slowest-anchored relative pin set a full recording-based
measurement would -- without needing one.

Pure functions here (`measured_by_camera`, `source_names_by_template`) do NO I/O and are unit
tested with NO rig (`tests/python/test_prerecord_phase_calibrate.py`). The thin CLI wrapper
(`main`) just wires argv/files to them; the actual OBS-WS apply is `phase_sync_calibrate.py`'s
job, invoked separately by the harness with this script's output file.

Usage:
    prerecord_phase_calibrate.py --jitter-json jitter.json --out strih-measured.json \
        [--imag-main-out imag-main-measured.json] [--imag-mv-out imag-mv-measured.json]
"""
from __future__ import annotations

import argparse
import json
import re
import sys

# Strih's own per-camera main-input naming convention (set-ndi-mapping.py DEFAULT_MAP,
# the #753 1:1 pivot -- 'NDI cam<N>' IS physical camera N).
_STRIH_SOURCE_RE = re.compile(r"^NDI cam(\d+)$")


def measured_by_camera(jitter_json: dict) -> dict:
    """From `genlock-jitter-report --json` output (keyed by STRIH source name
    ``"NDI cam<N>"``), reconstruct each camera's measured absolute cam->strih transit latency:
    ``latency_ms`` (the EFFECTIVE pin active during the sampled window) plus
    ``mean_head_skew_ms`` (the SIGNED mean deviation of the actual arrival from that pin's own
    release schedule -- #757). Keyed by camera NUMBER (not source name), so the caller can
    re-template it under a DIFFERENT host's own naming convention (imag's ``"NDI CAM<N>"`` /
    ``"MV CAM<N>"``) without re-deriving anything.

    A source whose name does not match strih's ``"NDI cam<N>"`` pattern, or whose
    ``latency_ms`` / ``mean_head_skew_ms`` is missing, non-numeric, or the value itself isn't a
    dict, is SKIPPED -- never a fabricated/guessed value. Returns ``{}`` for empty/malformed
    input (never raises -- the caller decides whether an empty result is fatal)."""
    out: dict = {}
    if not isinstance(jitter_json, dict):
        return out
    for name, s in jitter_json.items():
        m = _STRIH_SOURCE_RE.match(name)
        if not m or not isinstance(s, dict):
            continue
        latency_ms = s.get("latency_ms")
        skew_ms = s.get("mean_head_skew_ms")
        if not isinstance(latency_ms, (int, float)) or isinstance(latency_ms, bool):
            continue
        if not isinstance(skew_ms, (int, float)) or isinstance(skew_ms, bool):
            continue
        out[int(m.group(1))] = float(latency_ms) + float(skew_ms)
    return out


def source_names_by_template(measured_by_cam: dict, name_template: str) -> dict:
    """Re-key ``{camera_number: measured_ms}`` under a per-host source-name template (e.g.
    ``"NDI cam{n}"`` for strih, ``"NDI CAM{n}"`` / ``"MV CAM{n}"`` for imag) -- values
    unchanged. Pure string formatting, no I/O, no rounding (phase_sync_calibrate.py's own
    kernel handles clamping/rounding)."""
    return {name_template.format(n=n): v for n, v in measured_by_cam.items()}


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--jitter-json", required=True, help="genlock-jitter-report --json output file"
    )
    ap.add_argument(
        "--out", required=True,
        help="write the strih-templated measured.json here ('NDI cam{n}')",
    )
    ap.add_argument(
        "--imag-main-out", default=None,
        help="ALSO write the imag-main-templated measured.json here ('NDI CAM{n}')",
    )
    ap.add_argument(
        "--imag-mv-out", default=None,
        help="ALSO write the imag-MV-clone-templated measured.json here ('MV CAM{n}')",
    )
    args = ap.parse_args(argv)

    with open(args.jitter_json, encoding="utf-8") as f:
        jitter_json = json.load(f)

    by_cam = measured_by_camera(jitter_json)
    if not by_cam:
        print(
            f"WARNING: prerecord_phase_calibrate: no usable 'NDI cam<N>' entries in "
            f"{args.jitter_json} -- writing nothing; caller must skip the apply step",
            file=sys.stderr,
        )
        return 1

    strih = source_names_by_template(by_cam, "NDI cam{n}")
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(strih, f, indent=2)
    print(f"prerecord_phase_calibrate: wrote {len(strih)} camera(s) -> {args.out}")

    if args.imag_main_out:
        imag_main = source_names_by_template(by_cam, "NDI CAM{n}")
        with open(args.imag_main_out, "w", encoding="utf-8") as f:
            json.dump(imag_main, f, indent=2)
        print(
            f"prerecord_phase_calibrate: wrote {len(imag_main)} camera(s) -> "
            f"{args.imag_main_out}"
        )

    if args.imag_mv_out:
        imag_mv = source_names_by_template(by_cam, "MV CAM{n}")
        with open(args.imag_mv_out, "w", encoding="utf-8") as f:
            json.dump(imag_mv, f, indent=2)
        print(
            f"prerecord_phase_calibrate: wrote {len(imag_mv)} camera(s) -> {args.imag_mv_out}"
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
