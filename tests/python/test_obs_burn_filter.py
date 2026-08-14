"""#334 — unit tests for scripts/obs_burn_filter.py burn-on gating + filter re-enable.

Root cause (#334): on the all-cambox run the strih(911002)+stream(911004) measurement burns
were ABSENT from the recording because the `DistroAV QR Burn (latency probe)` EFFECT filter on
the program input was `filterEnabled=False`. A disabled effect filter's video_render is never
invoked by OBS, so the burn never renders even though the per-source `genlock_burn` bool is true
and the C++ setter fires. Two pure-logic defects in obs_burn_filter.py made this silent:

  1. `check` computed burn_on without checking the filter's ENABLED state, so a present-but-
     disabled filter reported burn_on=True (FALSE POSITIVE) — the recording-e2e.sh [4b/8]
     pre-record gate passed while the burn never rendered.
  2. `add` early-returned when the filter already existed, so it never re-enabled a filter that
     existed-but-was-disabled.

These tests pin the corrected logic with NO live OBS: `compute_burn_on` is exercised directly,
and `cmd_check`/`cmd_add` are driven through a fake `_rpc` that returns canned WebSocket
responses and records the calls issued.
"""
import pathlib
import sys

# obs_burn_filter does `from obs_phase2 import _conn, _rpc`, so the scripts/ dir must be importable.
_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import obs_burn_filter  # noqa: E402


class FakeObs:
    """Minimal in-memory OBS-WebSocket stand-in: serves GetSourceFilterList / GetInputSettings
    and applies SetSourceFilterEnabled / SetInputSettings / CreateSourceFilter, recording calls."""

    def __init__(self, *, filter_present, filter_enabled=True, genlock_burn=False,
                 kind_registered=True):
        self.filters = []
        if filter_present:
            self.filters.append({
                "filterName": obs_burn_filter.BURN_FILTER_NAME,
                "filterKind": obs_burn_filter.BURN_FILTER_KIND,
                "filterEnabled": filter_enabled,
            })
        self.genlock_burn = genlock_burn
        self.kind_registered = kind_registered
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False):
        self.calls.append((method, params or {}))
        if method == "GetSourceFilterList":
            return {"filters": [dict(f) for f in self.filters]}
        if method == "GetSourceFilterKindList":
            kinds = [obs_burn_filter.BURN_FILTER_KIND] if self.kind_registered else []
            return {"sourceFilterKinds": kinds}
        if method == "GetInputSettings":
            return {"inputSettings": {obs_burn_filter.BURN_SETTING: self.genlock_burn}}
        if method == "SetInputSettings":
            self.genlock_burn = params["inputSettings"][obs_burn_filter.BURN_SETTING]
            return {}
        if method == "SetSourceFilterEnabled":
            for f in self.filters:
                if f["filterName"] == params["filterName"]:
                    f["filterEnabled"] = params["filterEnabled"]
            return {}
        if method == "CreateSourceFilter":
            self.filters.append({
                "filterName": params["filterName"],
                "filterKind": params["filterKind"],
                "filterEnabled": True,
            })
            return {}
        return {}

    def set_enabled_calls(self):
        return [
            (m, p) for (m, p) in self.calls
            if m == "SetSourceFilterEnabled"
            and p.get("filterName") == obs_burn_filter.BURN_FILTER_NAME
        ]


# ---- compute_burn_on: the pure gate (genlock_burn AND present AND enabled) --------------------

def test_compute_burn_on_false_when_filter_disabled():
    # #334 regression: a present-but-DISABLED effect filter never renders the burn, so burn_on
    # MUST be False even with genlock_burn=True. This is the false-positive the gate now catches.
    assert obs_burn_filter.compute_burn_on(True, True, False) is False


def test_compute_burn_on_true_only_when_burn_present_and_enabled():
    assert obs_burn_filter.compute_burn_on(True, True, True) is True


def test_compute_burn_on_false_when_genlock_off_or_absent():
    assert obs_burn_filter.compute_burn_on(False, True, True) is False   # bool off
    assert obs_burn_filter.compute_burn_on(None, True, True) is False    # bool unknown
    assert obs_burn_filter.compute_burn_on(True, False, True) is False   # filter absent
    assert obs_burn_filter.compute_burn_on(True, False, None) is False   # absent => enabled None


# ---- cmd_check: burn_on must reflect the enabled state, and print filter_enabled --------------

def test_cmd_check_burn_on_false_when_present_but_disabled(monkeypatch, capsys):
    # The #334 false positive end-to-end through cmd_check: genlock_burn=True + filter present
    # but DISABLED must print burn_on=False (old code printed burn_on=True).
    fake = FakeObs(filter_present=True, filter_enabled=False, genlock_burn=True)
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    obs_burn_filter.cmd_check(object(), "NDI cam5")
    out = capsys.readouterr().out
    assert "burn_on=False" in out
    assert "filter_enabled=False" in out


def test_cmd_check_burn_on_true_when_present_and_enabled(monkeypatch, capsys):
    fake = FakeObs(filter_present=True, filter_enabled=True, genlock_burn=True)
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    obs_burn_filter.cmd_check(object(), "NDI cam5")
    out = capsys.readouterr().out
    assert "burn_on=True" in out
    assert "filter_enabled=True" in out


# ---- cmd_add: must re-enable a present-but-disabled filter ------------------------------------

def test_cmd_add_reenables_present_but_disabled_filter(monkeypatch, capsys):
    # #334: add must ENABLE an existing-but-disabled filter (old code early-returned and left it
    # disabled). After add, a SetSourceFilterEnabled{filterEnabled:true} must have been issued and
    # genlock_burn must be true.
    fake = FakeObs(filter_present=True, filter_enabled=False, genlock_burn=False)
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    obs_burn_filter.cmd_add(object(), "NDI cam5")
    enable_calls = fake.set_enabled_calls()
    assert any(p.get("filterEnabled") is True for (_, p) in enable_calls), \
        "cmd_add must issue SetSourceFilterEnabled{filterEnabled:true} for a disabled filter"
    assert fake.filters[0]["filterEnabled"] is True
    assert fake.genlock_burn is True


# =============================================================================================
# #938/#1011 — the EXHAUSTIVE burn sweep enumerator. The burn OFF/CHECK/RESTORE target set must be
# enumerated from OBS reality (GetInputList -> every ndi_source input), never a static 3-input
# list (#938 rig-mode obs_burn_targets) nor a CAMERA_ACTIVE_SET-derived list (#1011 recording-e2e).
# Live 2026-08-07 pre-broadcast leak: strih 'NDI cam3' (cam4's on-air feed, OUTSIDE the active set)
# and stream 'phase2-probe-src' carried genlock_burn=true past the pinned OFF/CHECK/RESTORE lists;
# only the pixel proof caught it. These tests reproduce that leak against a multi-input fake WS.
import json  # noqa: E402


class FakeFleetObs:
    """Multi-input in-memory OBS-WebSocket stand-in for the exhaustive sweep: serves GetInputList
    + per-input GetInputSettings/GetSourceFilterList and applies SetInputSettings, recording calls,
    so cmd_sweep_check/cmd_sweep_off are exercised with NO live OBS."""

    def __init__(self, inputs, kind_registered=True):
        self.inputs = {}
        self.order = []
        for i in inputs:
            self.inputs[i["name"]] = {
                "kind": i.get("kind", "ndi_source"),
                "genlock_burn": i.get("genlock_burn", False),
                "filter_present": i.get("filter_present", True),
                "filter_enabled": i.get("filter_enabled", True),
            }
            self.order.append(i["name"])
        self.kind_registered = kind_registered
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False):
        params = params or {}
        self.calls.append((method, params))
        if method == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": self.inputs[n]["kind"]}
                               for n in self.order]}
        if method == "GetSourceFilterKindList":
            kinds = [obs_burn_filter.BURN_FILTER_KIND] if self.kind_registered else []
            return {"sourceFilterKinds": kinds}
        if method == "GetInputSettings":
            st = self.inputs.get(params.get("inputName"), {})
            return {"inputSettings": {obs_burn_filter.BURN_SETTING: st.get("genlock_burn")}}
        if method == "GetSourceFilterList":
            st = self.inputs.get(params.get("sourceName"), {})
            if st.get("filter_present"):
                return {"filters": [{
                    "filterName": obs_burn_filter.BURN_FILTER_NAME,
                    "filterKind": obs_burn_filter.BURN_FILTER_KIND,
                    "filterEnabled": st.get("filter_enabled", True),
                }]}
            return {"filters": []}
        if method == "SetInputSettings":
            n = params["inputName"]
            self.inputs[n]["genlock_burn"] = params["inputSettings"][obs_burn_filter.BURN_SETTING]
            return {}
        return {}


# A strih-like fleet: cam1 program (off), an out-of-set leaked cam3, a leaked probe input, and a
# NON-ndi source that must never be swept.
_LEAKY_FLEET = [
    {"name": "NDI cam1", "kind": "ndi_source", "genlock_burn": False},
    {"name": "NDI cam3", "kind": "ndi_source", "genlock_burn": True},          # leak (issue 246/844)
    {"name": "phase2-probe-src", "kind": "ndi_source", "genlock_burn": True},  # leak
    {"name": "Colour Bars", "kind": "color_source_v3", "filter_present": False},
]


def test_ndi_source_input_names_filters_to_ndi_kind():
    names = obs_burn_filter.ndi_source_input_names([
        {"inputName": "NDI cam1", "inputKind": "ndi_source"},
        {"inputName": "Colour Bars", "inputKind": "color_source_v3"},
        {"inputName": "NDI cam3", "inputKind": "ndi_source"},
    ])
    assert names == ["NDI cam1", "NDI cam3"]


def test_sweep_check_flags_leaked_out_of_set_inputs(monkeypatch, capsys):
    fake = FakeFleetObs(_LEAKY_FLEET)
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    rc = obs_burn_filter.cmd_sweep_check(object(), "10.77.9.202")
    data = json.loads(capsys.readouterr().out.strip())
    by = {d["input"]: d["burn_on"] for d in data}
    assert by["NDI cam3"] is True, "the out-of-set leaked input must be flagged burn_on"
    assert by["phase2-probe-src"] is True
    assert by["NDI cam1"] is False
    assert "Colour Bars" not in by, "a non-ndi_source input must never be swept"
    assert rc != 0, "a live burn anywhere must make sweep-check exit non-zero (contract gate)"


def test_sweep_off_clears_every_leaked_ndi_input(monkeypatch, capsys):
    fake = FakeFleetObs(_LEAKY_FLEET)
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    rc = obs_burn_filter.cmd_sweep_off(object(), "10.77.9.202")
    assert fake.inputs["NDI cam3"]["genlock_burn"] is False, \
        "sweep-off must clear an out-of-set input, not just a pinned program input"
    assert fake.inputs["phase2-probe-src"]["genlock_burn"] is False
    assert rc == 0
    rc2 = obs_burn_filter.cmd_sweep_check(object(), "10.77.9.202")
    assert rc2 == 0, "after sweep-off no ndi input renders a burn"


def test_sweep_off_noop_when_all_clear(monkeypatch, capsys):
    fake = FakeFleetObs([{"name": "NDI cam1", "genlock_burn": False}])
    monkeypatch.setattr(obs_burn_filter, "_rpc", fake.rpc)
    rc = obs_burn_filter.cmd_sweep_off(object(), "10.77.9.202")
    assert rc == 0
    assert "no ndi input" in capsys.readouterr().out.lower()
