"""issue 1230 -- imag NDI-name heal-all contract (revert of #1218's idle policy).

Owner ruling 2026-08-30 ("ziadne nechcem mat hlupe idle policy"): the #1218 active-set idle policy
is REMOVED. imag keeps ALL seven cameras NAMED + alive. What is KEPT is the #1158 name-healing via
the shared #795-safe obs_phase2.reenforce_ndi_name.

These tests lock the NEW contract:
  1. every #1218 idle-policy symbol is GONE from imag_scenes.py (no active/inactive split, no idle),
  2. the ONE enforce_ndi_names(obs) point heals ALL of CAMS to their baseline name (ungated + gated),
  3. the gated path restores genlock_fifo True only on HEALED, never on OFFLINE (no #70 empty-queue),
  4. verify_parity / ndi_source_mismatches check every camera's baseline name unconditionally.

Tier-0: pure importlib + a FakeObs/FakeOp modelling the WS calls -- no OBS/ssh/live rig. Mirrors
test_imag_scenes_verify_parity.py's importlib/FakeObs conventions.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]

# The full fleet slot count imag seeds (env-overridable IMAG_SCENE_CAM_COUNT, default 7).
CAMS = list(range(1, 8))


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _mod():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_ndi_heal_ut")


# --------------------------------------------------------------------------- the idle policy is GONE


def test_idle_policy_symbols_are_removed():
    """issue 1230: the whole #1218 idle-policy surface must be gone -- no active/inactive split,
    no idle payload, no state-file/CLI plumbing."""
    m = _mod()
    for gone in ("desired_ndi_state", "is_deliberate_idle", "ndi_policy_action",
                 "enforce_ndi_active_policy", "parse_active_cams", "format_active_cams",
                 "resolve_active_cams", "read_active_cams_state_file",
                 "write_active_cams_state_local", "write_active_cams_state_remote",
                 "active_set_idle_report", "ACTIVE_CAMS_STATE_FILE"):
        assert not hasattr(m, gone), f"{gone} must be removed (issue 1230 -- no idle policy)"
    # the heal-all replacement exists; the KEPT lazy obs_phase2 import stays
    assert hasattr(m, "enforce_ndi_names"), "enforce_ndi_names(obs) must be the new heal point"
    assert hasattr(m, "_obs_phase2_module"), "the lazy obs_phase2 import (#1158 healing) stays"


def test_seed_and_verify_parity_no_longer_take_active_cams():
    """The active_cams parameter is dropped from every threading site."""
    import inspect
    m = _mod()
    assert "active_cams" not in inspect.signature(m.seed).parameters
    assert "active_cams" not in inspect.signature(m.verify_parity).parameters
    assert "active_cams" not in inspect.signature(m.ndi_source_mismatches).parameters
    assert "active_cams" not in inspect.signature(m.enforce_ndi_names).parameters


# --------------------------------------------------------------------------- enforce_ndi_names


class FakeObs:
    """Records req() calls and models per-input settings with overlay-merge semantics."""

    def __init__(self, initial=None, ws=None):
        self.calls = []
        self.settings = {k: dict(v) for k, v in (initial or {}).items()}
        if ws is not None:
            self.ws = ws

    def req(self, rtype, payload=None, ignore_err=False):
        p = payload or {}
        self.calls.append((rtype, p))
        if rtype == "GetInputSettings":
            return {"inputSettings": dict(self.settings.get(p["inputName"], {}))}
        if rtype == "SetInputSettings":
            cur = self.settings.setdefault(p["inputName"], {})
            cur.update(p.get("inputSettings", {}))  # overlay:True merge
            return {}
        return {}


def test_enforce_ungated_names_and_genlocks_every_camera():
    """No `.ws` on the fake -> the ungated direct-set path. EVERY camera (no active/inactive split)
    gets its baseline name + genlock_fifo True, all with overlay:True (preserves the 3ms pin)."""
    m = _mod()
    obs = FakeObs()  # no ws
    result = m.enforce_ndi_names(obs)
    for n in CAMS:
        assert obs.settings[f"NDI CAM{n}"]["ndi_source_name"] == f"CAM{n} (usb)"
        assert obs.settings[f"NDI CAM{n}"]["genlock_fifo"] is True
        assert result[n] == "name:set(ungated)"
    sets = [p for r, p in obs.calls if r == "SetInputSettings"]
    assert sets and all(p.get("overlay") is True for p in sets)
    # never an empty name / idle payload written anywhere
    assert all(p["inputSettings"].get("ndi_source_name", "x") != "" for p in sets)


class FakeOp:
    """Stand-in for the obs_phase2 module: records reenforce_ndi_name calls, returns a configurable
    status, carries the REENFORCE_* constants enforce_ndi_names branches on."""

    REENFORCE_HEALED = "healed"
    REENFORCE_OFFLINE = "offline"
    REENFORCE_VERIFY_FAILED = "verify_failed"

    def __init__(self, status="healed"):
        self.calls = []
        self.status = status

    def reenforce_ndi_name(self, ws, inp, name):
        self.calls.append((ws, inp, name))
        return self.status


def test_enforce_gated_uses_reenforce_ndi_name_for_every_camera():
    """With `.ws` present AND obs_phase2 importable, EVERY camera flows through the shared
    #795-safe obs_phase2.reenforce_ndi_name for the NAME (never a divergent inline set)."""
    m = _mod()
    fake_op = FakeOp(status="healed")
    m._obs_phase2_module = lambda: fake_op
    ws_sentinel = object()
    obs = FakeObs(ws=ws_sentinel)
    result = m.enforce_ndi_names(obs)
    for n in CAMS:
        assert (ws_sentinel, f"NDI CAM{n}", f"CAM{n} (usb)") in fake_op.calls
        assert result[n] == "name:healed"
    # reenforce_ndi_name owns the NAME -- no direct ndi_source_name set on the gated path
    name_sets = [p for r, p in obs.calls
                 if r == "SetInputSettings" and "ndi_source_name" in p["inputSettings"]]
    assert name_sets == []


def test_enforce_gated_restores_genlock_fifo_only_on_heal():
    """On HEALED the gated path restores genlock_fifo True (a box carrying a persisted #1218 idle
    genlock_fifo False must genlock again, not decode-and-bypass -- kept from the #1218 review)."""
    m = _mod()
    fake_op = FakeOp(status="healed")
    m._obs_phase2_module = lambda: fake_op
    obs = FakeObs(initial={"NDI CAM1": {"ndi_source_name": "", "genlock_fifo": False}}, ws=object())
    result = m.enforce_ndi_names(obs)
    assert result[1] == "name:healed"
    assert obs.settings["NDI CAM1"]["genlock_fifo"] is True
    # the genlock restore is overlay:True (only touches genlock_fifo, preserves the 3ms pin)
    fifo_sets = [p for r, p in obs.calls
                 if r == "SetInputSettings" and "genlock_fifo" in p["inputSettings"]]
    assert fifo_sets and all(p.get("overlay") is True for p in fifo_sets)


def test_enforce_gated_offline_does_not_touch_genlock_fifo():
    """An OFFLINE (not-in-finder) name is left as-is -- we must NOT set genlock_fifo True either
    (that would run the consume path against an empty '' input, #70)."""
    m = _mod()
    fake_op = FakeOp(status="offline")
    m._obs_phase2_module = lambda: fake_op
    obs = FakeObs(initial={"NDI CAM1": {"ndi_source_name": "", "genlock_fifo": False}}, ws=object())
    result = m.enforce_ndi_names(obs)
    assert result[1] == "name:offline"
    assert obs.settings["NDI CAM1"]["genlock_fifo"] is False, (
        "an OFFLINE (unhealed) name must not get genlock_fifo True -- no empty-queue consume path (#70)")


# --------------------------------------------------------------------------- verify parity (all-named)


def test_ndi_source_mismatches_flags_an_empty_binding_as_a_problem():
    """issue 1230: no idle exemption -- an empty `NDI CAM{n}` binding is ALWAYS a mismatch now."""
    m = _mod()
    good = dict(m.CANONICAL_NDI_SOURCES)
    assert m.ndi_source_mismatches(good) == []
    bad = dict(good)
    bad["NDI CAM4"] = ""  # would have been "correctly idled" under #1218 -- now a problem
    problems = m.ndi_source_mismatches(bad)
    assert any("NDI CAM4" in p for p in problems), problems


def test_verify_parity_ok_on_all_named_box_and_never_writes():
    m = _mod()

    class ParityObs:
        def __init__(self):
            self.wrote = False

        def req(self, rtype, payload=None, ignore_err=False):
            if rtype == "GetSceneList":
                # canonical order is the REVERSE of what verify_parity expects, per its own reversal
                order = list(reversed(m.CANONICAL_SCENE_ORDER))
                return {"scenes": [{"sceneName": s} for s in order]}
            if rtype == "GetInputList":
                return {"inputs": [{"inputName": k, "inputKind": "ndi_source"}
                                   for k in m.CANONICAL_NDI_SOURCES]}
            if rtype == "GetInputSettings":
                name = payload["inputName"]
                return {"inputSettings": {"ndi_source_name": m.CANONICAL_NDI_SOURCES[name]}}
            self.wrote = True
            return {}

    obs = ParityObs()
    m.verify_parity(obs)  # must not sys.exit on a fully-named box
    assert obs.wrote is False, "verify_parity must be READ-ONLY"
