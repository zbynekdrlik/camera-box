"""#312 Phase-2 — unit tests for the PURE all-cambox switch-schedule generation.

scripts/switch_schedule.py turns the recording-e2e ALL_CAMBOX sweep (cut each cambox into strih
program ~SEGMENT_SECS, cycling the sweep to cover DURATION) into the ordered, non-overlapping
``[{cambox,start_ns,end_ns}]`` windows recording-verdict --switch-schedule consumes (the same
shape src/probe/recording_segments.rs validates). These tests pin the pure logic — sweep parsing
(scene names contain spaces), segment cycling, duration coverage, and the ordered/non-overlapping
window assembly — with NO rig and NO OBS.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "switch_schedule.py"
_spec = importlib.util.spec_from_file_location("switch_schedule", _MOD_PATH)
switch_schedule = importlib.util.module_from_spec(_spec)
sys.modules["switch_schedule"] = switch_schedule
_spec.loader.exec_module(switch_schedule)

S = 1_000_000_000  # one second in ns
DEFAULT_SWEEP = "Cam 5:CAM1 Cam 3:CAM2 Cam 1:CAM4"


def test_parse_sweep_handles_scene_names_with_spaces():
    # The default sweep's scene names contain spaces ("Cam 5"); the pair separator is ':' and the
    # label (CAM1/2/4) carries no space — a naive whitespace split would shred the pairs.
    assert switch_schedule.parse_sweep(DEFAULT_SWEEP) == [
        ("Cam 5", "CAM1"),
        ("Cam 3", "CAM2"),
        ("Cam 1", "CAM4"),
    ]


def test_parse_sweep_rejects_empty_and_malformed():
    with pytest.raises(ValueError):
        switch_schedule.parse_sweep("")
    with pytest.raises(ValueError):
        switch_schedule.parse_sweep("   ")
    # A trailing scene with no ':<label>' is malformed (it would switch nothing for that segment).
    with pytest.raises(ValueError):
        switch_schedule.parse_sweep("Cam 5:CAM1 Cam 3")
    # An empty label after ':'.
    with pytest.raises(ValueError):
        switch_schedule.parse_sweep("Cam 5:")


def test_n_segments_covers_the_duration():
    # ceil(duration / segment_secs), at least 1 — rounds UP so the tail is always covered.
    assert switch_schedule.n_segments(30, 90) == 3
    assert switch_schedule.n_segments(30, 75) == 3
    assert switch_schedule.n_segments(30, 120) == 4
    assert switch_schedule.n_segments(30, 1) == 1
    assert switch_schedule.n_segments(30, 1800) == 60


def test_n_segments_rejects_nonpositive():
    with pytest.raises(ValueError):
        switch_schedule.n_segments(0, 90)
    with pytest.raises(ValueError):
        switch_schedule.n_segments(30, 0)


def test_plan_segments_cycles_the_sweep():
    sweep = switch_schedule.parse_sweep(DEFAULT_SWEEP)  # 3 boxes
    # 3 segments → each box once, in order.
    assert [lbl for _s, lbl in switch_schedule.plan_segments(sweep, 30, 90)] == [
        "CAM1", "CAM2", "CAM4",
    ]
    # 4 segments → the sweep CYCLES: the first box reappears as segment 4.
    assert [lbl for _s, lbl in switch_schedule.plan_segments(sweep, 30, 120)] == [
        "CAM1", "CAM2", "CAM4", "CAM1",
    ]
    # 7 segments → two full cycles + one (proves the modulo cycling, not a one-off wrap).
    assert [lbl for _s, lbl in switch_schedule.plan_segments(sweep, 30, 210)] == [
        "CAM1", "CAM2", "CAM4", "CAM1", "CAM2", "CAM4", "CAM1",
    ]
    # The scene travels with the label (so the harness switches the right OBS scene).
    assert switch_schedule.plan_segments(sweep, 30, 90)[0] == ("Cam 5", "CAM1")


def test_build_switch_schedule_windows_ordered_nonoverlapping_cover_duration():
    sweep = switch_schedule.parse_sweep(DEFAULT_SWEEP)
    segment_secs, duration = 30, 120  # → 4 segments, the 3-box sweep cycles
    n = switch_schedule.n_segments(segment_secs, duration)
    start_ns = 1_000_000_000_000
    # Synthetic captured boundaries: one CLOSING boundary per segment (exactly what the bash loop
    # captures — each later switch + the final StopRecord).
    boundaries = [start_ns + (i + 1) * segment_secs * S for i in range(n)]
    windows = switch_schedule.build_switch_schedule(
        sweep, segment_secs, duration, start_ns, boundaries
    )
    assert len(windows) == n
    # Labels cycle the sweep correctly.
    assert [w["cambox"] for w in windows] == ["CAM1", "CAM2", "CAM4", "CAM1"]
    # Every window has start_ns < end_ns.
    for w in windows:
        assert w["start_ns"] < w["end_ns"]
    # Ordered + non-overlapping (half-open, contiguous): each window ends exactly where the next
    # starts — the same invariant validate_schedule enforces (start_ns >= prev.end_ns).
    for prev, cur in zip(windows, windows[1:]):
        assert cur["start_ns"] >= prev["end_ns"]
        assert cur["start_ns"] == prev["end_ns"]
    # Cover the duration: the total span is at least the requested duration.
    span_ns = windows[-1]["end_ns"] - windows[0]["start_ns"]
    assert span_ns >= duration * S
    # The first window opens exactly at start_ns.
    assert windows[0]["start_ns"] == start_ns


def test_build_switch_schedule_rejects_boundary_count_mismatch():
    sweep = switch_schedule.parse_sweep(DEFAULT_SWEEP)
    # 3 segments expected (90s / 30s) but only 2 boundaries supplied → a harness bug, fail loud.
    with pytest.raises(ValueError):
        switch_schedule.build_switch_schedule(sweep, 30, 90, 0, [30 * S, 60 * S])


def test_build_switch_schedule_rejects_nonincreasing_boundaries():
    sweep = switch_schedule.parse_sweep(DEFAULT_SWEEP)
    # 3 segments, but a boundary goes BACKWARDS (0 → 30s → 20s): window 1 would have
    # start_ns(30s) >= end_ns(20s) → must raise (never a zero-width / overlapping window).
    with pytest.raises(ValueError):
        switch_schedule.build_switch_schedule(sweep, 30, 90, 0, [30 * S, 20 * S, 90 * S])
