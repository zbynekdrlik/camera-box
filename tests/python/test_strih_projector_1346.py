"""issue 1346 -- unit tests for the strih-lx fixed HDMI fullscreen projector seed
(scripts/strih_scenes.py projector helpers + seed_projector glue) and the setup-strih.sh
SaveProjectors / strih-lx-projector.json wiring.

Owner ROZHODNUTE (19.9.2026): the strih-lx HDMI output is an OBS fullscreen projector on the HDMI
display, selectable in OBS between Program and Multiview, PERSISTED across relaunches (SaveProjectors
=true). Not Xorg/DRM-lease. This is the Pristup-1 design.

Tier-0 runnable (pytest on the pure helpers + a fake-WS seed + script-text anchors; no cargo, no
rig). The pure helpers (projector_type_to_mix / projector_monitor_index / projector_already_saved /
read_projector_type / write_projector_type) carry no rig dependency; seed_projector is exercised with
a fake OBS client (no live projector is ever opened here -- the live open is a supervisor rig step).
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

# The two obs-websocket 5 video-mix constants (OpenVideoMixProjector videoMixType).
_MIX_PROGRAM = "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"
_MIX_MULTIVIEW = "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"
# OBS ProjectorType saved in a scene collection's saved_projectors: 3 = StudioProgram, 4 = Multiview.
_TYPE_STUDIOPROGRAM = 3
_TYPE_MULTIVIEW = 4


# --- projector_type_to_mix ------------------------------------------------------------------------

def test_projector_type_to_mix_both_mappings():
    assert _mod.projector_type_to_mix("program") == _MIX_PROGRAM
    assert _mod.projector_type_to_mix("multiview") == _MIX_MULTIVIEW
    # the module constants agree with the literal WS strings
    assert _mod.PROJECTOR_MIX_PROGRAM == _MIX_PROGRAM
    assert _mod.PROJECTOR_MIX_MULTIVIEW == _MIX_MULTIVIEW


def test_projector_type_to_mix_rejects_unknown():
    for bad in ("", "preview", "source", "PROGRAM", "multi", None):
        with pytest.raises(ValueError):
            _mod.projector_type_to_mix(bad)


# --- projector_monitor_index ----------------------------------------------------------------------

def test_projector_monitor_index_only_edp_is_none():
    # today's box: one internal panel, no external monitor -> None (SKIP, never fall back to eDP)
    mons = [{"monitorName": "eDP-2(0)", "monitorIndex": 0}]
    assert _mod.projector_monitor_index(mons) is None


def test_projector_monitor_index_picks_the_first_external():
    mons = [
        {"monitorName": "eDP-2(0)", "monitorIndex": 0},
        {"monitorName": "HDMI-A-1(1)", "monitorIndex": 1},
    ]
    assert _mod.projector_monitor_index(mons) == 1


def test_projector_monitor_index_two_externals_takes_the_first():
    mons = [
        {"monitorName": "HDMI-A-1(0)", "monitorIndex": 0},
        {"monitorName": "DP-1(1)", "monitorIndex": 1},
    ]
    assert _mod.projector_monitor_index(mons) == 0


def test_projector_monitor_index_empty_is_none():
    assert _mod.projector_monitor_index([]) is None
    assert _mod.projector_monitor_index(None) is None


# --- projector_already_saved ----------------------------------------------------------------------

def test_projector_already_saved_type_and_monitor_match():
    saved = [{"monitor": 1, "type": _TYPE_MULTIVIEW}]
    # a Multiview (type 4) projector on monitor 1 is already saved -> True (skip re-open)
    assert _mod.projector_already_saved(saved, _TYPE_MULTIVIEW, 1) is True
    # a StudioProgram (type 3) projector is NOT saved -> False (would need opening)
    assert _mod.projector_already_saved(saved, _TYPE_STUDIOPROGRAM, 1) is False
    # right type, wrong monitor -> False
    assert _mod.projector_already_saved(saved, _TYPE_MULTIVIEW, 0) is False


def test_projector_already_saved_empty_is_false():
    assert _mod.projector_already_saved([], _TYPE_MULTIVIEW, 1) is False
    assert _mod.projector_already_saved(None, _TYPE_MULTIVIEW, 1) is False


# --- read/write projector config (json default = multiview; not-overwritten by the CLI writer) ----

def test_read_projector_type_default_is_multiview_when_absent(tmp_path):
    missing = tmp_path / "nope.json"
    assert _mod.read_projector_type(str(missing)) == "multiview"
    assert _mod.DEFAULT_PROJECTOR_TYPE == "multiview"


def test_read_projector_type_reads_program_and_falls_back_on_garbage(tmp_path):
    p = tmp_path / "strih-lx-projector.json"
    p.write_text(json.dumps({"type": "program"}))
    assert _mod.read_projector_type(str(p)) == "program"
    p.write_text("not json {")
    assert _mod.read_projector_type(str(p)) == "multiview"
    p.write_text(json.dumps({"type": "bogus"}))
    assert _mod.read_projector_type(str(p)) == "multiview"


def test_write_then_read_projector_type_roundtrip(tmp_path):
    p = tmp_path / "strih-lx-projector.json"
    _mod.write_projector_type("program", str(p))
    assert json.loads(p.read_text()) == {"type": "program"}
    assert _mod.read_projector_type(str(p)) == "program"
    _mod.write_projector_type("multiview", str(p))
    assert _mod.read_projector_type(str(p)) == "multiview"
    with pytest.raises(ValueError):
        _mod.write_projector_type("bogus", str(p))


# --- seed_projector: fake-WS behaviour (no live projector ever opened) -----------------------------

class _FakeObs:
    """Minimal Obs stand-in: records req() calls and replies to GetMonitorList /
    GetSceneCollectionList. Never opens a real projector."""

    def __init__(self, monitors, collection="strih-lx"):
        self._monitors = monitors
        self._collection = collection
        self.calls = []

    def req(self, req_type, data=None, ignore_err=False):
        self.calls.append((req_type, data))
        if req_type == "GetMonitorList":
            return {"monitors": self._monitors}
        if req_type == "GetSceneCollectionList":
            return {"currentSceneCollectionName": self._collection}
        return {}

    def opened(self):
        return [d for t, d in self.calls if t == "OpenVideoMixProjector"]


def test_seed_projector_no_external_monitor_skips_never_opens(tmp_path, capsys):
    obs = _FakeObs([{"monitorName": "eDP-2(0)", "monitorIndex": 0}])
    status = _mod.seed_projector(obs, config_path=str(tmp_path / "nope.json"),
                                 cfg_dir=str(tmp_path))
    out = capsys.readouterr().out
    assert "projector: no external monitor, skipping" in out
    assert obs.opened() == [], "must NOT open a projector when only the eDP panel exists"
    assert status == "skipped-no-hdmi"


def test_seed_projector_opens_multiview_on_hdmi_when_not_saved(tmp_path, capsys):
    obs = _FakeObs([
        {"monitorName": "eDP-2(0)", "monitorIndex": 0},
        {"monitorName": "HDMI-A-1(1)", "monitorIndex": 1},
    ])
    # default config (absent) -> multiview; empty cfg_dir -> no saved collection file -> opens.
    status = _mod.seed_projector(obs, config_path=str(tmp_path / "nope.json"),
                                 cfg_dir=str(tmp_path))
    opened = obs.opened()
    assert len(opened) == 1
    assert opened[0]["videoMixType"] == _MIX_MULTIVIEW
    assert opened[0]["monitorIndex"] == 1
    assert status == "opened-multiview"


def test_seed_projector_idempotent_via_glob_when_collection_name_slugified(tmp_path, capsys):
    # OBS slugifies a display-name into a different filename ("My Show" -> My_Show.json). The WS name
    # ("My Show") has no matching <name>.json, so the resolver globs all collection files and STILL
    # finds the saved Multiview projector -> skip (no duplicate window). Guards FINDING 4b-1.
    scenes = tmp_path / "basic" / "scenes"
    scenes.mkdir(parents=True)
    (scenes / "My_Show.json").write_text(json.dumps(
        {"name": "My Show", "saved_projectors": [{"monitor": 1, "type": 4}]}
    ))
    obs = _FakeObs([
        {"monitorName": "eDP-2(0)", "monitorIndex": 0},
        {"monitorName": "HDMI-A-1(1)", "monitorIndex": 1},
    ], collection="My Show")  # no "My Show.json" -> exact-name miss -> glob fallback
    status = _mod.seed_projector(obs, config_path=str(tmp_path / "nope.json"),
                                 cfg_dir=str(tmp_path))
    assert obs.opened() == [], "glob fallback must find the slugified collection's saved projector"
    assert status == "already-saved"


def test_seed_projector_idempotent_skips_when_already_saved(tmp_path, capsys):
    # a collection file with a Multiview (type 4) projector already saved on monitor 1
    scenes = tmp_path / "basic" / "scenes"
    scenes.mkdir(parents=True)
    (scenes / "strih-lx.json").write_text(json.dumps(
        {"name": "strih-lx", "saved_projectors": [{"monitor": 1, "type": 4}]}
    ))
    obs = _FakeObs([
        {"monitorName": "eDP-2(0)", "monitorIndex": 0},
        {"monitorName": "HDMI-A-1(1)", "monitorIndex": 1},
    ], collection="strih-lx")
    status = _mod.seed_projector(obs, config_path=str(tmp_path / "nope.json"),
                                 cfg_dir=str(tmp_path))
    assert obs.opened() == [], "must NOT re-open a projector OBS already restores from saved_projectors"
    assert status == "already-saved"


# --- CLI: --projector is a valid mode; a bad value is rejected before any WS connect ---------------

def test_projector_cli_rejects_bad_value_without_touching_obs():
    r = subprocess.run(
        [sys.executable, str(SCRIPTS / "strih_scenes.py"), "--projector", "bogus"],
        capture_output=True, text=True,
    )
    assert r.returncode != 0
    # argparse `choices` rejects it before any manifest read or WS connect
    assert "projector" in r.stderr.lower()


# --- static anchors: setup-strih.sh + strih_scenes.py wiring ---------------------------------------

def _read(name):
    return (SCRIPTS / name).read_text()


def test_setup_strih_preseeds_saveprojectors_before_the_obs_enable():
    s = _read("setup-strih.sh")
    # anchor the FUNCTIONAL heredoc line (the configparser upsert), NOT the prose comment/echo that
    # also mention "SaveProjectors=true" -- so this proves the actual pre-seed runs before the enable.
    save = s.find('for kv in ("SaveProjectors=true", "ProjectorAlwaysOnTop=true")')
    enable = s.find("systemctl --user enable strih-obs.service")
    assert save != -1, (
        "setup-strih.sh step 7 must pre-seed SaveProjectors=true + ProjectorAlwaysOnTop=true in "
        "user.ini via the `for kv in (\"SaveProjectors=true\", \"ProjectorAlwaysOnTop=true\")` upsert"
    )
    assert enable != -1, "setup-strih.sh step 8 must enable strih-obs.service"
    assert save < enable, "the SaveProjectors=true pre-seed (step 7) must precede the OBS enable (step 8)"


def test_setup_strih_writes_projector_json_default_multiview_without_overwrite():
    s = _read("setup-strih.sh")
    # the default projector config is written, defaulting to multiview
    assert '{"type":"multiview"}' in s, (
        "setup-strih.sh must write /opt/camera-box/strih-lx-projector.json defaulting to multiview"
    )
    assert "/opt/camera-box/strih-lx-projector.json" in s
    # do NOT overwrite an operator's existing choice -- the write is guarded by a not-exists check
    guard = s.find("[ ! -f /opt/camera-box/strih-lx-projector.json ]")
    assert guard != -1, (
        "the strih-lx-projector.json write must be guarded by `[ ! -f ... ]` (do not overwrite the "
        "operator's choice)"
    )


def test_strih_scenes_seeds_projector_after_the_input_seed():
    s = _read("strih_scenes.py")
    boot_call = s.find("statuses = bootstrap(obs, plan")
    proj_call = s.find("seed_projector(obs)")
    assert boot_call != -1, "strih_scenes.py --bootstrap must run the input seed (statuses = bootstrap(obs, plan ...))"
    assert proj_call != -1, "strih_scenes.py --bootstrap must call seed_projector(obs)"
    assert boot_call < proj_call, "the projector seed must run AFTER the input seed"
