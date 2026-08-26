"""#1158 — unit tests for obs_phase2.reenforce_ndi_name, the shared "re-enforce an NDI input's
source name, safely" primitive (discoverability-gated + read-back-verified) used by BOTH
strih_mv_scenes.reattach()'s vanished-branch AND set-ndi-mapping.py --heal.

An EMPTY ndi_source_name STOPS the DistroAV receiver thread, so the in-loop #767/#1096 watchdogs can
never revive it — the primitive re-applies a name, but ONLY when discoverable (a name absent from the
DistroAV editable-combo list would MANGLE, #795), and verifies via read-back so a mangle is a LOUD
detected result, never silent corruption.
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_reenforce", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_reenforce"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


class _FakeRpc:
    """Fake obs_phase2._rpc: tracks a single input's current ndi_source_name (updated by
    SetInputSettings, reflected by GetInputSettings) and a fixed DistroAV finder list.
    `mangle_to` (if set) makes SetInputSettings store a DIFFERENT value than requested — the
    #795 mangle shape, so the read-back mismatches."""

    def __init__(self, current, finder, mangle_to=None):
        self.current = current
        self.finder = list(finder)
        self.mangle_to = mangle_to
        self.set_calls = []

    def __call__(self, ws, rtype, rdata=None, ignore_err=False):
        if rtype == "GetInputPropertiesListPropertyItems":
            return {"propertyItems": [{"itemValue": v} for v in self.finder]}
        if rtype == "GetInputSettings":
            return {"inputSettings": {"ndi_source_name": self.current}}
        if rtype == "SetInputSettings":
            self.set_calls.append(rdata)
            requested = rdata["inputSettings"]["ndi_source_name"]
            self.current = self.mangle_to if self.mangle_to is not None else requested
            return {}
        raise AssertionError(f"unexpected rpc call: {rtype}")


def _patch(monkeypatch, fake):
    monkeypatch.setattr(obs_phase2, "_rpc", fake)


def test_healed_when_desired_is_discoverable(monkeypatch):
    fake = _FakeRpc(current="", finder=["CAM1 (usb)", "CAM2 (usb)"])
    _patch(monkeypatch, fake)
    status = obs_phase2.reenforce_ndi_name(object(), "NDI cam1", "CAM1 (usb)")
    assert status == obs_phase2.REENFORCE_HEALED
    assert fake.current == "CAM1 (usb)"
    assert fake.set_calls == [
        {"inputName": "NDI cam1",
         "inputSettings": {"ndi_source_name": "CAM1 (usb)"},
         "overlay": True}
    ]


def test_offline_when_desired_absent_from_finder_never_sets(monkeypatch):
    # #795: setting a name absent from the combo list MANGLES it — so an offline desired is NEVER
    # written. The input is left exactly as-is (here still empty), and the caller screams.
    fake = _FakeRpc(current="", finder=["CAM2 (usb)"])
    _patch(monkeypatch, fake)
    status = obs_phase2.reenforce_ndi_name(object(), "NDI cam1", "CAM1 (usb)")
    assert status == obs_phase2.REENFORCE_OFFLINE
    assert fake.current == ""
    assert fake.set_calls == []


def test_offline_when_desired_is_empty_never_sets(monkeypatch):
    fake = _FakeRpc(current="CAM1 (usb)", finder=["CAM1 (usb)"])
    _patch(monkeypatch, fake)
    status = obs_phase2.reenforce_ndi_name(object(), "NDI cam1", "")
    assert status == obs_phase2.REENFORCE_OFFLINE
    assert fake.set_calls == []


def test_verify_failed_on_readback_mismatch_is_loud_not_silent(monkeypatch):
    # desired IS discoverable, so we set it — but OBS stored a mangled value; the read-back catches
    # it and returns VERIFY_FAILED (never a false HEALED).
    fake = _FakeRpc(current="", finder=["CAM1 (usb)"], mangle_to="CAM1 (usb) MANGLED")
    _patch(monkeypatch, fake)
    status = obs_phase2.reenforce_ndi_name(object(), "NDI cam1", "CAM1 (usb)")
    assert status == obs_phase2.REENFORCE_VERIFY_FAILED
    assert len(fake.set_calls) == 1  # it DID attempt the set (that is where the mangle happened)


def test_status_constants_are_distinct():
    vals = {obs_phase2.REENFORCE_HEALED,
            obs_phase2.REENFORCE_OFFLINE,
            obs_phase2.REENFORCE_VERIFY_FAILED}
    assert len(vals) == 3
