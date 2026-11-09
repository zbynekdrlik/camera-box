"""issue 1495 (live stream-snv 9.11.2026): the launch seed's program-audio replace must work while another
holder keeps a reference to the old input, and must never leave the box without it.

Live: the seed switched stream-snv's `mbc` from the obs-vban source to dante_input. obs-websocket's
RemoveInput answered 100, yet GetInputList kept listing `mbc` (old kind) and CreateInput refused the
name, so the seed printed `replace FAILED`. The old input had already left every scene (the remove signal
deletes the scene items), so the program audio was off the scenes until an OBS restart; after it the
create path put a fresh `mbc` into the current program scene `Development` (it was only in `PRO`) and the
58 ms sync offset was lost.

Why it lingered (read in the vendored sources): obs_source_remove only marks the source removed; it stays
listed and findable by name until its last strong reference goes. The av-sync dock looks the measurement
source up by NAME every 5 s and keeps the result (issue 1470, cb_refresh_measure_source), and a name lookup
does not skip a removed source, so the dock re-pins the removed `mbc` forever.

The fake OBS below models exactly that: sources are objects, RemoveInput marks one removed and deletes
its scene items, a removed source is destroyed only when no holder pins it, and the DOCK re-pins whatever
a by-name lookup of `mbc` returns every few requests (a removed source included). SetInputName renames
the object (the name lookup then finds the new input or nothing, and the dock lets go of the old one).
"""
import importlib.util
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
VBAN_KIND = "net.nagater.obs-vban.source"
DANTE_KIND = "dante_input"
NAME = "mbc"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1495", SCRIPTS / "program_audio_vban.py")


def _scenes():
    return _load("strih_scenes_1495", SCRIPTS / "strih_scenes.py")


class Src:
    _n = 0

    def __init__(self, name, kind, sync=0, volume=1.0, muted=False, filters=None):
        Src._n += 1
        self.uid = Src._n
        self.name, self.kind = name, kind
        self.removed = self.destroyed = False
        self.sync, self.volume, self.muted, self.balance = sync, volume, muted, 0.5
        self.monitor, self.tracks = "OBS_MONITORING_TYPE_NONE", {"1": True, "2": False}
        self.filters = list(filters or [])


class LingeringObs:
    """An OBS whose stream-snv collection holds `mbc` in PRO (visible, index 1) and `Zaloha` (hidden),
    never in the current program scene `Development`. DOCK_PIN: a holder that re-acquires
    obs_get_source_by_name(NAME) every DOCK_EVERY requests (the av-sync dock). RENAME_OK: whether OBS
    answers SetInputName. REFUSE_CREATE: CreateInput calls refused (an error answer, nothing created)."""

    def __init__(self, old_kind=VBAN_KIND, dock_pin=True, dock_every=3, rename_ok=True, refuse_create=0,
                 old_name=NAME, extra=()):
        self.calls, self.events, self.heard = [], [], []
        self.rename_ok, self.refuse_create = rename_ok, refuse_create
        self.dock_pin, self.dock_every, self.dock_held = dock_pin, dock_every, None
        gain = {"filterName": "Sluchadla operatora", "filterKind": "monitor_filter", "filterIndex": 0,
                "filterEnabled": True, "filterSettings": {"device": "x"}}
        lim = {"filterName": "Limiter", "filterKind": "limiter_filter", "filterIndex": 1,
               "filterEnabled": False, "filterSettings": {"threshold": -6.0}}
        old = Src(old_name, old_kind, sync=58, volume=0.7, filters=[gain, lim])
        pgm, cam = Src("NDI 2ME PGM", "ndi_source"), Src("Kamera", "ndi_source")
        self.sources = [old, pgm, cam] + [Src(n, k) for n, k in extra]
        self.next_item = 100
        self.scenes = {"PRO": [], "Development": [], "Zaloha": []}
        self._add_item("PRO", pgm, True)
        self._add_item("PRO", old, True)
        self._add_item("PRO", cam, True)
        self._add_item("Development", pgm, True)
        self._add_item("Zaloha", old, False)
        self.dock_refresh()

    # --- the model ------------------------------------------------------------------------------------
    def _add_item(self, scene, src, enabled):
        self.next_item += 1
        self.scenes[scene].append({"id": self.next_item, "src": src, "enabled": enabled})
        return self.next_item

    def by_name(self, name):
        for s in self.sources:
            if s.name == name and not s.destroyed:
                return s
        return None

    def gc(self):
        for s in self.sources:
            if s.removed and not s.destroyed and s is not self.dock_held:
                s.destroyed = True

    def dock_refresh(self):
        if self.dock_pin:
            self.dock_held = self.by_name(NAME)   # a name lookup finds a removed source too
        self.gc()

    def audible(self, scene):
        """The audio-input sources a scene mixes now (visible items of a non-ndi source)."""
        return [it["src"] for it in self.scenes[scene] if it["enabled"] and it["src"].kind != "ndi_source"]

    # --- obs-websocket ----------------------------------------------------------------------------------
    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if len(self.calls) % self.dock_every == 0:
            self.dock_refresh()
        r = self._answer(t, d or {})
        self.gc()
        self.events.append({sc: [s.uid for s in self.audible(sc)] for sc in self.scenes})
        self.heard.append(self.headphones())
        return r

    def headphones(self):
        """The live sources the operator hears on the stream headphones: monitoring and an enabled
        monitor filter output whatever the scene items show (a removed source is taken as silent)."""
        return [s.uid for s in self.sources if not s.removed and not s.destroyed
                and (s.monitor != "OBS_MONITORING_TYPE_NONE"
                     or any(f["filterKind"] == "monitor_filter" and f["filterEnabled"] for f in s.filters))]

    def _answer(self, t, d):
        if t == "GetInputList":
            return {"inputs": [{"inputName": s.name, "inputKind": s.kind}
                               for s in self.sources if not s.destroyed]}
        if t == "GetInputKindList":
            return {"inputKinds": [VBAN_KIND, DANTE_KIND, "pulse_input_capture"]}
        if t == "GetSceneList":
            return {"currentProgramSceneName": "Development",
                    "scenes": [{"sceneName": n} for n in self.scenes]}
        if t == "GetSceneItemList":
            return {"sceneItems": [{"sceneItemId": it["id"], "sourceName": it["src"].name,
                                    "sceneItemEnabled": it["enabled"], "sceneItemIndex": i}
                                   for i, it in enumerate(self.scenes[d["sceneName"]])]}
        src = self.by_name(d.get("inputName") or d.get("sourceName") or "")
        if t == "SetInputName":
            if not self.rename_ok or src is None or self.by_name(d["newInputName"]) is not None:
                return {}
            src.name = d["newInputName"]
            return {}
        if t == "RemoveInput":
            if src is not None and not src.removed:
                src.removed = True
                for items in self.scenes.values():
                    items[:] = [it for it in items if it["src"] is not src]
            return {}
        if t == "CreateInput":
            if src is not None or self.refuse_create:
                self.refuse_create = max(0, self.refuse_create - 1)
                return {}
            new = Src(d["inputName"], d["inputKind"])
            self.sources.append(new)
            return {"sceneItemId": self._add_item(d["sceneName"], new, d.get("sceneItemEnabled", True))}
        if t == "CreateSceneItem":
            if src is None or src.removed:
                return {}
            return {"sceneItemId": self._add_item(d["sceneName"], src, d.get("sceneItemEnabled", True))}
        if t in ("SetSceneItemEnabled", "SetSceneItemIndex"):
            items = self.scenes[d["sceneName"]]
            for i, it in enumerate(items):
                if it["id"] == d["sceneItemId"]:
                    if t == "SetSceneItemEnabled":
                        it["enabled"] = d["sceneItemEnabled"]
                    else:
                        items.insert(d["sceneItemIndex"], items.pop(i))
            return {}
        if src is None:
            return {}
        getters = {"GetInputVolume": {"inputVolumeMul": src.volume}, "GetInputMute": {"inputMuted": src.muted},
                   "GetInputAudioMonitorType": {"monitorType": src.monitor},
                   "GetInputAudioTracks": {"inputAudioTracks": src.tracks},
                   "GetInputAudioBalance": {"inputAudioBalance": src.balance},
                   "GetInputAudioSyncOffset": {"inputAudioSyncOffset": src.sync},
                   "GetSourceFilterList": {"filters": [dict(f) for f in src.filters]},
                   "GetInputSettings": {"inputSettings": {}}}
        if t in getters:
            return getters[t]
        setters = {"SetInputVolume": ("volume", "inputVolumeMul"), "SetInputMute": ("muted", "inputMuted"),
                   "SetInputAudioMonitorType": ("monitor", "monitorType"),
                   "SetInputAudioTracks": ("tracks", "inputAudioTracks"),
                   "SetInputAudioBalance": ("balance", "inputAudioBalance"),
                   "SetInputAudioSyncOffset": ("sync", "inputAudioSyncOffset")}
        if t in setters:
            attr, key = setters[t]
            setattr(src, attr, d[key])
        elif t == "CreateSourceFilter":
            src.filters.append({"filterName": d["filterName"], "filterKind": d["filterKind"],
                                "filterIndex": len(src.filters), "filterEnabled": True,
                                "filterSettings": d["filterSettings"]})
        elif t == "SetSourceFilterEnabled":
            for f in src.filters:
                if f["filterName"] == d["filterName"]:
                    f["filterEnabled"] = d["filterEnabled"]
        return {}

    # --- what the tests read ------------------------------------------------------------------------------
    def items_of(self, src):
        return {sc: [(i, it["enabled"]) for i, it in enumerate(items) if it["src"] is src]
                for sc, items in self.scenes.items()}

    def listed(self):
        return {s.name: s.kind for s in self.sources if not s.destroyed}


@pytest.fixture(autouse=True)
def _no_sleep(monkeypatch):
    monkeypatch.setattr("time.sleep", lambda s: None)


def _seed(obs):
    return _scenes().seed_program_audio_input(obs, NAME, dict(PAV.DANTE_SEED))


def test_the_model_reproduces_the_live_failure_of_a_remove_first_replace():
    # the fake is the live OBS: a removed `mbc` stays listed under its name while the dock re-pins it
    obs = LingeringObs()
    old = obs.by_name(NAME)
    for _ in range(40):
        obs.req("RemoveInput", {"inputName": NAME})
        obs.req("GetInputList")
    assert old.removed and not old.destroyed and obs.listed()[NAME] == VBAN_KIND
    obs.req("CreateInput", {"sceneName": "PRO", "inputName": NAME, "inputKind": DANTE_KIND, "inputSettings": {}})
    assert obs.listed()[NAME] == VBAN_KIND, "CreateInput refuses the name the removed source still holds"
    assert obs.audible("PRO") == [], "and the program audio has left every scene"


def test_a_replace_succeeds_while_another_holder_pins_the_old_input_by_name():
    obs = LingeringObs()
    st = _seed(obs)
    assert st.startswith("replaced (was %s" % VBAN_KIND), st
    new = obs.by_name(NAME)
    assert new is not None and new.kind == DANTE_KIND and not new.removed
    assert {n: k for n, k in obs.listed().items() if k not in ("ndi_source",)} == {NAME: DANTE_KIND}, \
        "the old input is gone once the dock looked `mbc` up again"


def test_the_operator_state_survives_the_replace():
    obs = LingeringObs()
    _seed(obs)
    new = obs.by_name(NAME)
    assert obs.items_of(new) == {"PRO": [(1, True)], "Development": [], "Zaloha": [(0, False)]}, \
        "every scene it was in, at its old index, hidden stays hidden, and never a scene it was not in"
    assert (new.sync, new.volume, new.muted) == (58, 0.7, False)
    assert [(f["filterName"], f["filterEnabled"]) for f in new.filters] == [
        ("Sluchadla operatora", True), ("Limiter", False)]


def test_the_old_input_is_removed_only_after_the_new_one_exists():
    obs = LingeringObs()
    _seed(obs)
    names = [(t, (d or {}).get("inputName")) for t, d in obs.calls]
    first_create = next(i for i, (t, _n) in enumerate(names) if t == "CreateInput")
    removes = [i for i, (t, _n) in enumerate(names) if t == "RemoveInput"]
    assert removes and min(removes) > first_create, "nothing is removed before the new input exists"
    assert all(n != NAME for t, n in names if t == "RemoveInput"), \
        "the remove targets the renamed old input, never the name the new input takes"


def test_the_program_audio_is_never_doubled_and_never_absent_for_longer_than_the_swap():
    obs = LingeringObs()
    _seed(obs)
    pro = [e["PRO"] for e in obs.events]
    assert all(len(a) <= 1 for a in pro), "old and new are never mixed together (no double sound)"
    gaps = [i for i, a in enumerate(pro) if not a]
    assert len(gaps) <= 3, "only the few requests between the old item's removal and the new item's show: %r" % gaps


def test_a_create_obs_refuses_keeps_the_old_input_in_every_scene():
    obs = LingeringObs(refuse_create=9)
    old = obs.by_name(NAME)
    st = _seed(obs)
    assert st.startswith("replace FAILED (was %s)" % VBAN_KIND), st
    assert "kept the old input" in st, st
    assert obs.by_name(NAME) is old and old.name == NAME and not old.removed, "renamed back, never removed"
    assert obs.items_of(old) == {"PRO": [(1, True)], "Development": [], "Zaloha": [(0, False)]}
    assert old.sync == 58
    assert not any(t == "RemoveInput" for t, _d in obs.calls)


def test_a_rename_obs_refuses_removes_nothing():
    obs = LingeringObs(rename_ok=False)
    old = obs.by_name(NAME)
    st = _seed(obs)
    assert st.startswith("replace FAILED (was %s)" % VBAN_KIND), st
    assert not any(t in ("RemoveInput", "CreateInput") for t, _d in obs.calls)
    assert obs.items_of(old)["PRO"] == [(1, True)] and old.name == NAME


def test_an_interrupted_replace_is_resumed_from_its_leftover_never_created_in_the_program_scene():
    # OBS died between the rename and the create: after the restart `mbc` is absent and the old input
    # sits under its aside name with every scene item and the operator state
    obs = LingeringObs(old_name=NAME + " (seed replace 1)", dock_pin=False)
    st = _seed(obs)
    assert st.startswith("replaced (resumed the interrupted replace of '%s (seed replace 1)'" % NAME), st
    new = obs.by_name(NAME)
    assert new.kind == DANTE_KIND
    assert obs.items_of(new) == {"PRO": [(1, True)], "Development": [], "Zaloha": [(0, False)]}
    assert new.sync == 58
    assert obs.by_name(NAME + " (seed replace 1)") is None


def test_a_leftover_beside_a_matching_input_is_removed_and_named():
    obs = LingeringObs(old_kind=DANTE_KIND, dock_pin=False,
                       extra=((NAME + " (seed replace 1)", VBAN_KIND),))
    st = _seed(obs)
    assert st == "matched (removed the leftover '%s (seed replace 1)' of an interrupted replace)" % NAME, st
    assert obs.by_name(NAME + " (seed replace 1)") is None and obs.by_name(NAME).kind == DANTE_KIND


def test_the_aside_name_never_takes_an_existing_input_and_a_stale_aside_is_dropped_after_the_replace():
    obs = LingeringObs(dock_pin=False, extra=((NAME + " (seed replace 1)", VBAN_KIND),))
    st = _seed(obs)
    assert st.startswith("replaced (was"), st
    renames = [d for t, d in obs.calls if t == "SetInputName"]
    assert renames[0]["newInputName"] == NAME + " (seed replace 2)", "a free aside name, never a taken one"
    assert "removed the leftover '%s (seed replace 1)'" % NAME in st, st
    assert obs.by_name(NAME + " (seed replace 1)") is None, "stale asides never accumulate"
    assert obs.by_name(NAME).kind == DANTE_KIND


def test_the_operator_headphones_never_hear_the_old_and_the_new_input_together():
    # monitoring and the operator's monitor filter play whatever the scene items show
    obs = LingeringObs()
    _seed(obs)
    assert all(len(h) <= 1 for h in obs.heard), "the new input's headphone output starts after the old one left"
    new = obs.by_name(NAME)
    assert obs.heard[-1] == [new.uid]


def test_a_replace_interrupted_after_its_create_is_finished_never_left_hidden():
    # OBS died after the new input was created (hidden, in PRO only, no state) and before the old one
    # went: the next seed sees `mbc` of the wanted kind beside the aside input that still carries the
    # visible items and the operator state
    obs = LingeringObs(old_name=NAME + " (seed replace 1)", dock_pin=False)
    new = Src(NAME, DANTE_KIND)
    obs.sources.append(new)
    obs._add_item("PRO", new, False)
    st = _seed(obs)
    assert st.startswith("matched (finished the interrupted replace of '%s (seed replace 1)'" % NAME), st
    assert obs.items_of(new) == {"PRO": [(1, True)], "Development": [], "Zaloha": [(0, False)]}
    assert obs.audible("PRO") == [new], "the program audio is back on PRO"
    assert new.sync == 58 and [f["filterName"] for f in new.filters] == ["Sluchadla operatora", "Limiter"]
    assert obs.by_name(NAME + " (seed replace 1)") is None


def test_a_lingering_removed_aside_is_never_a_resume_source():
    # a removed aside a holder still keeps is listed but in no scene: `mbc` absent then is a plain create,
    # never a resume that would leave the new input in no scene at all
    obs = LingeringObs(dock_pin=False)
    old = obs.by_name(NAME)
    obs.dock_refresh = lambda: None
    obs.dock_held = old   # the holder keeps its reference to the old source throughout
    obs.req("SetInputName", {"inputName": NAME, "newInputName": NAME + " (seed replace 1)"})
    obs.req("RemoveInput", {"inputName": NAME + " (seed replace 1)"})
    assert obs.listed()[NAME + " (seed replace 1)"] == VBAN_KIND and NAME not in obs.listed()
    st = _seed(obs)
    assert st == "created", st
    assert obs.by_name(NAME).kind == DANTE_KIND
