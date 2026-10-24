"""issue 1462 (24.10.2026, design 6377006564) -- the ported stream-snv collection's leftovers.

stream-snv's collection came from the Windows SNV stream box, whose `Development` nested `PRO`. Live after
the 24.10 deploy, with `Development` on the program in Studio Mode:
  * `Development` still held `PRO` and the pulse capture `Zvuk z mixu`: the create-only seeder never
    removes an item, so the production scene rendered on the stream program;
  * the launch seed's `RemoveInput 'Zvuk z mixu'` changed nothing. libobs removes a removed source's scene
    items only when that scene RENDERS, and in Studio Mode the program renders a private duplicate, so the
    real `Development` never prunes and its item keeps the input alive.

The fake below models that libobs behaviour: RemoveInput only marks an input removed; the input stays
listed while any scene item still references it, and goes when its last item is removed.
"""
import importlib.util
import pathlib

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1462", SCRIPTS / "program_audio_vban.py")
SDS = _load("stream_dev_safe_1462", SCRIPTS / "stream_dev_safe.py")

SPEC = {"scene": "Development", "video_input": "NDI 2ME PGM", "audio_input": "Merací klip",
        "audio_file": "/opt/camera-box/measurement-clip-v1.mp4"}


class StudioModeObs:
    """An OBS whose scenes never render (Studio Mode's program is a private duplicate)."""

    def __init__(self, inputs, scenes):
        self.inputs = dict(inputs)                       # name -> (kind, settings)
        self.scenes = {s: [dict(i) for i in items] for s, items in scenes.items()}
        self.removed = set()
        self.calls = []

    def _referenced(self, name):
        return any(i["sourceName"] == name for items in self.scenes.values() for i in items)

    def _drop_if_free(self, name):
        if name in self.removed and not self._referenced(name):
            self.inputs.pop(name, None)

    def req(self, t, d=None, ignore_err=False):
        d = d or {}
        self.calls.append((t, dict(d)))
        if t == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": k} for n, (k, _s) in self.inputs.items()]}
        if t == "GetSpecialInputs":
            return {k: None for k in ("desktop1", "desktop2", "mic1", "mic2", "mic3", "mic4")}
        if t == "GetInputSettings":
            return {"inputSettings": self.inputs[d["inputName"]][1]}
        if t == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.scenes]}
        if t == "GetSceneItemList":
            return {"sceneItems": [dict(i) for i in self.scenes.get(d["sceneName"], [])]}
        if t == "RemoveInput":
            self.removed.add(d["inputName"])
            self._drop_if_free(d["inputName"])
            return {}
        if t == "RemoveSceneItem":
            items = self.scenes[d["sceneName"]]
            gone = [i for i in items if i["sceneItemId"] == d["sceneItemId"]]
            self.scenes[d["sceneName"]] = [i for i in items if i["sceneItemId"] != d["sceneItemId"]]
            for i in gone:
                self._drop_if_free(i["sourceName"])
            return {}
        return {}

    def writes(self):
        return [(t, d) for t, d in self.calls if not t.startswith("Get")]


def _item(name, item_id, index, enabled=True):
    return {"sourceName": name, "sceneItemId": item_id, "sceneItemIndex": index, "sceneItemEnabled": enabled}


def _live_snv():
    """The stream-snv state read live on 24.10.2026 01:40."""
    inputs = {"mbc": ("net.nagater.obs-vban.source", {}),
              "Zvuk z mixu": ("pulse_input_capture", {"device_id": "strih-program-source"}),
              "NDI 2ME PGM": ("ndi_source", {}),
              "Merací klip": ("ffmpeg_source", {"local_file": SPEC["audio_file"]})}
    scenes = {"PRO": [_item("NDI 2ME PGM", 1, 0), _item("mbc", 2, 1)],
              "Development": [_item("PRO", 1, 0), _item("Zvuk z mixu", 2, 1, enabled=False),
                              _item("Merací klip", 3, 2), _item("NDI 2ME PGM", 4, 3)]}
    return StudioModeObs(inputs, scenes)


# --- the pulse capture seed ------------------------------------------------------------------------------------

def test_a_pulse_capture_referenced_by_an_unrendered_scene_is_really_removed():
    obs = _live_snv()
    status = PAV.drop_pulse_capture_inputs(obs, keep="mbc", sleep=lambda s: None)
    assert "Zvuk z mixu" not in obs.inputs, (status, obs.calls)
    assert "removed 'Zvuk z mixu' (pulse_input_capture on strih-program-source)" in status, status
    assert not any(i["sourceName"] == "Zvuk z mixu" for i in obs.scenes["Development"])


def test_the_pulse_seed_removes_only_the_items_of_that_input():
    obs = _live_snv()
    PAV.drop_pulse_capture_inputs(obs, keep="mbc", sleep=lambda s: None)
    removed_items = [d for t, d in obs.writes() if t == "RemoveSceneItem"]
    assert removed_items == [{"sceneName": "Development", "sceneItemId": 2}], removed_items
    assert [i["sourceName"] for i in obs.scenes["PRO"]] == ["NDI 2ME PGM", "mbc"]


# --- the development-scene seed ---------------------------------------------------------------------------------

def test_the_dev_scene_seed_removes_the_nested_production_scene_and_the_mixer():
    obs = _live_snv()
    plan = SDS.apply_dev_safe(obs, SPEC)
    names = sorted(i["sourceName"] for i in obs.scenes["Development"])
    assert names == ["Merací klip", "NDI 2ME PGM"], (plan, names)
    assert "remove_item PRO" in plan and "remove_item Zvuk z mixu" in plan, plan


def test_the_dev_scene_seed_never_touches_another_scene_or_the_program():
    obs = _live_snv()
    SDS.apply_dev_safe(obs, SPEC)
    for t, d in obs.writes():
        assert t not in ("SetCurrentProgramScene", "SetCurrentPreviewScene", "RemoveInput", "RemoveScene"), (t, d)
        assert d.get("sceneName") == SPEC["scene"], (t, d)
    assert [i["sourceName"] for i in obs.scenes["PRO"]] == ["NDI 2ME PGM", "mbc"]


def test_after_the_seed_the_verdict_has_no_scene_membership_failure():
    obs = _live_snv()
    SDS.apply_dev_safe(obs, SPEC)
    rows = SDS.dev_safe_verdict(list(obs.scenes), obs.scenes["Development"], True,
                                {"local_file": SPEC["audio_file"]}, SPEC)
    assert not any(lvl == "FAIL" and "holds" in text for lvl, text in rows), rows


def test_a_clean_dev_scene_gets_no_removal():
    obs = _live_snv()
    obs.scenes["Development"] = [_item("Merací klip", 3, 0), _item("NDI 2ME PGM", 4, 1)]
    assert SDS.apply_dev_safe(obs, SPEC) == []
    assert obs.writes() == []
