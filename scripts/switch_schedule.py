#!/usr/bin/env python3
"""#312 Phase-2 — PURE switch-schedule generation for the all-cambox sweep.

The recording-e2e all-cambox sweep (scripts/recording-e2e.sh ALL_CAMBOX=1) cuts each active
cambox into strih PROGRAM for ~SEGMENT_SECS, cycling the sweep until the total reaches
DURATION, while ONE continuous stream recording runs. All boxes capture the SAME cam2-painted
tick through the HDMI splitter, so per-segment painted-tick continuity == per-box zero-loss.

Each segment's program switch is timestamped on dev1's CLOCK_REALTIME epoch-ns — which
DanteSync slaves to the painter, so it IS the burn ``gen_ts_ns`` timeline the verdict keys on
(recording-verdict --switch-schedule). This module turns the captured switch boundaries into
the ordered, non-overlapping ``[{cambox,start_ns,end_ns}]`` windows the verdict consumes (the
same shape src/probe/recording_segments.rs validates on load).

The pure functions here (parse_sweep, n_segments, plan_segments, build_switch_schedule) do NO
I/O, so they are unit-tested in CI (tests/python/test_switch_schedule.py) with no rig, no OBS.
"""
import argparse
import json
import math
import sys


def parse_sweep(spec):
    """Parse a ``"<scene>:<label> <scene>:<label> ..."`` sweep spec into ``[(scene, label), ...]``.

    The default sweep is ``"Cam 5:CAM1 Cam 3:CAM2 Cam 1:CAM4"``: SCENE NAMES CONTAIN SPACES
    (``"Cam 5"``), so a plain whitespace split breaks the pairs. Each pair's LABEL (``CAM1`` /
    ``CAM2`` / ``CAM4`` — no spaces, no ``:``) is the token after the ``:``; the scene is
    everything before it. We split on whitespace and re-join tokens until one carries the ``:``
    that closes the pair: that token's pre-``:`` part finishes the scene, its post-``:`` part is
    the label.

    Raises ValueError on an empty spec, a trailing scene with no ``:<label>``, an empty scene, or
    an empty label — a bad sweep must fail LOUDLY, never silently switch the wrong/zero camboxes.
    """
    if not spec or not spec.strip():
        raise ValueError("sweep is empty — at least one '<scene>:<label>' pair is required")
    pairs = []
    scene_words = []
    for tok in spec.split():
        if ":" in tok:
            head, label = tok.rsplit(":", 1)
            if head:
                scene_words.append(head)
            scene = " ".join(scene_words)
            label = label.strip()
            if not scene:
                raise ValueError(f"sweep pair has an empty scene before ':' (token {tok!r})")
            if not label:
                raise ValueError(f"sweep pair has an empty label after ':' (token {tok!r})")
            pairs.append((scene, label))
            scene_words = []
        else:
            scene_words.append(tok)
    if scene_words:
        raise ValueError(
            f"sweep has a trailing scene with no ':<label>': {' '.join(scene_words)!r}"
        )
    if not pairs:
        raise ValueError("sweep parsed to zero '<scene>:<label>' pairs")
    return pairs


def n_segments(segment_secs, duration):
    """Number of segments to COVER ``duration`` at ``segment_secs`` each: ``ceil(duration /
    segment_secs)``, at least 1. The sweep cycles across these segments (a box reappears when the
    sweep is shorter than the segment count)."""
    if segment_secs <= 0:
        raise ValueError(f"segment_secs must be > 0, got {segment_secs}")
    if duration <= 0:
        raise ValueError(f"duration must be > 0, got {duration}")
    return max(1, math.ceil(duration / segment_secs))


def plan_segments(sweep, segment_secs, duration):
    """The ordered ``(scene, label)`` per segment, CYCLING ``sweep`` to cover ``duration``.

    Segment ``i`` uses ``sweep[i % len(sweep)]`` — so with a 3-box sweep and 4 segments the
    first box repeats as segment 4. ``sweep`` is ``[(scene, label), ...]`` (already parsed)."""
    if not sweep:
        raise ValueError("sweep is empty")
    n = n_segments(segment_secs, duration)
    return [sweep[i % len(sweep)] for i in range(n)]


def build_switch_schedule(sweep, segment_secs, duration, start_ns, boundaries_ns):
    """PURE: assemble the ordered, non-overlapping ``[{cambox,start_ns,end_ns}]`` switch-schedule
    windows from the sweep plan + the captured switch boundaries.

    Args:
      sweep:         ``[(scene, label), ...]`` the cambox cut order (cycled to cover ``duration``).
      segment_secs:  nominal seconds per segment (with ``duration``, decides the segment count).
      duration:      total target seconds (the sweep cycles until the segments cover it).
      start_ns:      epoch-ns of the FIRST segment's program switch (window[0].start_ns), on the
                     burn ``gen_ts_ns`` timeline (dev1 CLOCK_REALTIME, DanteSync-slaved to the
                     painter).
      boundaries_ns: the captured epoch-ns boundary CLOSING each segment — segment ``i`` ends at
                     ``boundaries_ns[i]`` (== segment ``i+1``'s switch time; the last entry is the
                     StopRecord time). ``len(boundaries_ns)`` MUST equal the segment count.

    Window ``i`` = ``{cambox: label_i, start_ns: points[i], end_ns: points[i+1]}`` over
    ``points = [start_ns] + boundaries_ns``, so windows are contiguous + non-overlapping by
    construction. Raises ValueError on a boundary-count mismatch or any non-increasing boundary
    (a window must have ``start_ns < end_ns``). The Rust verdict re-validates on load (belt+braces).
    """
    plan = plan_segments(sweep, segment_secs, duration)
    n = len(plan)
    boundaries_ns = [int(b) for b in boundaries_ns]
    if len(boundaries_ns) != n:
        raise ValueError(
            f"expected {n} segment-closing boundaries (one per segment) for a {duration}s run at "
            f"{segment_secs}s/segment, got {len(boundaries_ns)}"
        )
    points = [int(start_ns)] + boundaries_ns
    windows = []
    for i, (_scene, label) in enumerate(plan):
        s, e = points[i], points[i + 1]
        if s >= e:
            raise ValueError(
                f"window {i} ({label}) must have start_ns < end_ns (switch boundaries must "
                f"strictly increase): start_ns={s} end_ns={e}"
            )
        windows.append({"cambox": label, "start_ns": s, "end_ns": e})
    return windows


def _cmd_plan(a):
    sweep = parse_sweep(a.sweep)
    for scene, label in plan_segments(sweep, a.segment_secs, a.duration):
        # TAB-separated so a scene name with spaces survives the harness's `read`/`mapfile`.
        print(f"{scene}\t{label}")


def _cmd_build(a):
    sweep = parse_sweep(a.sweep)
    boundaries = [int(x) for x in a.boundaries.split(",") if x.strip()]
    windows = build_switch_schedule(sweep, a.segment_secs, a.duration, a.start_ns, boundaries)
    json.dump(windows, sys.stdout, indent=2)
    sys.stdout.write("\n")


def main():
    ap = argparse.ArgumentParser(
        description="#312 Phase-2 all-cambox switch-schedule generation (pure)"
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_plan = sub.add_parser("plan", help="print the per-segment '<scene>\\t<label>' cut plan")
    p_plan.add_argument("--sweep", required=True)
    p_plan.add_argument("--segment-secs", type=int, required=True)
    p_plan.add_argument("--duration", type=int, required=True)
    p_plan.set_defaults(func=_cmd_plan)

    p_build = sub.add_parser(
        "build", help="emit the ordered switch-schedule JSON from the captured boundaries"
    )
    p_build.add_argument("--sweep", required=True)
    p_build.add_argument("--segment-secs", type=int, required=True)
    p_build.add_argument("--duration", type=int, required=True)
    p_build.add_argument("--start-ns", type=int, required=True)
    p_build.add_argument(
        "--boundaries", required=True,
        help="comma-separated epoch-ns, one CLOSING boundary per segment",
    )
    p_build.set_defaults(func=_cmd_build)

    a = ap.parse_args()
    a.func(a)


if __name__ == "__main__":
    main()
