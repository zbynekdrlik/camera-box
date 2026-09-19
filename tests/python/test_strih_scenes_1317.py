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
