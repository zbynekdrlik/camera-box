"""#1161 -- a floor-3 pin INCREASE does not move the presented frame, so a one-canvas-frame residual
survives apply. Root cause (traced in vendor/obs-studio/libobs/obs-source.c + src/genlock_backlog.rs):
obs_source_set_genlock_latency_ms clears the phase anchor but never the locked conveyor boundary and
never forces a re-acquire, and should_converge_phase only sheds DOWNWARD toward max(reserve, floor) --
so raising a source's genlock_latency_ms (the floor-3 plan's lever for delaying a faster camera into
parity) moves only the CONFIG value, never the presented frame. The frame-mover is issue 1003's
Stage-2 ACQUIRE bracketing gate (a genlock-C change, live-only); this aligner cannot add hold.

This ticket does NOT make the frame move (out of the aligner's reach). The smallest honest fix is to
(a) classify the "plan asks the FIFO to add hold" case precisely, (b) attribute the persistent
re-measure residual to that structural limit instead of a generic "did NOT hold" that reads as
flakiness/settle, and (c) emit before/after telemetry -- WITHOUT widening the owner's same-frame
parity bar. Tier-0: pure classifier/formatter unit tests + an align() flow test against the
monkeypatched barrier/apply seams (no rig, no cargo).
"""
import pathlib
import re
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


def _payload(run_id, frame_id, gen_ts_ns):
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    return f"P{body}.{zlib.crc32(body.encode()) & 0xFFFFFFFF}"


def _raw_round(spread, base):
    """One barrier round as decoded texts: {src: ([painter texts], t_send_ns)}, cam1 (the slowest)
    lagging the aligned siblings by `spread` ids -> the round's cross-camera frame_id spread ==
    spread. The dual-QR carries two ids per shot, exactly like the live painter."""
    fids = {"NDI cam1": base - spread, "NDI cam2": base, "NDI cam3": base, "NDI cam4": base}
    shot = {}
    for s in SRC:
        f = fids[s]
        shot[s] = ([_payload(RUN, f, f * ID_NS), _payload(RUN, f - 1, (f - 1) * ID_NS)], 0)
    return shot


class _ScriptedBarrier:
    """monkeypatch stand-in for qa.barrier_screenshot: one scripted round per call from a spread
    sequence, REPEATING the last spread after the list is exhausted (a rig stuck at its final steady
    state -- exactly #1161: the pin change never moves the frame, so the verify phase keeps showing
    the same spread). Shared across the initial + verify measure phases of one align()."""

    def __init__(self, spreads):
        self.spreads = list(spreads)
        self.i = 0

    def __call__(self, sources, host, password, width, height):
        sp = self.spreads[self.i] if self.i < len(self.spreads) else self.spreads[-1]
        self.i += 1
        return _raw_round(sp, base=30000 + self.i * 20)


# --------------------------------------------------------------------------- #
# pins_requiring_more_hold -- the pure classifier: which sources does the plan ask to hold LONGER?
# --------------------------------------------------------------------------- #
class TestPinsRequiringMoreHold:
    def test_flags_only_the_increases(self):
        # the #1161 live plan: cam1/cam2 unchanged, cam3 +33, cam4 +18 (existing pin + measured delta)
        pre = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20, "NDI cam4": 20}
        plan = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 53, "NDI cam4": 38}
        assert qa.pins_requiring_more_hold(pre, plan) == {"NDI cam3": 33, "NDI cam4": 18}

    def test_decreases_and_flat_are_not_flagged(self):
        pre = {"NDI cam1": 20, "NDI cam2": 6, "NDI cam3": 6}
        plan = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 3}   # decrease, flat, decrease
        assert qa.pins_requiring_more_hold(pre, plan) == {}

    def test_unknown_pre_pin_is_skipped_never_fabricated(self):
        pre = {"NDI cam1": 3}                                   # cam2 pre-pin unknown
        plan = {"NDI cam1": 20, "NDI cam2": 40}
        assert qa.pins_requiring_more_hold(pre, plan) == {"NDI cam1": 17}

    def test_min_increase_threshold_is_respected(self):
        pre = {"NDI cam1": 3, "NDI cam2": 3}
        plan = {"NDI cam1": 4, "NDI cam2": 20}
        assert qa.pins_requiring_more_hold(pre, plan, min_increase_ms=5) == {"NDI cam2": 17}


# --------------------------------------------------------------------------- #
# hold_inert_abort_reason / format_pin_apply_report -- precise, actionable diagnosis
# --------------------------------------------------------------------------- #
class TestAbortReason:
    def test_names_source_increase_readback_and_the_owning_fix(self):
        msg = qa.hold_inert_abort_reason(
            {"NDI cam3": 33}, {"NDI cam3": 53}, {"NDI cam3": 83.5})
        assert "NDI cam3" in msg
        assert "+33 ms hold" in msg
        assert "pin now 53 ms" in msg and "read-back confirmed" in msg
        assert "83.5 ms" in msg
        # attributes it to the genlock FIFO structural limit + the owning fix, NOT a generic "did NOT hold"
        assert "genlock FIFO did NOT add the requested hold" in msg
        assert "issue 1003" in msg
        # never widens the parity bar
        assert "NOT widened" in msg and "same-frame" in msg

    def test_report_shows_config_moved_but_residual_did_not_close(self):
        rpt = qa.format_pin_apply_report(
            {"NDI cam3": 20}, {"NDI cam3": 53},
            {"NDI cam3": 33.0}, {"NDI cam3": 83.5},
            {"NDI cam3": 33})
        assert "pin 20ms -> 53ms (read-back)" in rpt        # config DID move
        assert "residual 33 -> 83.5 ms" in rpt              # presented frame did NOT
        assert "HOLD-INERT" in rpt

    def test_report_tolerates_an_unverifiable_residual_map(self):
        # a "unverifiable" (non-dict) post-deltas must not crash the telemetry
        rpt = qa.format_pin_apply_report(
            {"NDI cam3": 20}, {"NDI cam3": 53}, {}, "unverifiable", {"NDI cam3": 33})
        assert "residual n/a -> n/a ms" in rpt


# --------------------------------------------------------------------------- #
# align() -- the whole flow: a plan that RAISES pins whose frame never moves aborts with the
# PRECISE genlock-FIFO attribution (not the generic "did NOT hold"), and never widens tolerance.
# --------------------------------------------------------------------------- #
def _align_stuck_after_apply(monkeypatch):
    """Phase1 converges stable-but-not-aligned at spread 2 (the aligner re-derives a floor-3 plan
    that RAISES cam2/3/4's pins); the pin change never moves the frame, so phase2 (verify) STAYS at
    spread 2. This models the LIVE #1161 dichotomy end-to-end: the config pin MOVES (the post-apply
    read-back echoes the applied plan) but the presented frame does NOT (verify still spread 2). So
    the read_current_pins stub returns floor-3 on the PRE-apply read and the applied plan on every
    later read-back, and apply_pins echoes + records the plan (the real writer is read-back-verified,
    so a live post-apply read returns exactly what was written)."""
    import apply_latency_pins
    import obs_phase2
    monkeypatch.setattr(qa, "barrier_screenshot", _ScriptedBarrier([10, 8, 5, 3, 2] + [2] * 20))

    applied = {}                       # captures the plan apply_pins wrote (the live rig's read-back)

    def _read_pins(s, h, p):
        # 1st read = PRE-apply (all at floor 3); once a plan is applied, the read-back reflects it.
        return dict(applied) if applied else {x: 3 for x in SRC}
    monkeypatch.setattr(qa, "read_current_pins", _read_pins)

    def _apply(ws, plan, execute):
        applied.update(plan)
        return plan
    monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

    class _WS:
        def close(self):
            pass
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
    return qa.align(SRC, "h", "pw", execute=True, stable_tail_rounds=3, stable_tol_ids=1,
                    min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                    floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                    max_measure_rounds=60, settle_s=0)


class TestAlignAttributesTheResidual:
    def test_persistent_residual_after_a_hold_increase_names_the_fifo_limit(self, monkeypatch):
        with pytest.raises(qa.AlignmentImpossible) as exc:
            _align_stuck_after_apply(monkeypatch)
        msg = str(exc.value)
        # the PRECISE #1161 attribution, not the pre-fix generic "alignment did NOT hold"
        assert "genlock FIFO did NOT add the requested hold" in msg
        assert "issue 1003" in msg
        assert "did NOT hold. Per-camera residual deltas" not in msg
        # never widens the owner's same-frame parity bar
        assert "NOT widened" in msg
        # the config DID move: the post-apply read-back reflects the RAISED pin, not the floor -- the
        # "pin now N ms, read-back confirmed" clause names a value above the floor, proving the
        # dichotomy (config moved, frame did not) is exercised end-to-end, not just in the unit tests.
        pins_now = [int(m) for m in re.findall(r"pin now (\d+) ms", msg)]
        assert pins_now and max(pins_now) > 3
