"""issue 1317 -- unit tests for scripts/strih_scenes.py (the strih-lx OBS input/scene/Studio-Mode
seeder) + static anchors on the wiring in strih-obs-start.sh and setup-strih.sh.

Tier-0 runnable (pytest on the pure helpers + script-text asserts; no cargo, no rig). The seeder
reuses the on-box obs_phase2 primitives; here only the PURE helpers (parse_seed_manifest /
seed_inputs / certified_genlock_settings / scene_order / input_parity_problems) are exercised.
"""
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location("strih_scenes", SCRIPTS / "strih_scenes.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()

# The real manifest shape setup-strih.sh step 6 writes from strih_lx_ndi_inputs /
# strih_lx_ndi_outputs + strih_lx_ndi_republishes / strih_lx_camera_latency_ms.
_TEN_INPUTS = [
    "CAM1 (usb)", "CAM2 (usb)", "CAM3 (usb)", "CAM4 (usb)",
    "CAM5 (usb)", "CAM6 (usb)", "CAM7 (usb)",
    "STRIH-SNV (2ME PGM)", "STRIH-SNV (2ME PVW)", "RESOLUME-SNV (cg-obs)",
]
_FIVE_OUTPUTS = [
    "STRIH-LX (2ME PGM)", "STRIH-LX (2ME PVW)",
    "STRIH-LX (interkom)", "STRIH-LX (MULTIVIEW)", "STRIH-LX (Grading)",
]
_REAL_MANIFEST = json.dumps(
    {"inputs": _TEN_INPUTS, "outputs": _FIVE_OUTPUTS, "camera_latency_ms": 3}
)

# issue 1317 (live finding 19.9.2026): the camera class carries the manifest floor as the REAL per-source
# genlock ms knob `genlock_latency_ms_src`, NOT the stock DistroAV `latency` receive-buffer MODE enum --
# the genlock build's certified coercion forces `latency` back to 0 (NORMAL) on every genlock_fifo
# input, so a seeded `latency: 3` read back 0 forever and --bootstrap re-wrote + name-re-enforced all
# 8 camera-class inputs on EVERY launch (never the pure read the update-only path promised).
_CERTIFIED = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "genlock_latency_ms_src": 3}
# issue 1317: the two per-class settings dicts. camera = the certified genlock baseline; feedback =
# the Windows strih `light.json` 2ME-input mode (ndi_sync=1 SOURCE_TIMING, latency=1), pinned with
# genlock_fifo=False EXPLICITLY (not omitted) so a SetInputSettings(overlay=True) heal actually clears
# a mis-seeded genlock_fifo=True off an already-existing input.
_CAMERA = _CERTIFIED
_FEEDBACK = {"ndi_bw_mode": 0, "genlock_fifo": False, "ndi_sync": 1, "latency": 1}
_CAMERA_INPUTS = [
    "CAM1 (usb)", "CAM2 (usb)", "CAM3 (usb)", "CAM4 (usb)",
    "CAM5 (usb)", "CAM6 (usb)", "CAM7 (usb)", "RESOLUME-SNV (cg-obs)",
]
_FEEDBACK_INPUTS = ["STRIH-SNV (2ME PGM)", "STRIH-SNV (2ME PVW)"]


# --- parse_seed_manifest --------------------------------------------------------------------------

def test_parse_seed_manifest_real_shape():
    inputs, outputs, latency = _mod.parse_seed_manifest(_REAL_MANIFEST)
    assert inputs == _TEN_INPUTS
    assert len(inputs) == 10
    assert outputs == _FIVE_OUTPUTS
    assert len(outputs) == 5
    assert latency == 3


def test_parse_seed_manifest_latency_default_on_missing():
    inputs, outputs, latency = _mod.parse_seed_manifest(json.dumps({"inputs": _TEN_INPUTS}))
    assert inputs == _TEN_INPUTS
    assert outputs == []
    assert latency == _mod.DEFAULT_CAMERA_LATENCY_MS == 3


def test_parse_seed_manifest_latency_default_on_garbage():
    _, _, latency = _mod.parse_seed_manifest(
        json.dumps({"inputs": [], "camera_latency_ms": "nope"})
    )
    assert latency == 3


def test_parse_seed_manifest_rejects_non_object():
    with pytest.raises(ValueError):
        _mod.parse_seed_manifest(json.dumps([1, 2, 3]))
    with pytest.raises(json.JSONDecodeError):
        _mod.parse_seed_manifest("not json {")


def test_parse_seed_manifest_drops_empty_and_nonstring_entries():
    inputs, outputs, _ = _mod.parse_seed_manifest(
        json.dumps({"inputs": ["CAM1 (usb)", "", None, 7, "CAM2 (usb)"], "outputs": [""]})
    )
    assert inputs == ["CAM1 (usb)", "CAM2 (usb)"]
    assert outputs == []


# --- certified_genlock_settings -------------------------------------------------------------------

def test_certified_genlock_settings_is_the_locked_baseline():
    assert _mod.certified_genlock_settings(3) == _CERTIFIED
    # the manifest floor rides the REAL genlock ms knob, never the stock `latency` mode enum
    assert _mod.certified_genlock_settings(5)["genlock_latency_ms_src"] == 5
    assert "latency" not in _mod.certified_genlock_settings(5)
    # fresh dict each call (never a shared mutable default)
    a = _mod.certified_genlock_settings(3)
    a["genlock_fifo"] = False
    assert _mod.certified_genlock_settings(3)["genlock_fifo"] is True


# --- seed_inputs ----------------------------------------------------------------------------------

def test_seed_inputs_emits_class_settings_and_names_per_input():
    # issue 1317: the settings dict is now per CLASS (camera vs feedback), not one uniform genlock
    # dict; the scene/input naming + top-level ndi_source_name shape is unchanged.
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    assert len(plan) == 10
    for item, src in zip(plan, _TEN_INPUTS):
        assert item["ndi_source_name"] == src
        assert item["scene"] == src                     # per-input scene = the source display name
        assert item["input"] == "NDI " + src            # input name kept DISTINCT from the scene
        want = _FEEDBACK if src in _FEEDBACK_INPUTS else _CAMERA
        assert item["settings"] == want
        # ndi_source_name is a TOP-LEVEL field, not inside the settings dict
        assert "ndi_source_name" not in item["settings"]


def test_seed_inputs_idempotency_shape_no_double_create():
    # a manifest that repeats a name must never yield two scenes/inputs for it
    plan = _mod.seed_inputs(["CAM1 (usb)", "CAM1 (usb)", "CAM2 (usb)", ""], 3)
    scenes = [p["scene"] for p in plan]
    assert scenes == ["CAM1 (usb)", "CAM2 (usb)"]
    assert len(plan) == 2


def test_scene_order_is_stable_deduped_manifest_order():
    assert _mod.scene_order(_TEN_INPUTS) == _TEN_INPUTS
    assert _mod.scene_order(["A", "A", "B"]) == ["A", "B"]


# --- input_parity_problems ------------------------------------------------------------------------

def _actual_from_plan(plan):
    """Build a healthy GetInputSettings-shaped {inputName: settings} from a plan -- genlock_fifo and
    ndi_sync taken from each item's OWN class settings (a feedback input is healthy at genlock_fifo
    False / ndi_sync 1, a camera at True / 2)."""
    return {
        item["input"]: {
            "ndi_source_name": item["ndi_source_name"],
            "genlock_fifo": bool(item["settings"].get("genlock_fifo")),
            "ndi_sync": item["settings"]["ndi_sync"],
        }
        for item in plan
    }


def test_input_parity_problems_all_present_is_empty():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    assert _mod.input_parity_problems(_actual_from_plan(plan), plan) == []


def test_input_parity_problems_flags_missing_input():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    actual = _actual_from_plan(plan)
    del actual["NDI CAM3 (usb)"]
    problems = _mod.input_parity_problems(actual, plan)
    assert any("MISSING" in p and "NDI CAM3 (usb)" in p for p in problems)


def test_input_parity_problems_flags_non_genlock_and_wrong_source():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    actual = _actual_from_plan(plan)
    actual["NDI CAM1 (usb)"] = {"ndi_source_name": "WRONG", "genlock_fifo": False, "ndi_sync": 2}
    problems = _mod.input_parity_problems(actual, plan)
    assert any("not genlock_fifo" in p for p in problems)
    assert any("ndi_source_name" in p and "WRONG" in p for p in problems)


def test_input_parity_problems_flags_wrong_ndi_sync():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    actual = _actual_from_plan(plan)
    # a source that decodes but on ndi_sync=1 (the #149 timing bug) must be flagged
    actual["NDI CAM2 (usb)"] = {"ndi_source_name": "CAM2 (usb)", "genlock_fifo": True, "ndi_sync": 1}
    problems = _mod.input_parity_problems(actual, plan)
    assert any("ndi_sync" in p and "NDI CAM2 (usb)" in p for p in problems)


# --- CLI safety: an explicit mode is required (no silent-mutation bare invocation) ----------------

def test_bare_invocation_requires_a_mode_and_never_touches_obs():
    # ap.error fires BEFORE any manifest read or WS connect, so this exercises the safety guard with
    # no rig: a bare `strih_scenes.py` must exit non-zero and NOT seed anything.
    r = subprocess.run(
        [sys.executable, str(SCRIPTS / "strih_scenes.py")],
        capture_output=True, text=True,
    )
    assert r.returncode != 0
    assert "specify a mode" in r.stderr.lower()


# --- static anchors: the launch-seed wiring -------------------------------------------------------

def _read(name):
    return (SCRIPTS / name).read_text()


def test_strih_obs_start_preflights_import_before_launch_and_seeds_after_ws():
    s = _read("strih-obs-start.sh")
    # #1156: preflight `import strih_scenes` BEFORE the OBS launch line (never Restart-loop a live OBS)
    pre = s.find("import strih_scenes")
    launch = s.find("OBS_PID=$!")
    assert pre != -1, "strih-obs-start.sh must preflight `import strih_scenes` (the #1156 pattern)"
    assert launch != -1, "strih-obs-start.sh must have the OBS launch (OBS_PID=$!)"
    assert pre < launch, "the import preflight must come BEFORE the OBS launch line"
    # --bootstrap seed runs AFTER the :4455 WS-up confirmation. Anchor the REAL invocation line
    # (`python3 "$SCN" --bootstrap`), never a bare "strih_scenes.py --bootstrap" that a nearby comment
    # also contains (the #712/#675 self-collision trap).
    ws_up = s.find("WS :4455 up")
    boot = s.find('python3 "$SCN" --bootstrap')
    assert ws_up != -1, "strih-obs-start.sh must confirm WS :4455 up"
    assert boot != -1, 'strih-obs-start.sh must run `python3 "$SCN" --bootstrap` (the seeder)'
    assert boot > ws_up, "the --bootstrap seed must run AFTER the :4455 wait"


def test_setup_strih_step6_installs_seeder_to_usr_local_bin():
    s = _read("setup-strih.sh")
    assert 'install -m 0755 "${HERE}/strih_scenes.py" /usr/local/bin/strih_scenes.py' in s, (
        "setup-strih.sh step 6 must install strih_scenes.py into /usr/local/bin"
    )


# --- issue 1317: per-input settings CLASS (camera vs 2ME feedback) ---------------------------------

def test_input_class_for_cameras_and_cg_obs_are_camera():
    for name in _CAMERA_INPUTS:
        assert _mod.input_class_for(name) == "camera", name
    # RESOLUME-SNV (cg-obs) is a genlocked SENDER (issue 1300) -> camera class, not feedback
    assert _mod.input_class_for("RESOLUME-SNV (cg-obs)") == "camera"


def test_input_class_for_2me_feedback_is_feedback_regardless_of_prefix():
    assert _mod.input_class_for("STRIH-SNV (2ME PGM)") == "feedback"
    assert _mod.input_class_for("STRIH-SNV (2ME PVW)") == "feedback"
    # the future STRIH-LX self-loop feedback (issue 1347) inherits the class by suffix, not prefix
    assert _mod.input_class_for("STRIH-LX (2ME PGM)") == "feedback"
    assert _mod.input_class_for("STRIH-LX (2ME PVW)") == "feedback"


def test_input_settings_for_camera_class_is_the_certified_genlock_dict():
    assert _mod.input_settings_for("CAM3 (usb)", 3) == _CAMERA
    assert _mod.input_settings_for("RESOLUME-SNV (cg-obs)", 3) == _CAMERA
    # camera latency rides the manifest floor on the REAL genlock ms knob
    assert _mod.input_settings_for("CAM1 (usb)", 5)["genlock_latency_ms_src"] == 5
    assert "latency" not in _mod.input_settings_for("CAM1 (usb)", 5)
    # fresh dict each call (never a shared mutable default)
    a = _mod.input_settings_for("CAM1 (usb)", 3)
    a["genlock_fifo"] = False
    assert _mod.input_settings_for("CAM1 (usb)", 3)["genlock_fifo"] is True


def test_input_settings_for_feedback_class_is_non_genlock():
    for name in _FEEDBACK_INPUTS + ["STRIH-LX (2ME PVW)"]:
        s = _mod.input_settings_for(name, 3)
        assert s["ndi_sync"] == 1                      # SOURCE_TIMING, mirroring the Windows light.json
        assert s["latency"] == 1                       # feedback latency is FIXED at 1
        assert s["ndi_bw_mode"] == 0
        # genlock_fifo is EXPLICIT False (not omitted) so an overlay heal clears a mis-seeded True
        assert s.get("genlock_fifo") is False
        assert "genlock_fifo" in s
    # feedback latency is independent of the camera manifest floor
    assert _mod.input_settings_for("STRIH-SNV (2ME PGM)", 9)["latency"] == 1


def test_seed_inputs_applies_the_class_per_input():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    by_src = {p["ndi_source_name"]: p for p in plan}
    assert by_src["CAM3 (usb)"]["settings"] == _CAMERA
    assert by_src["RESOLUME-SNV (cg-obs)"]["settings"] == _CAMERA
    assert by_src["STRIH-SNV (2ME PGM)"]["settings"] == _FEEDBACK
    assert by_src["STRIH-SNV (2ME PVW)"]["settings"] == _FEEDBACK


def test_seed_inputs_real_manifest_yields_8_camera_2_feedback():
    inputs, _outputs, latency = _mod.parse_seed_manifest(_REAL_MANIFEST)
    plan = _mod.seed_inputs(inputs, latency)
    classes = [_mod.input_class_for(p["ndi_source_name"]) for p in plan]
    assert len(plan) == 10
    assert classes.count("camera") == 8
    assert classes.count("feedback") == 2


def test_input_classes_summary_reports_counts_and_per_input_class():
    # the report body verify-strih.sh item 4b surfaces (report-only)
    s = _mod.input_classes_summary(_TEN_INPUTS)
    assert s.startswith("8 camera, 2 feedback")
    assert "STRIH-SNV (2ME PGM)=feedback" in s
    assert "RESOLUME-SNV (cg-obs)=camera" in s
    assert "CAM3 (usb)=camera" in s
    # dedup like scene_order (a repeated name is counted once)
    assert _mod.input_classes_summary(["CAM1 (usb)", "CAM1 (usb)"]).startswith("1 camera, 0 feedback")


def test_input_parity_problems_flags_a_feedback_input_left_genlocked():
    # a 2ME feedback input that is STILL (wrongly) genlock_fifo=True must be flagged by the report
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    actual = _actual_from_plan(plan)
    actual["NDI STRIH-SNV (2ME PGM)"] = {
        "ndi_source_name": "STRIH-SNV (2ME PGM)", "genlock_fifo": True, "ndi_sync": 1,
    }
    problems = _mod.input_parity_problems(actual, plan)
    assert any("genlock_fifo" in p and "STRIH-SNV (2ME PGM)" in p for p in problems)


# --- issue 1317: --bootstrap UPDATE path (drive with a fake WS client, no network) -----------------

class _FakeObs:
    """Records every obs.req; answers GetInputSettings / GetInputDefaultSettings from a canned map so
    bootstrap can compute the effective settings with no network. No `.ws` attribute -> the
    ndi_source_name re-enforce takes the direct (ungated) path."""

    def __init__(self, existing):
        self._existing = existing  # {inputName: explicit-settings-dict}
        self.calls = []

    def req(self, req_type, data=None, ignore_err=False):
        self.calls.append((req_type, data or {}))
        if req_type == "GetInputDefaultSettings":
            # the genlock build's ndi_source defaults: latency MODE 0 (NORMAL) + the 3 ms floor pin
            return {"defaultInputSettings": {"ndi_bw_mode": 0, "ndi_sync": 2, "latency": 0,
                                             "genlock_latency_ms_src": 3}}
        if req_type == "GetInputSettings":
            name = (data or {}).get("inputName")
            return {"inputSettings": dict(self._existing.get(name, {}))}
        return {}


def _settings_updates(calls):
    """The SetInputSettings calls that carry the CLASS settings (an ndi_sync key) -- the update
    primitive, distinct from the ndi_source_name-only re-enforce SetInputSettings."""
    return [d for (t, d) in calls
            if t == "SetInputSettings" and "ndi_sync" in (d.get("inputSettings") or {})]


def test_bootstrap_updates_a_mismatched_existing_input():
    plan = _mod.seed_inputs(["STRIH-SNV (2ME PGM)"], 3)  # a feedback input
    inp = plan[0]["input"]
    # existing input MIS-seeded as a genlocked camera (the exact issue-1317 bug)
    existing = {inp: {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 3,
                      "ndi_source_name": "STRIH-SNV (2ME PGM)"}}
    obs = _FakeObs(existing)
    _mod.bootstrap(obs, plan, studio=False)
    updates = _settings_updates(obs.calls)
    assert len(updates) == 1, obs.calls
    s = updates[0]["inputSettings"]
    assert s["ndi_sync"] == 1 and s["latency"] == 1 and s["genlock_fifo"] is False
    assert updates[0].get("overlay") is True


def test_bootstrap_emits_no_settings_update_for_a_matching_input():
    plan = _mod.seed_inputs(["CAM3 (usb)"], 3)  # a camera input
    inp = plan[0]["input"]
    # already correctly seeded to the camera class -> effective matches -> NO settings update emitted.
    # The explicit settings are what a LIVE genlock receiver reads back (strih-lx 19.9.2026): the
    # certified coercion normalises the stock `latency` mode to 0, and the 3 ms floor sits on
    # `genlock_latency_ms_src` (the build default, so it may even be ABSENT from the explicit set).
    existing = {inp: {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 0,
                      "ndi_source_name": "CAM3 (usb)"}}
    obs = _FakeObs(existing)
    _mod.bootstrap(obs, plan, studio=False)
    assert _settings_updates(obs.calls) == [], obs.calls


def test_bootstrap_is_a_pure_read_for_a_live_certified_camera_with_explicit_pin():
    plan = _mod.seed_inputs(["CAM1 (usb)"], 3)
    inp = plan[0]["input"]
    # the exact live read-back of a healthy strih-lx camera input after a prior seed: latency mode
    # coerced to 0, the ms pin explicit at the floor -> a pure read, no re-write, no name re-enforce
    existing = {inp: {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 0,
                      "genlock_latency_ms_src": 3, "ndi_source_name": "CAM1 (usb)"}}
    obs = _FakeObs(existing)
    _mod.bootstrap(obs, plan, studio=False)
    assert _settings_updates(obs.calls) == [], obs.calls
    assert not any(c[0] == "SetInputSettings" for c in obs.calls)


def test_settings_update_needed_treats_absent_genlock_fifo_as_false():
    # obs-websocket OMITS a key left at its (unregistered zero) default; DistroAV's genlock_fifo is
    # not in the type defaults, so a healthy seeded FEEDBACK input reads back WITHOUT the key. Absent
    # must count as False (issue 1317 review) so it is not needlessly re-written every launch.
    desired = dict(_mod.feedback_settings(), ndi_source_name="X")
    healthy = {"ndi_bw_mode": 0, "ndi_sync": 1, "latency": 1, "ndi_source_name": "X"}  # no genlock_fifo
    assert _mod.settings_update_needed(healthy, desired) is False
    # a mis-seeded feedback input STILL genlocked must be healed
    assert _mod.settings_update_needed(dict(healthy, genlock_fifo=True), desired) is True
    # a real non-genlock drift (e.g. wrong ndi_sync) still fires
    assert _mod.settings_update_needed(dict(healthy, ndi_sync=2), desired) is True


def test_bootstrap_no_update_for_healthy_feedback_input_with_genlock_fifo_absent():
    plan = _mod.seed_inputs(["STRIH-SNV (2ME PGM)"], 3)  # a feedback input
    inp = plan[0]["input"]
    # healthy feedback input: GetInputSettings omits genlock_fifo (at default) -> pure read, no update
    existing = {inp: {"ndi_bw_mode": 0, "ndi_sync": 1, "latency": 1,
                      "ndi_source_name": "STRIH-SNV (2ME PGM)"}}
    obs = _FakeObs(existing)
    _mod.bootstrap(obs, plan, studio=False)
    assert _settings_updates(obs.calls) == [], obs.calls


# --- issue 1317 (this lane): strih ROLE seeder -- explicit-name OBJECT entries + update-only --------
# The notebook now runs the OPERATOR (production) collection, whose INPUT names are the canonical
# strih names the whole E2E tooling addresses (NDI camN, NDI 2ME PVW/PGM (mv), cg/CG-obs) -- NOT the
# parallel-phase derived NDI CAMn (usb). So the manifest carries EXPLICIT-name objects
# {sender,input,scene} + "mode":"update-only": --bootstrap heals the certified genlock class onto
# inputs that ALREADY exist and NEVER CreateScene/CreateInput; a missing declared input is REPORTED.
_OBJ_INPUTS = [
    {"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"},
    {"sender": "CAM2 (usb)", "input": "NDI cam2", "scene": "Cam 2"},
    {"sender": "STRIH-SNV (2ME PVW)", "input": "NDI 2ME PVW", "scene": "2ME PVW"},
    {"sender": "STRIH-SNV (2ME PGM)", "input": "NDI 2ME PGM (mv)", "scene": "2ME PGM"},
    {"sender": "RESOLUME-SNV (cg-obs)", "input": "cg", "scene": "CG"},
    {"sender": "RESOLUME-SNV (cg-obs)", "input": "CG-obs", "scene": "CG-obs"},
]
_OBJ_MANIFEST = json.dumps(
    {"mode": "update-only", "inputs": _OBJ_INPUTS, "outputs": _FIVE_OUTPUTS, "camera_latency_ms": 3}
)


def test_parse_seed_manifest_keeps_object_entries():
    # object entries are PRESERVED (not dropped like a non-string int/None); the bare-string entries a
    # parallel/imag manifest carries still round-trip unchanged.
    inputs, _outputs, latency = _mod.parse_seed_manifest(_OBJ_MANIFEST)
    assert latency == 3
    assert inputs == _OBJ_INPUTS
    # a mixed manifest keeps valid strings AND valid objects, drops the junk (int / None / empty / a
    # dict missing a field)
    mixed = json.dumps({"inputs": [
        "CAM1 (usb)", 7, None, "",
        {"sender": "X", "input": "NDI x", "scene": "X"},
        {"sender": "Y", "input": ""},  # missing/empty fields -> dropped
    ]})
    inputs2, _o, _l = _mod.parse_seed_manifest(mixed)
    assert inputs2 == ["CAM1 (usb)", {"sender": "X", "input": "NDI x", "scene": "X"}]


def test_parse_seed_mode_reads_update_only_and_defaults_create():
    assert _mod.parse_seed_mode(_OBJ_MANIFEST) == "update-only"
    # absent mode -> the parallel/imag default (create)
    assert _mod.parse_seed_mode(_REAL_MANIFEST) == "create"
    assert _mod.parse_seed_mode(json.dumps({"inputs": [], "mode": "create"})) == "create"
    # garbage / non-object never crashes -> create
    assert _mod.parse_seed_mode("not json {") == "create"
    assert _mod.parse_seed_mode(json.dumps([1, 2])) == "create"


def test_seed_inputs_object_entries_use_the_explicit_names():
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    assert len(plan) == 6
    by_input = {p["input"]: p for p in plan}
    # a camera object entry: explicit input/scene, sender as ndi_source_name, certified genlock class
    cam = by_input["NDI cam1"]
    assert cam["scene"] == "Cam 1"
    assert cam["ndi_source_name"] == "CAM1 (usb)"
    assert cam["settings"] == _CAMERA
    # the CG sender is a genlocked source (issue 1300) -> camera class
    assert by_input["cg"]["settings"] == _CAMERA
    assert by_input["CG-obs"]["ndi_source_name"] == "RESOLUME-SNV (cg-obs)"


def test_seed_inputs_object_2me_input_is_feedback_by_input_name():
    # the operator's 2ME inputs are `NDI 2ME PVW` / `NDI 2ME PGM (mv)` -- neither ENDS in `(2ME PGM)`,
    # so classification must recognise the 2ME marker as a SUBSTRING of the input (or the sender).
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    by_input = {p["input"]: p for p in plan}
    assert by_input["NDI 2ME PVW"]["settings"] == _FEEDBACK
    assert by_input["NDI 2ME PGM (mv)"]["settings"] == _FEEDBACK


def test_input_class_for_recognises_the_2me_marker_by_substring():
    # the explicit operator input names carry the marker WITHOUT the `(...)` the parallel senders had
    assert _mod.input_class_for("NDI 2ME PVW") == "feedback"
    assert _mod.input_class_for("NDI 2ME PGM (mv)") == "feedback"
    # cameras + cg are unaffected
    assert _mod.input_class_for("NDI cam1") == "camera"
    assert _mod.input_class_for("cg") == "camera"
    assert _mod.input_class_for("CG-obs") == "camera"


class _FakeObsUpdate:
    """A fake WS client for the update-only path: answers GetInputList from `present` (a list of ndi_source
    input names) and GetInputSettings from `settings` (a {name: explicit-settings} map). No `.ws`."""

    def __init__(self, present, settings=None):
        self._present = list(present)
        self._settings = settings or {}
        self.calls = []

    def req(self, req_type, data=None, ignore_err=False):
        self.calls.append((req_type, data or {}))
        if req_type == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": "ndi_source"} for n in self._present]}
        if req_type == "GetInputDefaultSettings":
            return {"defaultInputSettings": {"ndi_bw_mode": 0, "ndi_sync": 2, "latency": 0,
                                             "genlock_latency_ms_src": 3}}
        if req_type == "GetInputSettings":
            return {"inputSettings": dict(self._settings.get((data or {}).get("inputName"), {}))}
        return {}


def test_bootstrap_update_only_never_creates_scenes_or_inputs():
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    present = [p["input"] for p in plan]  # every declared input already exists
    obs = _FakeObsUpdate(present)
    _mod.bootstrap(obs, plan, studio=True, update_only=True)
    kinds = [t for (t, _d) in obs.calls]
    assert "CreateScene" not in kinds, obs.calls
    assert "CreateInput" not in kinds, obs.calls


def test_bootstrap_update_only_reports_a_missing_declared_input_never_creates_it():
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    present = [p["input"] for p in plan if p["input"] != "NDI cam2"]  # cam2 missing on the box
    obs = _FakeObsUpdate(present)
    result = _mod.bootstrap(obs, plan, studio=False, update_only=True)
    assert "missing" in result["NDI cam2"].lower()
    kinds = [t for (t, _d) in obs.calls]
    assert "CreateInput" not in kinds and "CreateScene" not in kinds
    # no SetInputSettings on the missing input (never touched)
    for (t, d) in obs.calls:
        if t == "SetInputSettings":
            assert d.get("inputName") != "NDI cam2"


def test_bootstrap_update_only_heals_a_class_mismatch_without_renaming_the_source():
    # a 2ME feedback input mis-seeded as genlocked (the exact drift) -> healed to the feedback class,
    # but the SetInputSettings must NOT carry ndi_source_name (the operator's binding is authoritative).
    plan = _mod.seed_inputs(
        [{"sender": "STRIH-SNV (2ME PGM)", "input": "NDI 2ME PGM (mv)", "scene": "2ME PGM"}], 3)
    inp = "NDI 2ME PGM (mv)"
    settings = {inp: {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 3,
                      "ndi_source_name": "the-operators-own-source"}}
    obs = _FakeObsUpdate([inp], settings)
    result = _mod.bootstrap(obs, plan, studio=False, update_only=True)
    sets = [d for (t, d) in obs.calls if t == "SetInputSettings"]
    assert len(sets) == 1, obs.calls
    s = sets[0]["inputSettings"]
    assert s["genlock_fifo"] is False and s["ndi_sync"] == 1
    assert "ndi_source_name" not in s, "update-only must never rename the operator's source"
    assert "healed" in result[inp].lower()


def test_bootstrap_update_only_is_a_pure_read_for_a_matching_existing_input():
    plan = _mod.seed_inputs(
        [{"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"}], 3)
    inp = "NDI cam1"
    # already the certified camera class (latency coerced to 0, the ms pin at the floor) -> no update
    settings = {inp: {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 0,
                      "genlock_latency_ms_src": 3, "ndi_source_name": "CAM1 (usb)"}}
    obs = _FakeObsUpdate([inp], settings)
    _mod.bootstrap(obs, plan, studio=False, update_only=True)
    assert not any(t == "SetInputSettings" for (t, _d) in obs.calls), obs.calls


def test_input_parity_problems_check_source_false_skips_the_ndi_source_name_check():
    # in update-only mode verify-parity checks the declared INPUTS + genlock class, NOT the
    # ndi_source_name (the operator's real senders are authoritative, not the manifest DATA).
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    actual = {p["input"]: {"ndi_source_name": "SOMETHING-ELSE",
                           "genlock_fifo": bool(p["settings"].get("genlock_fifo")),
                           "ndi_sync": p["settings"]["ndi_sync"]}
              for p in plan}
    assert _mod.input_parity_problems(actual, plan, check_source=False) == []
    # with the source check on, the mismatch IS flagged (the default behaviour is unchanged)
    assert _mod.input_parity_problems(actual, plan, check_source=True) != []


# --- issue 1317 review BLOCKER: input_classes_summary / verify_parity must not crash on objects -----

def test_input_classes_summary_handles_object_entries():
    # the report body must NOT raise on object {sender,input,scene} entries (it iterated raw entries
    # and hit `TypeError: unhashable type: 'dict'`, which killed verify_parity on the operator
    # collection -> the genlock-class drift check was permanently dead on the production strih).
    s = _mod.input_classes_summary(_OBJ_INPUTS)
    assert s.startswith("4 camera, 2 feedback"), s
    assert "STRIH-SNV (2ME PVW)=feedback" in s
    assert "CAM1 (usb)=camera" in s
    # the legacy bare-string shape is unchanged (dedup + sender label)
    assert _mod.input_classes_summary(["CAM1 (usb)", "CAM1 (usb)"]).startswith("1 camera, 0 feedback")


def test_verify_parity_does_not_crash_on_object_entries():
    # end-to-end: verify_parity over the operator (object-entry, update-only) manifest must reach its
    # verdict line, not raise. All inputs present + right class -> no problems -> no SystemExit.
    plan = _mod.seed_inputs(_OBJ_INPUTS, 3)
    present = [p["input"] for p in plan]
    settings = {p["input"]: {"ndi_source_name": "operator-owned",
                             "genlock_fifo": bool(p["settings"].get("genlock_fifo")),
                             "ndi_sync": p["settings"]["ndi_sync"]}
                for p in plan}
    obs = _FakeObsUpdate(present, settings)
    _mod.verify_parity(obs, _OBJ_MANIFEST)  # must NOT raise TypeError


# --- issue 1344: the `ASIO zvuk` program-audio input seeder (the ONE allowed create) -------------


def test_audio_input_action_matrix():
    K = _mod.AUDIO_INPUT_KIND
    assert _mod.audio_input_action(False, None) == "create"
    assert _mod.audio_input_action(True, "asio_input_capture") == "replace"
    assert _mod.audio_input_action(True, K) == "ok"
    assert _mod.audio_input_action(True, "wasapi_input_capture") == "replace"


def test_program_audio_input_kind_from_inputs():
    inputs = [
        {"inputName": "NDI cam1", "inputKind": "ndi_source"},
        {"inputName": _mod.AUDIO_INPUT_NAME, "inputKind": "asio_input_capture"},
    ]
    assert _mod.program_audio_input_kind_from_inputs(inputs) == "asio_input_capture"
    assert _mod.program_audio_input_kind_from_inputs([]) is None
    assert _mod.program_audio_input_kind_from_inputs(None) is None


def test_program_audio_input_settings_binds_the_strih_program_monitor():
    s = _mod.program_audio_input_settings()
    assert s == {"device_id": "strih-program.monitor"}
    assert _mod.AUDIO_INPUT_NAME == "ASIO zvuk"
    assert _mod.AUDIO_INPUT_KIND == "pulse_input_capture"
