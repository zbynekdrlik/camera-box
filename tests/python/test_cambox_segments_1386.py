"""issue 1386 item 5 -- the ONE per-camera grouping of a verdict's per-window segments.

scripts/cambox_segments.py `per_camera` is shared by the soak's loss columns
(av_soak_decision._loss_by_camera, which adds the gate's own per-window term on top) and the Discord
report's per-camera zero-loss lines (e2e_discord_report._aggregate_segments). This pins its rules,
that both consumers really delegate to it, and that on the real verdict fixtures the two consumers'
per-camera counts and strict pass agree by construction. Tier-0: pure python.
"""
import glob
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.normpath(os.path.join(HERE, "..", "..", "scripts"))
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)

import av_soak_decision as asd  # noqa: E402
import cambox_segments as cs  # noqa: E402
import e2e_discord_report as edr  # noqa: E402

FIXTURES = sorted(glob.glob(os.path.join(HERE, "fixtures", "e2e_discord_report", "*.json"))
                  + glob.glob(os.path.join(HERE, "fixtures", "av_soak", "*.json")))


def _seg(cam, **kw):
    s = {"cambox": cam, "pass": True, "frames": 900, "copies": 0, "gaps": 0, "undecodable": 0}
    s.update(kw)
    return s


def test_groups_by_lower_cased_camera_in_first_seen_order_and_sums_the_counters():
    segs = [_seg("CAM3", copies=1), _seg("CAM1", gaps=2), _seg("cam3", undecodable=4, frames=100)]
    agg = cs.per_camera(segs)
    assert list(agg) == ["cam3", "cam1"]
    assert (agg["cam3"]["frames"], agg["cam3"]["copies"], agg["cam3"]["undecodable"]) == (1000, 1, 4)
    assert agg["cam1"]["gaps"] == 2
    assert agg["cam3"]["segments"] == [segs[0], segs[2]]


def test_cams_keeps_only_the_named_cameras():
    agg = cs.per_camera([_seg("CAM1"), _seg("CAM2"), _seg("CAM4")], cams=["cam1", "cam4"])
    assert list(agg) == ["cam1", "cam4"]


def test_pass_is_the_and_of_a_true_pass_and_a_flag_is_the_or():
    agg = cs.per_camera([_seg("CAM1", multi_source={"x": 1}), _seg("CAM1"), _seg("CAM2")],
                        flags=("multi_source", "switch_in_transient"))
    assert agg["cam1"]["multi_source"] is True and agg["cam2"]["multi_source"] is False
    assert agg["cam1"]["switch_in_transient"] is False
    assert agg["cam1"]["pass"] is True
    bad = cs.per_camera([_seg("CAM1"), _seg("CAM1", **{"pass": False})])
    assert bad["cam1"]["pass"] is False
    for not_true in (1, "true", None):
        assert cs.per_camera([_seg("CAM1", **{"pass": not_true})])["cam1"]["pass"] is False


def test_a_count_that_is_not_a_finite_number_counts_zero_and_a_float_is_truncated():
    for junk in (None, "900", True, float("nan"), float("inf")):
        assert cs.per_camera([_seg("CAM1", frames=junk)])["cam1"]["frames"] == 0
    assert cs.per_camera([_seg("CAM1", frames=900.9)])["cam1"]["frames"] == 900


def test_malformed_input_never_crashes():
    assert cs.per_camera(None) == {} and cs.per_camera({"cambox": "CAM1"}) == {}
    assert list(cs.per_camera([_seg("CAM1"), "junk", 7])) == ["cam1"]


def test_both_consumers_delegate_to_the_one_grouping(monkeypatch):
    calls = []
    real = cs.per_camera

    def spy(*a, **kw):
        calls.append(kw.get("flags"))
        return real(*a, **kw)

    monkeypatch.setattr(cs, "per_camera", spy)
    assert asd.cambox_segments is cs and edr.cambox_segments is cs
    asd._loss_by_camera([_seg("CAM1")], ["cam1"], {})
    edr._aggregate_segments([_seg("CAM1")])
    assert calls == [("multi_source",), ("switch_in_transient",)]


def test_the_two_consumers_agree_on_every_real_fixture():
    assert FIXTURES
    checked = 0
    for f in FIXTURES:
        v = json.load(open(f))
        cont = v.get("all_cambox_continuity") or {}
        for segs in (cont.get("segments"), (cont.get("imag") or {}).get("segments")):
            if not segs:
                continue
            cams = sorted({str(s.get("cambox", "")).lower() for s in segs})
            soak = asd._loss_by_camera(segs, cams, cont)
            disc = edr._aggregate_segments(segs)
            assert set(soak) == set(disc), f
            for cam in soak:
                for k in ("frames", "copies", "gaps", "undecodable"):
                    assert soak[cam][k] == disc[cam][k], (f, cam, k)
                assert soak[cam]["strict"] == disc[cam]["pass"], (f, cam)
                checked += 1
    assert checked > 0


# -- the two per-window limits (issue 1386 slice C): the soak's gate term and the report's
#    continuity line judge a window with the same predicates ------------------------------------

def test_over_copies_gaps_tolerance_compares_copies_and_gaps_with_the_window_tolerance():
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", copies=6), 5) is True
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", gaps=6), 5) is True
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", copies=5, gaps=5), 5) is False
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", copies=1), 0) is True
    # a count that is not a finite number counts 0, a float is truncated (the per_camera rule)
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", copies=None, gaps="9"), 0) is False
    assert cs.over_copies_gaps_tolerance(_seg("CAM1", copies=5.9), 5) is False


def test_a_multi_source_window_is_never_over_its_tolerance():
    ms = {"tag": "multi-source", "multi_path_suspect_fraction": 0.4}
    assert cs.over_copies_gaps_tolerance(_seg("CAM2", copies=40, gaps=40, multi_source=ms), 5) is False
    assert cs.over_copies_gaps_tolerance(_seg("CAM2", copies=40, multi_source=None), 5) is True


def test_over_window_floor_needs_frames_and_more_undecodable_than_the_floor():
    assert cs.over_window_floor(_seg("CAM1", undecodable=5), 4) is True
    assert cs.over_window_floor(_seg("CAM1", undecodable=4), 4) is False
    assert cs.over_window_floor(_seg("CAM1", undecodable=9, frames=0), 4) is False
    assert cs.over_window_floor(_seg("CAM1", undecodable=9, frames=None), 4) is False


def test_both_consumers_judge_a_window_with_the_shared_limits(monkeypatch):
    calls = []
    real_tol, real_floor = cs.over_copies_gaps_tolerance, cs.over_window_floor

    def spy_tol(seg, tol):
        calls.append(("tol", tol))
        return real_tol(seg, tol)

    def spy_floor(seg, floor):
        calls.append(("floor", floor))
        return real_floor(seg, floor)

    monkeypatch.setattr(cs, "over_copies_gaps_tolerance", spy_tol)
    monkeypatch.setattr(cs, "over_window_floor", spy_floor)
    cont = {"copies_gaps_tolerance_gates_overall_pass": True, "undecodable_floor_gates_overall_pass": True,
            "per_window_undecodable_floor": 6, "copies_gaps_tolerance": 5}
    assert asd.gate_window_term(_seg("CAM1", copies=6), cont) is False
    assert calls == [("floor", 6), ("tol", 5)]
    calls.clear()
    v = {"overall_pass": False, "all_cambox_continuity": dict(cont, overall_pass=False,
                                                              segments=[_seg("CAM3", gaps=7)])}
    labels = [lbl for lbl, _ in edr._blocking_failures(v) if "kontinuita" in lbl.lower()]
    assert labels and "CAM3" in labels[0]
    assert ("tol", 5) in calls and ("floor", 6) in calls
