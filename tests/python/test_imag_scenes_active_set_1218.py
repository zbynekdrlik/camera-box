"""issue 1218 -- imag active-set-aware NDI idle policy.

imag-nb thermal-throttles because it decodes camera NDI feeds OUTSIDE the active set for nothing
(an inactive camera's `NDI CAM{n}` receiver runs a full 1080p60 decode). The fix makes the imag
seed/enforce path active-set-aware: an INACTIVE camera's receiver is idled (ndi_source_name "" +
genlock_fifo off) while an ACTIVE camera keeps its baseline name -- routed through ONE policy point
(enforce_ndi_active_policy) that every vector uses.

These tests exercise the PURE decision functions directly (Tier-0, no WS), the state-file
resolution, enforce_ndi_active_policy against a FakeObs (both the ungated direct-set path and the
obs_phase2-gated reenforce path), and verify_parity end-to-end -- mirroring
test_imag_scenes_verify_parity.py's importlib/FakeObs conventions.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]

# The full fleet slot count imag seeds. ACTIVE/INACTIVE below are ILLUSTRATIVE fixtures for the
# general active/inactive idle-policy mechanism this file tests -- they are NOT tied to today's
# real CAMERA_ACTIVE_SET default (which, as of issue 1216's 2026-08-30 completion, is the full
# seven-camera fleet with nothing inactive). The mechanism must work for ANY active/inactive
# split, so keeping cam4+cam5 as the illustrative "some inactive" example here stays valid
# regardless of the live default in scripts/camera-set.sh.
ACTIVE = {1, 2, 3, 6, 7}
INACTIVE = {4, 5}


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _mod():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_active_set_ut")


def _obs_phase2():
    return _load(REPO / "scripts" / "obs_phase2.py", "obs_phase2_active_set_ut")


# --------------------------------------------------------------------------- parse / format


def test_parse_active_cams_variants():
    m = _mod()
    assert m.parse_active_cams("cam1 cam2 cam6") == {1, 2, 6}
    assert m.parse_active_cams("cam1,cam2 , cam6") == {1, 2, 6}
    assert m.parse_active_cams("CAM1 Cam2") == {1, 2}  # case-insensitive
    # None / blank / all-junk -> None (no set knowledge -> baseline-heal)
    assert m.parse_active_cams(None) is None
    assert m.parse_active_cams("   ") is None
    assert m.parse_active_cams("strih stream") is None


def test_format_active_cams_roundtrip():
    m = _mod()
    assert m.format_active_cams({6, 1, 2}) == "cam1 cam2 cam6"  # sorted
    assert m.format_active_cams(None) == ""
    assert m.format_active_cams(set()) == ""


# --------------------------------------------------------------------------- desired_ndi_state


def test_desired_ndi_state_active_and_inactive():
    m = _mod()
    for n in ACTIVE:
        assert m.desired_ndi_state(n, ACTIVE) == {
            "ndi_source_name": f"CAM{n} (usb)", "genlock_fifo": True}
    for n in INACTIVE:
        assert m.desired_ndi_state(n, ACTIVE) == {"ndi_source_name": "", "genlock_fifo": False}


def test_desired_ndi_state_touches_only_name_and_genlock():
    """The payload carries EXACTLY the two keys the policy owns -- so an overlay:True write leaves the
    per-source genlock_latency_ms_src 3ms pin (and everything else) untouched."""
    m = _mod()
    assert set(m.desired_ndi_state(2, ACTIVE)) == {"ndi_source_name", "genlock_fifo"}
    assert set(m.desired_ndi_state(4, ACTIVE)) == {"ndi_source_name", "genlock_fifo"}
    assert "latency" not in m.desired_ndi_state(2, ACTIVE)
    assert "genlock_latency_ms_src" not in m.desired_ndi_state(4, ACTIVE)


def test_desired_ndi_state_parity_with_obs_phase2_idle_restore():
    """The design mandates the payload be byte-for-byte obs_phase2._idle_restore_settings(...).
    Active <-> _idle_restore_settings("CAMn (usb)"); inactive <-> _idle_restore_settings("")."""
    m = _mod()
    op = _obs_phase2()
    for n in ACTIVE:
        assert m.desired_ndi_state(n, ACTIVE) == op._idle_restore_settings(f"CAM{n} (usb)")
    for n in INACTIVE:
        assert m.desired_ndi_state(n, ACTIVE) == op._idle_restore_settings("")


# --------------------------------------------------------------------------- #1158 discriminator


def test_is_deliberate_idle_discriminator():
    m = _mod()
    # deliberate idle: empty name AND genlock_fifo explicitly False
    assert m.is_deliberate_idle({"ndi_source_name": "", "genlock_fifo": False}) is True
    # accidental wedge: empty name but genlock_fifo still True (or absent) -> NOT deliberate
    assert m.is_deliberate_idle({"ndi_source_name": "", "genlock_fifo": True}) is False
    assert m.is_deliberate_idle({"ndi_source_name": ""}) is False  # genlock_fifo absent
    assert m.is_deliberate_idle({}) is False
    # a bound camera is never a deliberate idle
    assert m.is_deliberate_idle({"ndi_source_name": "CAM1 (usb)", "genlock_fifo": True}) is False


def test_ndi_policy_action_with_known_set():
    m = _mod()
    for n in ACTIVE:
        assert m.ndi_policy_action(n, ACTIVE) == ("reenforce", f"CAM{n} (usb)")
    for n in INACTIVE:
        assert m.ndi_policy_action(n, ACTIVE) == ("idle", "")


def test_ndi_policy_action_no_set_knowledge_heals_wedge_preserves_idle():
    m = _mod()
    # no knowledge + a deliberate idle -> leave it (preserve the operator/policy idle)
    assert m.ndi_policy_action(4, None, {"ndi_source_name": "", "genlock_fifo": False}) == ("leave", None)
    # no knowledge + an accidental wedge (empty name, genlock still on) -> heal to baseline
    assert m.ndi_policy_action(4, None, {"ndi_source_name": "", "genlock_fifo": True}) == (
        "reenforce", "CAM4 (usb)")
    # no knowledge + a normally-bound input -> reenforce (baseline-heal, pre-1218 behavior)
    assert m.ndi_policy_action(2, None, {"ndi_source_name": "CAM2 (usb)", "genlock_fifo": True}) == (
        "reenforce", "CAM2 (usb)")


# --------------------------------------------------------------------------- state file


def test_read_active_cams_state_file_missing_is_none(tmp_path):
    m = _mod()
    assert m.read_active_cams_state_file(str(tmp_path / "nope")) is None


def test_write_then_read_and_resolve_state_file(tmp_path):
    m = _mod()
    p = tmp_path / "imag-active-cams"
    m.write_active_cams_state_local("cam1 cam2 cam6", path=str(p))
    assert p.read_text() == "cam1 cam2 cam6\n"
    assert m.read_active_cams_state_file(str(p)) == "cam1 cam2 cam6"
    # resolve: flag wins over the file
    assert m.resolve_active_cams("cam7", state_path=str(p)) == {7}
    # resolve: no flag -> the state file
    assert m.resolve_active_cams(None, state_path=str(p)) == {1, 2, 6}
    # resolve: no flag + missing file -> None (baseline-heal preserved)
    assert m.resolve_active_cams(None, state_path=str(tmp_path / "gone")) is None
    # resolve: an empty state file -> None
    m.write_active_cams_state_local("", path=str(p))
    assert m.resolve_active_cams(None, state_path=str(p)) is None


# --------------------------------------------------------------------------- ndi_source_mismatches


def _actual_from_active(active):
    """Model a box where every inactive camera is correctly idled ('') and every active camera +
    every non-cam input is bound to its baseline."""
    m = _mod()
    actual = {}
    for name, want in m.CANONICAL_NDI_SOURCES.items():
        import re
        mm = re.fullmatch(r"NDI CAM(\d+)", name)
        if mm and int(mm.group(1)) not in active:
            actual[name] = ""
        else:
            actual[name] = want
    return actual


def test_ndi_source_mismatches_active_set_aware_idle_is_ok():
    m = _mod()
    actual = _actual_from_active(ACTIVE)
    # active-set-aware: an idled inactive camera is NOT a problem
    assert m.ndi_source_mismatches(actual, active_cams=ACTIVE) == []
    # without active_cams (pre-1218) the same box FAILS: an idled cam looks like a mismatch
    problems = m.ndi_source_mismatches(actual)
    assert any("NDI CAM4" in p for p in problems)


def test_ndi_source_mismatches_flags_inactive_still_bound():
    m = _mod()
    actual = _actual_from_active(ACTIVE)
    actual["NDI CAM4"] = "CAM4 (usb)"  # inactive but still decoding -> must be flagged NOT IDLE
    problems = m.ndi_source_mismatches(actual, active_cams=ACTIVE)
    assert any("NOT IDLE" in p and "NDI CAM4" in p for p in problems)


def test_ndi_source_mismatches_still_catches_active_drift():
    m = _mod()
    actual = _actual_from_active(ACTIVE)
    actual["NDI CAM2"] = "CAM9 (usb)"  # active camera drifted -> still a MISMATCH
    problems = m.ndi_source_mismatches(actual, active_cams=ACTIVE)
    assert any("MISMATCH" in p and "NDI CAM2" in p for p in problems)


def test_active_set_idle_report_lists_correctly_idled():
    m = _mod()
    actual = _actual_from_active(ACTIVE)
    idled = m.active_set_idle_report(actual, ACTIVE)
    assert set(idled) == {"NDI CAM4", "NDI CAM5"}
    assert m.active_set_idle_report(actual, None) == []


# --------------------------------------------------------------------------- enforce_ndi_active_policy


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


def test_enforce_ungated_idles_inactive_and_sets_active():
    """No `.ws` on the fake -> the ungated (direct SetInputSettings) path. Active cams get the
    baseline name + genlock on; inactive cams get idled ('' + genlock off) and read-back-verified."""
    m = _mod()
    obs = FakeObs()  # no ws
    result = m.enforce_ndi_active_policy(obs, ACTIVE)
    for n in INACTIVE:
        assert obs.settings[f"NDI CAM{n}"]["ndi_source_name"] == ""
        assert obs.settings[f"NDI CAM{n}"]["genlock_fifo"] is False
        assert result[n] == "idle:ok"
    for n in ACTIVE:
        assert obs.settings[f"NDI CAM{n}"]["ndi_source_name"] == f"CAM{n} (usb)"
        assert obs.settings[f"NDI CAM{n}"]["genlock_fifo"] is True
        assert result[n] == "active:set(ungated)"
    # every SetInputSettings the policy issued used overlay:True (preserves the 3ms pin)
    sets = [p for r, p in obs.calls if r == "SetInputSettings"]
    assert sets and all(p.get("overlay") is True for p in sets)


def test_enforce_no_set_knowledge_preserves_deliberate_idle_heals_wedge():
    m = _mod()
    initial = {
        "NDI CAM4": {"ndi_source_name": "", "genlock_fifo": False},   # deliberate idle -> preserve
        "NDI CAM5": {"ndi_source_name": "", "genlock_fifo": True},    # accidental wedge -> heal
        "NDI CAM1": {"ndi_source_name": "CAM1 (usb)", "genlock_fifo": True},  # bound -> reenforce
    }
    obs = FakeObs(initial=initial)  # no ws
    result = m.enforce_ndi_active_policy(obs, None)
    # deliberate idle untouched
    assert obs.settings["NDI CAM4"] == {"ndi_source_name": "", "genlock_fifo": False}
    assert result[4] == "idle-preserved"
    # wedge healed back to baseline
    assert obs.settings["NDI CAM5"]["ndi_source_name"] == "CAM5 (usb)"
    assert result[5] == "active:set(ungated)"


class FakeOp:
    """Stand-in for the obs_phase2 module: records reenforce_ndi_name calls, returns a configurable
    status, and carries the REENFORCE_* constants enforce_ndi_active_policy branches on."""

    REENFORCE_HEALED = "healed"
    REENFORCE_OFFLINE = "offline"
    REENFORCE_VERIFY_FAILED = "verify_failed"

    def __init__(self, status="healed"):
        self.calls = []
        self.status = status

    def reenforce_ndi_name(self, ws, inp, name):
        self.calls.append((ws, inp, name))
        return self.status


def test_enforce_gated_uses_reenforce_ndi_name(monkeypatch):
    """With `.ws` present AND obs_phase2 importable, active cams flow through the shared
    obs_phase2.reenforce_ndi_name (#795-safe, discoverability-gated) for the NAME; inactive cams
    still idle."""
    m = _mod()
    fake_op = FakeOp(status="healed")
    monkeypatch.setattr(m, "_obs_phase2_module", lambda: fake_op)
    ws_sentinel = object()
    obs = FakeObs(ws=ws_sentinel)
    result = m.enforce_ndi_active_policy(obs, ACTIVE)
    # active cams -> reenforce path, called with the raw ws + baseline name
    assert result[1] == "active:healed"
    assert (ws_sentinel, "NDI CAM1", "CAM1 (usb)") in fake_op.calls
    # reenforce_ndi_name owns the NAME — no ndi_source_name SetInputSettings for active cams
    active_name_sets = [p for r, p in obs.calls
                        if r == "SetInputSettings" and "ndi_source_name" in p["inputSettings"]
                        and p["inputSettings"]["ndi_source_name"] not in ("",)]
    assert active_name_sets == []
    # inactive cams still idled through the direct payload
    for n in INACTIVE:
        assert obs.settings[f"NDI CAM{n}"]["ndi_source_name"] == ""
        assert result[n] == "idle:ok"


def test_enforce_gated_reactivation_restores_genlock_fifo():
    """#1218 review RED: obs_phase2.reenforce_ndi_name writes ONLY ndi_source_name. A camera moved
    inactive->active carries a persisted genlock_fifo:False (from its idle), so the gated path MUST
    also restore genlock_fifo:True (once the name HEALED) or the receiver decodes again but silently
    bypasses the genlock FIFO."""
    m = _mod()
    # each _mod() call loads a FRESH module by importlib, so a direct setattr never leaks across tests
    fake_op = FakeOp(status="healed")
    m._obs_phase2_module = lambda: fake_op
    # cam1 reactivated: persisted idle state (name cleared, genlock_fifo False)
    obs = FakeObs(initial={"NDI CAM1": {"ndi_source_name": "", "genlock_fifo": False}}, ws=object())
    result = m.enforce_ndi_active_policy(obs, {1})
    assert result[1] == "active:healed"
    # genlock_fifo restored to True even though reenforce_ndi_name only touched the name
    assert obs.settings["NDI CAM1"]["genlock_fifo"] is True, (
        "a reactivated camera must genlock again — genlock_fifo True must be restored on the gated path")


def test_enforce_gated_offline_does_not_touch_genlock_fifo():
    """When the name is OFFLINE (not in the finder) reenforce does not set it — and we must NOT set
    genlock_fifo:True either (that would run the consume path against an empty '' input, #70)."""
    m = _mod()
    fake_op = FakeOp(status="offline")
    m._obs_phase2_module = lambda: fake_op
    obs = FakeObs(initial={"NDI CAM1": {"ndi_source_name": "", "genlock_fifo": False}}, ws=object())
    result = m.enforce_ndi_active_policy(obs, {1})
    assert result[1] == "active:offline"
    assert obs.settings["NDI CAM1"]["genlock_fifo"] is False, (
        "an OFFLINE (unhealed) name must not get genlock_fifo True — no empty-queue consume path (#70)")


def test_enforce_idle_verify_failure_is_reported():
    """If a receiver refuses to clear its name, the idle result must be a LOUD VERIFY_FAILED, never a
    silent 'ok' (#1158 -- an unexpected non-empty name is a real problem)."""
    m = _mod()

    class StubbornObs(FakeObs):
        def req(self, rtype, payload=None, ignore_err=False):
            p = payload or {}
            if rtype == "SetInputSettings" and p.get("inputSettings", {}).get("ndi_source_name") == "":
                self.calls.append((rtype, p))
                return {}  # pretend the clear did not stick
            if rtype == "GetInputSettings" and p["inputName"] in ("NDI CAM4", "NDI CAM5"):
                return {"inputSettings": {"ndi_source_name": "CAM4 (usb)"}}
            return super().req(rtype, payload, ignore_err)

    obs = StubbornObs()
    result = m.enforce_ndi_active_policy(obs, ACTIVE)
    assert "VERIFY_FAILED" in result[4]


def test_enforce_idle_readback_failure_is_unverified_not_ok():
    """#1218 review: a FAILED idle read-back (GetInputSettings suppressed by ignore_err -> {}) must
    be UNVERIFIED, never a silent idle:ok — the docstring promises 'never a silent failure to idle'."""
    m = _mod()

    class BlindReadObs(FakeObs):
        def req(self, rtype, payload=None, ignore_err=False):
            p = payload or {}
            if rtype == "GetInputSettings" and p["inputName"] in ("NDI CAM4", "NDI CAM5"):
                return {}  # read-back failed / returned nothing
            return super().req(rtype, payload, ignore_err)

    obs = BlindReadObs()
    result = m.enforce_ndi_active_policy(obs, ACTIVE)
    assert "UNVERIFIED" in result[4] and "UNVERIFIED" in result[5], result


def test_enforce_routes_payloads_through_desired_ndi_state():
    """#1218 review (yellow): the REAL writes must be the parity-tested desired_ndi_state payloads,
    not a divergent inline copy. The ungated active write and the idle write must equal
    desired_ndi_state's active/idle payloads exactly."""
    m = _mod()
    obs = FakeObs()  # ungated (no ws)
    m.enforce_ndi_active_policy(obs, ACTIVE)
    for n in ACTIVE:
        assert obs.settings[f"NDI CAM{n}"] == m.desired_ndi_state(n, ACTIVE)
    for n in INACTIVE:
        assert obs.settings[f"NDI CAM{n}"] == m.desired_ndi_state(n, ACTIVE)


# --------------------------------------------------------------------------- verify_parity end-to-end


class ParityObs:
    """Models GetSceneList / GetInputList / GetInputSettings for verify_parity()."""

    def __init__(self, ndi_bindings, scene_order):
        self.ndi = ndi_bindings
        self.scene_order = scene_order
        self.mutations = []

    def req(self, rtype, payload=None, ignore_err=False):
        p = payload or {}
        if rtype == "GetSceneList":
            # WS returns the REVERSE of the canonical top-to-bottom order
            return {"scenes": [{"sceneName": s} for s in reversed(self.scene_order)]}
        if rtype == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": "ndi_source"} for n in self.ndi]}
        if rtype == "GetInputSettings":
            return {"inputSettings": {"ndi_source_name": self.ndi.get(p["inputName"])}}
        # any create/set is a mutation -- verify_parity must be read-only
        self.mutations.append((rtype, p))
        return {}


def _canonical_order():
    m = _mod()
    return list(m.CANONICAL_SCENE_ORDER)


def test_verify_parity_passes_on_active_set_idled_box():
    m = _mod()
    bindings = _actual_from_active(ACTIVE)
    obs = ParityObs(bindings, _canonical_order())
    m.verify_parity(obs, active_cams=ACTIVE)  # must NOT sys.exit
    assert obs.mutations == [], "verify_parity must stay read-only"


def test_verify_parity_keeps_ndi_sources_ok_a_standalone_whole_line(capsys):
    """verify-imag.sh's imag_parity_output_ok matches `ndi sources: OK` with grep -qxF (WHOLE line),
    so the active-set idle report MUST be a SEPARATE line, never an inline suffix on it."""
    m = _mod()
    bindings = _actual_from_active(ACTIVE)  # cam4/cam5 idled
    obs = ParityObs(bindings, _canonical_order())
    m.verify_parity(obs, active_cams=ACTIVE)
    lines = capsys.readouterr().out.splitlines()
    assert "ndi sources: OK" in lines, "the whole-line 'ndi sources: OK' must survive for grep -qxF"
    assert "scene order: OK" in lines
    assert any(ln.startswith("ndi idle (active-set): ") for ln in lines)


def test_verify_parity_fails_when_inactive_still_bound():
    m = _mod()
    bindings = _actual_from_active(ACTIVE)
    bindings["NDI CAM4"] = "CAM4 (usb)"  # inactive camera still decoding
    obs = ParityObs(bindings, _canonical_order())
    raised = False
    try:
        m.verify_parity(obs, active_cams=ACTIVE)
    except SystemExit as e:
        raised = e.code not in (0, None)
    assert raised, "verify_parity must fail when an inactive camera is still bound (still decoding)"


# --------------------------------------------------------------------------- main() state-file guard


def _run_script(args, state_file):
    # Redirect ONLY the state-file path via the IMAG_ACTIVE_CAMS_STATE_FILE seam — NEVER override
    # HOME, which would move Python's user-site (~/.local) and break `import websocket` in the child.
    import os as _os
    import subprocess as _sp
    env = dict(_os.environ)
    env["IMAG_ACTIVE_CAMS_STATE_FILE"] = str(state_file)
    return _sp.run(
        [sys.executable, str(REPO / "scripts" / "imag_scenes.py")] + args,
        capture_output=True, text=True, timeout=30, env=env,
    )


def test_main_unparseable_active_cams_warns_and_skips_state_write(tmp_path):
    """#1218 review (blue): an EXPLICIT --active-cams that parses to no cam numbers must WARN and NOT
    write junk to the box's state file (which the next --bootstrap would read). It still baseline-heals
    (safe), but silently writing junk is the failure. (The run then exits non-zero at the unreachable
    OBS connect — expected; we assert only the WARN + the absent state file.)"""
    sf = tmp_path / "imag-active-cams"
    r = _run_script(["--host", "127.0.0.1", "--active-cams", "notacam", "--enforce-ndi-policy"], sf)
    assert "WARN #1218" in r.stderr, r.stderr
    assert not sf.exists(), "junk --active-cams must NOT be written to the state file"


def test_main_valid_active_cams_writes_state_file_locally(tmp_path):
    """A VALID explicit --active-cams on the local host writes the one-line state file (before the WS
    connect), so the next on-box --bootstrap reads a fresh copy."""
    sf = tmp_path / "imag-active-cams"
    r = _run_script(["--host", "127.0.0.1", "--active-cams", "cam1 cam2", "--enforce-ndi-policy"], sf)
    assert sf.exists(), r.stderr
    assert sf.read_text() == "cam1 cam2\n"
