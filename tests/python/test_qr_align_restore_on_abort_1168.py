"""#1168 -- an ABORTED [4i/8align] must RESTORE the strih pins it wrote.

Root cause: qr_align_pins.align() reads current_pins at its start, then in the --execute path applies
the plan and (on a re-measure that stays off-parity) raises AlignmentImpossible WITHOUT restoring the
pre-align pins -- so an aborted run leaves the rig on the partial plan pins (run 34973535496: cam1 20 /
cam4 19 / cam5-7 36/36/37 left DIRTY, restored by hand). cleanup()'s `teardown --host STRIH` restores
only the stream-hold / measurement-eq snapshots (obs_phase2.py), never the aligner's own pins.

These tests exercise the full align() against a FAKE ws/apply/measure stub (no rig, no cargo, Tier-0)
and assert that after a post-apply abort the LAST apply_pins call restored the plan's sources to the
pre-align current_pins -- and that a SUCCESS path does NOT restore.
"""
import pathlib
import sys
import zlib

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

SRC = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]
RUN = 4242
ID_NS = 8_333_333


def _payload(run_id: int, frame_id: int, gen_ts_ns: int) -> str:
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    crc = zlib.crc32(body.encode()) & 0xFFFFFFFF
    return f"P{body}.{crc}"


def _raw_round(spread, base=30000):
    """One barrier round: cam1 lags the aligned siblings by `spread` ids -> cross-camera spread."""
    fids = {"NDI cam1": base - spread, "NDI cam2": base, "NDI cam3": base, "NDI cam4": base}
    shot = {}
    for s in SRC:
        f = fids[s]
        shot[s] = ([_payload(RUN, f, f * ID_NS), _payload(RUN, f - 1, (f - 1) * ID_NS)], 0)
    return shot


class _StuckBarrier:
    """Scripted spreads, REPEATING the last (a rig stuck at its final steady state -- the pin change
    never moves the frame, so the verify phase keeps showing the same spread -> post-apply abort)."""

    def __init__(self, spreads):
        self.spreads = list(spreads)
        self.i = 0

    def __call__(self, sources, host, password, width, height):
        sp = self.spreads[self.i] if self.i < len(self.spreads) else self.spreads[-1]
        self.i += 1
        return _raw_round(sp, base=30000 + self.i * 20)


class _HealAfterApply:
    """Off-parity (spread 4) UNTIL the plan is applied, then aligned (spread 0): a SUCCESS path that
    exercises a real apply so the "no restore on success" guard is meaningful."""

    def __init__(self, applied):
        self.applied = applied
        self.i = 0

    def __call__(self, sources, host, password, width, height):
        self.i += 1
        sp = 0 if self.applied else 4
        return _raw_round(sp, base=30000 + self.i * 20)


def _wire(monkeypatch, barrier, pre_pins):
    """Wire the fake WS layer. Returns (apply_calls, applied): apply_calls records EVERY apply_pins
    plan (so a restore call is visible); applied is the live read-back state."""
    import apply_latency_pins
    import obs_phase2
    monkeypatch.setattr(qa, "barrier_screenshot", barrier)
    applied = {}
    apply_calls = []

    def _read_pins(s, h, p):
        return dict(applied) if applied else dict(pre_pins)
    monkeypatch.setattr(qa, "read_current_pins", _read_pins)

    def _apply(ws, plan, execute):
        apply_calls.append(dict(plan))
        applied.update(plan)
        return dict(plan)
    monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

    class _WS:
        def close(self):
            pass
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
    return apply_calls, applied


def _align(pre_pins):
    return qa.align(SRC, "h", "pw", execute=True, stable_tail_rounds=3, stable_tol_ids=1,
                    min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                    floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                    max_measure_rounds=60, settle_s=0)


class TestRestoreOnAbort:
    def test_stuck_abort_restores_the_preapply_floor_pins(self, monkeypatch):
        # Two-phase reset leaves the align set at the floor before align() runs -> pre = floor 3.
        pre = {s: 3 for s in SRC}
        apply_calls, applied = _wire(monkeypatch, _StuckBarrier([10, 8, 6, 4] + [4] * 20), pre)
        with pytest.raises(qa.AlignmentImpossible):
            _align(pre)
        # The plan RAISED some pins (the aligner's write); after the abort the LAST apply_pins call
        # must have restored EXACTLY those sources back to their pre-align values.
        assert len(apply_calls) >= 2, "expected a restore apply after the abort, got only the plan"
        plan = apply_calls[0]
        assert apply_calls[-1] == {s: pre[s] for s in plan}
        # and the live read-back ends on the restored (floor) pins, never the partial plan
        assert applied == {s: pre[s] for s in plan}

    def test_successful_align_does_not_restore(self, monkeypatch):
        pre = {s: 3 for s in SRC}
        applied_ref = {}

        class _Heal(_HealAfterApply):
            pass
        barrier = _Heal(applied_ref)
        apply_calls, applied = _wire(monkeypatch, barrier, pre)
        # share the same applied dict the barrier watches
        barrier.applied = applied
        result = _align(pre)
        assert result["status"] == "aligned"
        assert len(apply_calls) == 1, "a successful align must apply the plan ONCE and never restore"


class TestRestorePinsHelper:
    def test_restore_pins_coerces_a_none_preapply_pin_to_the_floor(self, monkeypatch):
        import apply_latency_pins
        import obs_phase2
        seen = {}

        def _apply(ws, plan, execute):
            seen.update(plan)
            return dict(plan)
        monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

        class _WS:
            def close(self):
                pass
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
        qa.restore_pins({"NDI cam1": 20, "NDI cam2": None}, "h", "pw", floor_ms=3)
        # cam1 restores to its recorded 20; cam2 (never-read -> None) restores to the floor.
        assert seen == {"NDI cam1": 20, "NDI cam2": 3}
