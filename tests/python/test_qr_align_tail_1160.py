"""#1160 -- the [4i/8align] aligner must measure to a STABLE TAIL and judge the steady state, never
the post-restart convergence transient.

The rig backlog (issue 1145) drains at ~0.3 frame/s, so a fresh restart / receiver reconnect / burn
toggle during the earlier E2E steps leaves cam1 MINUTES over the align bound while it catches up.
The old aligner measured a FIXED 9-round window and medianed the WHOLE window: a run whose per-round
cross-camera spread decayed 10,10,11,12,9,9,9,7,2 ids -> median spread 9 (not aligned) + worst delta
75 ms > the 66 ms sanity bound -> abort, though steady state was <=2 ids seconds later (live run
32429396384, 02:27).

The fix: keep measuring (budget-bounded ~90 s) until the last K rounds are MUTUALLY stable (their
cross-camera spreads within `stable_tol` of each other -- the pairwise "mutually stable" form, which
subsumes round-to-round <=1 AND rejects a slow monotonic ramp), then compute the verdict from that
STABLE TAIL only. No threshold is weakened -- the 66 ms sanity bound, the <=1-id parity gate and the
min-valid/parity-rounds minimums are UNCHANGED, applied to the tail. A rig that never stabilizes
within the bound FAILS with the full per-round table printed (a degraded/over-rate grabber).

Tier-0: the stability decision (`_stable_tail_start`, `measure_tail_status`) is PURE and unit-tested
with no rig; the `measure_stable_tail` loop and `align()` flow are exercised against a monkeypatched
`barrier_screenshot` (the #1159 `ticks_from_raw` seam), no rig, no cargo.
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
ID_NS = 8_333_333            # per-frame_id gen_ts step (fixtures only; the code reads gen_ts)
SRC = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]


# --------------------------------------------------------------------------- #
# fixtures: build round-ticks (and raw decoded texts) where cam1 lags the aligned
# siblings by `spread` frame_ids -> the round's cross-camera frame_id spread == spread.
# --------------------------------------------------------------------------- #
def _payload(run_id, frame_id, gen_ts_ns):
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    return f"P{body}.{zlib.crc32(body.encode()) & 0xFFFFFFFF}"


def _round(spread, base=20000, *, drop=None):
    """{src: (frame_id, gen_ts_ns, t_send_ns)|None}. cam1 lags by `spread` ids (oldest gen_ts);
    cam2/3/4 aligned at `base`. `drop` (a source name) makes that camera undecoded (partial round)."""
    fids = {"NDI cam1": base - spread, "NDI cam2": base, "NDI cam3": base, "NDI cam4": base}
    rnd = {}
    for s in SRC:
        if s == drop:
            rnd[s] = None
        else:
            f = fids[s]
            rnd[s] = (f, f * ID_NS, 0)
    return rnd


def _rounds(spreads):
    return [_round(sp, base=20000 + i * 20) for i, sp in enumerate(spreads)]


def _raw_round(spread, base):
    """One barrier round as decoded texts: {src: ([painter texts], t_send_ns)}, cam1 lagging."""
    fids = {"NDI cam1": base - spread, "NDI cam2": base, "NDI cam3": base, "NDI cam4": base}
    shot = {}
    for s in SRC:
        f = fids[s]
        shot[s] = ([_payload(RUN, f, f * ID_NS), _payload(RUN, f - 1, (f - 1) * ID_NS)], 0)
    return shot


class _ScriptedBarrier:
    """A monkeypatch stand-in for qa.barrier_screenshot: returns one scripted round per call from a
    spread sequence, REPEATING the last spread after the list is exhausted (models a rig that stays
    at its final steady state). Shared across the initial + verify measure phases of one align()."""

    def __init__(self, spreads):
        self.spreads = list(spreads)
        self.i = 0

    def __call__(self, sources, host, password, width, height):
        sp = self.spreads[self.i] if self.i < len(self.spreads) else self.spreads[-1]
        self.i += 1
        return _raw_round(sp, base=30000 + self.i * 20)


# --------------------------------------------------------------------------- #
# _stable_tail_start -- the maximal in-tol FULL-round suffix ending at the last round
# --------------------------------------------------------------------------- #
class TestStableTailStart:
    def test_decay_then_stable_returns_the_converged_suffix(self):
        # 10,10,11,12,9,9,9,7,2,2,1,1,1: from the end, 2,2,1,1,1 all within tol=1 (7 breaks it).
        rounds = _rounds([10, 10, 11, 12, 9, 9, 9, 7, 2, 2, 1, 1, 1])
        start = qa._stable_tail_start(rounds, SRC, 3, 1)
        assert start == 8, f"stable suffix should start at index 8 (spreads 2,2,1,1,1), got {start}"

    def test_none_when_last_k_not_stable(self):
        # last 3 = 2,7,3 -> max-min 5 > tol -> no stable tail ending at the last round.
        assert qa._stable_tail_start(_rounds([1, 1, 2, 7, 3]), SRC, 3, 1) is None

    def test_immediately_stable_whole_window(self):
        assert qa._stable_tail_start(_rounds([1, 0, 1]), SRC, 3, 1) == 0

    def test_monotonic_ramp_is_rejected_by_max_min(self):
        # 1,2,3: round-to-round delta is each <=1, but max-min over the window is 2 -> NOT stable.
        assert qa._stable_tail_start(_rounds([1, 2, 3]), SRC, 3, 1) is None

    def test_a_partial_round_at_the_end_breaks_the_suffix(self):
        rounds = _rounds([1, 1, 1])
        rounds[-1] = _round(1, drop="NDI cam3")  # last round undecoded on one camera
        assert qa._stable_tail_start(rounds, SRC, 3, 1) is None

    def test_a_partial_round_mid_window_bounds_the_suffix(self):
        # partial at idx1 -> the contiguous full-round suffix is idx2..4 (len 3, stable).
        rounds = _rounds([9, 9, 1, 0, 1])
        rounds[1] = _round(9, drop="NDI cam2")
        assert qa._stable_tail_start(rounds, SRC, 3, 1) == 2


# --------------------------------------------------------------------------- #
# measure_tail_status -- converged-aligned / converged-stable / stable-need-more / unstable
# --------------------------------------------------------------------------- #
def _status(rounds, *, k=3, tol=1, parity=1, min_parity=3, min_valid=5):
    return qa.measure_tail_status(
        rounds, SRC, stable_tail_rounds=k, stable_tol_ids=tol, parity_tol_ids=parity,
        min_parity_rounds=min_parity, min_valid_rounds=min_valid)


class TestMeasureTailStatus:
    def test_converged_aligned_stops_at_k_without_min_valid(self):
        # tail 1,0,1 is stable AND aligned (median 1 <= parity 1) -> done on just K=3 rounds.
        st = _status(_rounds([10, 9, 1, 0, 1]))
        assert st.done is True and st.reason == "converged-aligned"
        assert st.tail_start == 2

    def test_converged_stable_needs_min_valid_rounds(self):
        # a stable-but-not-aligned tail (spread 2) with >= min_valid rounds -> re-derive path.
        st = _status(_rounds([10, 2, 2, 2, 2, 2]))
        assert st.done is True and st.reason == "converged-stable"

    def test_stable_but_too_few_rounds_is_not_done(self):
        # stable at spread 2 (not aligned) but only 3 tail rounds < min_valid 5 -> keep measuring.
        st = _status(_rounds([10, 9, 2, 2, 2]))
        assert st.done is False and st.reason == "stable-need-more"

    def test_unstable_window_is_not_done(self):
        st = _status(_rounds([10, 3, 11, 4, 12]))
        assert st.done is False and st.reason == "unstable"


# --------------------------------------------------------------------------- #
# measure_stable_tail -- the budget/max-rounds-bounded loop over barrier_screenshot
# --------------------------------------------------------------------------- #
def _measure(monkeypatch, spreads, *, run_id=None, max_rounds=40, budget_s=1e9):
    monkeypatch.setattr(qa, "barrier_screenshot", _ScriptedBarrier(spreads))
    return qa.measure_stable_tail(
        SRC, "h", "pw", width=1920, height=1080, run_id=run_id,
        stable_tail_rounds=3, stable_tol_ids=1, parity_tol_ids=1, min_parity_rounds=3,
        min_valid_rounds=5, budget_s=budget_s, max_rounds=max_rounds, inter_round_s=0)


class TestMeasureStableTail:
    def test_stops_when_converged_aligned(self, monkeypatch):
        rounds, run_id, st = _measure(monkeypatch, [10, 10, 8, 5, 3, 2, 1, 1, 1])
        assert run_id == RUN
        assert st.done is True and st.reason == "converged-aligned"
        tail = rounds[st.tail_start:]
        assert all(qa.frame_id_spread(r) <= 1 for r in tail)

    def test_re_derive_path_accumulates_min_valid_rounds(self, monkeypatch):
        # converges to a stable-but-not-aligned spread 2 -> must gather >= min_valid tail rounds.
        rounds, _run, st = _measure(monkeypatch, [10, 8, 5, 3, 2, 2, 2, 2, 2, 2, 2])
        assert st.done is True and st.reason == "converged-stable"
        assert len(rounds[st.tail_start:]) >= 5

    def test_budget_bound_stops_a_never_stable_rig(self, monkeypatch):
        rounds, _run, st = _measure(monkeypatch, [10, 3] * 20, max_rounds=8)
        assert len(rounds) == 8            # bounded, never ran away
        assert st.done is False            # a bouncing spread never stabilizes


# --------------------------------------------------------------------------- #
# align() -- the whole flow judges the STABLE TAIL (the behavioral #1160 fix)
# --------------------------------------------------------------------------- #
def _align(monkeypatch, spreads, *, execute=False, pins=None):
    monkeypatch.setattr(qa, "barrier_screenshot", _ScriptedBarrier(spreads))
    monkeypatch.setattr(qa, "read_current_pins", lambda s, h, p: dict(pins or {x: 3 for x in SRC}))
    return qa.align(
        SRC, "h", "pw", execute=execute, stable_tail_rounds=3, stable_tol_ids=1,
        min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
        floor_ms=3, width=1920, height=1080, measure_budget_s=1e9, max_measure_rounds=40,
        settle_s=0)


class TestAlignJudgesTheStableTail:
    def test_converging_then_aligned_rig_passes(self, monkeypatch):
        # #1160 RED->GREEN: the decay window would abort under the old whole-window verdict; the
        # stable-tail measure keeps going to convergence and PASSES.
        res = _align(monkeypatch, [10, 10, 11, 12, 9, 7, 3, 1, 0, 1])
        assert res["status"] == "already-aligned"
        assert res["stable"] is True and res["measure_reason"] == "converged-aligned"
        assert res["pre_spread_ids"] <= 1

    def test_never_stabilizing_rig_fails_with_the_table(self, monkeypatch):
        monkeypatch.setattr(qa, "barrier_screenshot", _ScriptedBarrier([10, 3] * 30))
        monkeypatch.setattr(qa, "read_current_pins", lambda s, h, p: {x: 3 for x in SRC})
        with pytest.raises(qa.AlignmentImpossible) as exc:
            qa.align(SRC, "h", "pw", execute=False, stable_tail_rounds=3, stable_tol_ids=1,
                     min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                     floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                     max_measure_rounds=8, settle_s=0)
        assert "did not STABILIZE" in str(exc.value) or "stabilize" in str(exc.value).lower()

    def test_immediately_stable_rig_is_the_fast_path(self, monkeypatch):
        res = _align(monkeypatch, [0, 1, 0, 1, 0])
        assert res["status"] == "already-aligned"
        assert res["measure_rounds_total"] <= 4        # stopped fast, no long wait

    def test_stable_but_not_aligned_tail_re_derives_a_floor3_plan(self, monkeypatch):
        # converges to a static residual spread 2 -> re-derive floor-3 pins FROM THE TAIL (plan-only).
        res = _align(monkeypatch, [10, 8, 5, 3, 2, 2, 2, 2, 2, 2, 2])
        assert res["status"] == "plan-only"
        assert res["plan"]["NDI cam1"] == 3            # slowest camera floors to 3
        assert all(3 <= v <= 66 for v in res["plan"].values())  # shallow, never a deep pin

    def test_execute_applies_then_verifies_a_stable_aligned_tail(self, monkeypatch):
        import apply_latency_pins
        import obs_phase2
        # phase1 converges to stable-not-aligned spread 2 (re-derive); phase2 (verify) converges aligned.
        monkeypatch.setattr(qa, "barrier_screenshot",
                            _ScriptedBarrier([10, 8, 5, 3, 2, 2, 2, 2, 1, 0, 1, 0, 1]))
        monkeypatch.setattr(qa, "read_current_pins", lambda s, h, p: {x: 3 for x in SRC})
        monkeypatch.setattr(apply_latency_pins, "apply_pins", lambda ws, plan, execute: plan)

        class _WS:
            def close(self):
                pass
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
        res = qa.align(SRC, "h", "pw", execute=True, stable_tail_rounds=3, stable_tol_ids=1,
                       min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                       floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                       max_measure_rounds=40, settle_s=0)
        assert res["status"] == "aligned"
        assert res["post_spread_ids"] <= 1


# --------------------------------------------------------------------------- #
# the per-round table marks which rounds were USED for the verdict (the stable tail)
# --------------------------------------------------------------------------- #
class TestTableMarksTheTail:
    def test_table_marks_tail_rounds_when_tail_start_given(self):
        rounds = _rounds([10, 9, 1, 0, 1])
        table = qa.format_round_table(rounds, SRC, tail_start=2)
        assert "used" in table            # the new column header
        assert "tail" in table            # the used-round marker
        # the transient rounds (0,1) are shown but not marked tail; the tail (2..4) is marked.
        lines = [ln for ln in table.splitlines() if ln.strip() and ln.split("|")[0].strip().isdigit()]
        assert lines[0].rstrip().endswith("|") is False  # row 0 has an (empty) used cell, not "tail"
        assert lines[2].rstrip().endswith("tail")
        assert lines[4].rstrip().endswith("tail")

    def test_table_without_tail_start_is_unchanged(self):
        # existing 2-arg callers get the old format (no used column).
        table = qa.format_round_table(_rounds([1, 2]), SRC)
        assert "used" not in table
