"""issue 1462 (24.10.2026) -- a seed removal from the PROGRAM scene must reach the program.

In Studio Mode OBS programs a private DUPLICATE of the scene, made when the scene was taken (and at load).
The launch seed removed the measurement clip item from stream-snv's `Development` (the scene on program),
the duplicate kept it, and the clip's QPSK marker stayed on program: PR 1472's E2E `[4b3/8]` read two marker
streams (POLLUTED) after the fix had "worked" on the scene itself. `refresh_program_scene` re-takes the
program scene through the preview (preview = it, transition, the previous preview back) when, and only when,
Studio Mode is on; it never re-takes the production scene `PRO` and never touches a broadcast.
"""
import importlib.util
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent.parent / "scripts"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


SDS = _load("stream_dev_safe_refresh_1462", SCRIPTS / "stream_dev_safe.py")


class Obs:
    def __init__(self, program="Development", preview="POST", studio=True, streaming=False):
        self.program, self.preview, self.studio, self.streaming = program, preview, studio, streaming
        self.calls = []

    def req(self, t, d=None, ignore_err=False):
        d = d or {}
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


def test_the_edited_program_scene_is_retaken_and_the_preview_restored():
    obs = Obs()
    assert SDS.refresh_program_scene(obs, "Development") == "re-took 'Development' onto program"
    assert obs.writes() == [("SetCurrentPreviewScene", {"sceneName": "Development"}),
                            ("TriggerStudioModeTransition", {}),
                            ("SetCurrentPreviewScene", {"sceneName": "POST"})]
    assert (obs.program, obs.preview) == ("Development", "POST")


def test_no_refresh_when_the_edited_scene_is_not_on_program():
    obs = Obs(program="PRE")
    assert SDS.refresh_program_scene(obs, "Development") == ""
    assert obs.writes() == []


def test_no_refresh_without_studio_mode():
    obs = Obs(studio=False)
    assert SDS.refresh_program_scene(obs, "Development") == ""
    assert obs.writes() == []


def test_never_retakes_the_production_scene():
    obs = Obs(program="PRO")
    assert SDS.refresh_program_scene(obs, "PRO") == ""
    assert obs.writes() == []


def test_never_during_a_broadcast():
    obs = Obs(streaming=True)
    msg = SDS.refresh_program_scene(obs, "Development")
    assert "streaming" in msg
    assert obs.writes() == []


def test_a_preview_of_pro_is_never_restored():
    obs = Obs(preview="PRO")
    SDS.refresh_program_scene(obs, "Development")
    assert ("SetCurrentPreviewScene", {"sceneName": "PRO"}) not in obs.writes()


def test_the_bootstrap_refreshes_after_a_dev_scene_or_pulse_removal():
    text = (SCRIPTS / "strih_scenes.py").read_text()
    assert "refresh_program_scene" in text
