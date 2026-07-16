"""#783 regression -- seed() must NEVER overwrite an EXISTING scene item's transform.

Live incident (2026-07-15): the boot autostart seed reset the operator's hand-tuned
LED-wall transforms to fullscreen on every boot/relaunch, because seed() called
SetSceneItemTransform unconditionally. The fix captures item pre-existence BEFORE
CreateInput and applies the default transform ONLY to a freshly-created item.

RED proof: this test FAILS against the pre-fix seed (git show e496d4aab:scripts/
imag_scenes.py -- verified by running it against that exact file copy); GREEN against
the fixed seed (a641724a7 and later, incl. the 41423c9c2-revert lineage).
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


class FakeObs:
    """Collects req() calls; models a rig where EVERY scene item ALREADY EXISTS."""

    def __init__(self, item_exists=True):
        self.calls = []
        self.item_exists = item_exists

    def req(self, rtype, payload=None, ignore_err=False):
        self.calls.append((rtype, payload or {}))
        if rtype == "GetSceneItemId":
            return {"sceneItemId": 7} if self.item_exists else {}
        if rtype == "GetVideoSettings":
            return {"fpsNumerator": 60, "fpsDenominator": 1,
                    "baseWidth": 1920, "baseHeight": 1080,
                    "outputWidth": 1920, "outputHeight": 1080}
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in
                    [f"Cam {n}" for n in range(1, 8)] + [f"MV Cam {n}" for n in range(1, 8)]]}
        if rtype == "GetCurrentProgramScene":
            return {"sceneName": "Cam 1"}
        return {}


def _seed_module():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_under_test")


def test_seed_never_touches_an_existing_items_transform():
    mod = _seed_module()
    obs = FakeObs(item_exists=True)
    mod.seed(obs)
    transforms = [c for c in obs.calls if c[0] == "SetSceneItemTransform"]
    assert transforms == [], (
        "seed() issued SetSceneItemTransform for PRE-EXISTING items -- this wipes the "
        f"operator's hand-tuned LED-wall transforms on every boot (#783): {transforms[:2]}"
    )


def test_seed_still_defaults_a_freshly_created_item():
    mod = _seed_module()

    class FreshObs(FakeObs):
        def __init__(self):
            super().__init__(item_exists=False)
            self._created = set()

        def req(self, rtype, payload=None, ignore_err=False):
            p = payload or {}
            if rtype in ("CreateInput", "CreateSceneItem"):
                self._created.add((p.get("sceneName"), p.get("inputName") or p.get("sourceName")))
            if rtype == "GetSceneItemId":
                self.calls.append((rtype, p))
                exists = any(sc == p.get("sceneName") for sc, _ in self._created)
                return {"sceneItemId": 7} if exists else {}
            return super().req(rtype, payload, ignore_err)

    obs = FreshObs()
    mod.seed(obs)
    transforms = [c for c in obs.calls if c[0] == "SetSceneItemTransform"]
    # 7 Cam + 7 MV Cam scenes; the exact count depends on the stub's fidelity — the
    # regression bar is only that FRESH items DO get a default (the #783 fix must not
    # swing to never-transform-anything).
    assert len(transforms) >= 7, (
        "a freshly-created item MUST still get the default fullscreen transform "
        f"(got {len(transforms)} transform calls)"
    )


class FakeObsWithForeignScene(FakeObs):
    """Scene list includes an OPERATOR-owned scene the seed must never touch (#785)."""

    def req(self, rtype, payload=None, ignore_err=False):
        if rtype == "GetSceneList":
            self.calls.append((rtype, payload or {}))
            return {"scenes": [{"sceneName": s} for s in
                    [f"Cam {n}" for n in range(1, 8)]
                    + [f"MV Cam {n}" for n in range(1, 8)]
                    + ["MW resolume imag", "resolume imag"]]}
        return super().req(rtype, payload, ignore_err)


def _multiview_writes(obs):
    return [(r, p) for r, p in obs.calls if r == "SetSourcePrivateSettings"]


def test_bare_seed_never_writes_multiview_membership_785():
    mod = _seed_module()
    mod.BOOTSTRAP = False
    obs = FakeObsWithForeignScene()
    mod.seed(obs)
    assert _multiview_writes(obs) == [], (
        "#785: a bare (non-bootstrap) seed must not write show_in_multiview on ANY scene — "
        "the blanket write is what kept un-ticking the operator's multiview choices"
    )


def test_bootstrap_seed_touches_only_seed_owned_scenes_785():
    mod = _seed_module()
    mod.BOOTSTRAP = True
    obs = FakeObsWithForeignScene()
    mod.seed(obs)
    writes = _multiview_writes(obs)
    touched = {p["sourceName"] for _, p in writes}
    assert touched, "#785: bootstrap must still enforce membership on the seed-owned scenes"
    foreign = {n for n in touched if not (n.startswith("MV Cam ") or (n.startswith("Cam ") and n[4:].isdigit()))}
    assert not foreign, (
        f"#785: bootstrap wrote show_in_multiview on OPERATOR scenes {foreign} — "
        "foreign scenes are never the seed's to manage"
    )
