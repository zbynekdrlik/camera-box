#!/usr/bin/env python3
"""#757 -- pre-record phase auto-pin: reconstruct each STRIH camera's absolute cam->strih
transit latency from a LIVE strih OBS log segment (`genlock-jitter-report --json` output),
WITHOUT a full recording+decode cycle, and write it as the `--measured-json` input
`scripts/phase_sync_calibrate.py` applies via its EXISTING `--apply` flow (unchanged -- this
module never re-implements the offset math).

**STRIH ONLY (binding user directive, 2026-07-15).** Per-camera pin EQUALIZATION -- holding
faster cameras back so every camera presents at the same instant -- is a STRIH-only concept.
imag is the LOW-LATENCY IMAG projection and runs EVERY NDI input pinned at the fixed 3ms floor,
always; see `scripts/imag_latency_enforce.py` for that self-healing enforcement, invoked
SEPARATELY by the harness (never fed a computed pin here). This module used to also emit
imag-templated measured.json files (an earlier design, since retired) -- do not resurrect that.

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

The measurement (`scripts/recording-e2e.sh`'s `[4g/8]` step): cycle strih's PREVIEW through
each camera briefly, read strih's LIVE `genlock-fifo audit` log for that window
(`genlock-jitter-report --json`), and reconstruct each camera's TRUE absolute transit time as

    measured_latency_ms[cam] = latency_ms[cam] + mean_head_skew_ms[cam]

-- the genlock pin ACTIVE during the window, corrected by the SIGNED mean deviation of the
actual arrival from that pin's own release schedule (`camera_box::jitter_audit`'s
`mean_head_skew_ms`, #757 -- see that field's own doc for the derivation). Feeding this into
the EXISTING #286 `compute_phase_sync_offsets` kernel (via `phase_sync_calibrate.py`, never
touched here) produces the SAME slowest-anchored relative pin set a full recording-based
measurement would -- without needing one.

**Jitter headroom margin (#757, 2026-07-15 live regression):** a camera pinned with ZERO
headroom above its own measured transit sits exactly at the ts-align release deadline, so
ordinary jitter flips individual frames across the slot boundary -- observed live as a uniform
copies≈gaps pattern on EVERY camera the first time auto-pin equalization actually ran.
`compute_margin_ms` estimates a per-run margin from the SAME calibration-window measurement
(the worst per-camera jitter observed, floored at a sane minimum) for the caller to pass to
`phase_sync_calibrate.py --margin-ms` -- see that flag's own doc for how the shift is applied.

Pure functions here (`measured_by_camera`, `source_names_by_template`, `compute_margin_ms`) do
NO I/O and are unit tested with NO rig (`tests/python/test_prerecord_phase_calibrate.py`). The
thin CLI wrapper (`main`) just wires argv/files to them; the actual OBS-WS apply is
`phase_sync_calibrate.py`'s job, invoked separately by the harness with this script's output.

Usage:
    prerecord_phase_calibrate.py --jitter-json jitter.json --out strih-measured.json \
        --margin-out margin.txt [--margin-floor-ms 10]
"""
from __future__ import annotations

import argparse
import json
import re
import sys

# Strih's own per-camera main-input naming convention (set-ndi-mapping.py DEFAULT_MAP,
# the #753 1:1 pivot -- 'NDI cam<N>' IS physical camera N).
_STRIH_SOURCE_RE = re.compile(r"^NDI cam(\d+)$")

DEFAULT_MARGIN_FLOOR_MS = 10.0


def measured_by_camera(jitter_json: dict) -> dict:
    """From `genlock-jitter-report --json` output (keyed by STRIH source name
    ``"NDI cam<N>"``), reconstruct each camera's measured absolute cam->strih transit latency:
    ``latency_ms`` (the EFFECTIVE pin active during the sampled window) plus
    ``mean_head_skew_ms`` (the SIGNED mean deviation of the actual arrival from that pin's own
    release schedule -- #757). Keyed by camera NUMBER (not source name).

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
    ``"NDI cam{n}"`` for strih) -- values unchanged. Pure string formatting, no I/O, no
    rounding (phase_sync_calibrate.py's own kernel handles clamping/rounding)."""
    return {name_template.format(n=n): v for n, v in measured_by_cam.items()}


def compute_margin_ms(jitter_json: dict, floor_ms: float = DEFAULT_MARGIN_FLOOR_MS) -> float:
    """#757 -- PURE: estimate the jitter-headroom margin for `phase_sync_calibrate.py
    --margin-ms` from THIS SAME calibration window's own measurement, so the margin tracks
    actual observed conditions instead of a hand-picked constant.

    Takes the WORST (max) `max_abs_head_skew_ms` across every strih `"NDI cam<N>"` source found
    in `jitter_json` -- the single biggest single-tick deviation any camera showed during the
    window, i.e. the largest jitter excursion a zero-margin pin would have to absorb -- and
    floors it at `floor_ms` (a margin that's too SMALL defeats the fix; one that's too LARGE
    only costs a few extra ms of end-to-end latency, so floor-not-cap is the safe direction).

    A source's `max_abs_head_skew_ms` that is missing or non-numeric is skipped (mirrors
    `measured_by_camera`'s own honesty rule). No cam sources found at all -> returns
    `floor_ms` unchanged (never a fabricated 0 -- an unmeasured window still gets the safe
    floor, not "no margin at all")."""
    worst = 0.0
    if isinstance(jitter_json, dict):
        for name, s in jitter_json.items():
            if not _STRIH_SOURCE_RE.match(name) or not isinstance(s, dict):
                continue
            v = s.get("max_abs_head_skew_ms")
            if isinstance(v, (int, float)) and not isinstance(v, bool):
                worst = max(worst, float(v))
    return max(floor_ms, worst)


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
        "--margin-out", default=None,
        help="ALSO write the computed jitter-headroom margin (ms, see compute_margin_ms) as "
             "a plain number to this file, for the caller to pass to "
             "phase_sync_calibrate.py --margin-ms",
    )
    ap.add_argument(
        "--margin-floor-ms", type=float, default=DEFAULT_MARGIN_FLOOR_MS,
        help=f"minimum margin regardless of measured jitter (default {DEFAULT_MARGIN_FLOOR_MS})",
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

    if args.margin_out:
        margin = compute_margin_ms(jitter_json, args.margin_floor_ms)
        with open(args.margin_out, "w", encoding="utf-8") as f:
            f.write(f"{margin:.1f}\n")
        print(f"prerecord_phase_calibrate: computed margin={margin:.1f}ms -> {args.margin_out}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
