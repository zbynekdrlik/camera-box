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
