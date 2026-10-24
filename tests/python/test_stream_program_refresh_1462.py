"""issue 1462 (24.10.2026) -- a seed removal from the PROGRAM scene must reach the program.

In Studio Mode OBS programs a private DUPLICATE of the scene, made when the scene was taken (and at load).
The launch seed removed the measurement clip item from stream-snv's `Development` (the scene on program),
the duplicate kept it, and the clip's QPSK marker stayed on program: PR 1472's E2E `[4b3/8]` read two marker
streams (POLLUTED) after the fix had "worked" on the scene itself. `obs_phase2.retake_program_scene`
re-takes the program scene through the preview (preview = it, one Studio Mode transition) when, and only
when, Studio Mode is on and nothing streams. It lives in obs_phase2 so it goes through the ONE
production-scene guard (`_rpc`), and it never re-takes a NEVER_PROGRAM_SCENES scene.
"""
import importlib.util
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent.parent / "scripts"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


OP = _load("obs_phase2_refresh_1462", SCRIPTS / "obs_phase2.py")
PROD = sorted(OP.NEVER_PROGRAM_SCENES)[0]


class Box:
    def __init__(self, program="Development", preview="POST", studio=True, streaming=False):
        self.program, self.preview, self.studio, self.streaming = program, preview, studio, streaming
        self.calls = []

    def rpc(self, ws, t, d=None):
        d = d or {}
        OP._refuse_forbidden_scene(t, d)   # the guard _rpc applies before anything is sent
        self.calls.append((t, dict(d)))
        if t == "GetStudioModeEnabled":
            return {"studioModeEnabled": self.studio}
        if t == "GetCurrentProgramScene":
            return {"currentProgramSceneName": self.program}
        if t == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": self.preview}
        if t == "GetStreamStatus":
            return {"outputActive": self.streaming}
        if t == "SetCurrentPreviewScene":
            self.preview = d["sceneName"]
        if t == "TriggerStudioModeTransition":
            self.program, self.preview = self.preview, self.program
        return {}

    def writes(self):
        return [(t, d) for t, d in self.calls if not t.startswith("Get")]


def test_the_edited_program_scene_is_retaken():
    box = Box()
    assert OP.retake_program_scene(None, "Development", rpc=box.rpc) == "re-took 'Development' onto program"
    assert box.writes() == [("SetCurrentPreviewScene", {"sceneName": "Development"}),
                            ("TriggerStudioModeTransition", {})]
    assert box.program == "Development"


def test_no_refresh_when_the_edited_scene_is_not_on_program():
    box = Box(program="PRE")
    assert OP.retake_program_scene(None, "Development", rpc=box.rpc) == ""
    assert box.writes() == []


def test_no_refresh_without_studio_mode():
    box = Box(studio=False)
    assert OP.retake_program_scene(None, "Development", rpc=box.rpc) == ""
    assert box.writes() == []


def test_never_retakes_the_production_scene():
    box = Box(program=PROD)
    assert OP.retake_program_scene(None, PROD, rpc=box.rpc) == ""
    assert box.writes() == []


def test_never_during_a_broadcast():
    box = Box(streaming=True)
    assert "streaming" in OP.retake_program_scene(None, "Development", rpc=box.rpc)
    assert box.writes() == []


def test_the_bootstrap_retakes_after_a_dev_scene_or_pulse_removal():
    text = (SCRIPTS / "strih_scenes.py").read_text()
    assert "retake_program_scene" in text
