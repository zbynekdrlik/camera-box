"""#1180 — unit tests for set-ndi-mapping.py's --verify-live pieces: the dependency-injected
liveness verify loop (verify_live_mapping) and its exit-code contract (_verify_live_exit_code).

The LIVENESS term: a name-only verify (--heal / reenforce_ndi_name read-back) passes on a receiver
that holds a frozen frame with a CORRECT name (the 2026-08-27 cam1 wedge). --verify-live samples
frame-delivery liveness over WS and FAILS LOUD (exit 1) on a FROZEN input so the caller escalates
to an OBS restart. No live OBS — verify_live_mapping takes an injected op/ws/sampler/log.
"""
import importlib.util
import pathlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"


def _load():
    spec = importlib.util.spec_from_file_location(
        "set_ndi_mapping_verify_live", _SCRIPTS / "set-ndi-mapping.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


m = _load()


# --- _verify_live_exit_code contract ---------------------------------------------------------

def test_verify_live_exit_code_frozen_is_1_even_if_something_live():
    assert m._verify_live_exit_code(live=3, frozen=1, inconclusive=0) == 1


def test_verify_live_exit_code_frozen_beats_inconclusive():
    assert m._verify_live_exit_code(live=0, frozen=1, inconclusive=2) == 1


def test_verify_live_exit_code_all_live_is_0():
    assert m._verify_live_exit_code(live=4, frozen=0, inconclusive=0) == 0


def test_verify_live_exit_code_inconclusive_only_is_3():
    assert m._verify_live_exit_code(live=0, frozen=0, inconclusive=2) == 3
    assert m._verify_live_exit_code(live=2, frozen=0, inconclusive=1) == 3  # not fully confirmed


def test_verify_live_exit_code_empty_is_0():
    assert m._verify_live_exit_code(live=0, frozen=0, inconclusive=0) == 0


# --- verify_live_mapping (the DI verify loop) ------------------------------------------------

class _FakeOp:
    """Stand-in for obs_phase2 exposing the LIVENESS_* constants + a scripted sampler keyed on
    the input name."""
    LIVENESS_LIVE = "live"
    LIVENESS_FROZEN = "frozen"
    LIVENESS_INCONCLUSIVE = "inconclusive"


def _sampler_from(states):
    def _s(ws, inp):
        return (states[inp], "" if states[inp] == "live" else f"{inp} verdict {states[inp]}")
    return _s


def test_verify_live_all_live():
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    op = _FakeOp()
    logs = []
    live, frozen, inconclusive = m.verify_live_mapping(
        op, object(), want, _sampler_from({"NDI cam1": "live", "NDI cam2": "live"}), logs.append)
    assert (live, frozen, inconclusive) == (2, 0, 0)
    assert logs == []  # a live receiver logs nothing


def test_verify_live_frozen_is_counted_and_logged_loud():
    # the 2026-08-27 cam1 wedge: name correct, frames frozen.
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    op = _FakeOp()
    logs = []
    live, frozen, inconclusive = m.verify_live_mapping(
        op, object(), want, _sampler_from({"NDI cam1": "frozen", "NDI cam2": "live"}), logs.append)
    assert (live, frozen, inconclusive) == (1, 1, 0)
    frozen_logs = [ln for ln in logs if "NDI cam1" in ln and "FROZEN" in ln]
    assert frozen_logs, logs
    # the loud line must point the caller at the real cure (an OBS restart), not a name re-set.
    assert any("restart" in ln.lower() for ln in frozen_logs)


def test_verify_live_inconclusive_is_left_as_is_and_logged():
    want = [("NDI cam1", "CAM1 (usb)")]
    op = _FakeOp()
    logs = []
    live, frozen, inconclusive = m.verify_live_mapping(
        op, object(), want, _sampler_from({"NDI cam1": "inconclusive"}), logs.append)
    assert (live, frozen, inconclusive) == (0, 0, 1)
    assert any("INCONCLUSIVE" in ln and "NDI cam1" in ln for ln in logs)
