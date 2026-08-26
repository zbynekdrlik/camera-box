"""#1158 — unit tests for set-ndi-mapping.py's self-heal pieces: the pure baseline lookup
(baseline_sender_for), the dependency-injected heal loop (heal_active_mapping), and the exit-code
contract (_heal_exit_code). No live OBS — heal_active_mapping takes injected op/ws/get_binding/log.
"""
import importlib.util
import pathlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"


def _load():
    spec = importlib.util.spec_from_file_location("set_ndi_mapping_heal", _SCRIPTS / "set-ndi-mapping.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


m = _load()


# --- baseline_sender_for (#399 fact table lookup) ---------------------------------------------

def test_baseline_sender_for_maps_input_to_usb_pin():
    assert m.baseline_sender_for("NDI cam1") == "CAM1 (usb)"
    assert m.baseline_sender_for("NDI cam4") == "CAM4 (usb)"


def test_baseline_sender_for_unknown_input_is_none():
    assert m.baseline_sender_for("NDI camX") is None
    assert m.baseline_sender_for("") is None


# --- _heal_exit_code contract ----------------------------------------------------------------

def test_heal_exit_code_verify_fail_is_1_even_if_something_healed():
    assert m._heal_exit_code(healed=2, offline=0, failed=1) == 1


def test_heal_exit_code_healed_is_0():
    assert m._heal_exit_code(healed=1, offline=0, failed=0) == 0


def test_heal_exit_code_nothing_healable_is_3():
    assert m._heal_exit_code(healed=0, offline=0, failed=0) == 3  # nothing drifted
    assert m._heal_exit_code(healed=0, offline=2, failed=0) == 3  # all drifted baselines offline


# --- heal_active_mapping (the DI heal loop) ---------------------------------------------------

class _FakeOp:
    """A stand-in for obs_phase2 exposing the REENFORCE_* constants + a scripted reenforce_ndi_name
    keyed on the input name."""
    REENFORCE_HEALED = "healed"
    REENFORCE_OFFLINE = "offline"
    REENFORCE_VERIFY_FAILED = "verify_failed"

    def __init__(self, statuses):
        self.statuses = dict(statuses)  # input -> status
        self.calls = []

    def reenforce_ndi_name(self, ws, inp, desired):
        self.calls.append((inp, desired))
        return self.statuses[inp]


def _get_binding_from(current):
    def _g(ws, inp):
        return current.get(inp, "")
    return _g


def test_heal_skips_correct_inputs_never_touches_them():
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    current = {"NDI cam1": "CAM1 (usb)", "NDI cam2": "CAM2 (usb)"}  # both correct
    op = _FakeOp({})
    logs = []
    healed, offline, failed, skipped = m.heal_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append)
    assert (healed, offline, failed, skipped) == (0, 0, 0, 2)
    assert op.calls == []  # correct inputs are never re-enforced


def test_heal_reenforces_empty_and_drifted_not_only_empty():
    # cam1 EMPTY, cam2 DRIFTED (a #795-style mangle leaves a non-empty wrong name), cam3 correct.
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)"), ("NDI cam3", "CAM3 (usb)")]
    current = {"NDI cam1": "", "NDI cam2": "CAM2 (usb) MANGLED", "NDI cam3": "CAM3 (usb)"}
    op = _FakeOp({"NDI cam1": "healed", "NDI cam2": "healed"})
    logs = []
    healed, offline, failed, skipped = m.heal_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append)
    assert (healed, offline, failed, skipped) == (2, 0, 0, 1)
    assert sorted(i for i, _ in op.calls) == ["NDI cam1", "NDI cam2"]  # both drifted, cam3 skipped


def test_heal_offline_baseline_left_as_is_and_logged():
    want = [("NDI cam1", "CAM1 (usb)")]
    current = {"NDI cam1": ""}
    op = _FakeOp({"NDI cam1": "offline"})
    logs = []
    healed, offline, failed, skipped = m.heal_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append)
    assert (healed, offline, failed, skipped) == (0, 1, 0, 0)
    assert any("OFFLINE" in ln and "NDI cam1" in ln for ln in logs)


def test_heal_verify_failed_counts_as_unhealed():
    want = [("NDI cam1", "CAM1 (usb)")]
    current = {"NDI cam1": ""}
    op = _FakeOp({"NDI cam1": "verify_failed"})
    logs = []
    healed, offline, failed, skipped = m.heal_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append)
    assert (healed, offline, failed, skipped) == (0, 0, 1, 0)

