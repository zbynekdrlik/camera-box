"""issue 1168 task 2/3 -- production per-box equalization + re-tighten on the JITTER-FLOOR instrument.

The mining (15.9. 20:15) shows a STABLE per-box presented-age offset (cam4 71.3 .. cam2 85.0 ms,
median cross-camera spread 13.7 ms) at the same 3 ms pin -- so production is NOT equalized between
runs. Task 2 wanted to carry that excess via strih pins; task 3 wanted a hard-fail re-tighten on the
residual. The DECISIVE physics (issue 1049 narrative in src/genlock_backlog.rs; issue 1252 gate; live
run 1899055119): the strih FIFO is a WHOLE-source-frame conveyor, so a SUB-source-frame cross-camera
spread is IRREDUCIBLE by any pin -- a whole-frame hold overshoots, a sub-frame hold limit-cycles and
DOUBLES the on-screen spread. These pure functions encode the DIRECTION-CORRECT (anchor = OLDEST /
max-floor camera) equalization and emit an above-floor pin ONLY when it measurably REDUCES the spread.
Tier-0: pytest, no rig (the issue-1199 python-mirror precedent).
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402
import latency_pins_verify as lpv  # noqa: E402

# The mining floors (15.9. 20:15) as {source: arrival_floor_ms}.
MINING = {
    "NDI cam1": 77.7, "NDI cam2": 85.0, "NDI cam3": 81.1, "NDI cam4": 71.3,
    "NDI cam5": 72.8, "NDI cam6": 75.0, "NDI cam7": 74.0,
}
SF = qa.SOURCE_FRAME_MS  # ~16.667 ms, one 60 fps source frame


# ------------------------------------------------------ cross_camera_floor_spread -------------------
def test_cross_camera_floor_spread_is_max_minus_min():
    assert abs(qa.cross_camera_floor_spread(MINING) - (85.0 - 71.3)) < 1e-9


def test_cross_camera_floor_spread_degenerate():
    assert qa.cross_camera_floor_spread({}) == 0.0
    assert qa.cross_camera_floor_spread({"NDI cam1": 80.0}) == 0.0


# ------------------------------------------------------ floor_spread_hard_fail -----------------------
def test_floor_spread_hard_fail_above_tolerance():
    assert qa.floor_spread_hard_fail(40.0, tolerance_ms=33.3) is True
    assert qa.floor_spread_hard_fail(13.7, tolerance_ms=33.3) is False


def test_floor_spread_hard_fail_none_is_honest_false():
    # honest-None: a missing measurement NEVER fabricates a fail.
    assert qa.floor_spread_hard_fail(None) is False


def test_floor_spread_default_tolerance_is_one_canvas_frame():
    # documented: the hard-fail bound is ~one 30 fps canvas frame (33.3 ms), NOT 15 ms -- a 15 ms
    # bound would false-fail (the 13.7 ms median + the 8.4 ms anchor run-level std routinely exceeds
    # it), and below one source frame the spread is irreducible by the pin conveyor.
    assert 33.0 <= qa.DEFAULT_FLOOR_SPREAD_TOLERANCE_MS <= 34.0


# ------------------------------------------------------ floor_equalization_plan ---------------------
def test_mining_data_is_sub_frame_and_NOT_reducible_so_plan_stays_floor():
    """THE core finding: the mining per-box excess is sub-source-frame, so a whole-frame hold
    overshoots and does NOT reduce the spread -> the plan MUST stay at the floor (no regression)."""
    plan, meta = qa.floor_equalization_plan(MINING)
    assert plan == {s: qa.DEFAULT_FLOOR_MS for s in MINING}
    assert meta["reducible"] is False
    assert meta["anchor"] == "NDI cam2"          # the OLDEST (max-floor) camera anchors, not cam4
    assert abs(meta["pre_spread_ms"] - 13.7) < 0.05
    assert meta["added_latency_ms"] == 0.0


def test_direction_anchor_is_the_oldest_camera_and_the_freshest_gets_the_pin():
    """An integer-source-frame spread IS reducible: the OLDEST (max-floor) camera anchors at the
    floor and the FRESHEST (min-floor) camera gets the added-latency pin -- the MIRROR of the
    dispatch's inverted worked numbers."""
    floors = {"NDI cam2": 88.0, "NDI cam4": 71.3}   # ~one source frame apart
    plan, meta = qa.floor_equalization_plan(floors)
    assert meta["reducible"] is True
    assert meta["anchor"] == "NDI cam2"             # oldest anchors at the floor
    assert plan["NDI cam2"] == qa.DEFAULT_FLOOR_MS
    assert plan["NDI cam4"] > qa.DEFAULT_FLOOR_MS   # the freshest camera carries the added latency
    assert meta["post_spread_ms"] < meta["pre_spread_ms"]


def test_over_budget_camera_is_clamped_to_floor_never_deep_pinned():
    """A fresher camera whose EQUALIZED present age would exceed the 94 ms ceiling is clamped to the
    floor (deep-pin doctrine) -> not reducible -> floor-only plan."""
    floors = {"NDI cam2": 96.0, "NDI cam4": 79.0}   # equalizing cam4 -> ~95.7 > 94
    plan, meta = qa.floor_equalization_plan(floors)
    assert plan == {s: qa.DEFAULT_FLOOR_MS for s in floors}
    assert meta["reducible"] is False


def test_degraded_spread_beyond_sanity_is_not_equalized():
    floors = {"NDI cam2": 100.0, "NDI cam4": 10.0}  # 90 ms spread > 66 ms sanity
    plan, meta = qa.floor_equalization_plan(floors)
    assert plan == {s: qa.DEFAULT_FLOOR_MS for s in floors}
    assert meta["reducible"] is False
    assert meta["reason"] == "degraded"


def test_sub_frame_only_flag():
    floors = {"NDI cam2": 78.0, "NDI cam4": 71.3}   # 6.7 ms < half a source frame
    plan, meta = qa.floor_equalization_plan(floors)
    assert meta["sub_frame_only"] is True
    assert plan == {s: qa.DEFAULT_FLOOR_MS for s in floors}


def test_empty_and_single_source():
    plan, meta = qa.floor_equalization_plan({})
    assert plan == {}
    assert meta["reducible"] is False
    plan, meta = qa.floor_equalization_plan({"NDI cam1": 80.0})
    assert plan == {"NDI cam1": qa.DEFAULT_FLOOR_MS}
    assert meta["reducible"] is False


# ------------------------------------------------------ baseline_strih_block ------------------------
def test_baseline_strih_block_all_floor_for_sub_frame_data():
    plan, _ = qa.floor_equalization_plan(MINING)
    block = qa.baseline_strih_block(plan, list(MINING), qa.DEFAULT_FLOOR_MS)
    assert block == {s: 3 for s in MINING}
    assert all(isinstance(v, int) for v in block.values())


def test_baseline_strih_block_carries_applied_pins():
    block = qa.baseline_strih_block({"NDI cam4": 20}, ["NDI cam2", "NDI cam4"], qa.DEFAULT_FLOOR_MS)
    assert block == {"NDI cam2": 3, "NDI cam4": 20}


# --------------------------------------------- 2c: verify reads an above-floor baseline pin ---------
def test_verify_box_reads_above_floor_baseline_pin_as_expected_not_drift():
    """2c confirm: latency_pins_verify.verify_box already diffs the LIVE pin against WHATEVER the
    persisted strih baseline says -- so an above-floor equalization pin in the baseline is the
    EXPECTED value (no false drift), and a live pin matching it reports clean."""
    baseline_box = {"NDI cam2": 3, "NDI cam4": 20}
    # live pins match the persisted equalization baseline -> no drift
    assert lpv.verify_box("strih", baseline_box, {"NDI cam2": 3, "NDI cam4": 20}) == []
    # a live pin that DIVERGED from the persisted baseline -> a drift line naming input+got+want
    drifts = lpv.verify_box("strih", baseline_box, {"NDI cam2": 3, "NDI cam4": 3})
    assert len(drifts) == 1 and "NDI cam4" in drifts[0]


# --------------------------------------------- CLI: --equalization-plan diagnostic ------------------
def _write_jitter(tmp_path, floors):
    """A minimal genlock-jitter-report --json dict carrying the given per-source arrival floors
    (latency_ms + mean_head_skew_ms = the floor; well-sampled so no phantom-floor drop)."""
    jj = {}
    for s, fl in floors.items():
        n = int(s.replace("NDI cam", ""))
        jj[s] = {"camera": n, "latency_ms": qa.DEFAULT_FLOOR_MS,
                 "mean_head_skew_ms": fl - qa.DEFAULT_FLOOR_MS, "samples": 30}
    p = tmp_path / "jitter.json"
    p.write_text(json.dumps(jj), encoding="utf-8")
    return p


def test_cli_equalization_plan_mode_prints_json(tmp_path):
    jj = _write_jitter(tmp_path, MINING)
    out = subprocess.run(
        [sys.executable, str(_SCRIPTS / "qr_align_pins.py"), "--equalization-plan",
         "--sources", ",".join(MINING), "--jitter-json", str(jj), "--host", "x"],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    payload = json.loads(out.stdout)
    assert payload["reducible"] is False           # sub-frame mining data
    assert payload["anchor"] == "NDI cam2"
    assert payload["plan"] == {s: 3 for s in MINING}
    assert "floor_spread_ms" in payload and abs(payload["floor_spread_ms"] - 13.7) < 0.05
