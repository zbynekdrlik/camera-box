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
    """Phase1 converges stable-but-not-aligned at spread 4 (the aligner re-derives a floor-3 plan
    that RAISES cam2/3/4's pins); the pin change never moves the frame, so phase2 (verify) STAYS at
    spread 4. This models the LIVE #1161 dichotomy end-to-end: the config pin MOVES (the post-apply
    read-back echoes the applied plan) but the presented frame does NOT (verify still spread 4). So
    the read_current_pins stub returns floor-3 on the PRE-apply read and the applied plan on every
    later read-back, and apply_pins echoes + records the plan (the real writer is read-back-verified,
    so a live post-apply read returns exactly what was written).

    #1252: the stable spread is 4 ids (~33 ms = 2 source frames), a CLEARLY-real misalignment, so the
    plan runs and the hold-inert path is exercised. A 2-id (~16.7 ms = one source frame) spread is
    now the already-aligned lock-phase quantum (within_aligned_quantum) and never reaches the plan."""
    import apply_latency_pins
    import obs_phase2
    monkeypatch.setattr(qa, "barrier_screenshot", _ScriptedBarrier([10, 8, 6, 4] + [4] * 20))

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


# --------------------------------------------------------------------------- #
# #1161 ALIGNER-SIDE FIX: compute pins ABOVE each source's arrival transport floor.
#
# The bug: floor3_pins computes `floor(3) + relative_delta`, which in the transport-dominated regime
# (frames arrive ~59-66 ms old, deltas ~1 canvas frame) lands the raised pins BELOW each source's
# arrival floor -> structurally inert (latency = max(pin, transport)). The fix targets an ABSOLUTE
# achievable latency = arrival_floor_i + delta_i (the slowest camera's floor) so the genlock-C
# ACQUIRE frame-mover (sibling branch) can actually add the hold, while the slowest keeps pin 3.
# The arrival floor is the strih genlock audit's `latency_ms + mean_head_skew_ms` (pin-clock,
# DanteSync-synced) -- painter-QR gen_ts is CLOCK_REALTIME vs dev1 CLOCK_MONOTONIC t_send, so the
# painter-QR gives only RELATIVE deltas, never an absolute floor.
# --------------------------------------------------------------------------- #
def _jitter(floors, pins):
    """A genlock-jitter-report --json dict for the given per-source arrival floors + current pins:
    latency_ms = the effective pin during the sampled window, mean_head_skew_ms = the signed mean
    deviation so latency_ms + mean_head_skew_ms == the actual present age (== the arrival floor when
    the pin sits below it). Keyed by strih source name, exactly as the real report."""
    return {s: {"samples": 40, "latency_ms": pins[s],
                "mean_head_skew_ms": float(floors[s] - pins[s]),
                "mean_abs_head_skew_ms": 2.0, "max_abs_head_skew_ms": 4,
                "delta_backward_regime_ticks": 0}
            for s in floors}


class TestArrivalFloorsFromJitter:
    def test_reconstructs_latency_plus_signed_skew_per_source(self):
        floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
        pins = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 17, "NDI cam4": 22}
        got = qa.arrival_floors_from_jitter(_jitter(floors, pins), SRC)
        assert got == pytest.approx(floors)

    def test_omits_a_source_absent_or_malformed_in_the_jitter_json(self):
        j = {"NDI cam1": {"latency_ms": 17, "mean_head_skew_ms": 49.0},   # -> 66
             "NDI cam2": {"latency_ms": 6},                                # malformed (no skew)
             "NDI foreign": {"latency_ms": 3, "mean_head_skew_ms": 1.0}}   # not a "NDI cam<N>"
        got = qa.arrival_floors_from_jitter(j, SRC)
        assert got == {"NDI cam1": pytest.approx(66.0)}   # cam2 skipped, foreign not requested


class TestFloorAwarePins:
    # The live #1161 case: cam1 slowest (arrival floor 66); the faster ones are ahead by their delta.
    FLOORS = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
    DELTAS = {"NDI cam1": 0.0, "NDI cam2": 3.0, "NDI cam3": 33.0, "NDI cam4": 20.0}  # hold to add

    def test_faster_cameras_get_the_current_pin_plus_their_present_age_gap(self):
        # issue 1253 ADDITIVE: new_pin_i = current_pin_i + delta_i (current pin defaults to the floor
        # when unread -- the execute path resets first), NOT the old absolute arrival_floor + delta.
        plan = qa.floor_aware_pins(self.FLOORS, self.DELTAS)
        assert plan["NDI cam2"] == 6 and plan["NDI cam3"] == 36 and plan["NDI cam4"] == 23
        # never the old max-model absolute target (which pinned every faster camera to the slowest's 66)
        for s in ("NDI cam2", "NDI cam3", "NDI cam4"):
            assert plan[s] != 66

    def test_slowest_camera_keeps_the_minimum_floor_pin(self):
        plan = qa.floor_aware_pins(self.FLOORS, self.DELTAS)
        assert plan["NDI cam1"] == 3   # slowest (delta 0) -> floor, inert, stays at its natural 66

    def test_additive_plan_equals_floor3_on_the_reset_path(self):
        # issue 1253: under the ADDITIVE FIFO the floor-aware plan (current_pin + delta, current at the
        # floor after the two-phase reset) is IDENTICALLY floor3_pins (floor + delta). The pre-1253
        # "floor3 lands below the arrival floor -> inert" claim was the max-model BUG this fix removes:
        # under the additive FIFO every pin adds hold, so floor3 aligns.
        old = qa.floor3_pins(self.DELTAS)
        fix = qa.floor_aware_pins(self.FLOORS, self.DELTAS)   # no current_pins -> defaults to the floor
        assert fix == old

    def test_fails_loud_when_floor_plus_delta_exceeds_the_abs_ceiling(self):
        # a transport floor so high that aligning would need a pin beyond the absolute budget ->
        # FAIL LOUD naming the camera, NEVER silently pin above the bound / widen it.
        floors = {"NDI cam1": 66.0, "NDI cam3": 66.0}
        deltas = {"NDI cam1": 0.0, "NDI cam3": 33.0}      # cam3 target 66+33 = 99 > 94
        with pytest.raises(qa.AlignmentImpossible) as exc:
            qa.floor_aware_pins(floors, deltas)
        msg = str(exc.value)
        assert "NDI cam3" in msg and "99" in msg and "94" in msg
        assert "do NOT raise the bound" in msg or "do not raise the bound" in msg

    def test_ceiling_is_the_owner_94ms_line_and_below_it_aligns(self):
        assert qa.DEFAULT_MAX_ABS_LATENCY_MS == 94
        # resulting present age 27 + 33 = 60 <= 94 -> within budget, aligns (does NOT fail). issue 1253
        # additive: the PIN is current(floor 3) + delta 33 = 36 (not the absolute present-age target 60).
        plan = qa.floor_aware_pins({"NDI cam1": 60.0, "NDI cam3": 27.0},
                                   {"NDI cam1": 0.0, "NDI cam3": 33.0})
        assert plan["NDI cam3"] == 36 and plan["NDI cam1"] == 3

    def test_fails_when_a_faster_camera_has_no_arrival_floor(self):
        # a faster camera missing from the jitter measurement cannot be pinned honestly -> FAIL,
        # never a fabricated floor.
        with pytest.raises(qa.AlignmentImpossible) as exc:
            qa.floor_aware_pins({"NDI cam1": 66.0}, {"NDI cam1": 0.0, "NDI cam3": 33.0})
        assert "NDI cam3" in str(exc.value)


# --------------------------------------------------------------------------- #
# align() FLOW: with the strih audit floors, the plan sets ABOVE-floor pins and the FIFO (modelled
# as present_age = max(pin, arrival_floor)) actually MOVES the faster cameras into parity -> aligned.
# RED (pre-fix): align ignores the floors, uses floor3 (below-floor) pins -> the FIFO stays
# misaligned -> the run FAILS. GREEN: floor-aware pins clear the floor -> the FIFO moves -> aligned.
# --------------------------------------------------------------------------- #
_BASE_NS = 30_000 * ID_NS


class _FifoBarrier:
    """A barrier stand-in that MODELS the genlock FIFO's ADDITIVE response to the applied pins (issue
    1252/1253, supervisor-confirmed from run 1899055119: post_residual = pre_residual + pin_delta):
    present_age_i = transport_i + live_pin_i, NOT the old max(pin, transport). transport_i is
    reconstructed from the arrival floor measured at the current pin (transport_i = floor_i -
    current_pin_i), so raising a pin ADDS its hold on top of the transport and a younger camera pinned
    by its present-age GAP is delayed into parity. gen_ts encodes present age EXACTLY (older present ->
    smaller gen_ts). Reads the live pins from the shared `applied` capture (empty before apply -> the
    current pins).

    (Migrated from the pre-1253 max(pin, arrival_floor) model, which was the very bug issue 1253 fixes
    -- the FIFO adds the pin, it does not clamp to the floor. Justification: run 1899055119.)"""

    def __init__(self, floors, current_pins, applied):
        self.transport = {s: floors[s] - current_pins[s] for s in floors}
        self.current_pins, self.applied = current_pins, applied

    def __call__(self, sources, host, password, width, height):
        live = self.applied if self.applied else self.current_pins
        shot = {}
        for s in sources:
            present_ms = self.transport[s] + live.get(s, 0)    # ADDITIVE, not max(pin, transport)
            gen_ts = int(_BASE_NS - present_ms * 1e6)          # older present -> smaller gen_ts
            fid = int(round(gen_ts / ID_NS))
            shot[s] = ([_payload(RUN, fid, gen_ts), _payload(RUN, fid - 1, gen_ts - ID_NS)], 0)
        return shot


def _align_with_floors(monkeypatch):
    import apply_latency_pins
    import obs_phase2
    floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
    current = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 17, "NDI cam4": 22}   # all BELOW their floor
    applied = {}
    monkeypatch.setattr(qa, "barrier_screenshot", _FifoBarrier(floors, current, applied))

    def _read_pins(s, h, p):
        return dict(applied) if applied else dict(current)
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
                    max_measure_rounds=60, settle_s=0,
                    jitter_json=_jitter(floors, current))


class TestAlignFloorAwareFlow:
    def test_above_floor_pins_move_the_fifo_into_parity(self, monkeypatch):
        result = _align_with_floors(monkeypatch)
        assert result["status"] == "aligned"
        assert result["post_spread_ids"] == 0
        # issue 1253 additive: each faster camera gets current_pin + its present-age gap so the additive
        # FIFO delays it into parity (resulting present age all 66). current = {3,6,17,22}, gaps = {3,33,20}.
        plan = result["plan"]
        assert plan["NDI cam2"] == 9 and plan["NDI cam3"] == 50 and plan["NDI cam4"] == 42
        assert plan["NDI cam1"] == 3


# --------------------------------------------------------------------------- #
# #1161 SECOND-ROUND (review findings): the audit "arrival floor" is the PRESENT AGE
# (max(pin, transport)) -> it equals the raw transport ONLY from an un-pinned start. Pins persist
# across runs, so run 2+ plans from a pinned steady state. Fixes: two-phase reset (reset_pins_to_floor
# so the re-fetched floors are true transports), sanity on PURE deltas when floors are present, a
# don't-tear-down safety for a pin-dominated co-slowest, a partial-audit graceful fallback, a sub-floor
# clamp, and a runtime floor_ms in the stuck-abort telemetry.
# --------------------------------------------------------------------------- #
class TestFloorAwarePinsSecondRound:
    def test_clamp_never_emits_a_sub_floor_pin(self):
        # 🔵4: a pathological audit floor below the pin floor must not produce a sub-floor pin.
        plan = qa.floor_aware_pins({"NDI cam1": 5.0, "NDI cam2": 0.0},
                                   {"NDI cam1": 0.0, "NDI cam2": 2.0}, floor_ms=3)
        assert plan["NDI cam2"] >= 3   # target 0+2=2 would be sub-floor -> clamped to 3

    def test_pin_dominated_co_slowest_is_not_torn_down(self, ):
        # 🔴1: from a pinned steady state, a co-slowest camera held at present 66 ONLY by its 66 pin
        # (pin-dominated: current_pin >= its audit present age) must NOT be reset to floor 3 (that
        # drops it to its true lower transport -> misaligned). Keep its pin; bring the faster one up.
        floors = {"NDI cam1": 49.0, "NDI cam2": 66.0, "NDI cam3": 66.0, "NDI cam4": 66.0}  # present ages
        deltas = {"NDI cam1": 17.0, "NDI cam2": 0.0, "NDI cam3": 0.0, "NDI cam4": 0.0}      # c1 faster
        current = {"NDI cam1": 3, "NDI cam2": 66, "NDI cam3": 66, "NDI cam4": 66}
        plan = qa.floor_aware_pins(floors, deltas, floor_ms=3, current_pins=current)
        # co-slowest kept at their pin (hold 0 -> current_pin + 0); issue 1253 additive: faster c1 is
        # its current pin 3 + present-age gap 17 = 20 (resulting present 46+20=66, aligned), NOT the
        # old absolute target 66. The don't-tear-down intent is subsumed by current_pin + hold.
        assert plan["NDI cam2"] == 66 and plan["NDI cam3"] == 66 and plan["NDI cam4"] == 66
        assert plan["NDI cam1"] == 20

    def test_transport_dominated_slowest_still_floors_after_a_reset(self):
        # the reset path: all pins at floor 3 (< transport), so no camera is pin-dominated -> the true
        # slowest floors to 3 and the faster ones are raised to its true transport.
        floors = {"NDI cam1": 49.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}  # true transports
        deltas = {"NDI cam1": 14.0, "NDI cam2": 0.0, "NDI cam3": 30.0, "NDI cam4": 17.0}
        current = {"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3, "NDI cam4": 3}
        plan = qa.floor_aware_pins(floors, deltas, floor_ms=3, current_pins=current)
        assert plan["NDI cam2"] == 3                       # true slowest (hold 0) -> keeps its floor pin
        # issue 1253 additive: new_pin = current(floor 3) + present-age gap (resulting present all 63)
        assert plan["NDI cam1"] == 17 and plan["NDI cam3"] == 33 and plan["NDI cam4"] == 20

    def test_pin_dominated_kept_despite_positive_skew_in_the_audit(self):
        # 🔵a: the audit's own +mean_head_skew can read a pin-dominated co-slowest's floor a couple ms
        # ABOVE its pin (cur 66, fl 68); the slack must still recognise it as pin-dominated, not tear
        # it down to the floor.
        plan = qa.floor_aware_pins({"NDI cam1": 49.0, "NDI cam2": 68.0},
                                   {"NDI cam1": 17.0, "NDI cam2": 0.0}, floor_ms=3,
                                   current_pins={"NDI cam1": 3, "NDI cam2": 66})
        assert plan["NDI cam2"] == 66   # kept (pin-dominated within slack), not torn to 3

    def test_pin_dominated_kept_when_absent_from_the_audit(self):
        # 🔵a: a co-slowest pinned UP (cur > floor) but MISSING from the audit (fl None) must keep its
        # pin, not floor to 3 (which would drop it to its unobservable lower transport).
        plan = qa.floor_aware_pins({"NDI cam1": 49.0},
                                   {"NDI cam1": 17.0, "NDI cam2": 0.0}, floor_ms=3,
                                   current_pins={"NDI cam1": 3, "NDI cam2": 66})
        assert plan["NDI cam2"] == 66

    def test_stuck_abort_reason_uses_runtime_floor_ms(self):
        # 🔵5: with --floor-ms 5, a source pinned at exactly 5 is a FLOORED (not raised) source and
        # must not be named as a stuck raised camera.
        msg = qa.floor_aware_stuck_abort_reason(
            {"NDI cam1": 66, "NDI cam2": 5}, {"NDI cam1": 49.0, "NDI cam2": 63.0},
            {"NDI cam1": 66, "NDI cam2": 5}, {"NDI cam1": 3.0}, floor_ms=5)
        # cam1 (66, above the runtime floor 5) is a RAISED-stuck source; cam2 (at the floor 5) is NOT
        # named as raised (it appears only in the verbatim plan dump, never as a "pinned to" clause).
        assert "'NDI cam1' pinned to 66 ms" in msg
        assert "'NDI cam2' pinned to" not in msg


class TestAlignSecondRound:
    def _align(self, monkeypatch, floors, current, jitter=True, apply_reset=True):
        import apply_latency_pins
        import obs_phase2
        applied = {}
        monkeypatch.setattr(qa, "barrier_screenshot", _FifoBarrier(floors, current, applied))

        def _read_pins(s, h, p):
            return dict(applied) if applied else dict(current)
        monkeypatch.setattr(qa, "read_current_pins", _read_pins)

        def _apply(ws, plan, execute):
            applied.update(plan)
            return plan
        monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

        class _WS:
            def close(self):
                pass
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
        jj = _jitter(floors, current) if jitter is True else jitter
        return qa.align(SRC, "h", "pw", execute=True, stable_tail_rounds=3, stable_tol_ids=1,
                        min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                        floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                        max_measure_rounds=60, settle_s=0, jitter_json=jj)

    def test_pinned_state_does_not_spuriously_fail_sanity(self, monkeypatch):
        # 🔴2: from a pinned steady state, a legit drift (true present spread 27 <= 66) must NOT hard
        # FAIL as "degraded grabber" on the pin-FOLDED metric (which over-reads past the 66 ms bound).
        # With sanity on PURE deltas, the run re-aligns (present ages {93,66,66,66} -> plan brings all
        # to 93). #1252: the spread is 27 ms (> the 25 ms lock-phase quantum, so the plan runs) yet the
        # target 66+27 = 93 stays <= the 94 ms ceiling (so it aligns, never budget-bound); a
        # <= one-source-frame spread would now be already-aligned (quantum gate).
        floors = {"NDI cam1": 93.0, "NDI cam2": 66.0, "NDI cam3": 66.0, "NDI cam4": 66.0}
        current = {"NDI cam1": 3, "NDI cam2": 66, "NDI cam3": 66, "NDI cam4": 66}
        result = self._align(monkeypatch, floors, current)
        assert result["status"] == "aligned"     # NOT AlignmentImpossible("degraded/underrun grabber")
        assert result["post_spread_ids"] == 0

    def test_sanity_abort_message_uses_the_judged_pure_deltas(self, monkeypatch):
        # 🔵b: on the floor-aware path sanity judges the PURE deltas, so the abort's per-camera map
        # must be the PURE map, NOT the pin-FOLDED median (cam2 folded ~140 from its 63 ms pin) -- the
        # folded map would mislead the exact "degraded grabber" diagnosis the owner reads.
        floors = {"NDI cam1": 70.0, "NDI cam2": 70.0, "NDI cam3": 70.0, "NDI cam4": 150.0}
        current = {"NDI cam1": 3, "NDI cam2": 63, "NDI cam3": 3, "NDI cam4": 3}
        with pytest.raises(qa.AlignmentImpossible) as exc:
            self._align(monkeypatch, floors, current)
        msg = str(exc.value)
        assert "140" not in msg      # the pin-folded cam2 delta must NOT appear
        assert "80.0" in msg         # the pure per-camera delta does

    def test_partial_audit_falls_back_not_abort(self, monkeypatch):
        # 🟡3: a jitter JSON missing a FASTER camera must degrade to the floor3 fallback (+warning),
        # never the hard floor-aware missing-floor abort. issue 1253: under the ADDITIVE FIFO the floor3
        # fallback (floor + delta) actually ALIGNS the barrier, so the run PASSES via the fallback path
        # (the pre-1253 max-model barrier left it misaligned -> a FAIL; the fallback is no longer inert).
        floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
        current = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 17, "NDI cam4": 22}
        partial = _jitter({"NDI cam1": 66.0, "NDI cam2": 63.0}, {"NDI cam1": 3, "NDI cam2": 6})  # cam3/4 absent
        result = self._align(monkeypatch, floors, current, jitter=partial)
        assert result["status"] == "aligned"          # the floor3 fallback aligns under the additive FIFO
        assert result["post_spread_ids"] == 0


class TestResetPinsToFloor:
    def test_applies_the_floor_to_every_source(self, monkeypatch):
        import apply_latency_pins
        import obs_phase2
        applied = {}

        def _apply(ws, plan, execute):
            applied.update(plan)
            return plan
        monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

        class _WS:
            def close(self):
                pass
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
        n = qa.reset_pins_to_floor(SRC, "h", "pw", floor_ms=3)
        assert applied == {s: 3 for s in SRC}
        assert n == len(SRC)


# --------------------------------------------------------------------------- #
# #1161 FINAL LANE (issue 1168 re-tighten path): BUDGET_BOUND soft-release. When the tail is STABLE
# and within the spread sanity, but the alignment correction for >=1 faster camera exceeds the 94 ms
# achievable-latency ceiling (a per-box arrival-floor difference physically budget-impossible to
# correct -- a pin above the ceiling is forbidden by the deep-pin doctrine), the run must NOT fail:
# apply NOTHING, persist the residual into the align JSON + emit a loud named marker, exit 0 so the
# E2E proceeds. floor_aware_partition() is the pure core that PARTITIONS the over-budget cameras
# instead of raising; floor_aware_pins() still RAISES on over_budget/missing (byte-unchanged).
# --------------------------------------------------------------------------- #
class TestFloorAwarePartition:
    def test_partitions_over_budget_without_raising(self):
        # the live issue-1168 shape: cam2/cam3 targets 129 > 94; cam4 within budget; cam1 slowest.
        floors = {"NDI cam1": 129.0, "NDI cam2": 100.0, "NDI cam3": 79.0, "NDI cam4": 40.0}
        deltas = {"NDI cam1": 0.0, "NDI cam2": 29.0, "NDI cam3": 50.0, "NDI cam4": 20.0}
        plan, over, missing = qa.floor_aware_partition(floors, deltas)   # does NOT raise
        assert missing == []
        obs = {s: t for s, fl, hl, t in over}
        assert set(obs) == {"NDI cam2", "NDI cam3"}         # 100+29=129, 79+50=129 -> over 94
        assert obs["NDI cam3"] == pytest.approx(129.0)
        # issue 1253 additive: within-budget faster camera gets current(floor 3) + delta 20 = 23 (its
        # resulting present age 40+20=60 <= 94); the over_budget `target` stays the RESULTING present age.
        assert plan["NDI cam4"] == 23
        assert plan["NDI cam1"] == 3                         # slowest floors
        # over-budget cameras are CLAMPED to the floor (a pin we cannot afford is never written up)
        assert plan["NDI cam2"] == 3 and plan["NDI cam3"] == 3

    def test_partition_equals_floor_aware_pins_when_all_within_budget(self):
        # equivalence on the within-budget path: partition's plan == floor_aware_pins' plan.
        floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
        deltas = {"NDI cam1": 0.0, "NDI cam2": 3.0, "NDI cam3": 33.0, "NDI cam4": 20.0}
        plan, over, missing = qa.floor_aware_partition(floors, deltas)
        assert over == [] and missing == []
        assert plan == qa.floor_aware_pins(floors, deltas)

    def test_partition_reports_missing_faster_floor_without_raising(self):
        plan, over, missing = qa.floor_aware_partition({"NDI cam1": 66.0},
                                                       {"NDI cam1": 0.0, "NDI cam3": 33.0})
        assert missing == ["NDI cam3"] and over == []

    def test_over_budget_camera_is_clamped_to_floor_not_pinned_up(self):
        # a pin we cannot afford is NEVER written up to the (over-ceiling) target.
        plan, over, _ = qa.floor_aware_partition({"NDI cam1": 129.0, "NDI cam3": 79.0},
                                                 {"NDI cam1": 0.0, "NDI cam3": 50.0}, floor_ms=3)
        assert [s for s, *_ in over] == ["NDI cam3"]
        assert plan["NDI cam3"] == 3

    def test_floor_aware_pins_still_raises_over_budget_after_the_refactor(self):
        # the HARD-FAIL direction is byte-unchanged: floor_aware_pins delegates to the partition then
        # RAISES on over_budget, naming the exact arithmetic (existing TestFloorAwarePins coverage).
        with pytest.raises(qa.AlignmentImpossible) as exc:
            qa.floor_aware_pins({"NDI cam1": 66.0, "NDI cam3": 66.0},
                                {"NDI cam1": 0.0, "NDI cam3": 33.0})
        msg = str(exc.value)
        assert "NDI cam3" in msg and "99" in msg and "94" in msg and "do NOT raise the bound" in msg


class TestBudgetBoundSoftRelease:
    def _budget_align(self, monkeypatch, floors, current):
        import apply_latency_pins
        import obs_phase2
        applied = {}
        monkeypatch.setattr(qa, "barrier_screenshot", _FifoBarrier(floors, current, applied))

        def _read_pins(s, h, p):
            return dict(applied) if applied else dict(current)
        monkeypatch.setattr(qa, "read_current_pins", _read_pins)

        def _apply(ws, plan, execute):
            applied.update(plan)
            return plan
        monkeypatch.setattr(apply_latency_pins, "apply_pins", _apply)

        class _WS:
            def close(self):
                pass
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw: _WS())
        result = qa.align(SRC, "h", "pw", execute=True, stable_tail_rounds=3, stable_tol_ids=1,
                          min_valid_rounds=5, min_parity_rounds=3, max_delta_ms=66.0, parity_tol_ids=1,
                          floor_ms=3, width=1920, height=1080, measure_budget_s=1e9,
                          max_measure_rounds=60, settle_s=0, jitter_json=_jitter(floors, current))
        return result, applied

    def test_stable_over_budget_tail_soft_releases_not_fails(self, monkeypatch):
        # present ages {129,100,79,90}: spread 50 <= 66 passes sanity, but every faster camera would
        # need a pin == 129 > 94 -> budget-impossible. RED (pre-fix): align() RAISES AlignmentImpossible
        # -> the run FAILS. GREEN: budget-bound soft-release, applies nothing, exit-0-shaped result.
        floors = {"NDI cam1": 129.0, "NDI cam2": 100.0, "NDI cam3": 79.0, "NDI cam4": 90.0}
        current = {"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3, "NDI cam4": 3}   # two-phase reset -> floor
        result, applied = self._budget_align(monkeypatch, floors, current)
        assert result["status"] == "budget-bound"
        assert result["budget_bound"] is True
        assert result["plan"] == {}          # apply NONE
        assert applied == {}                  # nothing written to the rig
        assert result["report_only_residual_ms"] == pytest.approx(50.0, abs=1.0)   # surviving spread
        srcs = {o["source"] for o in result["over_budget"]}
        assert "NDI cam3" in srcs
        for o in result["over_budget"]:
            assert o["target_ms"] > o["bound_ms"]     # every entry genuinely exceeds the ceiling

    def test_within_budget_still_aligns_never_budget_bound(self, monkeypatch):
        # REGRESSION: a within-budget floor set (max floor 66 <= 94) must still ALIGN, never soft-release.
        floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
        current = {"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3, "NDI cam4": 3}
        result, applied = self._budget_align(monkeypatch, floors, current)
        assert result["status"] == "aligned"
        assert applied != {}                  # within-budget pins WERE applied (the frame moved)
        assert "budget_bound" not in result

    def test_hold_inert_within_budget_still_fails_not_budget_bound(self, monkeypatch):
        # REQUIREMENT 4: a WITHIN-budget pin that is HOLD-INERT (frame does not move) is STILL a FAIL,
        # never folded into budget-bound. _ScriptedBarrier stays stuck at spread 2 after apply (the pin
        # config moves but the presented frame does not) and NO jitter -> the inert floor+delta path.
        with pytest.raises(qa.AlignmentImpossible) as exc:
            _align_stuck_after_apply(monkeypatch)
        assert "genlock FIFO did NOT add the requested hold" in str(exc.value)   # a real defect FAIL

    def test_main_exits_zero_on_budget_bound(self, monkeypatch, capsys):
        # exit 0 -> the E2E proceeds; the JSON persists the residual (stdout) + a loud summary (stderr).
        fake = {"status": "budget-bound", "budget_bound": True,
                "over_budget": [{"source": "NDI cam3", "arrival_floor_ms": 79.0, "delta_ms": 50.0,
                                 "target_ms": 129.0, "bound_ms": 94}],
                "report_only_residual_ms": 50.0, "plan": {}, "pre_spread_ids": 6,
                "tail_rounds": 3, "measure_rounds_total": 8, "measure_reason": "converged-stable"}
        monkeypatch.setattr(qa, "align", lambda *a, **k: fake)
        rc = qa.main(["--host", "h", "--sources", "NDI cam1,NDI cam3", "--execute"])
        assert rc == 0
        out = capsys.readouterr()
        assert '"status": "budget-bound"' in out.out     # persisted into the align JSON (stdout)
        assert "1168" in out.err                          # loud summary names the tracking ticket


# --------------------------------------------------------------------------- #
# issue 1253 -- the FIFO is ADDITIVE (present_age = transport + pin), NOT max(pin, transport).
# The issue-1161 formula wrote an ABSOLUTE present-age target (arrival_floor_i + delta_i) as the PIN,
# which under the additive FIFO adds ON TOP of the transport -> overshoot by ~the arrival-floor
# baseline (the run 1899055119 +83 ms doubling). The additive-correct plan ADDS the present-age gap to
# the CURRENT pin (new_pin_i = current_pin_i + delta_i) so every camera converges to the max present
# age. RED here on the current max-model formula (the overshoot leaves the verify tail off-parity ->
# AlignmentImpossible); GREEN once the formula is additive.
# --------------------------------------------------------------------------- #
def _align_additive_overshoot(monkeypatch):
    """A GENUINE >= 2-source-frame cross-camera spread (40 ms, past the #1252 quantum, within the 66 ms
    sanity and the 94 ms ceiling) under the ADDITIVE FIFO. cam1 is the oldest present (63 ms); the
    others are 40/30/20 ms younger. The additive-correct plan holds each younger camera by exactly its
    present-age gap -> all converge to 63 ms. The max-model plan would pin them to 63 ms ABSOLUTE ->
    additive present 83/93/103 ms -> overshoot -> off-parity abort."""
    import apply_latency_pins
    import obs_phase2
    transports = {"NDI cam1": 60, "NDI cam2": 20, "NDI cam3": 30, "NDI cam4": 40}
    current = {s: 3 for s in SRC}
    floors = {s: transports[s] + current[s] for s in SRC}       # present ages at the floor: {63,23,33,43}
    applied = {}
    monkeypatch.setattr(qa, "barrier_screenshot", _FifoBarrier(floors, current, applied))

    def _read_pins(s, h, p):
        return dict(applied) if applied else dict(current)
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
                    max_measure_rounds=60, settle_s=0, jitter_json=_jitter(floors, current))


class TestAdditiveFifoNoOvershoot:
    def test_additive_plan_aligns_without_overshoot(self, monkeypatch):
        # RED (max-model pin = arrival_floor + delta): under the ADDITIVE FIFO the pin adds ON TOP of
        # the transport, so an absolute present-age target overshoots -> the verify tail stays
        # off-parity -> AlignmentImpossible. GREEN (pin = current_pin + delta): each younger camera is
        # delayed by exactly its present-age gap -> all present ages converge to 63 ms -> aligned.
        result = _align_additive_overshoot(monkeypatch)
        assert result["status"] == "aligned"
        assert result["post_spread_ids"] == 0
        plan = result["plan"]
        # the plan ADDS the present-age gap to the CURRENT pin, never the absolute present-age target
        assert plan["NDI cam1"] == 3                        # oldest present -> keeps the floor
        assert plan["NDI cam2"] == 43                       # 3 + gap 40
        assert plan["NDI cam3"] == 33                       # 3 + gap 30
        assert plan["NDI cam4"] == 23                       # 3 + gap 20
        # never the max-model absolute target (which would be 63 for every younger camera)
        assert plan["NDI cam2"] != 63 and plan["NDI cam3"] != 63 and plan["NDI cam4"] != 63
