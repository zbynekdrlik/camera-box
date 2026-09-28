"""issue 1386 -- the ONE per-camera aggregation of a recording verdict's per-window segments.

A recording verdict lists one entry per camera window in `all_cambox_continuity.segments[]` (and the
imag leg's `all_cambox_continuity.imag.segments[]`); a camera owns several windows when the sweep
cycles. Two consumers group them per camera:

  * scripts/av_soak_decision.py `_loss_by_camera` -- the 8 h soak's loss columns, graded on the
    gate's own per-window term (`gate_window_term`, which stays in the soak: it is its only user);
  * scripts/e2e_discord_report.py `_aggregate_segments` -- the per-camera zero-loss lines.

Both used to carry their own copy of the grouping. `per_camera` is it, once. Pure, std only.

The same two consumers also judge ONE window against the gate's two per-window limits: the soak
grades it with `gate_window_term`; the report's red continuity line names the camera over its
tolerance and adds the optical-floor clause. `over_copies_gaps_tolerance` and `over_window_floor`
are those two limits, once (issue 1386).

Input rules (the recording-verdict JSON writes integer counts and boolean `pass`, so none of these
bite on a real verdict; they keep a malformed file from crashing a report):
  * `segments` that is not a list -> no cameras; a segment that is not a dict is skipped;
  * a count that is not a finite int/float (None, a string, a bool, NaN) counts 0; a float is
    truncated to int before it is summed;
  * `pass` is the AND of `seg["pass"] is True` (a missing or non-True value fails the camera);
  * each name in `flags` is the OR of `bool(seg[name])` over the camera's segments.
"""
import math

COUNTERS = ("frames", "copies", "gaps", "undecodable")


def _count(v):
    """A per-window count as an int: 0 unless it is a finite int/float that is not a bool."""
    if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v):
        return int(v)
    return 0


def per_camera(segments, cams=None, flags=(), key="cambox"):
    """Group `segments` by lower-cased `seg[key]`, in first-seen order.

    `cams` (optional) keeps only those camera names. Returns
    ``{cam: {"frames", "copies", "gaps", "undecodable": int sums, "pass": bool,
    <flag>: bool for each flag, "segments": [the camera's segments in order]}}``."""
    agg = {}
    for seg in segments if isinstance(segments, list) else []:
        if not isinstance(seg, dict):
            continue
        cam = str(seg.get(key, "")).lower()
        if cams is not None and cam not in cams:
            continue
        a = agg.get(cam)
        if a is None:
            a = {k: 0 for k in COUNTERS}
            a["pass"] = True
            a.update({f: False for f in flags})
            a["segments"] = []
            agg[cam] = a
        for k in COUNTERS:
            a[k] += _count(seg.get(k))
        a["pass"] = a["pass"] and seg.get("pass") is True
        for f in flags:
            a[f] = a[f] or bool(seg.get(f))
        a["segments"].append(seg)
    return agg


def over_copies_gaps_tolerance(seg, tol):
    """True iff the window's copies or gaps exceed `tol` -- the copies/gaps half of the gate's
    per-window term (src/window_gate.rs, with the per-cambox tolerance a window carries). A
    multi-source window (issue 1367, a truthy `multi_source` tag) is never over: its copies/gaps are
    report-only, it is judged by its node burn. `tol` must be a number."""
    if seg.get("multi_source"):
        return False
    return _count(seg.get("copies")) > tol or _count(seg.get("gaps")) > tol


def over_window_floor(seg, floor):
    """True iff a window that recorded frames has more undecodable frames than the per-window
    optical floor -- the floor half of the gate's per-window term (issue 905 item 3). A window
    with no frames is not over the floor (the gate fails it for its frame count instead)."""
    return _count(seg.get("frames")) > 0 and _count(seg.get("undecodable")) > floor
