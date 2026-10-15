"""issue 1367 -- the E2E sweep cuts the strih program with the Cut transition, and puts the
operator's transition back afterwards.

16.10.2026: both release-PR full-path E2E runs failed all_cambox_continuity with one ~0.73 s jump
right after the guard of every window after the first. The pixel proof showed blended frames: the
operator had left strih on `Fade 500 ms` after the evening production, so every E2E cut mixed the
outgoing and incoming camera. The harness must measure camera continuity, not the operator's
transition, so it records the transition, sets `Cut` for its own switches and restores the
operator's choice in cleanup(). The save file is durable: an aborted run never loses the
operator's original to a later run's `Cut`.

Tier-0: the module functions + the CLI handlers with a fake `_rpc`, and the wiring in
recording-e2e.sh read as text.
"""
import argparse
import importlib.util
import json
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MOD_PATH = _ROOT / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_transition_1367", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_transition_1367"] = obs_phase2
_spec.loader.exec_module(obs_phase2)

FADE = {"transitionName": "Fade", "transitionKind": "fade_transition", "transitionFixed": False,
        "transitionDuration": 500}
CUT = {"transitionName": "Cut", "transitionKind": "cut_transition", "transitionFixed": True,
       "transitionDuration": None}


# ---- pure decisions --------------------------------------------------------------------------

def test_cut_plan_records_the_operators_transition_and_sets_cut():
    plan = obs_phase2.transition_cut_plan(FADE, saved=None)
    assert plan["save"] == {"name": "Fade", "duration_ms": 500, "fixed": False}
    assert plan["set"] == "Cut"


def test_cut_plan_never_overwrites_an_existing_save():
    # an earlier aborted run already holds the operator's original; this run's state is Cut
    saved = {"name": "Fade", "duration_ms": 500, "fixed": False}
    plan = obs_phase2.transition_cut_plan(CUT, saved=saved)
    assert plan["save"] is None
    assert plan["set"] is None   # already on Cut: no request, same as a Cut box without a save


def test_cut_plan_on_a_cut_box_saves_cut_and_sets_nothing():
    plan = obs_phase2.transition_cut_plan(CUT, saved=None)
    assert plan["save"] == {"name": "Cut", "duration_ms": None, "fixed": True}
    assert plan["set"] is None


def test_restore_plan_sets_name_then_duration_for_a_non_fixed_transition():
    reqs = obs_phase2.transition_restore_plan({"name": "Fade", "duration_ms": 500, "fixed": False})
    assert reqs == [("SetCurrentSceneTransition", {"transitionName": "Fade"}),
                    ("SetCurrentSceneTransitionDuration", {"transitionDuration": 500})]


def test_restore_plan_for_a_fixed_transition_sets_no_duration():
    reqs = obs_phase2.transition_restore_plan({"name": "Cut", "duration_ms": None, "fixed": True})
    assert reqs == [("SetCurrentSceneTransition", {"transitionName": "Cut"})]


def test_restore_plan_without_a_save_is_empty():
    assert obs_phase2.transition_restore_plan(None) == []


# ---- the CLI handlers with a fake transport ------------------------------------------------------

def _fake(monkeypatch, current):
    calls = []
    state = {"current": dict(current)}

    def rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append((rtype, rdata or {}))
        if rtype == "GetCurrentSceneTransition":
            return dict(state["current"])
        if rtype == "SetCurrentSceneTransition":
            state["current"] = dict(CUT if rdata["transitionName"] == "Cut" else FADE)
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="", port=None: object())
    monkeypatch.setattr(obs_phase2, "_rpc", rpc)
    return calls


def _args(save, host="10.77.9.202"):
    return argparse.Namespace(host=host, password="", save=str(save))


def test_cut_then_restore_round_trip(monkeypatch, tmp_path):
    save = tmp_path / "strih-transition-saved.json"
    calls = _fake(monkeypatch, FADE)
    obs_phase2.transition_cut(_args(save))
    assert json.loads(save.read_text()) == {"name": "Fade", "duration_ms": 500, "fixed": False}
    assert ("SetCurrentSceneTransition", {"transitionName": "Cut"}) in calls
    calls.clear()
    obs_phase2.transition_restore(_args(save))
    assert calls[:2] == [("SetCurrentSceneTransition", {"transitionName": "Fade"}),
                         ("SetCurrentSceneTransitionDuration", {"transitionDuration": 500})]
    assert not save.exists()


def test_a_second_cut_after_an_aborted_run_keeps_the_operators_original(monkeypatch, tmp_path):
    save = tmp_path / "strih-transition-saved.json"
    save.write_text(json.dumps({"name": "Fade", "duration_ms": 500, "fixed": False}))
    _fake(monkeypatch, CUT)   # the aborted run left strih on Cut
    obs_phase2.transition_cut(_args(save))
    assert json.loads(save.read_text())["name"] == "Fade"


def test_restore_without_a_save_sends_nothing(monkeypatch, tmp_path):
    calls = _fake(monkeypatch, CUT)
    obs_phase2.transition_restore(_args(tmp_path / "absent.json"))
    assert calls == []


# ---- wiring in recording-e2e.sh ------------------------------------------------------------------

def test_recording_e2e_forces_cut_before_the_first_strih_program_switch_and_restores_in_cleanup():
    s = (_ROOT / "scripts" / "recording-e2e.sh").read_text()
    assert '. "$HERE/lib/strih-transition.sh"' in s
    force = s.find('strih_transition_force_cut "$HERE" "$STRIH"')
    first_switch = s.find('STRIH_OUT=$(python3 "$HERE/obs_phase2.py" prod-scene --host "$STRIH"')
    assert force != -1 and first_switch != -1 and force < first_switch
    cleanup = s[s.find("cleanup() {"):]
    cleanup = cleanup[:cleanup.find("\n}\n")]
    assert 'strih_transition_restore "$HERE" "$STRIH"' in cleanup


def test_the_lib_exposes_both_functions():
    lib = (_ROOT / "scripts" / "lib" / "strih-transition.sh").read_text()
    assert "strih_transition_force_cut()" in lib
    assert "strih_transition_restore()" in lib
