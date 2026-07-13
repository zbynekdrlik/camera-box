"""#747 — unit tests for scripts/warm_cam_scenes.py.

Companion to the frozen-camera-gate.py per-source warm-up (#747): this script cycles EVERY
strih 'Cam N' scene onto PREVIEW briefly, right before [5/8] StartRecord, so the [6/8]
ALL_CAMBOX sweep's very first program cut to each camera is not a cold DistroAV receiver
connect (post-#730/#508 Multiview decoupling removed the last always-on surface for these
raw main inputs).
"""
import importlib.util
from pathlib import Path

import pytest

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "warm_cam_scenes",
        SCRIPTS / "warm_cam_scenes.py",
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


# ---------------------------------------------------------------------------
# cam_scenes — pure filter+sort of a strih GetSceneList result down to real 'Cam N' scenes
# ---------------------------------------------------------------------------

class TestCamScenes:
    def test_filters_to_cam_n_scenes_only(self):
        scenes = ["Cam 1", "Multiview", "Cam 3", "Control", "Cam 2"]
        assert _mod.cam_scenes(scenes) == ["Cam 1", "Cam 2", "Cam 3"]

    def test_sorts_numerically_not_lexically(self):
        # Lexical sort would put 'Cam 10' before 'Cam 2' -- must sort by the NUMBER.
        scenes = ["Cam 10", "Cam 2", "Cam 1"]
        assert _mod.cam_scenes(scenes) == ["Cam 1", "Cam 2", "Cam 10"]

    def test_empty_when_no_cam_scenes(self):
        assert _mod.cam_scenes(["Multiview", "Control", "PHASE2-PROBE"]) == []

    def test_ignores_near_miss_names(self):
        # 'MV Cam N' (the #730 low-bandwidth twins) and anything not an exact 'Cam N'
        # match must NOT be treated as a real camera scene here.
        assert _mod.cam_scenes(["MV Cam 1", "Camera 2", "Cam1", "Cam 5"]) == ["Cam 5"]


# ---------------------------------------------------------------------------
# warm_all — cycle each scene onto PREVIEW, settle, restore the original preview after
# ---------------------------------------------------------------------------

def _fake_rpc(calls, studio=True, orig_preview="Multiview"):
    def rpc(ws, rtype, rdata=None, ignore_err=False):
        calls.append((rtype, rdata))
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": studio}
        if rtype == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": orig_preview}
        return {}
    return rpc


def test_warm_all_cycles_each_scene_and_restores_original(monkeypatch):
    calls = []
    monkeypatch.setattr(_mod.op, "_rpc", _fake_rpc(calls, studio=True, orig_preview="Multiview"))
    sleeps = []
    monkeypatch.setattr(_mod.time, "sleep", lambda s: sleeps.append(s))

    warmed = _mod.warm_all(ws=object(), scenes=["Cam 1", "Cam 2"], settle_s=1.5)

    assert warmed == ["Cam 1", "Cam 2"]
    preview_calls = [c for c in calls if c[0] == "SetCurrentPreviewScene"]
    assert preview_calls == [
        ("SetCurrentPreviewScene", {"sceneName": "Cam 1"}),
        ("SetCurrentPreviewScene", {"sceneName": "Cam 2"}),
        ("SetCurrentPreviewScene", {"sceneName": "Multiview"}),  # restore, last
    ]
    assert sleeps == [1.5, 1.5]


def test_warm_all_noop_when_studio_off(monkeypatch):
    calls = []

    def rpc(ws, rtype, rdata=None, ignore_err=False):
        calls.append((rtype, rdata))
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": False}
        raise AssertionError(f"unexpected RPC {rtype!r} when Studio Mode is off")

    monkeypatch.setattr(_mod.op, "_rpc", rpc)
    monkeypatch.setattr(_mod.time, "sleep", lambda s: None)

    warmed = _mod.warm_all(ws=object(), scenes=["Cam 1"], settle_s=1.5)
    assert warmed == []
    assert [c for c in calls if c[0] == "SetCurrentPreviewScene"] == []


def test_warm_all_noop_when_no_scenes_makes_zero_rpc_calls(monkeypatch):
    calls = []
    monkeypatch.setattr(_mod.op, "_rpc", lambda *a, **k: calls.append(a) or {})

    warmed = _mod.warm_all(ws=object(), scenes=[], settle_s=1.0)

    assert warmed == []
    assert calls == [], "no scenes to warm -- must not even check Studio Mode"


def test_warm_all_restores_even_when_a_mid_loop_scene_errors(monkeypatch):
    # SetCurrentPreviewScene calls are ignore_err=True at the RPC layer in real use, but the
    # restore in `finally` must still fire even if something in the loop body raises.
    calls = []

    def rpc(ws, rtype, rdata=None, ignore_err=False):
        calls.append((rtype, rdata))
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": True}
        if rtype == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": "Multiview"}
        if rtype == "SetCurrentPreviewScene" and rdata == {"sceneName": "Cam 2"}:
            raise RuntimeError("simulated ws failure")
        return {}

    monkeypatch.setattr(_mod.op, "_rpc", rpc)
    monkeypatch.setattr(_mod.time, "sleep", lambda s: None)

    with pytest.raises(RuntimeError, match="simulated ws failure"):
        _mod.warm_all(ws=object(), scenes=["Cam 1", "Cam 2"], settle_s=1.0)

    preview_calls = [c for c in calls if c[0] == "SetCurrentPreviewScene"]
    assert preview_calls[-1] == ("SetCurrentPreviewScene", {"sceneName": "Multiview"}), (
        "the original preview must be restored even when a mid-loop warm call raises"
    )
