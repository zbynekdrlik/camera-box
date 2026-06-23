"""#183: unit tests for the test-window genlock_preload FORCE + RESTORE.

The recording-based E2E records the REAL prod scene, whose certified genlock input runs the
PRODUCTION genlock_preload (≈31 ≈ 1s of video delay — there ONLY for prod audio-sync, #97).
For the lowest-latency zero-loss TEST that delay is irrelevant noise: the test must measure
the TRUE genlock hop (~33ms) at preload=1. So prod-scene FORCES the recorded prod input's
genlock_preload to the test value for the recording window, saves the prod value, and teardown
RESTORES it (prod audio-sync untouched after the test).

These tests pin that force/restore behaviour against a mock WebSocket (no live OBS): the right
input is targeted, ONLY genlock_preload is changed, the prod value is saved + restored, and
the prod input is never touched when --upstream is omitted.
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_preload", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_preload"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


class FakeWS:
    """Records every _rpc call (op, payload) so a test can assert what SetInputSettings ran."""

    def __init__(self):
        self.calls = []

    # _rpc(ws, op, payload) calls ws.call(...) under the hood; the harness uses a thin
    # wrapper, so patch _rpc to capture instead (simpler + version-independent).


def _patch_capture(monkeypatch):
    """Patch _rpc to record calls and _save_state to a no-op; return the calls list."""
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        return {}

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_save_state", lambda state: None)
    return calls


def test_force_and_restore_functions_exist():
    assert callable(obs_phase2._force_test_preload)
    assert callable(obs_phase2._restore_test_preload)


def test_force_sets_only_genlock_preload_on_the_matched_prod_input(monkeypatch):
    calls = _patch_capture(monkeypatch)
    # The prod genlock input is found with preload=31 (the prod audio-sync depth).
    monkeypatch.setattr(
        obs_phase2, "_find_prod_genlock_input",
        lambda ws, host, up: ("NDI cam5", {"genlock_preload": 31, "genlock_fifo": True,
                                           "ndi_sync": 2, "ndi_source_name": up}),
    )
    state = {}
    forced = obs_phase2._force_test_preload(None, "10.0.0.1", "CAM1 (usb)", 1, state)
    assert forced is True
    # Exactly ONE SetInputSettings, on the matched input, changing ONLY genlock_preload to 1.
    sets = [c for c in calls if c[0] == "SetInputSettings"]
    assert len(sets) == 1, f"expected one SetInputSettings, got {sets}"
    _, payload = sets[0]
    assert payload["inputName"] == "NDI cam5"
    assert payload["inputSettings"] == {"genlock_preload": 1}, \
        "MUST change only genlock_preload — never the #63/#149 locked baseline"
    # The prod value was saved for teardown.
    saved = state["10.0.0.1"][obs_phase2._TEST_PRELOAD_STATE_KEY]
    assert saved == {"input": "NDI cam5", "preload": 31}


def test_restore_puts_back_the_saved_prod_preload(monkeypatch):
    calls = _patch_capture(monkeypatch)
    state = {"10.0.0.1": {obs_phase2._TEST_PRELOAD_STATE_KEY:
                          {"input": "NDI 2ME PGM", "preload": 31}}}
    obs_phase2._restore_test_preload(None, "10.0.0.1", state)
    sets = [c for c in calls if c[0] == "SetInputSettings"]
    assert len(sets) == 1
    _, payload = sets[0]
    assert payload["inputName"] == "NDI 2ME PGM"
    assert payload["inputSettings"] == {"genlock_preload": 31}, \
        "teardown MUST restore the exact prod preload (audio-sync untouched)"
    # The state entry is cleared so a re-run never double-restores.
    assert obs_phase2._TEST_PRELOAD_STATE_KEY not in state["10.0.0.1"]


def test_force_noop_when_no_upstream_leaves_prod_untouched(monkeypatch):
    calls = _patch_capture(monkeypatch)
    # _find_prod_genlock_input must never even be called when upstream is empty.
    monkeypatch.setattr(
        obs_phase2, "_find_prod_genlock_input",
        lambda *a, **k: (_ for _ in ()).throw(AssertionError("must not be called")),
    )
    state = {}
    assert obs_phase2._force_test_preload(None, "h", "", 1, state) is False
    assert [c for c in calls if c[0] == "SetInputSettings"] == []
    assert state == {}


def test_force_noop_when_prod_already_at_test_value(monkeypatch):
    calls = _patch_capture(monkeypatch)
    monkeypatch.setattr(
        obs_phase2, "_find_prod_genlock_input",
        lambda ws, host, up: ("NDI cam5", {"genlock_preload": 1}),
    )
    state = {"h": {obs_phase2._TEST_PRELOAD_STATE_KEY: {"input": "x", "preload": 9}}}
    # Already at 1 → no SetInputSettings, and any stale saved entry is cleared so teardown
    # never wrongly restores a value we did not change this run.
    assert obs_phase2._force_test_preload(None, "h", "CAM1 (usb)", 1, state) is False
    assert [c for c in calls if c[0] == "SetInputSettings"] == []
    assert obs_phase2._TEST_PRELOAD_STATE_KEY not in state["h"]


def test_restore_noop_when_nothing_forced(monkeypatch):
    calls = _patch_capture(monkeypatch)
    state = {"h": {"prev_scene": "Cam 5"}}  # no _TEST_PRELOAD_STATE_KEY
    obs_phase2._restore_test_preload(None, "h", state)
    assert [c for c in calls if c[0] == "SetInputSettings"] == []


def test_prod_scene_parses_upstream_and_test_preload(monkeypatch):
    # `prod-scene --upstream X --test-preload N` must parse and reach prod_scene.
    captured = {}
    monkeypatch.setattr(obs_phase2, "prod_scene",
                        lambda a: captured.update(up=a.upstream, tp=a.test_preload))
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "prod-scene", "--host", "h", "--program-scene", "Cam 5",
         "--upstream", "CAM1 (usb)", "--test-preload", "1"],
    )
    obs_phase2.main()
    assert captured == {"up": "CAM1 (usb)", "tp": 1}


def test_prod_scene_test_preload_defaults_to_1(monkeypatch):
    captured = {}
    monkeypatch.setattr(obs_phase2, "prod_scene",
                        lambda a: captured.update(up=a.upstream, tp=a.test_preload))
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "prod-scene", "--host", "h", "--program-scene", "Cam 5"],
    )
    obs_phase2.main()
    # Default: test-preload=1, upstream empty (⇒ prod left untouched).
    assert captured == {"up": "", "tp": 1}
