"""Issue 1460 slice 1 -- the OBS side of the VBAN receiver bench + its OBS-log grader.

scripts/vban_bench_obs.py creates the TEMPORARY obs-vban test input on strih-snv OBS (program scene,
dedicated track only, monitoring off, RecTracks |= track, active -- re-programming the scene once
under Studio Mode's scene duplication) and removes it again; scripts/vban_bench_log.py grades the
OBS log rows of one profile. These tests drive both with a fake OBS (the obs-websocket requests the
module sends, answered from a small model) and real OBS log line shapes.

Tier-0: the stdlib, no rig, no cargo.
"""
import json
import pathlib
import sys

import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import vban_bench_log as vl  # noqa: E402
import vban_bench_obs as vo  # noqa: E402

NAME = "VBAN bench (issue 1460)"
LIBOBS = pathlib.Path(__file__).resolve().parents[2] / "vendor" / "obs-studio" / "libobs"
OBS_SOURCE_C = LIBOBS / "obs-source.c"
OBS_AUDIO_C = LIBOBS / "obs-audio.c"


class FakeObs:
    """A strih-snv OBS as the bench sees it: Studio Mode with scene duplication (a new item in the
    program scene is not active until the scene is programmed again, and OBS's swap then puts the
    old program into the preview)."""

    def __init__(self, **over):
        self.kinds = [vo.VBAN_INPUT_KIND, "ndi_source", "pulse_input_capture"]
        self.inputs = {
            "ASIO zvuk": {"kind": "pulse_input_capture", "settings": {}, "tracks": {"1": True}},
            "NDI cam1": {"kind": "ndi_source", "settings": {}, "tracks": {str(i): True for i in range(1, 7)}},
        }
        self.params = {("Output", "Mode"): "Advanced", ("AdvOut", "RecType"): "Standard",
                       ("AdvOut", "RecFormat2"): "mkv", ("AdvOut", "RecTracks"): "1",
                       ("AdvOut", "TrackIndex"): "1"}
        self.studio, self.program, self.preview = True, "Cam 1", "Cam 3"
        self.duplication = True
        self.recording = False
        self.active = set()
        self.calls = []
        self.fail = {}
        self.removing = {}
        self.remove_lag = 0
        self.__dict__.update(over)

    def rpc(self, rtype, rdata=None, ignore_err=False):
        d = rdata or {}
        self.calls.append((rtype, d))
        if rtype in self.fail:
            if ignore_err:
                return {}
            raise RuntimeError("%s failed" % rtype)
        if rtype == "GetInputKindList":
            return {"inputKinds": self.kinds}
        if rtype == "GetInputList":
            listed = [{"inputName": n, "inputKind": i["kind"]} for n, i in self.inputs.items()]
            # A removed input OBS still lists while its release runs (live strih-snv 28.10.2026).
            if self.removing and self.remove_lag > 0:
                self.remove_lag -= 1
                listed += [{"inputName": n, "inputKind": k} for n, k in self.removing.items()]
            return {"inputs": listed}
        if rtype == "GetInputSettings":
            return {"inputSettings": self.inputs[d["inputName"]]["settings"]} if d["inputName"] in self.inputs else {}
        if rtype == "GetInputAudioTracks":
            return {"inputAudioTracks": self.inputs[d["inputName"]]["tracks"]}
        if rtype == "SetInputAudioTracks":
            self.inputs[d["inputName"]]["tracks"] = dict(d["inputAudioTracks"])
            return {}
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": self.studio}
        if rtype == "GetCurrentProgramScene":
            return {"currentProgramSceneName": self.program}
        if rtype == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": self.preview}
        if rtype == "SetCurrentPreviewScene":
            self.preview = d["sceneName"]
            return {}
        if rtype == "SetCurrentProgramScene":
            if self.studio and self.duplication:
                self.preview = self.program           # the swap
                self.active |= {n for n, i in self.inputs.items() if i.get("scene") == d["sceneName"]}
            self.program = d["sceneName"]
            return {}
        if rtype == "GetRecordStatus":
            return {"outputActive": self.recording}
        if rtype == "GetProfileParameter":
            v = self.params.get((d["parameterCategory"], d["parameterName"]))
            return {"parameterValue": v, "defaultParameterValue": None}
        if rtype == "SetProfileParameter":
            self.params[(d["parameterCategory"], d["parameterName"])] = d["parameterValue"]
            return {}
        if rtype == "CreateInput":
            self.inputs[d["inputName"]] = {"kind": d["inputKind"], "settings": dict(d["inputSettings"]),
                                           "tracks": {str(i): True for i in range(1, 7)}, "scene": d["sceneName"],
                                           "monitor": "OBS_MONITORING_TYPE_NONE"}
            if not (self.studio and self.duplication):
                self.active.add(d["inputName"])
            return {}
        if rtype == "RemoveInput":
            gone = self.inputs.pop(d["inputName"], None)
            if gone is not None:
                self.removing[d["inputName"]] = gone["kind"]
            self.active.discard(d["inputName"])
            return {}
        if rtype == "SetInputAudioMonitorType":
            self.inputs[d["inputName"]]["monitor"] = d["monitorType"]
            return {}
        if rtype == "GetInputAudioMonitorType":
            return {"monitorType": self.inputs[d["inputName"]]["monitor"]}
        if rtype == "GetSourceActive":
            return {"videoActive": d["sourceName"] in self.active, "videoShowing": False}
        raise AssertionError("unexpected request %s" % rtype)


def _setup(obs, tmp_path, **kw):
    args = dict(input_name=NAME, port=6995, stream_name="bench1460", track=6)
    args.update(kw)
    return vo.setup(obs.rpc, str(tmp_path / "state.json"), sleep=lambda s: None, **args)


# --- pure helpers ---------------------------------------------------------------------------------

def test_only_the_dedicated_track_is_on():
    assert vo.tracks_payload(6) == {"1": False, "2": False, "3": False, "4": False, "5": False, "6": True}


@pytest.mark.parametrize("track", [2, 3, 4, 5])
def test_every_other_track_choice_routes_to_that_track_only(track):
    on = [k for k, v in vo.tracks_payload(track).items() if v]
    assert on == [str(track)] and len(vo.tracks_payload(track)) == 6


@pytest.mark.parametrize("rectracks,track,index", [(33, 6, 1), (3, 2, 1), (1 | 4 | 32, 6, 2), (32, 6, 0)])
def test_the_audio_stream_index_follows_the_recorded_tracks(rectracks, track, index):
    assert vo.audio_stream_index(rectracks, track) == index


def test_a_track_outside_rectracks_has_no_stream():
    with pytest.raises(ValueError):
        vo.audio_stream_index(1, 6)


def test_rectracks_gains_only_the_test_track():
    assert vo.rectracks_with(1, 6) == 33 and vo.rectracks_with(33, 6) == 33


# --- preflight: read-only, refuses before any change ----------------------------------------------

def test_a_healthy_strih_passes_the_preflight_and_names_the_track_feeders():
    obs = FakeObs()
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert f["reasons"] == []
    assert f["track_feeders"] == ["NDI cam1"]          # reported, not a refusal
    assert not [c for c in obs.calls if c[0].startswith(("Set", "Create", "Remove"))]


@pytest.mark.parametrize("track,expect", [(1, "track 1 is the program mix"), (7, "must be 2..6"),
                                          (2, "is the stream track")])
def test_the_program_and_stream_tracks_are_refused(track, expect):
    obs = FakeObs()
    obs.params[("AdvOut", "TrackIndex")] = "2"
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", track)
    assert any(expect in r for r in f["reasons"]), f["reasons"]


@pytest.mark.parametrize("over,expect", [
    ({"kinds": ["ndi_source"]}, "is not loaded"),
    ({"recording": True}, "a recording is running"),
    ({"params": {("Output", "Mode"): "Simple"}}, "not Advanced"),
])
def test_an_unsafe_obs_is_refused(over, expect):
    obs = FakeObs(**over)
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert any(expect in r for r in f["reasons"]), f["reasons"]


def test_a_production_program_scene_is_refused_before_any_change():
    # issue 1380: the setup re-programs the current program scene; the stream box's production
    # scene is never programmed by our tooling, so the bench refuses it in the read-only preflight.
    obs = FakeObs()
    obs.program = "PRO"
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert any("never programs" in r for r in f["reasons"]), f["reasons"]
    assert not [c for c in obs.calls if c[0].startswith(("Set", "Create", "Remove"))]


@pytest.mark.parametrize("port", [6980, 6981, 6990, 80])
def test_a_real_receiver_port_is_refused_by_the_preflight_and_the_setup(tmp_path, port):
    obs = FakeObs()
    assert any(str(port) in r for r in vo.preflight(obs.rpc, NAME, port, "bench1460", 6)["reasons"])
    with pytest.raises(ValueError):
        _setup(obs, tmp_path, port=port)
    assert NAME not in obs.inputs and not (tmp_path / "state.json").exists()


def test_a_leftover_while_rectracks_holds_the_track_is_refused():
    # the leftover's run ORed the track in; its original RecTracks is unknown, so never adopt it
    obs = FakeObs()
    obs.inputs[NAME] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": 6995, "stream_name": "bench1460"},
                        "tracks": {}}
    obs.params[("AdvOut", "RecTracks")] = "33"
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert any("original value is unknown" in r for r in f["reasons"])


def test_a_port_another_vban_input_listens_on_is_refused():
    obs = FakeObs()
    obs.inputs["FOH"] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": 6995, "stream_name": "foh"}, "tracks": {}}
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert any("already listens on port 6995" in r for r in f["reasons"])


def test_a_same_name_vban_input_on_another_stream_is_not_a_leftover():
    # kind + port match but the stream name differs: someone else's receiver, never removed
    obs = FakeObs()
    obs.inputs[NAME] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": 6995, "stream_name": "foh"}, "tracks": {}}
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert not f["leftover_input"] and any("is not an earlier bench run" in r for r in f["reasons"])
    res = vo.teardown(obs.rpc, {"input_name": NAME, "port": 6995, "stream_name": "bench1460", "created": True})
    assert NAME in obs.inputs and not res["ok"]


def test_a_foreign_input_with_the_bench_name_is_refused_but_a_leftover_is_not():
    obs = FakeObs()
    obs.inputs[NAME] = {"kind": "ndi_source", "settings": {}, "tracks": {}}
    assert any("is not an earlier bench run" in r for r in vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)["reasons"])
    obs.inputs[NAME] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": 6995, "stream_name": "bench1460"}, "tracks": {}}
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert f["reasons"] == [] and f["leftover_input"]


# --- setup ----------------------------------------------------------------------------------------

def test_setup_creates_routes_and_activates_the_input(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    inp = obs.inputs[NAME]
    assert inp["kind"] == vo.VBAN_INPUT_KIND and inp["scene"] == "Cam 1"
    assert inp["settings"] == {"port": 6995, "stream_name": "bench1460", "ip_from": ""}
    assert inp["tracks"] == vo.tracks_payload(6) and inp["monitor"] == vo.MONITOR_OFF
    assert obs.params[("AdvOut", "RecTracks")] == "33"
    assert NAME in obs.active
    assert obs.preview == "Cam 3"                       # the swap's preview change put back
    assert st["audio_stream_index"] == 1 and st["rectracks_orig"] == 1 and st["rectracks_set"] == 33
    assert json.loads((tmp_path / "state.json").read_text())["steps"] == ["create", "rectracks", "reprogram", "active"]
    # the tracks are set BEFORE anything else can make the input audible
    names = [c[0] for c in obs.calls]
    assert names.index("SetInputAudioTracks") < names.index("SetProfileParameter") < names.index("SetCurrentProgramScene")


def test_without_scene_duplication_nothing_is_reprogrammed(tmp_path):
    obs = FakeObs(duplication=False)
    _setup(obs, tmp_path)
    assert "SetCurrentProgramScene" not in [c[0] for c in obs.calls]


def test_extra_settings_reach_the_input(tmp_path):
    obs = FakeObs()
    _setup(obs, tmp_path, extra={"receive_buffer_ms": 40})
    assert obs.inputs[NAME]["settings"]["receive_buffer_ms"] == 40


@pytest.mark.parametrize("key", ["port", "stream_name", "ip_from"])
def test_an_extra_setting_never_overrides_the_bench_keys(tmp_path, key):
    # --extra-setting port=6980 would point the pilot at the program port the bench refuses
    obs = FakeObs()
    with pytest.raises(ValueError):
        _setup(obs, tmp_path, extra={key: 6980})
    assert NAME not in obs.inputs and not (tmp_path / "state.json").exists()


def test_a_failure_after_the_create_is_a_bench_error_never_a_refusal(tmp_path):
    obs = FakeObs()
    real = obs.rpc

    def broken(rtype, rdata=None, ignore_err=False):
        if rtype == "SetInputAudioMonitorType":
            raise ValueError("garbled reply")
        return real(rtype, rdata, ignore_err)
    with pytest.raises(vo.BenchError):
        vo.setup(broken, str(tmp_path / "state.json"), NAME, 6995, "bench1460", 6)
    assert json.loads((tmp_path / "state.json").read_text())["created"]


def test_a_non_numeric_port_setting_never_crashes_the_preflight():
    obs = FakeObs()
    obs.inputs[NAME] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": "x", "stream_name": "bench1460"},
                        "tracks": {}}
    f = vo.preflight(obs.rpc, NAME, 6995, "bench1460", 6)
    assert any("is not an earlier bench run" in r for r in f["reasons"])


def test_a_leftover_bench_input_is_removed_first(tmp_path):
    obs = FakeObs()
    obs.inputs[NAME] = {"kind": vo.VBAN_INPUT_KIND, "settings": {"port": 6995, "stream_name": "bench1460"},
                        "tracks": {}}
    st = _setup(obs, tmp_path)
    assert st["steps"][0] == "remove-leftover"
    assert [c[0] for c in obs.calls].count("RemoveInput") == 1


def test_a_refused_preflight_changes_nothing(tmp_path):
    obs = FakeObs(recording=True)
    with pytest.raises(ValueError):
        _setup(obs, tmp_path)
    assert not [c for c in obs.calls if c[0].startswith(("Set", "Create", "Remove"))]
    assert not (tmp_path / "state.json").exists()


def test_a_wrong_track_read_back_fails_after_the_create_and_teardown_removes_it(tmp_path):
    obs = FakeObs()
    real = obs.rpc

    def broken(rtype, rdata=None, ignore_err=False):
        if rtype == "SetInputAudioTracks":
            return {}                                  # OBS ignored the routing
        return real(rtype, rdata, ignore_err)
    with pytest.raises(vo.BenchError):
        vo.setup(broken, str(tmp_path / "state.json"), NAME, 6995, "bench1460", 6)
    state = json.loads((tmp_path / "state.json").read_text())
    assert state["created"]
    res = vo.teardown(obs.rpc, state)
    assert res["ok"] and NAME not in obs.inputs


def test_an_input_that_never_turns_active_fails(tmp_path):
    obs = FakeObs()
    real = obs.rpc
    clock = [0.0]

    def never_active(rtype, rdata=None, ignore_err=False):
        if rtype == "GetSourceActive":
            return {"videoActive": False}
        return real(rtype, rdata, ignore_err)

    def sleep(s):
        clock[0] += s
    with pytest.raises(vo.BenchError, match="not active"):
        vo.setup(never_active, str(tmp_path / "state.json"), NAME, 6995, "bench1460", 6,
                 sleep=sleep, monotonic=lambda: clock[0])


# --- teardown -------------------------------------------------------------------------------------

def test_teardown_puts_the_preview_back_after_a_setup_that_died_after_the_reprogram(tmp_path):
    obs = FakeObs()
    real = obs.rpc
    clock = [0.0]

    def never_active(rtype, rdata=None, ignore_err=False):
        if rtype == "GetSourceActive":
            return {"videoActive": False}
        return real(rtype, rdata, ignore_err)

    def sleep(s):
        clock[0] += s
    with pytest.raises(vo.BenchError):
        vo.setup(never_active, str(tmp_path / "state.json"), NAME, 6995, "bench1460", 6,
                 sleep=sleep, monotonic=lambda: clock[0])
    assert obs.preview == "Cam 1"                       # OBS's swap moved the preview
    res = vo.teardown(obs.rpc, json.loads((tmp_path / "state.json").read_text()))
    assert res["ok"], res
    assert obs.preview == "Cam 3" and NAME not in obs.inputs


def test_teardown_never_undoes_a_preview_the_operator_staged_after_a_failed_setup(tmp_path):
    obs = FakeObs()
    real = obs.rpc
    clock = [0.0]

    def never_active(rtype, rdata=None, ignore_err=False):
        if rtype == "GetSourceActive":
            return {"videoActive": False}
        return real(rtype, rdata, ignore_err)

    def sleep(s):
        clock[0] += s
    with pytest.raises(vo.BenchError):
        vo.setup(never_active, str(tmp_path / "state.json"), NAME, 6995, "bench1460", 6,
                 sleep=sleep, monotonic=lambda: clock[0])
    obs.preview = "Cam 5"                               # staged by the cutter later (a show)
    res = vo.teardown(obs.rpc, json.loads((tmp_path / "state.json").read_text()))
    assert res["ok"], res
    assert obs.preview == "Cam 5" and NAME not in obs.inputs


def test_teardown_after_a_completed_setup_never_touches_the_preview(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    obs.preview = "Cam 5"                               # the operator's own choice since
    vo.teardown(obs.rpc, st)
    assert obs.preview == "Cam 5"


def test_teardown_removes_the_input_and_restores_rectracks(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    res = vo.teardown(obs.rpc, st)
    assert res["ok"], res
    assert NAME not in obs.inputs and obs.params[("AdvOut", "RecTracks")] == "1"
    assert vo.teardown(obs.rpc, st)["ok"]               # idempotent


def test_teardown_waits_for_obs_to_release_a_removed_input(tmp_path):
    # Live 28.10.2026: the bench aborted with "still there after RemoveInput" on a healthy OBS; the
    # input was gone a moment later. The read-back is a bounded settle poll, never one read.
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    obs.remove_lag = 3
    slept = []
    res = vo.teardown(obs.rpc, st, sleep=slept.append)
    assert res["ok"], res
    assert any("removed the input" in a for a in res["actions"]), res
    assert slept and sum(slept) <= 10, slept


def test_teardown_still_names_an_input_that_never_goes_away(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    obs.remove_lag = 10 ** 6
    res = vo.teardown(obs.rpc, st, sleep=lambda s: None)
    assert not res["ok"] and any("still there after RemoveInput" in x for x in res["left"]), res


def test_teardown_leaves_a_foreign_input_and_a_foreign_rectracks_alone(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    obs.inputs[NAME] = {"kind": "ndi_source", "settings": {}, "tracks": {}}
    obs.params[("AdvOut", "RecTracks")] = "5"
    res = vo.teardown(obs.rpc, st)
    assert not res["ok"]
    assert NAME in obs.inputs and obs.params[("AdvOut", "RecTracks")] == "5"
    assert any("not the bench's" in x for x in res["left"]) and any("left alone" in x for x in res["left"])


# --- the OBS log rows -----------------------------------------------------------------------------

ASRC_OK = ("13:01:02.345: asrc: source '%s' estimated=1.38ppm applied=1.30ppm outer_bias=0.00ppm "
           "cumulative_correction=0.078ms/60s starved_blocks=0 (#803/#806/#960) level=0.4ms target=0.0ms "
           "integral=0.000ppm (#1335) steps=0 last_step_ms=0.0 restore=0 (#1335) level_avg=0.10ms (#1367)"
           " recover_ms=0.0 (issue 1372) timecode=1 place_jumps=0 last_jump_ms=0.0 (issue 1367)"
           " basis=stamp (issue 1425)" % NAME)


def test_the_asrc_line_shape_is_the_vendored_one():
    # the grader keys on the format string obs-source.c logs; a reworded line must fail here first
    src = OBS_SOURCE_C.read_text()
    for token in ("asrc: source '%s' estimated=%.2fppm", "steps=%u", "place_jumps=%u"):
        assert token in src, token
    assert "\"buffering-guard: '%s' %s:" in OBS_AUDIO_C.read_text()


def test_a_clean_log_passes():
    r = vl.rows([ASRC_OK, "13:01:10.000: vban-source '%s': received=11250 lost=6 late=0 reanchors=0" % NAME],
                NAME)
    g = vl.grade(r, expected_lost=6, expect_ppm=1.4)
    assert g["verdict"] == "PASS", g


def test_a_buffering_guard_on_the_test_input_fails_and_others_are_notes():
    lines = [ASRC_OK,
             "13:02:00.000: buffering-guard: '%s' fell behind the mix (frozen at 85 ms): ..." % NAME,
             "13:02:01.000: buffering-guard: 'NDI test' is not mixed: ..."]
    g = vl.grade(vl.rows(lines, NAME))
    assert g["verdict"] == "FAIL" and any("pop" in x for x in g["reasons"])
    assert g["notes"][0].startswith("1 buffering-guard line(s) named other inputs")


@pytest.mark.parametrize("swap,expect", [("steps=0", "steps=2"), ("place_jumps=0", "place_jumps=3")])
def test_asrc_steps_or_place_jumps_fail(swap, expect):
    g = vl.grade(vl.rows([ASRC_OK.replace(swap, expect)], NAME))
    assert g["verdict"] == "FAIL" and any(expect.split("=")[0] in x for x in g["reasons"])


def test_an_asrc_estimate_off_the_injected_offset_fails():
    g = vl.grade(vl.rows([ASRC_OK], NAME), expect_ppm=50.0)
    assert any("estimated=+1.38" in x for x in g["reasons"])


def test_no_asrc_line_fails():
    g = vl.grade(vl.rows(["13:00:00.000: some other line"], NAME))
    assert any("not on the genlock ASRC path" in x for x in g["reasons"])


def test_the_stock_missing_packet_lines_must_add_up_to_the_scheduled_losses():
    lines = [ASRC_OK, "13:03:00.000: source '%s': missing 1 packet(s) at frame 22500" % NAME,
             "13:04:00.000: source '%s': missing 2 packet(s) at frame 45000" % NAME]
    assert vl.grade(vl.rows(lines, NAME), expected_lost=3)["verdict"] == "PASS"
    g = vl.grade(vl.rows(lines, NAME), expected_lost=6)
    assert any("logged 3 missing packet(s), the emulator dropped 6" in x for x in g["reasons"])


def test_a_late_or_reanchored_status_fails():
    st = "13:01:10.000: vban-source '%s': received=11250 lost=6 late=2 re-anchors=1" % NAME
    g = vl.grade(vl.rows([ASRC_OK, st], NAME), expected_lost=6)
    assert any("late=2" in x for x in g["reasons"]) and any("re_anchors=1" in x for x in g["reasons"])


def test_a_line_without_numbers_never_hides_the_status_row():
    st = "13:01:10.000: vban-source '%s': received=11250 lost=6 late=0 reanchors=0" % NAME
    removed = "13:05:00.000: User Removed source '%s' (net.nagater.obs-vban.source)" % NAME
    g = vl.grade(vl.rows([ASRC_OK, st, removed], NAME), expected_lost=5)
    assert any("lost=6, the emulator dropped 5" in x for x in g["reasons"])


def test_a_basis_other_than_stamp_fails():
    g = vl.grade(vl.rows([ASRC_OK.replace("basis=stamp", "basis=arrival")], NAME))
    assert any("basis=arrival" in x for x in g["reasons"])


def test_level_avg_off_target_fails_but_the_first_line_is_the_settle():
    off = ASRC_OK.replace("level_avg=0.10ms", "level_avg=7.50ms")
    assert vl.grade(vl.rows([off, ASRC_OK], NAME))["verdict"] == "PASS"
    only = vl.grade(vl.rows([off], NAME))
    assert only["verdict"] == "PASS" and any("not graded" in x for x in only["notes"])
    g = vl.grade(vl.rows([ASRC_OK, off], NAME))
    assert any("level_avg=7.50" in x for x in g["reasons"])


def test_steps_on_an_earlier_asrc_line_fail():
    g = vl.grade(vl.rows([ASRC_OK.replace("steps=0", "steps=1"), ASRC_OK], NAME))
    assert any("steps=1" in x for x in g["reasons"])


TEL = ("13:02:00.000: audio-telemetry #800 '%s': ts_lag_ms=12 buffered_ms=85 pending=0 "
       "timing_adjust_ms=0 mixed_late=%d")


def test_mixed_late_on_the_test_input_fails():
    assert vl.grade(vl.rows([ASRC_OK, TEL % (NAME, 0)], NAME))["verdict"] == "PASS"
    g = vl.grade(vl.rows([ASRC_OK, TEL % (NAME, 3)], NAME))
    assert any("mixed_late=3" in x for x in g["reasons"])


def test_a_telemetry_row_never_hides_a_failing_status_row():
    st = "13:01:10.000: vban-source '%s': received=11250 lost=0 late=2 reanchors=1" % NAME
    g = vl.grade(vl.rows([ASRC_OK, st, TEL % (NAME, 0)], NAME), expected_lost=0)
    assert any("late=2" in x for x in g["reasons"]) and any("reanchors=1" in x for x in g["reasons"])


def test_a_late_slot_on_an_earlier_status_row_fails():
    bad = "13:01:10.000: vban-source '%s': received=1000 lost=0 late=1 reanchors=0" % NAME
    good = "13:01:20.000: vban-source '%s': received=2000 lost=0 late=0 reanchors=0" % NAME
    g = vl.grade(vl.rows([ASRC_OK, bad, good], NAME), expected_lost=0)
    assert any("late=1" in x for x in g["reasons"])


def test_the_end_of_stream_fill_is_not_a_lost_packet():
    # Live 28.10.2026: when the emulator stops, the receiver fills the slots up to its stretch end
    # (`missing 181 packet(s) before the stretch ended`). That is the sender stopping, never a loss
    # inside the run: the bench read it as "logged 181 missing packet(s), the emulator dropped 0".
    lines = [ASRC_OK, "13:03:00.000: source '%s': missing 1 packet(s) before frame 22503 -- concealed: "
             "the previous packet repeated, crossfaded" % NAME,
             "13:12:00.000: source '%s': missing 181 packet(s) before the stretch ended -- silence for "
             "those slots" % NAME]
    r = vl.rows(lines, NAME)
    assert r["missing_packets_total"] == 1 and r["missing_packets_events"] == 1, r
    g = vl.grade(r, expected_lost=1)
    assert not any("missing packet(s)" in x for x in g["reasons"]), g["reasons"]


def test_the_log_cli(tmp_path):
    log = tmp_path / "obs.log"
    log.write_text(ASRC_OK + "\n")
    assert vl.main(["--log", str(log), "--input-name", NAME, "--json"]) == 0
    assert vl.main(["--log", str(tmp_path / "missing.log"), "--input-name", NAME]) == 3
