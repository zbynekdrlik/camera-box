"""#791 -- imag reprovision parity: prove OPERATOR parity, not just system parity.

The replacement notebook (.187) was certified "ready" by verify-imag.sh while missing 4 NDI
sources, 4 scenes (incl. Cam 7/MV Cam 7), and carrying a scrambled 13-scene order instead of the
canonical 17-scene layout (Scene, Cam 7..Cam 1, resolume imag, MV Cam 1..7, MW resolume imag) --
because nothing ever checked the FULL scene set/order or the Resolume/overlay NDI bindings, only
the "Cam N"/"MV Cam N" COUNT (imag_scenes.py's bare seed() report).

These tests exercise the PURE comparison functions (scene_order_mismatch / ndi_source_mismatches)
directly, and verify_parity() end-to-end against a FakeObs modelling both a fully-correct box and
several distinct partial-parity boxes (missing cam7, missing Resolume sources, scrambled order) --
mirroring test_imag_scenes_transform_preserve.py's FakeObs/importlib pattern.
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
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_verify_parity_under_test")


# The exact live-captured canonical order (byte-identical on .182 and .187, 2026-07-27/28).
CANONICAL_ORDER = [
    "Scene", "Cam 7", "Cam 6", "Cam 5", "Cam 4", "Cam 3", "Cam 2", "Cam 1",
    "resolume imag",
    "MV Cam 1", "MV Cam 2", "MV Cam 3", "MV Cam 4", "MV Cam 5", "MV Cam 6", "MV Cam 7",
    "MW resolume imag",
]

CANONICAL_NDI = {
    "NDI CAM1": "CAM1 (usb)", "NDI CAM2": "CAM2 (usb)", "NDI CAM3": "CAM3 (usb)",
    "NDI CAM4": "CAM4 (usb)", "NDI CAM5": "CAM5 (usb)", "NDI CAM6": "CAM6 (usb)",
    "NDI CAM7": "CAM7 (usb)",
    "MW imag resolume": "RESOLUME-SNV (Arena - To imag obs)",
    "NDI resolume imag": "RESOLUME-SNV (Arena - To imag obs)",
    "imag overlay": "RESOLUME-SNV (Arena - imag overlay)",
}


def test_canonical_constants_match_the_live_captured_layout():
    mod = _scenes_module()
    assert list(mod.CAMS) == [1, 2, 3, 4, 5, 6, 7]
    assert mod.CANONICAL_SCENE_ORDER == CANONICAL_ORDER
    assert mod.CANONICAL_NDI_SOURCES == CANONICAL_NDI


def test_scene_order_mismatch_ok_on_exact_canonical_order():
    mod = _scenes_module()
    assert mod.scene_order_mismatch(list(CANONICAL_ORDER)) == ""


def test_scene_order_mismatch_reports_missing_cam7_scenes():
    """The #791 live incident, reproduced: the replacement notebook's fresh scene set was missing
    Cam 7 and MV Cam 7 entirely (imag_scenes.py's own pre-fix CAMS=range(1, 7) never created
    them)."""
    mod = _scenes_module()
    without_cam7 = [s for s in CANONICAL_ORDER if s not in ("Cam 7", "MV Cam 7")]
    problem = mod.scene_order_mismatch(without_cam7)
    assert problem != ""
    assert "Cam 7" in problem and "MV Cam 7" in problem


def test_scene_order_mismatch_reports_missing_resolume_scenes():
    """The #791 live incident: a from-scratch box's automated seeder never creates the hand-built
    'resolume imag' / 'MW resolume imag' scenes at all (imag_scenes.py's own #785 OPERATOR-WINS
    carve-out deliberately never touches non-Cam-N scenes)."""
    mod = _scenes_module()
    without_resolume = [s for s in CANONICAL_ORDER if "resolume" not in s.lower()]
    problem = mod.scene_order_mismatch(without_resolume)
    assert problem != ""
    assert "resolume imag" in problem and "MW resolume imag" in problem


def test_scene_order_mismatch_reports_a_pure_ordering_drift():
    """Same SET of scenes, wrong SEQUENCE -- the #791 live incident's 13-scene inverted grouping
    (MV block first, then Cam block) had roughly the right membership but the wrong order."""
    mod = _scenes_module()
    scrambled = list(reversed(CANONICAL_ORDER))
    problem = mod.scene_order_mismatch(scrambled)
    assert problem != ""
    assert "ORDER" in problem


def test_ndi_source_mismatches_empty_on_exact_canonical_bindings():
    mod = _scenes_module()
    assert mod.ndi_source_mismatches(dict(CANONICAL_NDI)) == []


def test_ndi_source_mismatches_reports_missing_resolume_sources():
    """The #791 live incident: 'NDI resolume imag' / 'MW imag resolume' / 'imag overlay' were all
    absent on the replacement notebook."""
    mod = _scenes_module()
    partial = {"NDI CAM1": "CAM1 (usb)"}
    problems = mod.ndi_source_mismatches(partial)
    assert any("NDI CAM7" in p for p in problems)
    assert any("MW imag resolume" in p for p in problems)
    assert any("NDI resolume imag" in p for p in problems)
    assert any("imag overlay" in p for p in problems)


def test_ndi_source_mismatches_reports_a_wrong_binding_not_just_presence():
    mod = _scenes_module()
    wrong = dict(CANONICAL_NDI)
    wrong["NDI CAM3"] = "CAM5 (usb)"  # bound to the wrong physical camera
    problems = mod.ndi_source_mismatches(wrong)
    assert len(problems) == 1
    assert "NDI CAM3" in problems[0] and "CAM5 (usb)" in problems[0] and "CAM3 (usb)" in problems[0]


class FakeParityObs:
    """Models GetSceneList / GetInputList / GetInputSettings for verify_parity()'s WS calls."""

    def __init__(self, ws_scene_order, ndi_sources):
        # GetSceneList's own array order is the REVERSE of the canonical/JSON order (live-verified
        # against .187, 2026-07-28) -- this fixture takes the WS-shaped (reversed) order directly,
        # matching what a real obs-websocket response actually looks like.
        self._scenes = ws_scene_order
        self._ndi = ndi_sources
        self.calls = []

    def req(self, rtype, payload=None, ignore_err=False):
        self.calls.append((rtype, payload or {}))
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self._scenes]}
        if rtype == "GetInputList":
            return {"inputs": [
                {"inputName": n, "inputKind": "ndi_source"} for n in self._ndi
            ]}
        if rtype == "GetInputSettings":
            name = payload["inputName"]
            return {"inputSettings": {"ndi_source_name": self._ndi.get(name)}}
        return {}


def test_verify_parity_prints_ok_and_never_writes_anything_on_a_fully_correct_box():
    mod = _scenes_module()
    obs = FakeParityObs(list(reversed(CANONICAL_ORDER)), dict(CANONICAL_NDI))
    mod.verify_parity(obs)  # must not sys.exit (no non-zero exit) on a fully matching box
    write_types = {"CreateScene", "CreateInput", "SetSceneItemTransform", "SetInputSettings"}
    assert not any(rtype in write_types for rtype, _ in obs.calls), (
        "#791: verify_parity() must be READ-ONLY -- it must never create/seed/mutate anything, "
        f"only report. Calls made: {obs.calls}"
    )


def test_verify_parity_exits_nonzero_on_the_791_live_incident_shape(capsys):
    """Reproduces the EXACT #791 live incident: cam7 scenes missing, Resolume/overlay NDI sources
    missing, scene order scrambled -- verify_parity() must catch it and exit non-zero, printing a
    human-readable report (the same live incident (n)'s old Cam-1-6-COUNT-only check silently
    passed)."""
    mod = _scenes_module()
    broken_order = [
        "MV Cam 6", "MV Cam 5", "MV Cam 4", "MV Cam 3", "MV Cam 2", "MV Cam 1",
        "Cam 6", "Cam 5", "Cam 4", "Cam 3", "Cam 2", "Cam 1", "Scene",
    ]
    broken_ndi = {f"NDI CAM{n}": f"CAM{n} (usb)" for n in range(1, 7)}
    obs = FakeParityObs(broken_order, broken_ndi)
    try:
        mod.verify_parity(obs)
        raised = False
    except SystemExit as exc:
        raised = True
        assert exc.code == 1
    assert raised, "#791: verify_parity() must sys.exit(1) when parity is broken"
    out = capsys.readouterr().out
    assert "scene order: MISMATCH" in out
    assert "ndi sources: " in out and "OK" not in out.split("ndi sources: ")[1].split("\n")[0][:2]
