"""#761 -- unit tests for scripts/mv_skew_snapshot.py, the PURE decision logic of the per-camera
MV-clone-vs-main presentation-skew measurement (order-reversed paired screenshots -> painter QR
gen_ts_ns decode -> skew median). Covers, with NO live OBS/network/cv2:

  a. parse_payload()  -- decode `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` with CRC-32 validation.
  b. tick_map()       -- {run_id: newest gen_ts_ns} from a list of decoded QR texts.
  c. dominant_run_id()-- the universal painter run_id (present in the most screenshots).
  d. common_tick_delta_ns() -- gen_ts(main) - gen_ts(MV) for a common run_id; honest None.
  e. skew_ms_from_pairs()   -- order-reversal cancels the inter-screenshot wall gap (the core).
  f. is_skew_alarming()     -- |median| > 1 frame flag.

These are pure functions -- importing the module here must NOT require cv2 / websocket / a rig.
"""
import pathlib
import sys
import zlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import mv_skew_snapshot as mvs  # noqa: E402


def _mk(run_id, frame_id, gen_ts_ns, *, corrupt_crc=False):
    """Build a valid (or CRC-corrupt) painter QR wire string, exactly as src/probe/payload.rs."""
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    crc = zlib.crc32(body.encode()) & 0xFFFFFFFF
    if corrupt_crc:
        crc ^= 1
    return f"P{body}.{crc}"


# --------------------------------------------------------------------------- parse_payload
def test_parse_payload_valid_roundtrip():
    s = _mk(1786886721, 1067419, 17792055009087)
    assert mvs.parse_payload(s) == (1786886721, 1067419, 17792055009087)


def test_parse_payload_rejects_bad_crc():
    assert mvs.parse_payload(_mk(1, 2, 3, corrupt_crc=True)) is None


def test_parse_payload_rejects_malformed():
    for bad in ["", "P", "1.2.3.4", "Pnope", "P1.2.3", "Pa.b.c.d", "P1.2.3.4.5"]:
        assert mvs.parse_payload(bad) is None, bad


# --------------------------------------------------------------------------- tick_map
def test_tick_map_groups_by_run_id_keeping_newest_gen_ts():
    # painter dual-QR pair (same run_id, adjacent frames) + a cam1-burn (other run_id)
    texts = [
        _mk(100, 5, 17000),
        _mk(100, 6, 17016),  # newer gen_ts for run_id 100
        _mk(911003, 5131072, 1786904514302230644),
    ]
    m = mvs.tick_map(texts)
    assert m == {100: 17016, 911003: 1786904514302230644}


def test_tick_map_ignores_undecodable_and_empty():
    assert mvs.tick_map(["garbage", _mk(7, 1, 999), "", _mk(7, 2, 111, corrupt_crc=True)]) == {7: 999}


# --------------------------------------------------------------------------- dominant_run_id
def test_dominant_run_id_picks_the_most_present():
    maps = [{100: 1, 911003: 9}, {100: 2}, {100: 3, 42: 5}, {42: 6}]
    assert mvs.dominant_run_id(maps) == 100  # present in 3 maps, others in <=2


def test_dominant_run_id_none_when_empty():
    assert mvs.dominant_run_id([]) is None
    assert mvs.dominant_run_id([{}, {}]) is None


# --------------------------------------------------------------------------- common_tick_delta_ns
def test_common_tick_delta_prefers_the_preferred_run_id():
    main = {100: 5000, 911003: 8000}
    mv = {100: 4000, 911003: 1000}
    # prefer painter run_id 100 -> 5000-4000 = 1000 (NOT the cam1-burn 911003)
    assert mvs.common_tick_delta_ns(main, mv, preferred=100) == 1000


def test_common_tick_delta_falls_back_to_a_common_run_id():
    main = {55: 5000}
    mv = {55: 4200}
    assert mvs.common_tick_delta_ns(main, mv, preferred=100) == 800


def test_common_tick_delta_none_when_no_common_run_id():
    assert mvs.common_tick_delta_ns({1: 10}, {2: 20}, preferred=99) is None


# --------------------------------------------------------------------------- skew_ms_from_pairs (CORE)
def test_skew_order_reversal_cancels_the_inter_shot_gap():
    # True skew S = +5ms (MV presents 5ms LATER => gen_ts(main) - gen_ts(MV) baseline +5ms).
    # Each pair: forward delta = S - Delta, reverse delta = S + Delta (Delta = wall gap, varies).
    ms = 1_000_000  # ns per ms
    S = 5 * ms
    pairs = []
    for delta in (900 * ms, 1100 * ms, 800 * ms, 1500 * ms):  # wildly varying WS-call gaps
        pairs.append((S - delta, S + delta))  # (forward_ns, reverse_ns)
    out = mvs.skew_ms_from_pairs(pairs)
    assert out["n_pairs"] == 4
    assert abs(out["median_ms"] - 5.0) < 1e-9  # Delta fully cancels -> exactly the true skew


def test_skew_shared_source_is_zero():
    # shared-source regime: main and MV draw the same texture => S=0; only the wall gap differs.
    ms = 1_000_000
    pairs = [(-d, d) for d in (1000 * ms, 1750 * ms, 900 * ms, 1200 * ms)]
    out = mvs.skew_ms_from_pairs(pairs)
    assert abs(out["median_ms"]) < 1e-9
    assert out["n_pairs"] == 4


def test_skew_empty_pairs_is_honest_none():
    out = mvs.skew_ms_from_pairs([])
    assert out["median_ms"] is None
    assert out["n_pairs"] == 0


def test_skew_median_is_robust_to_one_outlier():
    ms = 1_000_000
    # three clean +2ms pairs + one wild outlier pair -> median stays ~2ms
    pairs = [(2 * ms - 1000 * ms, 2 * ms + 1000 * ms)] * 3 + [(500 * ms, 500 * ms)]
    out = mvs.skew_ms_from_pairs(pairs)
    assert abs(out["median_ms"] - 2.0) < 1e-9


# --------------------------------------------------------------------------- is_skew_alarming
def test_is_skew_alarming_uses_one_frame_threshold():
    assert mvs.is_skew_alarming(0.5) is False
    assert mvs.is_skew_alarming(-0.5) is False
    assert mvs.is_skew_alarming(30.0) is True
    assert mvs.is_skew_alarming(-30.0) is True
    assert mvs.is_skew_alarming(None) is False
    # exactly at the 60fps frame boundary (~16.667ms) is NOT alarming; just past it IS.
    assert mvs.is_skew_alarming(16.0) is False
    assert mvs.is_skew_alarming(17.0) is True
