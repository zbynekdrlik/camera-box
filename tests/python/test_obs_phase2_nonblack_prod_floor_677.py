"""#677 — unit tests: prod_scene()'s non-black self-check must use a LOOSER mean floor than
switch()'s #312 dual-QR-monitor gate, so a legitimately dim (but real) production scene is not
misclassified as BLACK.

## The bug (#677)

_assert_program_nonblack's shared min_mean floor (env OBS_NONBLACK_MIN_MEAN, default 20) is tuned
for the #312 bright dual-QR test monitor (settled mean ~105). But prod_scene() (called on every
recording-e2e.sh [4/8] step, and by CI's full-path-e2e.yml gate) routes to whatever the CERTIFIED
production scene shows — a real camera view, which can legitimately be dim. Live repro
(2026-07-11, reproducing #627): prod scene 'PRO' read peak=231, mean=18.0 — clearly non-black
content (peak 231 out of 255, a healthy recording followed) — but mean 18.0 < the shared floor of
20, so the #163 self-check falsely aborted BEFORE StartRecord with "renders BLACK" 3/3 times.

## The fix this test locks

prod_scene() now passes its OWN, looser mean floor (env OBS_NONBLACK_MIN_MEAN_PROD, default 5) to
_assert_program_nonblack — well above the ~2.7 mid-renegotiation garbage frame the floor exists to
reject (see #312/test_obs_phase2_nonblack_mean_gate.py), well below the 18.0 dim-but-real repro
frame. switch() (#312's all-cambox sweep, which targets the bright dual-QR monitor specifically)
is UNCHANGED — it keeps the strict default (env OBS_NONBLACK_MIN_MEAN, default 20) by omitting the
min_mean override entirely, so _assert_program_nonblack resolves it internally exactly as before.
"""
import importlib.util
import pathlib
import sys
import types

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_677", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_677"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


class FakeWS:
    """Minimal websocket stand-in (prod_scene/switch call ws.close() in their finally)."""

    def close(self):
        pass


def _fake_rpc_factory(curr_prog, scenes):
    def fake_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
        if op == "GetCurrentProgramScene":
            return {"currentProgramSceneName": curr_prog}
        if op == "GetStudioModeEnabled":
            return {"studioModeEnabled": False}
        if op == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": curr_prog}
        if op == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in scenes]}
        if op == "GetOutputSettings":
            return {"outputSettings": {"ndi_name": "PROG (FAKED)"}}
        # CreateScene, SetCurrentProgramScene, SetCurrentPreviewScene, etc. -> no-op {}
        return {}
    return fake_rpc


def _prod_scene_args(target):
    a = types.SimpleNamespace()
    a.host = "10.77.9.204"
    a.password = ""
    a.program_scene = target
    a.ensure_source = ""
    a.upstream = ""
    a.test_preload = 1
    return a


def _patch_prod_scene_side_effects(monkeypatch, spy_assert):
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: {})
    monkeypatch.setattr(obs_phase2, "_save_state", lambda state: None)
    monkeypatch.setattr(obs_phase2, "_force_test_preload",
                        lambda ws, host, upstream, tp, state: None)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", spy_assert)
    monkeypatch.delenv("OBS_NONBLACK_MIN_MEAN_PROD", raising=False)


def test_prod_scene_passes_looser_mean_floor_than_312_switch_default(monkeypatch):
    """#677: prod_scene()'s non-black check must pass an explicit min_mean well below the
    #312-tuned default of 20 — so a real dim scene (mean 18.0, the live repro) is not
    misclassified as black, while still rejecting the mid-renegotiation garbage (mean 2.7)."""
    target = "PRO"
    fake_rpc = _fake_rpc_factory(curr_prog="OTHER", scenes=[target, "OTHER"])
    calls = []

    def spy_assert(ws, host, scene, label, black_hint, min_mean=None):
        calls.append(min_mean)

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_prod_scene_side_effects(monkeypatch, spy_assert)

    obs_phase2.prod_scene(_prod_scene_args(target))

    assert len(calls) == 1, f"expected exactly one _assert_program_nonblack call, got {calls}"
    min_mean = calls[0]
    assert min_mean is not None, "prod_scene must pass an explicit min_mean override"
    assert min_mean < 18.0, (
        f"prod_scene's mean floor ({min_mean}) must be BELOW the live #677 repro mean "
        f"(18.0) so a real dim scene is not misclassified as black"
    )
    assert min_mean > 2.7, (
        f"prod_scene's mean floor ({min_mean}) must stay ABOVE the mid-renegotiation "
        f"garbage-frame mean (2.7, see #312/test_obs_phase2_nonblack_mean_gate.py) so it "
        f"still rejects a genuinely dead/renegotiating source"
    )


def test_prod_scene_mean_floor_env_overridable(monkeypatch):
    """The prod floor must be tunable via OBS_NONBLACK_MIN_MEAN_PROD (mirrors the existing
    OBS_NONBLACK_MIN_MEAN pattern for #312's switch())."""
    target = "PRO"
    fake_rpc = _fake_rpc_factory(curr_prog="OTHER", scenes=[target, "OTHER"])
    calls = []

    def spy_assert(ws, host, scene, label, black_hint, min_mean=None):
        calls.append(min_mean)

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    _patch_prod_scene_side_effects(monkeypatch, spy_assert)
    monkeypatch.setenv("OBS_NONBLACK_MIN_MEAN_PROD", "7.5")

    obs_phase2.prod_scene(_prod_scene_args(target))

    assert calls == [7.5]


def test_switch_keeps_312_strict_default_floor_regression(monkeypatch):
    """Regression: switch() (#312 dual-QR sweep) must NOT be affected by the #677 fix — it
    keeps the strict default (no min_mean override -> _assert_program_nonblack resolves it
    internally via OBS_NONBLACK_MIN_MEAN, default 20, exactly as before)."""
    calls = []

    def spy_assert(ws, host, scene, label, black_hint, min_mean=None):
        calls.append(min_mean)

    fake_rpc = _fake_rpc_factory(curr_prog="Cam 5", scenes=["Cam 5"])
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pwd: FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", spy_assert)

    a = types.SimpleNamespace(host="10.77.9.202", password="", program_scene="Cam 5")
    obs_phase2.switch(a)

    assert calls == [None], (
        "switch() must call _assert_program_nonblack with NO min_mean override, so it keeps "
        "resolving the strict #312 default internally"
    )


def test_luma_is_black_677_repro_frame_passes_prod_floor_fails_312_floor():
    """Direct pure-function proof of the #677 repro: peak=231, mean=18.0 (the real dim scene
    captured live on 'PRO') is BLACK under the #312 floor (20) but NON-BLACK under a looser
    prod floor (5) — the exact discrimination the fix relies on."""
    assert obs_phase2._luma_is_black(luma_max=231, luma_mean=18.0, min_mean=20) is True, (
        "documents the #677 bug: the shared #312-tuned floor misclassifies real dim content"
    )
    assert obs_phase2._luma_is_black(luma_max=231, luma_mean=18.0, min_mean=5) is False, (
        "the fix: a looser prod floor correctly classifies it as non-black"
    )
