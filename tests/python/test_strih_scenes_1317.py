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

_CERTIFIED = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 3}


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
    # latency rides the manifest floor, NOT obs_phase2's probe default 0
    assert _mod.certified_genlock_settings(5)["latency"] == 5
    # fresh dict each call (never a shared mutable default)
    a = _mod.certified_genlock_settings(3)
    a["genlock_fifo"] = False
    assert _mod.certified_genlock_settings(3)["genlock_fifo"] is True


# --- seed_inputs ----------------------------------------------------------------------------------

def test_seed_inputs_emits_certified_settings_and_names_per_input():
    plan = _mod.seed_inputs(_TEN_INPUTS, 3)
    assert len(plan) == 10
    for item, src in zip(plan, _TEN_INPUTS):
        assert item["ndi_source_name"] == src
        assert item["scene"] == src                     # per-input scene = the source display name
        assert item["input"] == "NDI " + src            # input name kept DISTINCT from the scene
        assert item["settings"] == _CERTIFIED
        assert item["settings"]["genlock_fifo"] is True
        assert item["settings"]["ndi_sync"] == 2
        assert item["settings"]["ndi_bw_mode"] == 0
        assert item["settings"]["latency"] == 3
        # ndi_source_name is a TOP-LEVEL field, not inside the certified settings dict
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
    """Build a healthy GetInputSettings-shaped {inputName: settings} from a plan."""
    return {
        item["input"]: {
            "ndi_source_name": item["ndi_source_name"],
            "genlock_fifo": True,
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
