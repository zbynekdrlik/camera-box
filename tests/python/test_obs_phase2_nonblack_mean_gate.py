"""#312 — unit test: the non-black self-check must reject a high-peak / near-black-MEAN frame.

## The bug (#312 runs 312006/312007)

After cam1's camera-box is restarted in step [2/8], its NDI source is mid-renegotiation when step
[4/8] routes the strih program to 'Cam 5' 0.1 s later. OBS renders a renegotiating frame that is
almost entirely black with a few moderately-bright pixels: luma **peak ~117 but mean ~2.7**. The
self-check gated ONLY on peak (`_luma_is_black` == peak 0), so peak 117 passed instantly as
"NON-BLACK" and the run recorded a black, all-undecodable program (then cascaded to the #328 stream
timeout). In the healthy 312005 run the same scene read mean ~105.

## The fix this test locks

The non-black check gains an optional MEAN floor (`min_mean`). When set (>0), a frame whose mean is
below the floor is treated as not-yet-non-black, so the existing POLL keeps WAITING (it does not
falsely pass) until cam1's NDI settles and delivers the real bright frame (mean ~105) — a
deterministic settle, not a fixed sleep. `min_mean=0` (the default) preserves the original peak-only
behavior for a legitimately dark-but-live prod camera. The proof self-check applies the floor via
`OBS_NONBLACK_MIN_MEAN` (default 20): well above the 2.7 renegotiation garbage, well below the ~105
bright QR-monitor frame.
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"


def _load(modname="obs_phase2_meangate"):
    spec = importlib.util.spec_from_file_location(modname, _MOD_PATH)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[modname] = mod
    spec.loader.exec_module(mod)
    return mod


obs_phase2 = _load()


def test_luma_is_black_mean_floor_rejects_high_peak_low_mean():
    # The exact 312006/312007 renegotiation frame: peak 117, mean 2.7. With a mean floor it is BLACK.
    assert obs_phase2._luma_is_black(luma_max=117, luma_mean=2.7, min_mean=20) is True
    # The healthy settled frame (mean ~105) is NOT black even though peak is the same.
    assert obs_phase2._luma_is_black(luma_max=117, luma_mean=105.0, min_mean=20) is False


def test_luma_is_black_default_min_mean_preserves_peak_only():
    # min_mean=0 (default) must NOT regress the dark-but-live behavior: a low-mean but non-zero-peak
    # frame is still non-black (a legitimately dark live camera carries a decodable signal).
    assert obs_phase2._luma_is_black(luma_max=117, luma_mean=2.7, min_mean=0) is False
    assert obs_phase2._luma_is_black(luma_max=117, luma_mean=2.7) is False
    assert obs_phase2._luma_is_black(luma_max=0, luma_mean=0.0, min_mean=20) is True


def test_blackcheck_verdict_waits_then_fails_on_low_mean():
    # A high-peak/low-mean renegotiation frame must keep the poll WAITING (not falsely OK), then
    # BLACK at the deadline — so the run never records the renegotiation garbage.
    assert obs_phase2._blackcheck_verdict(
        luma_max=117, elapsed_s=0.1, timeout_s=20.0, luma_mean=2.7, min_mean=20) == "WAIT"
    assert obs_phase2._blackcheck_verdict(
        luma_max=117, elapsed_s=21.0, timeout_s=20.0, luma_mean=2.7, min_mean=20) == "BLACK"
    # Once cam1 settles (mean ~105) the same poll passes immediately.
    assert obs_phase2._blackcheck_verdict(
        luma_max=117, elapsed_s=0.1, timeout_s=20.0, luma_mean=105.0, min_mean=20) == "OK"
