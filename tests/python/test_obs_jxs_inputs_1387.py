"""issue 1387 -- scripts/obs_jxs_inputs.py: the JPEG XS inputs a box's facts declare, seeded at every OBS
launch (strih-obs-start.sh) and graded by verify-strih item 49.

Driven with a fake obs-websocket `rpc` (no network): a matching input is a pure read, a drifted one gets
ONE overlay of the differing keys, an absent one is created in its own holding scene (never an operator
scene), an input of another kind under the declared name is never touched, and genlock_burn is never
written. The kind and the setting keys are pinned to the shipped DistroAV source.
"""
import importlib.util
import json
import re
import socket
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
MOD_PATH = REPO / "scripts" / "obs_jxs_inputs.py"
SOURCE = REPO / "vendor" / "distroav" / "src" / "jxs-source.cpp"

_spec = importlib.util.spec_from_file_location("obs_jxs_inputs", MOD_PATH)
oji = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(oji)

CAM2 = {"name": "JXS cam2 src", "scene": "JXS cam2",
        "settings": {"jxs_port": 7002, "jxs_camera": 2, "jxs_max_bpp": 0, "genlock_fifo": True,
                     "genlock_latency_ms_src": 36, "genlock_grid": True}}
PGM = {"name": "JXS PGM src", "scene": "JXS PGM",
       "settings": {"jxs_port": 7100, "jxs_camera": 100, "jxs_max_bpp": 0, "genlock_fifo": True,
                    "genlock_latency_ms_src": 1087, "genlock_grid": False}}
# The kind's defaults as the shipped jxs-source.cpp getdefaults sets them (GetInputDefaultSettings).
DEFAULTS = {"jxs_port": 7000, "jxs_camera": 0, "jxs_max_bpp": 0, "genlock_fifo": True,
            "genlock_latency_ms_src": 3, "genlock_burn": False, "genlock_grid": True}


def _manifest(*ents):
    return json.dumps({"issue": 1387, "inputs": list(ents)})


class FakeObs:
    """A fake OBS: inputs {name: (kind, explicit settings)}, scenes {scene: [(source, enabled)]}. Logs every
    request; a write mutates the state the way obs-websocket does."""

    def __init__(self, inputs=None, scenes=None):
        self.inputs = {k: (v[0], dict(v[1])) for k, v in (inputs or {}).items()}
        self.scenes = {k: list(v) for k, v in (scenes or {}).items()}
        self.log = []

    def rpc(self, req, data=None, ignore_err=False):
        data = data or {}
        self.log.append((req, json.loads(json.dumps(data))))
        if req == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": k} for n, (k, _) in self.inputs.items()]}
        if req == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.scenes]}
        if req == "GetSceneItemList":
            return {"sceneItems": [{"sourceName": s, "sceneItemEnabled": e}
                                   for s, e in self.scenes.get(data["sceneName"], [])]}
        if req == "GetInputSettings":
            if data["inputName"] not in self.inputs:
                return {}
            return {"inputSettings": dict(self.inputs[data["inputName"]][1])}
        if req == "GetInputDefaultSettings":
            return {"defaultInputSettings": dict(DEFAULTS)} if data["inputKind"] == oji.INPUT_KIND else {}
        if req == "CreateScene":
            assert data["sceneName"] not in self.scenes
            self.scenes[data["sceneName"]] = []
            return {}
        if req == "CreateInput":
            assert data["inputName"] not in self.inputs
            self.inputs[data["inputName"]] = (data["inputKind"], dict(data["inputSettings"]))
            self.scenes[data["sceneName"]].append((data["inputName"], data.get("sceneItemEnabled", True)))
            return {}
        if req == "SetInputSettings":
            kind, st = self.inputs[data["inputName"]]
            assert data.get("overlay") is True
            st.update(data["inputSettings"])
            return {}
        raise AssertionError(f"unexpected request {req}")

    def writes(self):
        return [r for r in self.log if not r[0].startswith("Get")]


def _live_cam2(**over):
    # the live strih-snv shape of 31.10.2026: grid left at its default (not stored)
    st = {"genlock_burn": False, "genlock_fifo": True, "genlock_latency_ms_src": 36, "jxs_camera": 2,
          "jxs_max_bpp": 0, "jxs_port": 7002}
    st.update(over)
    return st


# --- the manifest -------------------------------------------------------------------------------------

def test_a_valid_manifest_parses_in_order():
    specs = oji.parse_manifest(_manifest(CAM2, PGM))
    assert [s.name for s in specs] == ["JXS cam2 src", "JXS PGM src"]
    assert specs[1].settings["genlock_grid"] is False and specs[0].scene == "JXS cam2"


@pytest.mark.parametrize("bad, why", [
    ("not json", "not JSON"),
    ("[]", "inputs"),
    (json.dumps({"inputs": [dict(CAM2, scene="JXS cam2 src")]}), "differ"),
    (json.dumps({"inputs": [dict(CAM2, name="")]}), "non-empty"),
    (json.dumps({"inputs": [dict(CAM2, settings=dict(CAM2["settings"], genlock_burn=True))]}), "exactly"),
    (json.dumps({"inputs": [dict(CAM2, settings=dict(CAM2["settings"], jxs_port="7002"))]}), "integer"),
    (json.dumps({"inputs": [dict(CAM2, settings=dict(CAM2["settings"], jxs_port=True))]}), "integer"),
    (json.dumps({"inputs": [dict(CAM2, settings=dict(CAM2["settings"], genlock_grid=1))]}), "true or false"),
    (json.dumps({"inputs": [CAM2, CAM2]}), "twice"),
    (json.dumps({"inputs": [CAM2, dict(PGM, settings=dict(PGM["settings"], jxs_port=7002))]}), "twice"),
])
def test_a_bad_manifest_is_refused_by_name(bad, why):
    with pytest.raises(oji.ManifestError) as e:
        oji.parse_manifest(bad)
    assert why in str(e.value)


# --- the decision -------------------------------------------------------------------------------------

def test_setting_diffs_reads_types_strictly():
    want = CAM2["settings"]
    eff = {**DEFAULTS, **_live_cam2()}
    assert oji.setting_diffs(eff, want) == []
    assert oji.setting_diffs({**eff, "genlock_latency_ms_src": 36.0}, want) == []
    assert oji.setting_diffs({**eff, "genlock_latency_ms_src": 3}, want) == [("genlock_latency_ms_src", 3, 36)]
    assert oji.setting_diffs({**eff, "genlock_grid": 1}, want) == [("genlock_grid", 1, True)]
    assert oji.setting_diffs({**eff, "jxs_port": "7002"}, want) == [("jxs_port", "7002", 7002)]
    missing = dict(eff)
    del missing["jxs_camera"]
    assert oji.setting_diffs(missing, want) == [("jxs_camera", None, 2)]


def test_plan_names_create_refuse_update_ok():
    spec = oji.parse_manifest(_manifest(CAM2))[0]
    eff = {**DEFAULTS, **_live_cam2()}
    assert oji.plan_input(spec, {}, {}) == ("create", None)
    assert oji.plan_input(spec, {"JXS cam2 src": "ndi_source"}, {}) == ("refuse", "ndi_source")
    assert oji.plan_input(spec, {"JXS cam2 src": oji.INPUT_KIND}, eff) == ("ok", None)
    assert oji.plan_input(spec, {"JXS cam2 src": oji.INPUT_KIND}, {**eff, "jxs_port": 7000})[0] == "update"


# --- --bootstrap ----------------------------------------------------------------------------------------

def test_the_live_strih_state_is_a_pure_read():
    obs = FakeObs({"JXS cam2 src": (oji.INPUT_KIND, _live_cam2())},
                  {"Cam 2": [("JXS cam2 src", True), ("NDI cam2", False)]})
    lines, ok = oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(CAM2)))
    assert ok and obs.writes() == []
    assert "matches its declared settings" in lines[0]


def test_a_drifted_input_gets_one_overlay_of_the_differing_keys_only():
    # the saved collection's pin 3 + a hand grid-off: healed, nothing else written
    obs = FakeObs({"JXS cam2 src": (oji.INPUT_KIND, _live_cam2(genlock_latency_ms_src=3, genlock_grid=False))},
                  {"Cam 2": [("JXS cam2 src", True)]})
    lines, ok = oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(CAM2)))
    assert ok
    assert obs.writes() == [("SetInputSettings", {"inputName": "JXS cam2 src", "overlay": True,
                                                  "inputSettings": {"genlock_latency_ms_src": 36,
                                                                    "genlock_grid": True}})]
    assert "genlock_latency_ms_src 3 -> 36" in lines[0]
    assert obs.inputs["JXS cam2 src"][1]["genlock_burn"] is False  # never written


def test_an_absent_input_is_created_in_its_own_holding_scene_never_an_operator_scene():
    obs = FakeObs({}, {"Cam 2": [("NDI cam2", True)], "PRO": [("NDI 2ME PGM", True)]})
    lines, ok = oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(CAM2, PGM)))
    assert ok
    assert [r[0] for r in obs.writes()] == ["CreateScene", "CreateInput", "CreateScene", "CreateInput"]
    assert obs.scenes["Cam 2"] == [("NDI cam2", True)] and obs.scenes["PRO"] == [("NDI 2ME PGM", True)]
    assert obs.scenes["JXS cam2"] == [("JXS cam2 src", True)] and obs.scenes["JXS PGM"] == [("JXS PGM src", True)]
    kind, st = obs.inputs["JXS PGM src"]
    assert kind == oji.INPUT_KIND and st == PGM["settings"] and "genlock_burn" not in st
    assert all("own scene" in ln for ln in lines)


def test_an_existing_holding_scene_is_reused():
    obs = FakeObs({}, {"JXS cam2": []})
    oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(CAM2)))
    assert [r[0] for r in obs.writes()] == ["CreateInput"]


def test_another_kind_under_the_declared_name_is_never_touched():
    obs = FakeObs({"JXS cam2 src": ("ndi_source", {"ndi_source_name": "CAM2 (usb)"})}, {"Cam 2": []})
    lines, ok = oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(CAM2)))
    assert not ok and obs.writes() == []
    assert "REFUSED" in lines[0] and "ndi_source" in lines[0]


def test_no_write_ever_names_genlock_burn_or_a_production_scene():
    obs = FakeObs({"JXS PGM src": (oji.INPUT_KIND, {"jxs_port": 7000, "genlock_burn": True})},
                  {"PRO": [("JXS PGM src", True)], "Development": [("PRO", True)]})
    oji.bootstrap(obs.rpc, oji.parse_manifest(_manifest(PGM)))
    for req, data in obs.writes():
        assert "genlock_burn" not in json.dumps(data)
        assert data.get("sceneName") not in ("PRO", "Development")
    assert obs.inputs["JXS PGM src"][1]["genlock_burn"] is True  # a TEST-mode burn is the sweeps' business


# --- --verify ------------------------------------------------------------------------------------------

def test_verify_passes_the_live_state_and_names_the_scenes():
    obs = FakeObs({"JXS cam2 src": (oji.INPUT_KIND, _live_cam2())},
                  {"Cam 2": [("JXS cam2 src", True), ("NDI cam2", False)], "MULTIVIEW": [("JXS cam2 src", True)],
                   "Cam 4": [("NDI cam4", True)], "Old": [("JXS cam2 src", False)]})
    rows = oji.verify_rows(obs.rpc, oji.parse_manifest(_manifest(CAM2)))
    assert rows[0] == ("PASS", "(jxs-inputs) 'JXS cam2 src': port 7002 camera 2 pin 36 ms grid on "
                               "genlock_fifo on no limit")
    assert rows[1][0] == "NOTE" and "Cam 2, MULTIVIEW" in rows[1][1] and "Old" not in rows[1][1]
    assert obs.writes() == []


def test_verify_fails_a_missing_a_drifted_and_a_wrong_kind_input():
    obs = FakeObs({"JXS cam2 src": (oji.INPUT_KIND, _live_cam2(genlock_latency_ms_src=3)),
                   "JXS PGM src": ("ffmpeg_source", {})}, {})
    cam3 = dict(CAM2, name="JXS cam3 src", scene="JXS cam3", settings=dict(CAM2["settings"], jxs_port=7003, jxs_camera=3))
    rows = oji.verify_rows(obs.rpc, oji.parse_manifest(_manifest(CAM2, PGM, cam3)))
    fails = [t for lvl, t in rows if lvl == "FAIL"]
    assert len(fails) == 3
    assert "genlock_latency_ms_src 3 != 36" in fails[0]
    assert "ffmpeg_source" in fails[1]
    assert "'JXS cam3 src' is missing" in fails[2] and "'JXS cam3'" in fails[2]
    assert obs.writes() == []


# --- the CLI -------------------------------------------------------------------------------------------

def _closed_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_cli_without_a_manifest_does_nothing(tmp_path, capsys):
    m = str(tmp_path / "absent.json")
    assert oji.main(["--verify", "--manifest", m]) == 0
    assert capsys.readouterr().out.startswith("NOTE (jxs-inputs) this box declares no JPEG XS input")
    assert oji.main(["--bootstrap", "--manifest", m]) == 0


def test_cli_a_bad_manifest_fails_verify_and_bootstrap(tmp_path, capsys):
    m = tmp_path / "jxs-seed.json"
    m.write_text("{}")
    assert oji.main(["--verify", "--manifest", str(m)]) == 0
    assert capsys.readouterr().out.startswith("FAIL (jxs-inputs) the manifest")
    assert oji.main(["--bootstrap", "--manifest", str(m)]) == 2


def test_cli_with_obs_down_notes_verify_and_fails_bootstrap(tmp_path, capsys):
    m = tmp_path / "jxs-seed.json"
    m.write_text(_manifest(CAM2))
    port = str(_closed_port())
    assert oji.main(["--verify", "--manifest", str(m), "--port", port]) == 0
    assert "NOTE (jxs-inputs) OBS WebSocket 127.0.0.1:" in capsys.readouterr().out
    assert oji.main(["--bootstrap", "--manifest", str(m), "--port", port]) == 1


# --- pinned to the shipped DistroAV source --------------------------------------------------------------

def test_the_kind_and_every_declared_key_are_the_shipped_sources():
    src = SOURCE.read_text()
    assert f'#define JXS_SOURCE_ID "{oji.INPUT_KIND}"' in src
    for key in oji.DECLARED_KEYS:
        assert re.search(r'#define PROP_\w+ "%s"' % re.escape(key), src), key
    # the defaults this test's fake OBS answers with are the shipped getdefaults
    assert "obs_data_set_default_bool(settings, PROP_GENLOCK_GRID, true);" in src
    assert "obs_data_set_default_int(settings, PROP_JXS_MAX_BPP, 0);" in src
