"""#866 -- imag OBS start must force the measurement burn OFF on every ndi input.

The per-source `genlock_burn` bool persists into OBS's saved scene collection; a runtime OFF (the
gate cleanup / obs_burn_filter.py remove) is never written to disk, so an OBS crash/reboot/manual
restart resurrects `genlock_burn=true` onto the LIVE IMAG projection (ticket's live evidence:
`Untitled.json` `NDI CAM1 burn=True` surviving a segfault-restart). `imag_scenes.py --bootstrap`
(run by imag-obs-start.sh on every fresh instance) must clear it, so a saved `true` can never
survive a restart.

Covers, with NO live OBS/rig (mirrors test_imag_latency_enforce.py's fake-ws + the module's
importlib load convention):
  a. ndi_source_names() -- pure: names of every `ndi_source` input in a GetInputList array, in
     order, skipping non-ndi kinds and malformed entries; NEVER a static/CAMS list
     (burn-target-enumeration rule).
  b. clear_measurement_burns() -- enumerates from OBS reality, clears genlock_burn=false ONLY on
     inputs that have it ON (SetInputSettings overlay-merge), read-back verifies each, FAILS LOUD
     (SystemExit) if any stays ON; no-op (never a non-zero exit) when none are ON.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _scenes_module():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_clear_burns_under_test")


class FakeObs:
    """Models an OBS with a set of inputs, some ndi with genlock_burn state.

    `state` maps inputName -> {"inputKind": str, "genlock_burn": bool|None}. SetInputSettings with
    an overlay merges genlock_burn into that state, UNLESS stuck=True (models a WS write that never
    lands, so read-back stays ON -> the loud failure path)."""

    def __init__(self, state, stuck=False):
        self.state = state
        self.stuck = stuck
        self.calls = []

    def req(self, rtype, payload=None, ignore_err=False):
        p = payload or {}
        self.calls.append((rtype, p))
        if rtype == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": s["inputKind"]}
                               for n, s in self.state.items()]}
        if rtype == "GetInputSettings":
            n = p["inputName"]
            return {"inputSettings": {"genlock_burn": self.state[n].get("genlock_burn")}}
        if rtype == "SetInputSettings":
            n = p["inputName"]
            gb = p.get("inputSettings", {}).get("genlock_burn")
            if gb is not None and not self.stuck:
                self.state[n]["genlock_burn"] = gb
            return {}
        return {}


# ---------------------------------------------------------------------------
# ndi_source_names -- pure
# ---------------------------------------------------------------------------

def test_ndi_source_names_filters_to_ndi_source_inputs_in_order():
    mod = _scenes_module()
    inputs = [
        {"inputName": "NDI CAM1", "inputKind": "ndi_source"},
        {"inputName": "Mic", "inputKind": "wasapi_input_capture"},
        {"inputName": "NDI resolume imag", "inputKind": "ndi_source"},
        {"inputName": "Colour", "inputKind": "color_source_v3"},
    ]
    assert mod.ndi_source_names(inputs) == ["NDI CAM1", "NDI resolume imag"]


def test_ndi_source_names_skips_malformed_and_empty():
    mod = _scenes_module()
    assert mod.ndi_source_names([]) == []
    assert mod.ndi_source_names(None) == []
    assert mod.ndi_source_names([
        {"inputKind": "ndi_source"},                 # no name
        {"inputName": "", "inputKind": "ndi_source"},  # empty name
        {"inputName": "NDI CAM2", "inputKind": "ndi_source"},
    ]) == ["NDI CAM2"]


# ---------------------------------------------------------------------------
# clear_measurement_burns -- live apply + verify against a fake ws
# ---------------------------------------------------------------------------

def test_clears_only_the_ndi_inputs_that_have_burn_on():
    mod = _scenes_module()
    obs = FakeObs({
        "NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True},
        "NDI CAM4": {"inputKind": "ndi_source", "genlock_burn": False},
        "NDI resolume imag": {"inputKind": "ndi_source", "genlock_burn": True},
        "Mic": {"inputKind": "wasapi_input_capture", "genlock_burn": None},
    })
    mod.clear_measurement_burns(obs)
    # both ON ndi inputs cleared; the OFF one + the non-ndi one untouched
    assert obs.state["NDI CAM1"]["genlock_burn"] is False
    assert obs.state["NDI resolume imag"]["genlock_burn"] is False
    # no SetInputSettings issued against the already-off or non-ndi inputs
    sets = [p["inputName"] for r, p in obs.calls if r == "SetInputSettings"]
    assert sorted(sets) == ["NDI CAM1", "NDI resolume imag"]


def test_no_write_when_no_burn_is_on():
    mod = _scenes_module()
    obs = FakeObs({
        "NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": False},
        "NDI CAM2": {"inputKind": "ndi_source", "genlock_burn": None},
    })
    mod.clear_measurement_burns(obs)  # must NOT raise
    assert [r for r, _ in obs.calls if r == "SetInputSettings"] == []


def test_set_input_settings_uses_overlay_merge_false():
    mod = _scenes_module()
    obs = FakeObs({"NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True}})
    mod.clear_measurement_burns(obs)
    setcall = next(p for r, p in obs.calls if r == "SetInputSettings")
    assert setcall.get("overlay") is True, "must overlay-merge, never clobber other source settings"
    assert setcall["inputSettings"]["genlock_burn"] is False


def test_stuck_clear_warns_loud_but_never_aborts_obs_start(capsys):
    """A clear that does not land IS a real leak, but must NEVER SystemExit: imag-obs-start.sh runs
    this under set -euo pipefail, so aborting would take OBS down / restart-loop the live projection
    while the same stuck write keeps failing. Warn LOUD (the [0/8] sweep-off is the backstop),
    attempt the write, and leave OBS up."""
    mod = _scenes_module()
    obs = FakeObs({"NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True}}, stuck=True)
    mod.clear_measurement_burns(obs)  # must NOT raise
    # it DID attempt the clear
    assert [p["inputName"] for r, p in obs.calls if r == "SetInputSettings"] == ["NDI CAM1"]
    # and warned loud about the input that stayed on
    out = capsys.readouterr().out
    assert "STILL ON" in out and "NDI CAM1" in out


def test_ws_exception_during_enumeration_warns_but_never_aborts_obs_start(capsys):
    """The #328 timeout-raise class: GetInputList (or any WS call) RAISES rather than returning an
    error result. This must be caught and warned, never propagate out to crash imag-obs-start.sh
    under set -euo pipefail (mirrors obs_burn_filter._all_ndi_inputs' try/except)."""
    mod = _scenes_module()

    class RaisingObs(FakeObs):
        def req(self, rtype, payload=None, ignore_err=False):
            if rtype == "GetInputList":
                raise TimeoutError("obs-websocket recv timed out (#328 class)")
            return super().req(rtype, payload, ignore_err)

    obs = RaisingObs({"NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True}})
    mod.clear_measurement_burns(obs)  # must NOT raise
    assert [r for r, _ in obs.calls if r == "SetInputSettings"] == []
    assert "WS error" in capsys.readouterr().out


def test_ws_exception_MID_sweep_warns_but_never_aborts_obs_start(capsys):
    """A WS error raised AFTER a successful GetInputList (e.g. during a per-input SetInputSettings)
    must also be caught — the whole sweep body is guarded, so no failure mode can crash OBS start."""
    mod = _scenes_module()

    class RaisingSetObs(FakeObs):
        def req(self, rtype, payload=None, ignore_err=False):
            if rtype == "SetInputSettings":
                raise ConnectionResetError("socket closed mid-sweep")
            return super().req(rtype, payload, ignore_err)

    obs = RaisingSetObs({"NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True}})
    mod.clear_measurement_burns(obs)  # must NOT raise
    assert "WS error" in capsys.readouterr().out


def test_enumeration_failure_warns_but_never_aborts_obs_start():
    """#1011 fail-closed: a GetInputList that FAILED (no `inputs` key) is 'could not enumerate',
    never 'no burns' — but at OBS start (imag-obs-start.sh, set -euo pipefail) this must NOT
    SystemExit (that would take OBS down on the live box); it warns and returns, never clearing."""
    mod = _scenes_module()

    class FailingEnumObs(FakeObs):
        def req(self, rtype, payload=None, ignore_err=False):
            self.calls.append((rtype, payload or {}))
            if rtype == "GetInputList":
                return {}  # WS error under ignore_err -> no `inputs` key
            return super().req(rtype, payload, ignore_err)

    obs = FailingEnumObs({"NDI CAM1": {"inputKind": "ndi_source", "genlock_burn": True}})
    mod.clear_measurement_burns(obs)  # must NOT raise
    assert [r for r, _ in obs.calls if r == "SetInputSettings"] == []
