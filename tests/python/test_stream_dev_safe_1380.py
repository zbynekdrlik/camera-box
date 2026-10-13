"""issue 1380 (ROZHODNUTE 13.10.2026, owner answer "1") -- the YouTube-SAFE `Development` scene of a
stream-ROLE box (stream-pp).

On SNV the Development scene nests the production scene `PRO`. On stream-pp `PRO` holds the live
mixer (`Zvuk z mixu`, continuous program audio, likely music), and an OBS mute is per input, never per
scene, so nesting `PRO` would put the mixer on YouTube. The stream role's Development therefore holds
exactly two items: the program video input (`NDI 2ME PGM`, muted) and the measurement clip
(`scripts/gen_measurement_clip.py`) as a looping local media source under it.

These tests pin, with fakes (no live OBS):
  * `parse_dev_scene` reads the manifest key and refuses a malformed one;
  * `dev_safe_plan` (fresh box, scene present, an operator-disabled item kept, the production scene
    never added, the clip always under the video);
  * `dev_safe_verdict` FAILs every unsafe shape and PASSes the clean one;
  * `apply_dev_safe` writes only to the development scene, never to `PRO`, never selects a scene;
  * the stream manifest carries the key, the strih manifest does not;
  * setup-strih's stream-only gate `6-devclip` and verify-strih's stream branch call the libs.
"""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MOD_PATH = _ROOT / "scripts" / "stream_dev_safe.py"
_spec = importlib.util.spec_from_file_location("stream_dev_safe_1380", _MOD_PATH)
sds = importlib.util.module_from_spec(_spec)
sys.modules["stream_dev_safe_1380"] = sds
_spec.loader.exec_module(sds)

SPEC = {
    "scene": "Development",
    "video_input": "NDI 2ME PGM",
    "audio_input": "Merací klip",
    "audio_file": "/opt/camera-box/measurement-clip-v1.mp4",
}
PROD = "PRO"
MIX = "Zvuk z mixu"


def _item(name, enabled=True, item_id=1, index=0):
    return {"sourceName": name, "sceneItemEnabled": enabled, "sceneItemId": item_id,
            "sceneItemIndex": index}


# ---- parse_dev_scene ---------------------------------------------------------------------------

def test_parse_dev_scene_reads_the_manifest_key():
    text = json.dumps({"mode": "update-only", "inputs": [], "dev_scene": SPEC})
    assert sds.parse_dev_scene(text) == SPEC


def test_parse_dev_scene_absent_key_is_none():
    assert sds.parse_dev_scene(json.dumps({"mode": "update-only", "inputs": []})) is None


@pytest.mark.parametrize("missing", ["scene", "video_input", "audio_input", "audio_file"])
def test_parse_dev_scene_refuses_a_missing_field(missing):
    spec = dict(SPEC)
    spec[missing] = ""
    with pytest.raises(sds.DevSafeError):
        sds.parse_dev_scene(json.dumps({"dev_scene": spec}))


def test_parse_dev_scene_refuses_the_production_scene_as_development():
    spec = dict(SPEC, scene=PROD)
    with pytest.raises(sds.DevSafeError):
        sds.parse_dev_scene(json.dumps({"dev_scene": spec}))


# ---- dev_safe_plan -----------------------------------------------------------------------------

def test_plan_fresh_box_creates_scene_clip_then_video():
    plan = sds.dev_safe_plan(["PRE", PROD, "POST"], [SPEC["video_input"], MIX], None, SPEC)
    assert plan == ["create_scene", "create_clip_input", "add_video_item"]


def test_plan_clip_input_exists_elsewhere_is_added_as_an_item_not_recreated():
    plan = sds.dev_safe_plan([PROD], [SPEC["video_input"], SPEC["audio_input"]], None, SPEC)
    assert plan == ["create_scene", "add_clip_item", "add_video_item"]


def test_plan_complete_scene_is_a_no_op():
    items = [_item(SPEC["audio_input"], index=0), _item(SPEC["video_input"], item_id=2, index=1)]
    plan = sds.dev_safe_plan([PROD, "Development"], [SPEC["video_input"], SPEC["audio_input"]],
                             items, SPEC)
    assert plan == []


def test_plan_operator_disabled_item_is_left_alone():
    items = [_item(SPEC["audio_input"], enabled=False), _item(SPEC["video_input"], item_id=2, index=1)]
    plan = sds.dev_safe_plan([PROD, "Development"], [SPEC["video_input"], SPEC["audio_input"]],
                             items, SPEC)
    assert plan == []


def test_plan_clip_added_under_an_existing_video_item_is_lowered():
    items = [_item(SPEC["video_input"])]
    plan = sds.dev_safe_plan([PROD, "Development"], [SPEC["video_input"]], items, SPEC)
    assert plan == ["create_clip_input", "lower_clip_item"]


def test_plan_never_adds_the_production_scene_or_the_mixer():
    plan = sds.dev_safe_plan([PROD], [SPEC["video_input"], MIX], None, SPEC)
    assert not any(PROD in a or MIX in a for a in plan)


def test_plan_refuses_when_the_video_input_does_not_exist():
    with pytest.raises(sds.DevSafeError):
        sds.dev_safe_plan([PROD], [MIX], None, SPEC)


# ---- dev_safe_verdict --------------------------------------------------------------------------

def _clean_items():
    return [_item(SPEC["audio_input"], index=0), _item(SPEC["video_input"], item_id=2, index=1)]


def _levels(rows):
    return [lvl for lvl, _ in rows]


def test_verdict_clean_scene_passes():
    rows = sds.dev_safe_verdict(["PRE", PROD, "Development"], _clean_items(), True,
                                {"local_file": SPEC["audio_file"], "looping": True}, SPEC)
    assert "FAIL" not in _levels(rows)
    assert "PASS" in _levels(rows)


def test_verdict_missing_scene_fails():
    rows = sds.dev_safe_verdict(["PRE", PROD], None, True, None, SPEC)
    assert _levels(rows) == ["FAIL"]
    assert "Development" in rows[0][1]


@pytest.mark.parametrize("extra", [PROD, MIX, "Pozadie hudba"])
def test_verdict_any_extra_item_fails(extra):
    items = _clean_items() + [_item(extra, item_id=9, index=2)]
    rows = sds.dev_safe_verdict([PROD, "Development"], items, True,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert "FAIL" in _levels(rows)
    assert any(extra in text for lvl, text in rows if lvl == "FAIL")


def test_verdict_unmuted_video_input_fails():
    rows = sds.dev_safe_verdict([PROD, "Development"], _clean_items(), False,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert any(lvl == "FAIL" and "muted" in text for lvl, text in rows)


def test_verdict_unknown_mute_state_fails_closed():
    rows = sds.dev_safe_verdict([PROD, "Development"], _clean_items(), None,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert "FAIL" in _levels(rows)


def test_verdict_missing_or_disabled_clip_item_fails():
    only_video = [_item(SPEC["video_input"])]
    rows = sds.dev_safe_verdict([PROD, "Development"], only_video, True, None, SPEC)
    assert "FAIL" in _levels(rows)
    disabled = [_item(SPEC["audio_input"], enabled=False), _item(SPEC["video_input"], 2, 1)]
    rows = sds.dev_safe_verdict([PROD, "Development"], disabled, True,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert "FAIL" in _levels(rows)


def test_verdict_clip_pointing_at_another_file_fails():
    rows = sds.dev_safe_verdict([PROD, "Development"], _clean_items(), True,
                                {"local_file": "/home/newlevel/Music/song.mp3"}, SPEC)
    assert any(lvl == "FAIL" and "song.mp3" in text for lvl, text in rows)


def test_verdict_clip_above_the_video_fails():
    items = [_item(SPEC["video_input"], index=0), _item(SPEC["audio_input"], item_id=2, index=1)]
    rows = sds.dev_safe_verdict([PROD, "Development"], items, True,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert "FAIL" in _levels(rows)


# ---- apply_dev_safe ----------------------------------------------------------------------------

class FakeObs:
    def __init__(self, scenes, inputs, items=None):
        self.scenes = list(scenes)
        self.inputs = list(inputs)
        self.items = {s: list(v) for s, v in (items or {}).items()}
        self.calls = []

    def req(self, req_type, data=None, ignore_err=False):
        data = data or {}
        self.calls.append((req_type, dict(data)))
        if req_type == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.scenes]}
        if req_type == "GetInputList":
            return {"inputs": [{"inputName": i} for i in self.inputs]}
        if req_type == "GetSceneItemList":
            return {"sceneItems": self.items.get(data["sceneName"], [])}
        if req_type == "GetVideoSettings":
            return {"baseWidth": 1920, "baseHeight": 1080}
        if req_type == "CreateScene":
            self.scenes.append(data["sceneName"])
            self.items[data["sceneName"]] = []
            return {}
        if req_type == "CreateInput":
            self.inputs.append(data["inputName"])
            lst = self.items.setdefault(data["sceneName"], [])
            lst.append(_item(data["inputName"], item_id=100 + len(lst), index=len(lst)))
            return {"sceneItemId": 100 + len(lst) - 1}
        if req_type == "CreateSceneItem":
            lst = self.items.setdefault(data["sceneName"], [])
            lst.append(_item(data["sourceName"], item_id=200 + len(lst), index=len(lst)))
            return {"sceneItemId": 200 + len(lst) - 1}
        if req_type == "GetSceneItemId":
            for it in self.items.get(data["sceneName"], []):
                if it["sourceName"] == data["sourceName"]:
                    return {"sceneItemId": it["sceneItemId"]}
            return {}
        return {}


def _writes(obs):
    return [(t, d) for t, d in obs.calls if not t.startswith("Get")]


def test_apply_fresh_box_creates_the_scene_and_writes_only_there():
    obs = FakeObs(["PRE", PROD], [SPEC["video_input"], MIX])
    plan = sds.apply_dev_safe(obs, SPEC)
    assert plan == ["create_scene", "create_clip_input", "add_video_item"]
    for req_type, data in _writes(obs):
        assert data.get("sceneName") in (None, SPEC["scene"]), (req_type, data)
        assert PROD not in json.dumps(data, ensure_ascii=False)
        assert MIX not in json.dumps(data, ensure_ascii=False)
        assert req_type not in ("SetCurrentProgramScene", "SetCurrentPreviewScene",
                                "SetInputMute", "RemoveInput", "RemoveSceneItem")
    create = [d for t, d in obs.calls if t == "CreateInput"][0]
    assert create["inputKind"] == "ffmpeg_source"
    assert create["inputSettings"]["local_file"] == SPEC["audio_file"]
    assert create["inputSettings"]["looping"] is True
    names = [it["sourceName"] for it in obs.items[SPEC["scene"]]]
    assert names == [SPEC["audio_input"], SPEC["video_input"]]


def test_apply_complete_scene_writes_nothing():
    items = {SPEC["scene"]: _clean_items()}
    obs = FakeObs([PROD, SPEC["scene"]], [SPEC["video_input"], SPEC["audio_input"]], items)
    assert sds.apply_dev_safe(obs, SPEC) == []
    assert _writes(obs) == []


def test_apply_lowers_a_clip_added_over_an_existing_video_item():
    obs = FakeObs([PROD, SPEC["scene"]], [SPEC["video_input"]],
                  {SPEC["scene"]: [_item(SPEC["video_input"])]})
    sds.apply_dev_safe(obs, SPEC)
    lowered = [d for t, d in obs.calls if t == "SetSceneItemIndex"]
    assert lowered and lowered[0]["sceneItemIndex"] == 0
    assert lowered[0]["sceneName"] == SPEC["scene"]


# ---- the manifest + the provisioning wiring ----------------------------------------------------

def _bash(script):
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, cwd=_ROOT,
                          env=dict(os.environ, LC_ALL="C.UTF-8"))


def _manifest(box):
    r = _bash(f'set -euo pipefail; . scripts/lib/strih-box-facts.sh; strih_box_load {box}; '
              f'. scripts/lib/strih-provision.sh; . scripts/lib/obs-box-role.sh; '
              f'obs_box_seed_manifest_json')
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def test_stream_manifest_carries_the_safe_dev_scene():
    m = _manifest("stream-pp")
    assert m["dev_scene"] == SPEC
    assert sds.parse_dev_scene(json.dumps(m)) == SPEC


def test_strih_manifest_has_no_dev_scene():
    assert "dev_scene" not in _manifest("strih-pp")


def test_step_6_devclip_applies_only_to_the_stream_role():
    for box, applies in (("stream-pp", True), ("strih-pp", False)):
        r = _bash(f'. scripts/lib/strih-box-facts.sh; strih_box_load {box}; '
                  f'. scripts/lib/strih-provision.sh; . scripts/lib/obs-box-role.sh; '
                  f'obs_box_step_applies 6-devclip')
        assert (r.returncode == 0) is applies, (box, r.stdout, r.stderr)


def test_setup_and_verify_call_the_stream_dev_libs():
    setup = (_ROOT / "scripts" / "setup-strih.sh").read_text()
    verify = (_ROOT / "scripts" / "verify-strih.sh").read_text()
    assert "obs_box_step_applies 6-devclip" in setup
    assert "obs_box_stream_dev_clip_provision" in setup
    assert "stream_dev_safe.py" in setup
    assert "obs_box_stream_role_grade" in verify
    assert len(verify.splitlines()) <= 1000


def test_strih_scenes_bootstrap_seeds_the_dev_scene_only_with_the_key():
    src = (_ROOT / "scripts" / "strih_scenes.py").read_text()
    assert "stream_dev_safe" in src
    assert "parse_dev_scene" in src


def test_stream_role_dev_scene_name_is_the_shared_development_scene_name():
    r = _bash('. scripts/lib/stream-dev-scene.sh; . scripts/lib/obs-box-role.sh; '
              'printf "%s|%s" "$STREAM_DEV_SCENE_DEFAULT" "$OBS_BOX_STREAM_DEV_SCENE"')
    shared, role = r.stdout.split("|")
    assert shared == role == SPEC["scene"]
