"""#1252 -- the [4i/8align] above-floor plan DOUBLED the cross-camera spread on run 1899055119.

Root cause (proven offline from the run's REAL recorded inputs, byte-identical to the strih log):
the cameras are already aligned to the floor-3 achievable limit (ONE camera -> splitter -> every
cambox sees the identical image; the frame_id tail shows the "slowest" camera alternately AHEAD and
behind = jitter around zero). The ~16.7 ms cross-camera "residual" the plan measured is exactly ONE
source frame (1000/60 = 16.67 ms) = the N=2 (60-into-30) lock-phase quantum, NOT a real transport
lag. cam3's arrival floor read 84 (= 67 + 16.7) from only samples=2 -- a phantom "slowest".

floor_aware_pins then (a) treated the phantom as a real lag and (b) applied it as pin_i =
arrival_floor_i + delta_i -- an ABSOLUTE present-age target (~87 ms) written into a pin lever the
live rig treats ADDITIVELY (post_residual = pre_residual + pin_delta, proven on the run), so a
desired +16.7 ms hold became a +83 ms pin raise. Five cameras were shoved ~83 ms later while the
phantom "reference" cam3 stayed at floor 3, so the spread GREW from one source frame to ~84-100 ms.

The pin lever provably cannot close a sub-source-frame quantum -- it is presentation-phase jitter
with no consistent slowest camera to pin against, the lever only ADDS delay (can only grow the
spread), no pin can go below the 3 ms floor to pull a camera earlier, and the measurement itself
carries a +/- one-source-frame quantum so a "fix" could not even be verified. So a cross-camera
present-age spread within one source frame + hysteresis is an ALREADY-ALIGNED verdict (floor-3
quantum), NOT a pin-worthy lag. This does NOT widen the owner's same-frame parity bar -- it
suppresses the ABOVE-FLOOR PIN PLAN when its own input is a sub-frame phantom, nothing else.

Tier-0: a pure decision (within_aligned_quantum) on the run's REAL deltas + an align() flow test
against monkeypatched barrier/read/apply seams (no rig, no cargo).
"""
import pathlib
import sys
import zlib

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

RUN = 1_867_252_327          # a realistic painter epoch run_id (>> any burn id)
ID_NS = 8_333_333            # gen_ts step per frame_id (fixtures only)
SRC = ["NDI cam1", "NDI cam3", "NDI cam4", "NDI cam5", "NDI cam6", "NDI cam7"]
SOURCE_FRAME_MS = 1000.0 / 60.0   # 16.667 ms -- one 60 fps source frame

# The REAL pre-pin present-age deltas from run 1899055119's align-step-log.txt (round_deltas over
# zero pins; cam3 = the phantom slowest at 0, every other camera ~one source frame "faster").
RUN_DELTAS = {"NDI cam1": 16.44, "NDI cam3": 0.0, "NDI cam4": 17.14,
              "NDI cam5": 16.64, "NDI cam6": 16.85, "NDI cam7": 16.84}

# The REAL post-reset arrival-floor audit (qr-align-jitter-1899055119.json) subset the plan consumed:
# arrival_floor = latency_ms + mean_head_skew_ms -> cam1 70, cam3 87 (phantom), cam4 74.5, cam5-7 70.
RUN_JITTER = {
    "NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 2},
    "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 84.0, "samples": 2},
    "NDI cam4": {"latency_ms": 3, "mean_head_skew_ms": 71.5, "samples": 2},
    "NDI cam5": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 2},
    "NDI cam6": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 2},
    "NDI cam7": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 2},
}


def _payload(run_id, frame_id, gen_ts_ns):
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    return f"P{body}.{zlib.crc32(body.encode()) & 0xFFFFFFFF}"


class _RunBarrier:
    """Reproduces run 1899055119's TAIL: each camera's gen_ts encodes its REAL present-age delta
    (cam3 the oldest/slowest -> d=0; every other camera newer by its RUN_DELTAS ms), and frame_id is
    set INDEPENDENTLY to a constant spread of 2 -- exactly the run's "two quantities disagree" shape
    (frame_id spread beyond the 1-id parity bar, present-age spread within one source frame). Returns
    a CONSTANT shot each round so the tail stabilizes off-parity, the state that reaches the plan."""

    BASE_NS = 10_000_000_000     # 10 s -- gen_ts baseline, comfortably above every delta
    FID_BASE = 1_200_000

    def __call__(self, sources, host, password, width, height):
        shot = {}
        for s in sources:
            d_ms = RUN_DELTAS[s]
            gen_ts = int(self.BASE_NS + d_ms * 1e6)       # larger gen_ts (newer) -> larger d_i
            fid = self.FID_BASE + (0 if s == "NDI cam3" else 2)   # constant 2-id spread
            shot[s] = ([_payload(RUN, fid, gen_ts),
                        _payload(RUN, fid - 1, gen_ts - ID_NS)], 0)
        return shot


def _align_run(monkeypatch, *, execute):
    import apply_latency_pins
    import obs_phase2
    current = {s: 3 for s in SRC}                          # all at the floor (post two-phase reset)
    applied = {}
    monkeypatch.setattr(qa, "barrier_screenshot", _RunBarrier())

    def _read_pins(s, h, p):
        return dict(applied) if applied else dict(current)
    monkeypatch.setattr(qa, "read_current_pins", _read_pins)

    def _apply(ws, plan, ex):
        applied.update(plan)
        return plan
    monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

    class _WS:
        def close(self):
            pass
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
    return qa.align(SRC, "h", "pw", execute=execute, stable_tail_rounds=3, stable_tol_ids=1,
                    min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                    floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                    max_measure_rounds=60, settle_s=0, jitter_json=RUN_JITTER)


class TestRunDeltasAreOneSourceFrame:
    def test_present_age_spread_is_one_source_frame(self):
        # the whole cross-camera spread is within ONE source frame -- the N=2 lock-phase quantum.
        assert max(RUN_DELTAS.values()) > SOURCE_FRAME_MS - 1.0
        assert max(RUN_DELTAS.values()) < 2.0 * SOURCE_FRAME_MS - 1.0


class TestWithinAlignedQuantum:
    def test_the_run_is_classified_already_aligned(self):
        # RED: within_aligned_quantum does not exist yet -> the run's sub-frame quantum is not
        # recognized, so the above-floor plan fires. GREEN: the run is already aligned at floor-3.
        assert qa.within_aligned_quantum(RUN_DELTAS) is True

    def test_a_real_two_source_frame_spread_is_not_within_quantum(self):
        # a genuine >= 2-source-frame misalignment must STILL be planned, never quantum-suppressed.
        deltas = {"NDI cam1": 0.0, "NDI cam3": 2.0 * SOURCE_FRAME_MS + 1.0}
        assert qa.within_aligned_quantum(deltas) is False

    def test_empty_deltas_are_not_aligned(self):
        assert qa.within_aligned_quantum({}) is False


class TestFloorAwarePlanDoublesTheQuantum:
    def test_current_plan_raises_the_runs_phantom_above_floor_pins(self):
        # documents the DEFECT on the real inputs: floor_aware_pins reproduces the run's byte-identical
        # +83 ms above-floor plan (post_residual = pre + pin_delta -> the spread doubles). Stays true
        # after the fix (floor_aware_pins is unchanged); the fix stops it being REACHED for a quantum.
        floors = qa.arrival_floors_from_jitter(RUN_JITTER, SRC)
        plan = qa.floor_aware_pins(floors, RUN_DELTAS)
        assert plan["NDI cam1"] == 86 and plan["NDI cam4"] == 92
        assert plan["NDI cam5"] == 87 and plan["NDI cam6"] == 87 and plan["NDI cam7"] == 87
        assert plan["NDI cam3"] == 3


class TestAlignReproducesTheRun:
    def test_run_is_recognized_already_aligned_no_pins_dry_run(self, monkeypatch):
        # RED: current code takes the floor-aware path and returns status "plan-only" with the +83 ms
        # phantom plan. GREEN: the quantum gate fires -> already-aligned, empty plan, PASS.
        result = _align_run(monkeypatch, execute=False)
        assert result["status"] == "already-aligned-quantum"
        assert result["plan"] == {}

    def test_run_applies_no_pins_on_execute(self, monkeypatch):
        # RED: on --execute the current code applies the +83 ms pins and the verify FAILs
        # (AlignmentImpossible). GREEN: the gate short-circuits before apply -> nothing is pinned.
        result = _align_run(monkeypatch, execute=True)
        assert result["status"] == "already-aligned-quantum"
        assert result["plan"] == {}
