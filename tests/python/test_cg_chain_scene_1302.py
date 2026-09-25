"""#1302 — unit tests for scripts/cg_chain_scene.py, the CG_CHAIN=1 scene helper.

The CG_CHAIN E2E profile (scripts/lib/cg-chain-e2e.sh) needs two OBS scene changes, both undone in
cleanup():
  - cg OBS (RESOLUME-SNV): cut program to the scene carrying the SongPlayer output (``sp-fast``)
    before its StartRecord, so the cg recording carries the SongPlayer burn;
  - strih: ONE tail CG window — find the scene that carries the ``CG-obs`` NDI input, show ONLY that
    item (live, the item is disabled and a browser overlay sits on top), and cut program to it.

These tests pin the PURE decisions (which scene, which item changes, what a restore does) and the
WS glue against a scripted fake ``rpc`` — no real OBS, no network.
"""
import importlib.util
import json
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "cg_chain_scene.py"
_spec = importlib.util.spec_from_file_location("cg_chain_scene_1302", _MOD_PATH)
cg_chain_scene = importlib.util.module_from_spec(_spec)
sys.modules["cg_chain_scene_1302"] = cg_chain_scene
_spec.loader.exec_module(cg_chain_scene)


def _item(item_id, name, enabled):
    return {"sceneItemId": item_id, "sourceName": name, "sceneItemEnabled": enabled}


# The live strih-lx collection shape (read 25.9.2026): CG-obs sits ONLY in "CG bridge", disabled,
# under an enabled CG-presenter browser overlay.
STRIH_ITEMS = {
    "Cam 1": [_item(1, "Cam 1", True)],
    "CG bridge": [_item(7, "CG-obs", False), _item(8, "CG-presenter", True)],
    "Ableset": [_item(3, "ableset", True)],
}


# ---- pure: which scene carries the input -------------------------------------------------------


def test_scenes_carrying_input_finds_the_only_cg_scene():
    assert cg_chain_scene.scenes_carrying_input(STRIH_ITEMS, "CG-obs") == ["CG bridge"]


def test_scenes_carrying_input_is_empty_when_no_scene_has_it():
    assert cg_chain_scene.scenes_carrying_input(STRIH_ITEMS, "nope") == []


def test_choose_scene_single_candidate():
    assert cg_chain_scene.choose_scene(["CG bridge"], "CG-obs") == "CG bridge"


def test_choose_scene_fails_loud_on_none():
    with pytest.raises(ValueError, match="CG-obs"):
        cg_chain_scene.choose_scene([], "CG-obs")


def test_choose_scene_fails_loud_on_ambiguity_without_override():
    with pytest.raises(ValueError, match="more than one"):
        cg_chain_scene.choose_scene(["A", "B"], "CG-obs")


def test_choose_scene_override_must_be_a_candidate():
    assert cg_chain_scene.choose_scene(["A", "B"], "CG-obs", override="B") == "B"
    with pytest.raises(ValueError, match="does not carry"):
        cg_chain_scene.choose_scene(["A", "B"], "CG-obs", override="C")


# ---- pure: solo plan + snapshot + restore -------------------------------------------------------


def test_solo_plan_enables_the_input_and_hides_every_other_item():
    plan = cg_chain_scene.solo_plan(STRIH_ITEMS["CG bridge"], "CG-obs")
    assert plan == [(7, True), (8, False)]


def test_solo_plan_only_lists_items_that_change():
    items = [_item(7, "CG-obs", True), _item(8, "CG-presenter", False)]
    assert cg_chain_scene.solo_plan(items, "CG-obs") == []


def test_solo_plan_fails_loud_when_the_input_is_not_in_the_scene():
    with pytest.raises(ValueError, match="CG-obs"):
        cg_chain_scene.solo_plan([_item(1, "x", True)], "CG-obs")


def test_snapshot_items_keeps_every_items_enabled_state():
    assert cg_chain_scene.snapshot_items(STRIH_ITEMS["CG bridge"]) == [
        {"id": 7, "enabled": False},
        {"id": 8, "enabled": True},
    ]


def test_restore_calls_put_program_back_then_items():
    state = {
        "host": "10.77.9.202",
        "scene": "CG bridge",
        "prev_program": "Cam 1",
        "items": [{"id": 7, "enabled": False}, {"id": 8, "enabled": True}],
    }
    assert cg_chain_scene.restore_calls(state) == [
        ("SetCurrentProgramScene", {"sceneName": "Cam 1"}),
        ("SetSceneItemEnabled", {"sceneName": "CG bridge", "sceneItemId": 7, "sceneItemEnabled": False}),
        ("SetSceneItemEnabled", {"sceneName": "CG bridge", "sceneItemId": 8, "sceneItemEnabled": True}),
    ]


def test_restore_calls_program_only_state_has_no_item_calls():
    state = {"host": "10.77.9.201", "scene": None, "prev_program": "sp-slow", "items": []}
    assert cg_chain_scene.restore_calls(state) == [
        ("SetCurrentProgramScene", {"sceneName": "sp-slow"}),
    ]


def test_restore_calls_without_a_previous_program_skips_the_cut():
    state = {"host": "h", "scene": None, "prev_program": "", "items": []}
    assert cg_chain_scene.restore_calls(state) == []


def test_restore_calls_put_the_previous_transition_back_last():
    # The cut runs under a forced Cut transition; the operator's own transition comes back LAST,
    # after the program + items are restored (still under Cut, so the restore is instant too).
    state = {"host": "h", "scene": None, "prev_program": "sp-slow", "items": [],
             "prev_transition": "Fade"}
    assert cg_chain_scene.restore_calls(state) == [
        ("SetCurrentProgramScene", {"sceneName": "sp-slow"}),
        ("SetCurrentSceneTransition", {"transitionName": "Fade"}),
    ]


def test_cut_transition_name_is_found_by_kind():
    listing = {"transitions": [
        {"transitionName": "Fade", "transitionKind": "fade_transition"},
        {"transitionName": "Strih", "transitionKind": "cut_transition"},
    ]}
    assert cg_chain_scene.cut_transition_name(listing) == "Strih"
    with pytest.raises(ValueError, match="cut"):
        cg_chain_scene.cut_transition_name({"transitions": [
            {"transitionName": "Fade", "transitionKind": "fade_transition"}]})


# ---- WS glue against a scripted fake rpc --------------------------------------------------------


class FakeObs:
    """A tiny scripted OBS: scene list, per-scene items, current program + transition. Records
    every call."""

    def __init__(self, items_by_scene, program, transition="Fade"):
        self.items = {k: [dict(i) for i in v] for k, v in items_by_scene.items()}
        self.program = program
        self.transition = transition
        self.calls = []

    def __call__(self, rtype, rdata=None):
        rdata = rdata or {}
        self.calls.append((rtype, rdata))
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.items]}
        if rtype == "GetSceneItemList":
            return {"sceneItems": [dict(i) for i in self.items[rdata["sceneName"]]]}
        if rtype == "GetCurrentProgramScene":
            return {"currentProgramSceneName": self.program}
        if rtype == "GetSceneTransitionList":
            return {"transitions": [
                {"transitionName": "Fade", "transitionKind": "fade_transition"},
                {"transitionName": "Cut", "transitionKind": "cut_transition"},
            ]}
        if rtype == "GetCurrentSceneTransition":
            return {"transitionName": self.transition}
        if rtype == "SetCurrentSceneTransition":
            self.transition = rdata["transitionName"]
            return {}
        if rtype == "SetCurrentProgramScene":
            if rdata["sceneName"] not in self.items:
                raise RuntimeError("no such scene")
            # A program cut under anything but Cut would blend the old program into the new one.
            assert self.transition == "Cut", "the program cut must run under the Cut transition"
            self.program = rdata["sceneName"]
            return {}
        if rtype == "SetSceneItemEnabled":
            for i in self.items[rdata["sceneName"]]:
                if i["sceneItemId"] == rdata["sceneItemId"]:
                    i["sceneItemEnabled"] = rdata["sceneItemEnabled"]
            return {}
        raise AssertionError(f"unexpected request {rtype}")


def test_strih_solo_writes_the_state_before_mutating_then_cuts(tmp_path):
    obs = FakeObs(STRIH_ITEMS, "Cam 1")
    state_path = tmp_path / "strih.json"
    seen = []
    scene = cg_chain_scene.strih_solo(
        obs, "10.77.9.202", "CG-obs", "", str(state_path), after_cut=seen.append)
    assert scene == "CG bridge"
    assert obs.program == "CG bridge"
    enabled = {i["sourceName"]: i["sceneItemEnabled"] for i in obs.items["CG bridge"]}
    assert enabled == {"CG-obs": True, "CG-presenter": False}
    state = json.loads(state_path.read_text())
    assert state == {
        "host": "10.77.9.202",
        "scene": "CG bridge",
        "prev_program": "Cam 1",
        "prev_transition": "Fade",
        "items": [{"id": 7, "enabled": False}, {"id": 8, "enabled": True}],
    }
    # The program cut is the LAST mutation (the recording sees a clean CG frame from the cut on),
    # and the non-black check runs on the cut scene right after it.
    mutations = [c for c in obs.calls if c[0].startswith("Set")]
    assert mutations[-1] == ("SetCurrentProgramScene", {"sceneName": "CG bridge"})
    assert seen == ["CG bridge"]


def test_strih_solo_leaves_obs_untouched_when_no_scene_carries_the_input(tmp_path):
    obs = FakeObs({"Cam 1": [_item(1, "Cam 1", True)]}, "Cam 1")
    with pytest.raises(ValueError):
        cg_chain_scene.strih_solo(obs, "h", "CG-obs", "", str(tmp_path / "s.json"))
    assert not [c for c in obs.calls if c[0].startswith("Set")]
    assert not (tmp_path / "s.json").exists()


def test_program_select_snapshots_previous_program_and_transition(tmp_path):
    obs = FakeObs({"sp-slow": [], "sp-fast": []}, "sp-slow")
    state_path = tmp_path / "cg.json"
    cg_chain_scene.program_select(obs, "10.77.9.201", "sp-fast", str(state_path))
    assert obs.program == "sp-fast"
    assert json.loads(state_path.read_text()) == {
        "host": "10.77.9.201",
        "scene": None,
        "prev_program": "sp-slow",
        "prev_transition": "Fade",
        "items": [],
    }


def test_program_select_fails_loud_on_a_missing_scene(tmp_path):
    obs = FakeObs({"sp-slow": []}, "sp-slow")
    with pytest.raises(ValueError, match="sp-fast"):
        cg_chain_scene.program_select(obs, "h", "sp-fast", str(tmp_path / "cg.json"))
    assert obs.program == "sp-slow"
    assert obs.transition == "Fade"
    assert not (tmp_path / "cg.json").exists()


def test_restore_round_trip_puts_everything_back_and_is_idempotent(tmp_path):
    obs = FakeObs(STRIH_ITEMS, "Cam 1")
    state_path = tmp_path / "strih.json"
    cg_chain_scene.strih_solo(obs, "h", "CG-obs", "", str(state_path))
    assert cg_chain_scene.restore(lambda host: obs, str(state_path)) is True
    assert obs.program == "Cam 1"
    assert obs.transition == "Fade"
    enabled = {i["sourceName"]: i["sceneItemEnabled"] for i in obs.items["CG bridge"]}
    assert enabled == {"CG-obs": False, "CG-presenter": True}
    # The state file is retired, so a second cleanup() pass is a no-op (never re-applies an old
    # snapshot over a later operator change).
    assert not state_path.exists()
    before = len(obs.calls)
    assert cg_chain_scene.restore(lambda host: obs, str(state_path)) is False
    assert len(obs.calls) == before


# ---- CLI wiring + WS session lifecycle -----------------------------------------------------------


def test_cli_parses_the_three_subcommands():
    ap = cg_chain_scene.build_parser()
    a = ap.parse_args(["strih-solo", "--host", "h", "--input", "CG-obs", "--state-file", "s"])
    assert (a.cmd, a.host, a.input, a.scene, a.state_file) == ("strih-solo", "h", "CG-obs", "", "s")
    a = ap.parse_args(["program", "--host", "h", "--scene", "sp-fast", "--state-file", "s"])
    assert (a.cmd, a.scene) == ("program", "sp-fast")
    a = ap.parse_args(["restore", "--state-file", "s"])
    assert (a.cmd, a.state_file) == ("restore", "s")


def test_main_closes_every_ws_session_it_opens(tmp_path, monkeypatch):
    obs = FakeObs({"sp-slow": [], "sp-fast": []}, "sp-slow")
    closed = []

    class FakeWs:
        def close(self):
            closed.append(True)

    monkeypatch.setattr(cg_chain_scene, "_ws_session", lambda host: (FakeWs(), obs))
    rc = cg_chain_scene.main(
        ["program", "--host", "h", "--scene", "sp-fast", "--state-file", str(tmp_path / "cg.json")])
    assert rc == 0
    assert closed == [True]
    rc = cg_chain_scene.main(["restore", "--state-file", str(tmp_path / "cg.json")])
    assert rc == 0
    assert closed == [True, True]
    # A failing command still closes its session.
    rc = cg_chain_scene.main(
        ["program", "--host", "h", "--scene", "nope", "--state-file", str(tmp_path / "x.json")])
    assert rc == 2
    assert closed == [True, True, True]
