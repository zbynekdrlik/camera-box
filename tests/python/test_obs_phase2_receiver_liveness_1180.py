"""#1180 — unit tests for the RECEIVER-liveness verify (obs_phase2), the LIVENESS term the
2026-08-27 strih NIC-swap aftermath proved was missing from the post-connect verify: a receiver
can hold a FROZEN frame with a CORRECT name (the issue-1158 wedged-thread class), and every
name-only verify (reenforce_ndi_name read-back, heal_active_mapping's skip-correct branch) reports
a false success. The signal is a WS screenshot-diff over the SAME connection the heal already uses:
a live NDI feed is never byte-identical across the window; a wedged receiver holding one frame is.

classify_receiver_liveness is PURE (a list of GetSourceScreenshot imageData samples → verdict);
sample_receiver_liveness is the impure WS poller (best-effort, never raises).
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_liveness", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_liveness"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# --- classify_receiver_liveness (PURE) -------------------------------------------------------

def test_two_identical_samples_is_frozen():
    state, reason = obs_phase2.classify_receiver_liveness(["PNGDATA_A", "PNGDATA_A"])
    assert state == obs_phase2.LIVENESS_FROZEN
    assert reason  # non-empty explanation for the caller's loud log


def test_changing_samples_is_live():
    state, reason = obs_phase2.classify_receiver_liveness(["PNGDATA_A", "PNGDATA_B"])
    assert state == obs_phase2.LIVENESS_LIVE
    assert reason == ""


def test_three_identical_samples_is_frozen():
    state, _ = obs_phase2.classify_receiver_liveness(["X", "X", "X"])
    assert state == obs_phase2.LIVENESS_FROZEN


def test_fewer_than_two_usable_samples_is_inconclusive():
    # a single shot, or an all-failed window, cannot prove frozen — never a false FROZEN.
    assert obs_phase2.classify_receiver_liveness(["only-one"])[0] == obs_phase2.LIVENESS_INCONCLUSIVE
    assert obs_phase2.classify_receiver_liveness([])[0] == obs_phase2.LIVENESS_INCONCLUSIVE
    assert obs_phase2.classify_receiver_liveness([None, None])[0] == obs_phase2.LIVENESS_INCONCLUSIVE


def test_none_samples_dropped_but_two_identical_reals_is_frozen():
    # a transient failed shot in the middle does not upgrade a genuine frozen signal to LIVE.
    state, _ = obs_phase2.classify_receiver_liveness(["A", None, "A"])
    assert state == obs_phase2.LIVENESS_FROZEN


def test_none_dropped_and_two_differing_reals_is_live():
    state, _ = obs_phase2.classify_receiver_liveness(["A", None, "B"])
    assert state == obs_phase2.LIVENESS_LIVE


def test_liveness_constants_are_distinct():
    vals = {obs_phase2.LIVENESS_LIVE, obs_phase2.LIVENESS_FROZEN, obs_phase2.LIVENESS_INCONCLUSIVE}
    assert len(vals) == 3


# --- sample_receiver_liveness (IMPURE WS poller) --------------------------------------------

class _FakeShots:
    """Fake obs_phase2._rpc for GetSourceScreenshot: returns each queued imageData in turn.
    A queued value of None (or {}) simulates a failed/empty screenshot. Also counts sleeps
    indirectly by recording how many shots were requested."""

    def __init__(self, image_datas):
        self.image_datas = list(image_datas)
        self.calls = 0

    def __call__(self, ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        assert rtype == "GetSourceScreenshot", rtype
        i = self.calls
        self.calls += 1
        data = self.image_datas[i] if i < len(self.image_datas) else None
        return {"imageData": data} if data is not None else {}


def _no_sleep(_s):
    return None


def test_sample_receiver_liveness_frozen_over_ws(monkeypatch):
    fake = _FakeShots(["FRAME_HELD", "FRAME_HELD", "FRAME_HELD"])
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    state, _ = obs_phase2.sample_receiver_liveness(
        object(), "NDI cam1", samples=3, interval_s=0, sleep=_no_sleep)
    assert state == obs_phase2.LIVENESS_FROZEN
    assert fake.calls == 3  # it actually took the requested number of shots


def test_sample_receiver_liveness_live_over_ws(monkeypatch):
    fake = _FakeShots(["FRAME_1", "FRAME_2", "FRAME_3"])
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    state, _ = obs_phase2.sample_receiver_liveness(
        object(), "NDI cam1", samples=3, interval_s=0, sleep=_no_sleep)
    assert state == obs_phase2.LIVENESS_LIVE


def test_sample_receiver_liveness_all_screenshots_fail_is_inconclusive(monkeypatch):
    fake = _FakeShots([None, None, None])  # every GetSourceScreenshot returned nothing
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    state, _ = obs_phase2.sample_receiver_liveness(
        object(), "NDI cam1", samples=3, interval_s=0, sleep=_no_sleep)
    assert state == obs_phase2.LIVENESS_INCONCLUSIVE


def test_sample_receiver_liveness_defaults_from_module_constants(monkeypatch):
    # with no explicit samples, it uses RECEIVER_LIVENESS_SAMPLES (>=2 so a verdict is possible).
    assert obs_phase2.RECEIVER_LIVENESS_SAMPLES >= 2
    fake = _FakeShots(["Z"] * obs_phase2.RECEIVER_LIVENESS_SAMPLES)
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    state, _ = obs_phase2.sample_receiver_liveness(
        object(), "NDI cam1", interval_s=0, sleep=_no_sleep)
    assert state == obs_phase2.LIVENESS_FROZEN
    assert fake.calls == obs_phase2.RECEIVER_LIVENESS_SAMPLES
