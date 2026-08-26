"""#1003 -- unit tests for scripts/qr_align_pins.py, the floor-3 per-run camera aligner.

The owner's binding rework mandate (issue 1003, ODMIETNUTÉ + REVERTNUTÉ, 2026-08-20):

  1. NAJPOMALŠIA (max-transport) camera gets pin 3 (floor); the others get 3 + their RELATIVE
     delivery delta -- alignment compensates only RELATIVE differences, never absolute depth (the
     rejected 90/160/184 added ~180 ms of needless chain latency).
  2. Deltas must be RE-DERIVED robustly (many rounds, median, exclude underrun/undecodable
     windows) -- the rejected MEQ single delivery-p50 sample baked in a degraded cam1 grabber.
  3. cam4 is on-air -> it MUST be in the alignment set.
  4. Alignment is an AUTOMATIC per-run process: measure -> align (floor 3) -> verify -> FAIL if it
     cannot align.

The signal is the painter QR wire string `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` (src/probe/
payload.rs): one camera is optically split to every box, so a SIMULTANEOUS barrier screenshot of
every strih input decodes a DIFFERENT painter frame per box, and gen_ts_ns (the painter's own
per-frame timestamp, identical across boxes for a given frame) encodes each box's delivery latency
EXACTLY, frame-rate-independent. frame_id spread <= 1 is the owner's "same monotonic + time in
every QR" parity check.

Tier-0: every function here is PURE or exercised against a FAKE ws stub -- no rig, no cargo. The
live-apply test monkeypatches ONE point (apply_latency_pins._rpc, reused by qr_align).
"""
import pathlib
import sys
import zlib

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402


# ---------------------------------------------------------------------------
# helpers: build a valid painter QR wire string (same CRC-32 as src/probe/payload.rs)
# ---------------------------------------------------------------------------
def _payload(run_id: int, frame_id: int, gen_ts_ns: int) -> str:
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    crc = zlib.crc32(body.encode()) & 0xFFFFFFFF
    return f"P{body}.{crc}"


ID_NS = 8_333_333  # per-frame_id gen_ts step in ns (dual-QR ~2 ids/frame @60fps -> ~8.33ms/id);
# the CODE never assumes this -- it reads gen_ts_ns directly. It only shapes realistic fixtures.
RUN = 4242


def _shot(frame_id: int, gen_ts_ns: int, extra=()):
    """One screenshot's decoded QR text list: the painter dual-QR (frame_id and frame_id-1, both
    the same run) + any extra foreign QRs (node-burns / a different run)."""
    texts = [_payload(RUN, frame_id, gen_ts_ns),
             _payload(RUN, frame_id - 1, gen_ts_ns - ID_NS)]
    texts.extend(extra)
    return texts


# ---------------------------------------------------------------------------
# pick_painter_tick -- max-frame_id painter payload for a run_id ("ber max")
# ---------------------------------------------------------------------------
class TestPickPainterTick:
    def test_picks_the_max_frame_id_of_the_dual_qr(self):
        texts = _shot(1000, 5 * ID_NS)
        assert qa.pick_painter_tick(texts, RUN) == (1000, 5 * ID_NS)

    def test_ignores_a_foreign_run_id(self):
        texts = _shot(1000, 5 * ID_NS, extra=[_payload(9999, 77777, 12345)])
        assert qa.pick_painter_tick(texts, RUN) == (1000, 5 * ID_NS)

    def test_rejects_a_crc_corrupt_qr(self):
        good = _payload(RUN, 500, 9 * ID_NS)
        bad = good[:-1] + ("0" if good[-1] != "0" else "1")  # flip last CRC digit
        assert qa.pick_painter_tick([bad], RUN) is None

    def test_none_when_no_matching_payload(self):
        assert qa.pick_painter_tick([_payload(1, 2, 3)], RUN) is None
        assert qa.pick_painter_tick(["garbage", ""], RUN) is None


# ---------------------------------------------------------------------------
# frame_id_spread -- the owner's parity metric (same monotonic in every QR)
# ---------------------------------------------------------------------------
class TestFrameIdSpread:
    def test_spread_is_max_minus_min_over_decoded(self):
        rnd = {"NDI cam1": (100, 0, 0), "NDI cam2": (102, 0, 0), "NDI cam3": (103, 0, 0)}
        assert qa.frame_id_spread(rnd) == 3

    def test_zero_when_all_equal(self):
        rnd = {"NDI cam1": (50, 0, 0), "NDI cam2": (50, 0, 0)}
        assert qa.frame_id_spread(rnd) == 0

    def test_none_when_fewer_than_two_decoded(self):
        assert qa.frame_id_spread({"NDI cam1": (5, 0, 0), "NDI cam2": None}) is None
        assert qa.frame_id_spread({}) is None


# ---------------------------------------------------------------------------
# round_deltas -- per-round relative ms delta d_i = m_i - min(m), m_i = gen_ts_ns/1e6 + pin
# ---------------------------------------------------------------------------
class TestRoundDeltas:
    def test_relative_ms_deltas_include_current_pins(self):
        # cam1 shows the OLDEST frame (smallest gen_ts) but has the smallest pin -> it is the
        # max-transport (slowest) camera and must anchor to delta 0.
        rnd = {
            "NDI cam1": (100, 0, 0),
            "NDI cam2": (102, 2 * ID_NS, 0),
            "NDI cam3": (103, 3 * ID_NS, 0),
        }
        pins = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        d = qa.round_deltas(rnd, pins)
        # m = gen_ts_ns/1e6 + pin: cam1=0+3=3, cam2=66.67+6=72.67, cam3=100+20=120 ; min=3
        assert d["NDI cam1"] == pytest.approx(0.0, abs=0.01)
        assert d["NDI cam2"] == pytest.approx(19.6667, abs=0.01)
        assert d["NDI cam3"] == pytest.approx(42.0, abs=0.01)

    def test_none_when_a_source_is_undecoded(self):
        rnd = {"NDI cam1": (100, 0, 0), "NDI cam2": None}
        assert qa.round_deltas(rnd, {"NDI cam1": 3, "NDI cam2": 6}) is None

    def test_t_send_stagger_is_compensated(self):
        # Two identical cameras (same pin, same true latency). cam2 was SERVED ~16.67 ms later
        # (higher t_send), so it latched a ~16.67 ms-NEWER frame (higher gen_ts). Without t_send
        # compensation cam2 would look 16.67 ms "faster" (a false delta); with it, both read 0.
        rnd = {
            "NDI cam1": (100, 0, 0),
            "NDI cam2": (102, 2 * ID_NS, 2 * ID_NS),
        }
        pins = {"NDI cam1": 3, "NDI cam2": 3}
        d = qa.round_deltas(rnd, pins)
        assert d["NDI cam1"] == pytest.approx(0.0, abs=0.01)
        assert d["NDI cam2"] == pytest.approx(0.0, abs=0.01)  # stagger removed, no false delta

    def test_none_when_a_pin_is_unknown(self):
        rnd = {"NDI cam1": (100, 0, 0), "NDI cam2": (101, ID_NS, 0)}
        assert qa.round_deltas(rnd, {"NDI cam1": 3, "NDI cam2": None}) is None


# ---------------------------------------------------------------------------
# robust_deltas -- median over VALID (all-decoded) rounds; excludes underrun/undecoded rounds
# ---------------------------------------------------------------------------
class TestRobustDeltas:
    def _rounds(self, per_round):
        """per_round: list of {source: (frame_id, gen_ts_ns) | None}."""
        return per_round

    def test_medians_over_valid_rounds(self):
        pins = {"NDI cam1": 3, "NDI cam2": 6}
        rounds = [
            {"NDI cam1": (100, 0, 0), "NDI cam2": (101, 1 * ID_NS, 0)},
            {"NDI cam1": (110, 10 * ID_NS, 0), "NDI cam2": (111, 11 * ID_NS, 0)},
            {"NDI cam1": (120, 20 * ID_NS, 0), "NDI cam2": (121, 21 * ID_NS, 0)},
        ]
        deltas, n_valid = qa.robust_deltas(rounds, pins, min_valid_rounds=2)
        assert n_valid == 3
        # every round: cam1 m=gen/1e6+3, cam2 m=gen/1e6+FRAME_MS+6 -> cam1 is min each round.
        assert deltas["NDI cam1"] == pytest.approx(0.0, abs=0.01)
        assert deltas["NDI cam2"] == pytest.approx(ID_NS / 1e6 + 3.0, abs=0.01)

    def test_excludes_undecoded_and_outlier_rounds_via_median(self):
        pins = {"NDI cam1": 3, "NDI cam2": 6}
        rounds = [
            {"NDI cam1": (100, 0, 0), "NDI cam2": (102, 2 * ID_NS, 0)},          # d2 ~ 63.7
            {"NDI cam1": (110, 10 * ID_NS, 0), "NDI cam2": None},             # DROPPED (undecoded)
            {"NDI cam1": (120, 20 * ID_NS, 0), "NDI cam2": (122, 22 * ID_NS, 0)},  # d2 ~ 63.7
            # an underrun outlier round: cam2 momentarily 10 frames behind -> median ignores it
            {"NDI cam1": (130, 30 * ID_NS, 0), "NDI cam2": (140, 40 * ID_NS, 0)},  # d2 ~ 336
        ]
        deltas, n_valid = qa.robust_deltas(rounds, pins, min_valid_rounds=2)
        assert n_valid == 3  # the undecoded round dropped
        # median of {63.7, 63.7, 336} = 63.7 -> the underrun outlier is excluded by the median
        assert deltas["NDI cam2"] == pytest.approx(2 * ID_NS / 1e6 + 3.0, abs=0.01)

    def test_fails_when_too_few_valid_rounds(self):
        pins = {"NDI cam1": 3, "NDI cam2": 6}
        rounds = [{"NDI cam1": (100, 0, 0), "NDI cam2": None}]
        with pytest.raises(qa.AlignmentImpossible):
            qa.robust_deltas(rounds, pins, min_valid_rounds=3)


# ---------------------------------------------------------------------------
# floor3_pins -- slowest (max-transport, min-m) camera -> 3; others 3 + relative delta
# ---------------------------------------------------------------------------
class TestFloor3Pins:
    def test_min_delta_camera_floors_to_three(self):
        deltas = {"NDI cam1": 0.0, "NDI cam2": 69.7, "NDI cam3": 117.0}
        pins = qa.floor3_pins(deltas)
        assert pins["NDI cam1"] == 3
        assert pins["NDI cam2"] == 73   # round(3 + 69.7)
        assert pins["NDI cam3"] == 120  # round(3 + 117.0)

    def test_floors_the_smallest_delta_even_when_not_zero(self):
        # if no source landed at exactly delta 0 (min shifted across rounds), still floor to 3.
        deltas = {"NDI cam1": 5.0, "NDI cam2": 25.0}
        pins = qa.floor3_pins(deltas)
        assert pins["NDI cam1"] == 3            # 3 + (5 - 5)
        assert pins["NDI cam2"] == 23           # 3 + (25 - 5)

    def test_supervisor_live_measurement_yields_shallow_pins_not_deep(self):
        # supervisor 2026-08-20 live read at pins 3/6/20/3, frame_ids 188800/188802/188803/188803
        # (dual-QR, ~1.5-frame spread, cam1 slowest). Model must produce SHALLOW floor-3 pins --
        # never the rejected deep 90/160/184.
        base = 188800
        rnd = {
            "NDI cam1": (base + 0, (base + 0) * ID_NS, 0),
            "NDI cam2": (base + 2, (base + 2) * ID_NS, 0),
            "NDI cam3": (base + 3, (base + 3) * ID_NS, 0),
            "NDI cam4": (base + 3, (base + 3) * ID_NS, 0),
        }
        pins_cur = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20, "NDI cam4": 3}
        d = qa.round_deltas(rnd, pins_cur)
        out = qa.floor3_pins(d)
        assert out["NDI cam1"] == 3                       # slowest -> floor
        assert all(3 <= v <= 130 for v in out.values())   # all "low tens", never ~180 deep
        assert out != {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}  # never the rejected set


# ---------------------------------------------------------------------------
# sanity_ok -- a delta above the bound = a degraded/underrun card, FAIL rather than ship a deep pin
# ---------------------------------------------------------------------------
class TestSanity:
    def test_small_deltas_pass(self):
        ok, slowest, widest, worst = qa.sanity_ok(
            {"a": 0.0, "b": 25.0, "c": 45.0}, max_delta_ms=90.0)
        assert ok is True

    def test_a_degraded_card_blowout_fails(self):
        ok, slowest, widest, worst = qa.sanity_ok({"a": 0.0, "b": 117.0}, max_delta_ms=90.0)
        assert ok is False
        assert slowest == "a"   # the min-delta (floored) camera -- the likely-degraded slow card
        assert widest == "b"    # the biggest gap from the slowest, NOT wrongly blamed as degraded
        assert worst == pytest.approx(117.0)

    def test_default_bound_rejects_the_owner_cited_94ms(self):
        # #1003 review 🔴: the DEFAULT bound must reject the owner's cited "94 ms between identical
        # cards is nonsense" -- a 100 ms default silently re-enabled the rejected deep-pin behavior.
        ok, _slow, _wide, worst = qa.sanity_ok({"a": 0.0, "b": 94.0})  # DEFAULT max_delta_ms
        assert ok is False and worst == pytest.approx(94.0)
        assert qa.DEFAULT_MAX_DELTA_MS < 94.0


# ---------------------------------------------------------------------------
# alignment_ok -- the re-measure parity gate (frame_id spread <= tol)
# ---------------------------------------------------------------------------
class TestAlignmentOk:
    def test_within_tolerance_passes(self):
        rnd = {"NDI cam1": (100, 0, 0), "NDI cam2": (101, 0, 0), "NDI cam3": (100, 0, 0)}
        assert qa.alignment_ok(rnd, tol_frame_ids=1) is True

    def test_over_tolerance_fails(self):
        rnd = {"NDI cam1": (100, 0, 0), "NDI cam2": (103, 0, 0)}
        assert qa.alignment_ok(rnd, tol_frame_ids=1) is False

    def test_unverifiable_round_is_not_a_pass(self):
        # fewer than two decoded -> cannot prove parity -> must NOT report aligned.
        assert qa.alignment_ok({"NDI cam1": (5, 0, 0), "NDI cam2": None}, tol_frame_ids=1) is False


# ---------------------------------------------------------------------------
# domains: the resolver only ever emits strih pins -- never imag, never the stream hold
# ---------------------------------------------------------------------------
class TestDomainSafety:
    def test_floor3_only_returns_the_sources_it_was_given(self):
        # qr_align is invoked with the strih align sources ONLY; it can never emit an imag or
        # stream-hold key because it only knows the strih inputs it measured.
        deltas = {"NDI cam1": 0.0, "NDI cam2": 20.0}
        out = qa.floor3_pins(deltas)
        assert set(out) == {"NDI cam1", "NDI cam2"}
        assert "NDI 2ME PGM" not in out
        assert "_all_ndi_inputs_ms" not in out
