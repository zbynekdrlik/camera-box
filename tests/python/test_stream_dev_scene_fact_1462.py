"""issue 1462 (24.10.2026, design 6379124377) -- the stream box's Development design is a box FACT.

PR 1472's first Full-path E2E on the Linux stream box failed at `[4b3/8]` (POLLUTED, cluster 2 < 4):
stream-snv's Development carried BOTH designs -- the issue-1380 SNV design (the nested `PRO`, so the program
carries `mbc` and the cam2 QPSK marker the E2E measures; the E2E re-nests it) and the stream-pp YouTube-safe
design (the measurement clip with its OWN marker, from the stream seed manifest's `dev_scene`). Two marker
streams in one program.

`STRIH_STREAM_DEV_SCENE` = `nested-pro` (stream-snv) | `clip` (stream-pp) | `none` (strih role):
  * only `clip` gets the manifest's `dev_scene` and setup step 6-devclip;
  * `nested-pro` gets `dev_scene_nested` -- its seed removes a leftover clip item from Development (never
    PRO, never another scene) and its verdict FAILs a clip there.
"""
import importlib.util
import json
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
BOXES = SCRIPTS / "strih-boxes"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


SDS = _load("stream_dev_safe_fact_1462", SCRIPTS / "stream_dev_safe.py")


@pytest.fixture
def boxes(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    return d


def _bash(body, boxes):
    env = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_") and k != "OBS_FLEET"}
    env["STRIH_BOXES_DIR"] = str(boxes)
    script = ('set -uo pipefail\n. scripts/lib/strih-box-facts.sh\n. scripts/lib/strih-provision.sh\n'
              '. scripts/lib/obs-box-role.sh\n' + body)
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=env, cwd=str(REPO),
                          timeout=60)


# --- the fact ---------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("box,want", [("stream-snv", "nested-pro"), ("stream-pp", "clip"),
                                      ("strih-snv", "none"), ("strih-pp", "none")])
def test_every_box_declares_its_development_design(boxes, box, want):
    r = _bash("strih_box_load %s || exit 9\nstrih_lx_stream_dev_scene" % box, boxes)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == want


@pytest.mark.parametrize("box,old,new,word", [
    ("stream-snv", "STRIH_STREAM_DEV_SCENE=nested-pro", "STRIH_STREAM_DEV_SCENE=both", "nested-pro, clip"),
    ("stream-snv", "STRIH_STREAM_DEV_SCENE=nested-pro", "STRIH_STREAM_DEV_SCENE=none", "stream role"),
    ("strih-snv", "STRIH_STREAM_DEV_SCENE=none", "STRIH_STREAM_DEV_SCENE=clip", "strih role"),
])
def test_the_loader_refuses_a_wrong_design(boxes, box, old, new, word):
    f = boxes / ("%s.env" % box)
    text = f.read_text()
    assert old in text
    f.write_text(text.replace(old, new))
    r = _bash("strih_box_load %s" % box, boxes)
    assert r.returncode != 0
    assert "STRIH_STREAM_DEV_SCENE" in r.stderr and word in r.stderr, r.stderr


# --- the manifest + the clip step -----------------------------------------------------------------------------

def _manifest(boxes, box):
    r = _bash("strih_box_load %s || exit 9\nobs_box_seed_manifest_json" % box, boxes)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def test_stream_snv_gets_the_nested_design_and_no_clip(boxes):
    m = _manifest(boxes, "stream-snv")
    assert "dev_scene" not in m
    assert m["dev_scene_nested"] == {"scene": "Development", "production": "PRO", "clip_input": "Merací klip"}


def test_stream_pp_keeps_the_clip_design(boxes):
    m = _manifest(boxes, "stream-pp")
    assert m["dev_scene"]["audio_input"] == "Merací klip"
    assert "dev_scene_nested" not in m


def test_the_clip_step_runs_only_for_the_clip_design(boxes):
    for box, applies in (("stream-pp", True), ("stream-snv", False), ("strih-snv", False)):
        r = _bash("strih_box_load %s || exit 9\nobs_box_step_applies 6-devclip" % box, boxes)
        assert (r.returncode == 0) is applies, (box, r.stdout, r.stderr)


# --- the nested design's seed + verdict -----------------------------------------------------------------------

NESTED = {"scene": "Development", "production": "PRO", "clip_input": "Merací klip"}


class FakeObs:
    def __init__(self, scenes):
        self.scenes = {s: [dict(i) for i in items] for s, items in scenes.items()}
        self.calls = []

    def req(self, t, d=None, ignore_err=False):
        d = d or {}
        self.calls.append((t, dict(d)))
        if t == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.scenes]}
        if t == "GetSceneItemList":
            return {"sceneItems": [dict(i) for i in self.scenes.get(d["sceneName"], [])]}
        if t == "RemoveSceneItem":
            self.scenes[d["sceneName"]] = [i for i in self.scenes[d["sceneName"]]
                                           if i["sceneItemId"] != d["sceneItemId"]]
        return {}


def _item(name, item_id):
    return {"sourceName": name, "sceneItemId": item_id, "sceneItemEnabled": True, "sceneItemIndex": item_id}


def _live():
    """stream-snv after the failed run: the clip AND the nested PRO in Development."""
    return FakeObs({"Development": [_item("Merací klip", 3), _item("NDI 2ME PGM", 4), _item("PRO", 5)],
                    "PRO": [_item("NDI 2ME PGM", 1), _item("mbc", 2)]})


def test_the_nested_seed_removes_only_the_clip_from_development():
    obs = _live()
    plan = SDS.apply_dev_nested(obs, NESTED)
    assert plan == ["remove_item Merací klip"]
    assert [i["sourceName"] for i in obs.scenes["Development"]] == ["NDI 2ME PGM", "PRO"]
    assert [i["sourceName"] for i in obs.scenes["PRO"]] == ["NDI 2ME PGM", "mbc"]
    writes = [(t, d) for t, d in obs.calls if not t.startswith("Get")]
    assert writes == [("RemoveSceneItem", {"sceneName": "Development", "sceneItemId": 3})]


def test_the_nested_seed_never_adds_pro():
    obs = FakeObs({"Development": [_item("NDI 2ME PGM", 4)], "PRO": [_item("mbc", 2)]})
    assert SDS.apply_dev_nested(obs, NESTED) == []
    assert [t for t, _ in obs.calls if not t.startswith("Get")] == []


def test_the_nested_verdict():
    rows = SDS.dev_nested_verdict(["PRO", "Development"],
                                  [_item("Merací klip", 3), _item("PRO", 5)], NESTED)
    assert any(lvl == "FAIL" and "Merací klip" in text for lvl, text in rows), rows
    rows = SDS.dev_nested_verdict(["PRO", "Development"], [_item("NDI 2ME PGM", 4), _item("PRO", 5)], NESTED)
    assert [lvl for lvl, _ in rows] == ["PASS"], rows
    rows = SDS.dev_nested_verdict(["PRO", "Development"], [_item("NDI 2ME PGM", 4)], NESTED)
    assert [lvl for lvl, _ in rows] == ["NOTE"], rows
    rows = SDS.dev_nested_verdict(["PRO"], None, NESTED)
    assert [lvl for lvl, _ in rows] == ["FAIL"], rows


def test_the_bootstrap_dispatches_on_the_manifest_key():
    text = (SCRIPTS / "strih_scenes.py").read_text()
    assert '"dev_scene_nested"' in text and "apply_dev_nested" in text
