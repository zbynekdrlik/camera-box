"""#761 -- unit tests for scripts/mv_skew_snapshot.py, the PURE decision logic of the per-camera
MV-clone-vs-main presentation-skew measurement (order-alternated paired screenshots -> painter QR
gen_ts_ns decode -> local-wall-gap (t_send) compensation -> skew median). Covers, with NO live
OBS/network/cv2:

  a. parse_payload()   -- decode `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` with CRC-32 validation.
  b. tick_map()        -- {run_id: newest gen_ts_ns} from a list of decoded QR texts.
  c. dominant_run_id() -- the universal painter run_id (present in the most screenshots).
  d. pick_common_run_id() -- a run_id common to both screenshots; honest None.
  e. skew_sample_ms()  -- the local-wall-gap compensation (the core: cancels the WS-call wall gap
                          regardless of the two scenes' asymmetric screenshot costs).
  f. skew_ms_from_samples() -- median + spread; honest None on no samples.
  g. finalize_camera_skew() -- alternating-sequence composition into one camera's result.
  h. is_skew_alarming()-- |median| > 1 frame flag.

These are pure functions -- importing the module here must NOT require cv2 / websocket / a rig.
"""
import pathlib
import sys
import zlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import mv_skew_snapshot as mvs  # noqa: E402

MS = 1_000_000  # ns per ms


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


# --------------------------------------------------------------------------- pick_common_run_id
def test_pick_common_run_id_prefers_the_preferred():
    assert mvs.pick_common_run_id({100: 5, 911003: 8}, {100: 4, 911003: 1}, preferred=100) == 100


def test_pick_common_run_id_falls_back_to_smallest_common():
    assert mvs.pick_common_run_id({55: 5, 70: 9}, {55: 4, 70: 2}, preferred=100) == 55


def test_pick_common_run_id_none_when_no_common():
    assert mvs.pick_common_run_id({1: 10}, {2: 20}, preferred=99) is None


# ------------------------------------------------- issue 1196: aux tick pair (911013) exclusion
def test_dominant_run_id_never_picks_a_reserved_id_even_when_universal():
    # The painted aux tick pair (911013) is on EVERY screenshot -- it TIES the painter on count
    # and its small id would win the tie-break, poisoning every skew sample with its constant
    # gen_ts_ns=0 (the #1159 class, recurring for painted content). The painter epoch id must win.
    maps = [{1_800_000_000: 1, 911013: 0}, {1_800_000_000: 2, 911013: 0}]
    assert mvs.dominant_run_id(maps) == 1_800_000_000
    # Burn-only / aux-only maps yield None (no non-reserved candidate), never a reserved pick.
    assert mvs.dominant_run_id([{911013: 0, 911002: 5}, {911013: 0}]) is None


def test_pick_common_run_id_never_samples_the_aux_tick_pair():
    # gen_ts_ns of the aux marks is a constant 0 -> a "skew sample" from 911013 would be pure
    # wall-gap. With only aux in common the sample is dropped honestly (None)...
    assert mvs.pick_common_run_id({911013: 0}, {911013: 0}, preferred=None) is None
    # ...and with a burn also common, the burn fallback (a real per-node render clock) still wins
    # over the aux even though 911013 would be the smaller... (911002 < 911013 anyway; pin the
    # exclusion with an id ordering where aux WOULD win min()):
    assert mvs.pick_common_run_id({911013: 0, 911002: 7}, {911013: 0, 911002: 9},
                                  preferred=None) == 911002


# --------------------------------------------------------------------------- skew_sample_ms (CORE)
def test_skew_sample_compensates_the_wall_gap():
    # True skew S = +5ms (MV presents 5ms LATER). Between the two shots the frame advanced by the
    # wall gap Delta; t_send captures that gap locally so it is added back exactly.
    S = 5 * MS
    for delta in (900 * MS, 1750 * MS, 300 * MS):
        # main captured at t0 (gen = C0), MV captured at t0+delta (gen = C0 + delta - S).
        t_send_main, t_send_mv = 0, delta
        gen_main = 1_000_000_000
        gen_mv = gen_main + delta - S
        assert abs(mvs.skew_sample_ms(gen_main, gen_mv, t_send_main, t_send_mv) - 5.0) < 1e-9


def test_skew_sample_shared_source_is_zero():
    # shared-source: MV and program draw the same texture (S=0); only the wall gap differs.
    delta = 1_750 * MS
    gen_main = 5_000_000_000
    gen_mv = gen_main + delta  # S=0 => gen advances purely by the wall gap
    assert abs(mvs.skew_sample_ms(gen_main, gen_mv, 0, delta)) < 1e-9


def test_skew_sample_sign_positive_means_mv_later():
    # MV frame OLDER than program by 20ms at equal capture times => +20ms (operator sees it later).
    assert abs(mvs.skew_sample_ms(1_000_000_000, 1_000_000_000 - 20 * MS, 100, 100) - 20.0) < 1e-9


# --------------------------------------------------------------------------- skew_ms_from_samples
def test_skew_from_samples_median_and_spread():
    out = mvs.skew_ms_from_samples([1.0, 3.0, 2.0, 100.0])  # one outlier
    assert out["median_ms"] == 2.5
    assert out["n_samples"] == 4
    assert out["min_ms"] == 1.0 and out["max_ms"] == 100.0
    assert out["stdev_ms"] is not None


def test_skew_from_samples_empty_is_honest_none():
    out = mvs.skew_ms_from_samples([])
    assert out["median_ms"] is None
    assert out["n_samples"] == 0


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


# --------------------------------------------------------------------------- finalize_camera_skew
def _cap(kind, run_id, gen_ts, t_send):
    return (kind, {run_id: gen_ts}, t_send)


def test_finalize_shared_source_regression_guard():
    # alternating main,MV,main,MV,...; true skew 0; wall gaps vary; t_send compensates each.
    P = 1786886721
    caps = []
    t = 0
    gen = 10_000_000_000
    for delta in (1000 * MS, 1750 * MS, 900 * MS, 1200 * MS):
        caps.append(_cap("main", P, gen, t))
        t += delta
        gen += delta  # shared source: content advances purely with wall time
        caps.append(_cap("mv", P, gen, t))
        t += 800 * MS
        gen += 800 * MS
    out = mvs.finalize_camera_skew(caps, preferred_run_id=P)
    assert out["n_samples"] >= 4  # both main->MV and MV->main adjacent pairs
    assert abs(out["median_ms"]) < 1e-6
    assert out["alarming"] is False
    assert out["run_id_used"] == P


def test_finalize_flags_a_real_lag():
    # MV consistently 30ms OLDER at equal-ish capture cadence => +30ms, alarming.
    P = 100
    S = 30 * MS
    caps = []
    t = 0
    genm = 5_000_000_000
    for _ in range(4):
        caps.append(_cap("main", P, genm, t))
        t += 100 * MS
        genm += 100 * MS
        # MV shows content 30ms older than a program-frame captured at this same instant
        caps.append(_cap("mv", P, genm - S, t))
        t += 100 * MS
        genm += 100 * MS
    out = mvs.finalize_camera_skew(caps, preferred_run_id=P)
    assert abs(out["median_ms"] - 30.0) < 1e-6
    assert out["alarming"] is True


def test_finalize_drops_pairs_with_no_common_tick():
    P = 100
    caps = [
        _cap("main", P, 5_000_000_000, 0),
        _cap("mv", P, 5_000_000_000, 0),      # usable (skew 0)
        ("main", {}, 100 * MS),               # undecodable -> both adjacent pairs dropped
        _cap("mv", P, 5_000_000_000, 200 * MS),
    ]
    out = mvs.finalize_camera_skew(caps, preferred_run_id=P)
    assert out["n_samples"] == 1
    assert abs(out["median_ms"]) < 1e-6


def test_finalize_all_undecodable_is_honest_none():
    caps = [("main", {}, 0), ("mv", {}, MS), ("main", {}, 2 * MS)]
    out = mvs.finalize_camera_skew(caps, preferred_run_id=100)
    assert out["median_ms"] is None
    assert out["n_samples"] == 0
    assert out["alarming"] is False
