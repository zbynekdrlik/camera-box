"""issue 1439, the stream role: program audio = the console's VBAN straight into the OBS obs-vban source.

Design revision 6338127636 (owner 22.10.2026: no intercom hub on the stream box). Covers:
  * the facts STRIH_PROGRAM_VBAN_STREAMS / _PORT / _RESERVE_MS (scripts/lib/strih-box-facts.sh): the
    values, the refusals, and the R clamp pinned to vendor/obs-vban/src/vban-receive-timeline.h;
  * the one source definition scripts/program_audio_vban.py (the kind pinned to the vendored plugin id),
    the stream seed manifest's program_audio_vban, and scripts/strih_scenes.py switching `Zvuk z mixu`
    to the obs-vban source keeping the operator's state (scenes + visibility, volume, mute, monitoring,
    tracks, balance, sync offset, filters), never removing it for a kind OBS has not loaded;
  * scripts/obs_collection_port.py's stream-pp preset pinned to the stream-pp box facts;
  * setup-strih step 13 on the stream role: no hub, a present one moved aside (strih_intercom_hub_remove),
    the step-4b / step-12 hub drop-ins not written;
  * verify-strih item 9 on the stream role (strih_program_vban_grade over a fake OBS log) and item 16
    ("no hub", strih_intercom_hub_absent_grade).

Tier-0: bash + pytest only (no OBS, no rig, no root); systemctl and OBS are fakes.
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"
HEADER = REPO / "vendor" / "obs-vban" / "src" / "vban-receive-timeline.h"
VBAN_SOURCE_C = REPO / "vendor" / "obs-vban" / "src" / "vban-source.c"
VBAN_CMAKE = REPO / "vendor" / "obs-vban" / "CMakeLists.txt"
VBAN_KIND = "net.nagater.obs-vban.source"

LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "RIG_GRANDMASTER_IP", "GH_TOKEN", "NDI_DISCOVERY_SENDER_HOSTS",
         "STRIH_HUB_UNIT_FILE", "STRIH_HUB_DROPIN_DIR", "STRIH_HUB_BIN", "STRIH_HUB_CONFIG_DIR",
         "STRIH_HUB_ASIDE_DIR", "STRIH_PROGRAM_VBAN_WAIT_S", "STRIH_PROGRAM_VBAN_KIND_FILE")


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1439", SCRIPTS / "program_audio_vban.py")


def _scenes():
    return _load("strih_scenes_1439", SCRIPTS / "strih_scenes.py")


def _port():
    return _load("obs_collection_port_1439", SCRIPTS / "obs_collection_port.py")


def _bash(body, *args, env=None, path_prefix=None):
    """BODY under the callers' strict mode, the provisioning libs sourced; values reach it as $1.. only."""
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        e["PATH"] = "%s:%s" % (path_prefix, e["PATH"])
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _boxes_with(tmp_path, box, old, new):
    d = tmp_path / "boxes"
    shutil.copytree(BOXES, d)
    f = d / ("%s.env" % box)
    t = f.read_text()
    assert t.count(old) == 1, (box, old)
    f.write_text(t.replace(old, new))
    return d


# --- the facts ---------------------------------------------------------------------------------------------

def test_the_three_vban_facts_sit_before_the_program_sender():
    r = _bash("strih_box_fact_keys")
    keys = r.stdout.split()
    i = keys.index("STRIH_PROGRAM_AUDIO_SAMPLER")
    assert keys[i + 1:i + 5] == ["STRIH_PROGRAM_VBAN_STREAMS", "STRIH_PROGRAM_VBAN_PORT",
                                 "STRIH_PROGRAM_VBAN_RESERVE_MS", "STRIH_PROGRAM_SENDER"]


@pytest.mark.parametrize("box,want", [("stream-pp", "stream|6980|40"), ("strih-snv", "none|none|none"),
                                      ("strih-pp", "none|none|none")])
def test_each_box_declares_its_program_vban(box, want):
    r = _bash('strih_box_load "$1"\nprintf "%s|%s|%s" "$(strih_lx_program_vban_streams)" '
              '"$(strih_lx_program_vban_port)" "$(strih_lx_program_vban_reserve_ms)"', box)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want


@pytest.mark.parametrize("box,old,new,word", [
    ("stream-pp", "STRIH_PROGRAM_VBAN_STREAMS=stream\n", "STRIH_PROGRAM_VBAN_STREAMS=none\n", "ONE VBAN stream"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_STREAMS=stream\n", "STRIH_PROGRAM_VBAN_STREAMS=stream strih\n", "ONE VBAN stream"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_STREAMS=stream\n", "STRIH_PROGRAM_VBAN_STREAMS=abcdefghijklmnopq\n", "ONE VBAN stream"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_PORT=6980\n", "STRIH_PROGRAM_VBAN_PORT=80\n", "1024..65535"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_PORT=6980\n", "STRIH_PROGRAM_VBAN_PORT=70000\n", "1024..65535"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_PORT=6980\n", "STRIH_PROGRAM_VBAN_PORT=none\n", "1024..65535"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_RESERVE_MS=40\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=9\n", "10..500"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_RESERVE_MS=40\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=501\n", "10..500"),
    ("stream-pp", "STRIH_PROGRAM_VBAN_RESERVE_MS=40\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=040\n", "10..500"),
    ("strih-snv", "STRIH_PROGRAM_VBAN_STREAMS=none\n", "STRIH_PROGRAM_VBAN_STREAMS=strih\n", "strih role"),
    ("strih-pp", "STRIH_PROGRAM_VBAN_PORT=none\n", "STRIH_PROGRAM_VBAN_PORT=6981\n", "strih role"),
])
def test_the_loader_refuses_a_wrong_program_vban(tmp_path, box, old, new, word):
    d = _boxes_with(tmp_path, box, old, new)
    r = _bash('strih_box_load "$1"', box, env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0
    assert "STRIH_PROGRAM_VBAN" in r.stderr and word in r.stderr, r.stderr


def _header_define(name):
    m = re.search(r"#define\s+%s\s+([0-9]+)" % name, HEADER.read_text())
    assert m, name
    return int(m.group(1))


def test_the_reserve_clamp_is_the_plugins_own_everywhere():
    lo, hi = _header_define("VBAN_RECV_RESERVE_MS_MIN"), _header_define("VBAN_RECV_RESERVE_MS_MAX")
    r = _bash('printf "%s %s" "$STRIH_BOX_VBAN_RESERVE_MS_MIN" "$STRIH_BOX_VBAN_RESERVE_MS_MAX"')
    assert r.stdout == "%d %d" % (lo, hi)
    assert (PAV.RESERVE_MS_MIN, PAV.RESERVE_MS_MAX) == (lo, hi)
    assert PAV.RESERVE_MS_DEFAULT == _header_define("VBAN_RECV_RESERVE_MS_DEFAULT")


def test_the_source_kind_is_the_vendored_plugin_id_everywhere():
    prefix = re.search(r'set\(ID_PREFIX "([^"]+)"\)', VBAN_CMAKE.read_text()).group(1)
    assert '.id = ID_PREFIX "source",' in VBAN_SOURCE_C.read_text()
    assert PAV.VBAN_SOURCE_KIND == prefix + "source" == VBAN_KIND
    r = _bash('printf "%s" "$STRIH_PROGRAM_VBAN_KIND"')
    assert r.stdout == VBAN_KIND
    assert VBAN_KIND in _port().LINUX_INPUT_KINDS


# --- the one source definition + the manifest ------------------------------------------------------------

def test_vban_source_settings_accepts_the_facts_and_refuses_the_rest():
    assert PAV.vban_source_settings(6980, "stream", 40) == {
        "port": 6980, "stream_name": "stream", "ip_from": "", "receive_buffer_ms": 40}
    for bad in ((80, "stream", 40), (6980, "", 40), (6980, "a" * 17, 40), (6980, "st ream", 40),
                (6980, "stream", 9), (6980, "stream", 501), (True, "stream", 40), ("6980", "stream", 40)):
        with pytest.raises(ValueError):
            PAV.vban_source_settings(*bad)


def _manifest(box):
    r = _bash('strih_box_load "$1"\nobs_box_seed_manifest_json', box)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_stream_manifest_carries_the_vban_source_settings():
    text = _manifest("stream-pp")
    d = json.loads(text)
    assert d["program_audio_vban"] == {"port": 6980, "stream_name": "stream", "receive_buffer_ms": 40}
    assert d["program_audio_input"] == "Zvuk z mixu"
    assert PAV.parse_program_audio_vban(text) == PAV.vban_source_settings(6980, "stream", 40)


def test_a_strih_manifest_carries_no_vban_block():
    text = _manifest("strih-snv")
    assert "program_audio_vban" not in text
    assert PAV.parse_program_audio_vban(text) is None


@pytest.mark.parametrize("text", ['{"program_audio_vban": 5}', '{"program_audio_vban": {"port": 6980}}',
                                  '{"program_audio_vban": {"port": 6980, "stream_name": "x", "receive_buffer_ms": 2}}'])
def test_a_malformed_vban_block_raises_never_reads_as_pulse(text):
    with pytest.raises(ValueError):
        PAV.parse_program_audio_vban(text)


# --- the seeder ------------------------------------------------------------------------------------------

class FakeObs:
    """An OBS with one program-audio input of KIND in scenes PRO (visible) and POST (hidden).

    RemoveInput and CreateInput change what GetInputList lists, like OBS: after a RemoveInput the name
    stays listed (and taken: CreateInput refuses it) for REMOVAL_LAG more GetInputList reads, and the
    first REFUSE_CREATE CreateInput calls are refused (an error answer, nothing created)."""

    def __init__(self, kind="pulse_input_capture", kinds=(VBAN_KIND,), settings=None, present=True,
                 removal_lag=0, refuse_create=0):
        self.calls = []
        self.kind, self.kinds, self.settings, self.present = kind, list(kinds), settings or {}, present
        self.removal_lag, self.refuse_create, self.still_listed = removal_lag, refuse_create, 0

    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if t == "GetInputList":
            listed = self.present
            if self.still_listed:
                self.still_listed -= 1
                self.present = self.still_listed > 0
            return {"inputs": [{"inputName": "Zvuk z mixu", "inputKind": self.kind}] if listed else []}
        if t == "RemoveInput":
            if self.present:
                self.still_listed = self.removal_lag
                self.present = self.still_listed > 0
            return {}
        if t == "CreateInput":
            if self.present or self.refuse_create:
                self.refuse_create = max(0, self.refuse_create - 1)
                return {}
            self.present, self.kind = True, d["inputKind"]
            return {"sceneItemId": 7}
        if t == "GetInputKindList":
            return {"inputKinds": self.kinds}
        if t == "GetSceneList":
            return {"currentProgramSceneName": "PRO",
                    "scenes": [{"sceneName": "POST"}, {"sceneName": "PRO"}, {"sceneName": "PRE"}]}
        if t == "GetSceneItemList":
            held = {"PRO": True, "POST": False}
            if d["sceneName"] in held:
                return {"sceneItems": [{"sourceName": "Zvuk z mixu", "sceneItemEnabled": held[d["sceneName"]]}]}
            return {"sceneItems": []}
        if t == "GetInputSettings":
            return {"inputSettings": self.settings}
        answers = {"GetInputVolume": {"inputVolumeMul": 0.5}, "GetInputMute": {"inputMuted": True},
                   "GetInputAudioMonitorType": {"monitorType": "OBS_MONITORING_TYPE_NONE"},
                   "GetInputAudioTracks": {"inputAudioTracks": {"1": True, "2": False}},
                   "GetInputAudioBalance": {"inputAudioBalance": 0.25},
                   "GetInputAudioSyncOffset": {"inputAudioSyncOffset": 120},
                   "GetSourceFilterList": {"filters": [
                       {"filterName": "Limiter", "filterKind": "limiter_filter", "filterIndex": 1,
                        "filterEnabled": False, "filterSettings": {"threshold": -6.0}},
                       {"filterName": "Gain", "filterKind": "gain_filter", "filterIndex": 0,
                        "filterEnabled": True, "filterSettings": {"db": 3.0}}]},
                   "CreateSceneItem": {"sceneItemId": 9}}
        return answers.get(t, {})

    def writes(self):
        return [(t, d) for t, d in self.calls if not t.startswith("Get")]


VBAN = PAV.vban_source_settings(6980, "stream", 40)


def test_a_pulse_input_becomes_the_vban_source_keeping_the_operator_state():
    s = _scenes()
    obs = FakeObs()
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res == "replaced (was pulse_input_capture; kept in 2 scene(s))"
    kinds = [t for t, _ in obs.calls]
    for t in ("GetSceneItemList", "GetInputVolume", "GetInputMute", "GetInputAudioMonitorType",
              "GetInputAudioTracks", "GetInputAudioBalance", "GetInputAudioSyncOffset", "GetSourceFilterList"):
        assert kinds.index(t) < kinds.index("RemoveInput"), t
    w = obs.writes()
    assert w[0] == ("RemoveInput", {"inputName": "Zvuk z mixu"})
    assert w[1] == ("CreateInput", {"sceneName": "POST", "inputName": "Zvuk z mixu", "inputKind": VBAN_KIND,
                                    "inputSettings": VBAN})
    # POST's item was hidden: it stays hidden; PRO gets a visible item
    assert w[2] == ("SetSceneItemEnabled", {"sceneName": "POST", "sceneItemId": 7, "sceneItemEnabled": False})
    assert w[3] == ("CreateSceneItem", {"sceneName": "PRO", "sourceName": "Zvuk z mixu"})
    rest = w[4:]
    assert ("SetInputVolume", {"inputName": "Zvuk z mixu", "inputVolumeMul": 0.5}) in rest
    assert ("SetInputMute", {"inputName": "Zvuk z mixu", "inputMuted": True}) in rest
    assert ("SetInputAudioMonitorType", {"inputName": "Zvuk z mixu", "monitorType": "OBS_MONITORING_TYPE_NONE"}) in rest
    assert ("SetInputAudioTracks", {"inputName": "Zvuk z mixu", "inputAudioTracks": {"1": True, "2": False}}) in rest
    assert ("SetInputAudioBalance", {"inputName": "Zvuk z mixu", "inputAudioBalance": 0.25}) in rest
    assert ("SetInputAudioSyncOffset", {"inputName": "Zvuk z mixu", "inputAudioSyncOffset": 120}) in rest
    filters = [d for t, d in rest if t == "CreateSourceFilter"]
    assert [f["filterName"] for f in filters] == ["Gain", "Limiter"], "filters in their index order"
    assert filters[1] == {"sourceName": "Zvuk z mixu", "filterName": "Limiter", "filterKind": "limiter_filter",
                          "filterSettings": {"threshold": -6.0}}
    assert ("SetSourceFilterEnabled", {"sourceName": "Zvuk z mixu", "filterName": "Limiter",
                                       "filterEnabled": False}) in rest
    assert not any(t == "SetSourceFilterEnabled" and d["filterName"] == "Gain" for t, d in rest)


def test_a_kind_obs_has_not_loaded_never_removes_the_input():
    s = _scenes()
    obs = FakeObs(kinds=("pulse_input_capture",))
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res.startswith("kept pulse_input_capture") and "obs-vban" in res
    assert obs.writes() == []


def test_an_unanswered_kind_list_never_removes_the_input():
    s = _scenes()
    obs = FakeObs(kinds=())
    obs.kinds = None
    assert s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN).startswith("kept ")
    assert obs.writes() == []


def test_a_matching_vban_input_is_left_alone():
    s = _scenes()
    obs = FakeObs(kind=VBAN_KIND, settings={"port": 6980, "stream_name": "stream", "receive_buffer_ms": 40})
    assert s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN) == "matched"
    assert obs.writes() == []


def test_the_operator_settings_of_a_vban_input_win_and_are_named():
    s = _scenes()
    obs = FakeObs(kind=VBAN_KIND, settings={"port": 6980, "stream_name": "stream", "receive_buffer_ms": 60})
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res.startswith("matched (the operator's settings differ from the box facts")
    assert "receive_buffer_ms 60 != 40" in res
    assert obs.writes() == []


def test_an_absent_input_is_created_as_the_vban_source():
    s = _scenes()
    obs = FakeObs(present=False)
    assert s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN) == "created"
    assert obs.writes() == [("CreateInput", {"sceneName": "PRO", "inputName": "Zvuk z mixu",
                                             "inputKind": VBAN_KIND, "inputSettings": VBAN})]


def _no_sleep(monkeypatch):
    slept = []
    monkeypatch.setattr("time.sleep", slept.append)
    return slept


def _restored(obs):
    return [t for t, _ in obs.calls if t in ("SetInputVolume", "SetInputMute", "SetInputAudioSyncOffset",
                                             "CreateSourceFilter", "CreateSceneItem")]


def test_a_name_obs_still_holds_after_remove_is_waited_for(monkeypatch):
    # review round 1: CreateInput right after RemoveInput can meet the old name still taken
    s = _scenes()
    slept = _no_sleep(monkeypatch)
    obs = FakeObs(removal_lag=3)
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res == "replaced (was pulse_input_capture; kept in 2 scene(s))"
    assert obs.present and obs.kind == VBAN_KIND
    assert [t for t, _ in obs.calls].count("CreateInput") == 1, "created once, after the name was free"
    assert slept and sum(slept) <= 3.0
    assert "SetInputVolume" in _restored(obs)


def test_a_refused_create_is_tried_once_more(monkeypatch):
    s = _scenes()
    _no_sleep(monkeypatch)
    obs = FakeObs(refuse_create=1)
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res == "replaced (was pulse_input_capture; kept in 2 scene(s))"
    assert obs.present and obs.kind == VBAN_KIND
    assert [t for t, _ in obs.calls].count("CreateInput") == 2


def test_a_create_obs_keeps_refusing_is_a_loud_failure_never_replaced(monkeypatch):
    s = _scenes()
    _no_sleep(monkeypatch)
    obs = FakeObs(refuse_create=9)
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res.startswith("replace FAILED (was pulse_input_capture)"), res
    assert "no 'Zvuk z mixu' input" in res and "restart OBS" in res
    assert [t for t, _ in obs.calls].count("CreateInput") == 2, "one retry, never a loop"
    assert _restored(obs) == [], "nothing is put back onto an input that does not exist"


def test_a_removal_obs_never_applies_is_bounded_and_named(monkeypatch):
    s = _scenes()
    slept = _no_sleep(monkeypatch)
    obs = FakeObs(removal_lag=10_000)
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res.startswith("replace FAILED (was pulse_input_capture)"), res
    assert "still lists 'Zvuk z mixu' as pulse_input_capture" in res
    assert sum(slept) <= 6.5, "two bounded waits for the name, never an endless one"
    assert [t for t, _ in obs.calls].count("RemoveInput") == 2, "the retry removes the old input again"
    assert _restored(obs) == []


def test_an_absent_input_obs_refuses_to_create_is_named(monkeypatch):
    s = _scenes()
    _no_sleep(monkeypatch)
    obs = FakeObs(present=False, refuse_create=9)
    res = s.seed_program_audio_input(obs, "Zvuk z mixu", VBAN)
    assert res.startswith("create FAILED"), res
    assert "no 'Zvuk z mixu' input" in res
    assert [t for t, _ in obs.calls].count("CreateInput") == 2
    assert "RemoveInput" not in [t for t, _ in obs.calls], "a create never removes anything"


def test_without_vban_the_seed_stays_the_pulse_capture():
    s = _scenes()
    obs = FakeObs(kind="pulse_input_capture")
    assert s.seed_program_audio_input(obs, "Zvuk z mixu") == "matched"
    obs = FakeObs(kind="wasapi_input_capture")
    s.seed_program_audio_input(obs, "Zvuk z mixu")
    created = [d for t, d in obs.calls if t == "CreateInput"][0]
    assert created["inputKind"] == "pulse_input_capture"
    assert not any(t == "GetInputKindList" for t, _ in obs.calls), "the pulse capture is always built in"


def test_an_unreadable_vban_block_leaves_the_input_untouched():
    s = _scenes()
    obs = FakeObs()
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu",
                      audio_vban="program_audio_vban is not an object")
    assert res["Zvuk z mixu"].startswith("untouched: the manifest's program_audio_vban is unreadable")
    assert not any(t in ("RemoveInput", "CreateInput") for t, _ in obs.calls)


def test_main_hands_the_manifest_vban_to_the_bootstrap():
    src = (SCRIPTS / "strih_scenes.py").read_text()
    assert "audio_vban = parse_program_audio_vban(manifest_text)" in src
    assert "audio_vban=audio_vban)" in src


# --- the collection port ---------------------------------------------------------------------------------

def test_the_stream_pp_preset_is_the_stream_pp_box_facts():
    r = _bash('strih_box_load stream-pp\nprintf "%s %s %s" "$(strih_lx_program_vban_port)" '
              '"$(strih_lx_program_vban_streams)" "$(strih_lx_program_vban_reserve_ms)"')
    port, stream, ms = r.stdout.split()
    assert _port().PRESETS["stream-pp"]["program_audio_vban"] == {
        "port": int(port), "stream_name": stream, "receive_buffer_ms": int(ms)}
    assert "program_audio_vban" not in _port().PRESETS["strih-pp"], "the strih role is not switched (issue 1439)"


@pytest.mark.parametrize("arg", ["6980:stream", "x:stream:40", "6980:stream:9"])
def test_a_malformed_program_audio_vban_refuses(tmp_path, arg):
    inp = tmp_path / "in.json"
    inp.write_text(json.dumps({"name": "x", "sources": [
        {"name": "Zvuk z mixu", "id": "wasapi_input_capture", "versioned_id": "wasapi_input_capture",
         "settings": {}}]}))
    m = _port()
    rc = m.main([str(inp), str(tmp_path / "out.json"), "--preset", "stream-pp", "--all-scenes",
                 "--program-audio-vban", arg])
    assert rc == 2
    assert not (tmp_path / "out.json").exists()


# --- setup-strih step 13 on the stream role: no hub, a present one moved aside -----------------------------

def _fake_systemctl(tmp_path, active, enabled, fail=""):
    stub = tmp_path / "stub"
    stub.mkdir(exist_ok=True)
    log = tmp_path / "systemctl.log"
    f = stub / "systemctl"
    f.write_text('#!/bin/bash\necho "$*" >> "%s"\n'
                 'case "$1" in\n'
                 '  is-active) [ "$2" = --quiet ] || echo %s; [ %s = active ] ;;\n'
                 '  is-enabled) [ "$2" = --quiet ] || echo %s; [ %s = enabled ] ;;\n'
                 '  %s) exit 1 ;;\n'
                 '  *) exit 0 ;;\n'
                 'esac\n' % (log, active, active, enabled, enabled, fail or "never-fails"))
    f.chmod(0o755)
    return stub, log


def _hub_env(tmp_path):
    return {"STRIH_HUB_UNIT_FILE": str(tmp_path / "etc/systemd/system/intercom-hub.service"),
            "STRIH_HUB_DROPIN_DIR": str(tmp_path / "etc/systemd/system/intercom-hub.service.d"),
            "STRIH_HUB_BIN": str(tmp_path / "usr/local/bin/intercom-hub"),
            "STRIH_HUB_CONFIG_DIR": str(tmp_path / "etc/intercom-hub"),
            "STRIH_HUB_ASIDE_DIR": str(tmp_path / "aside")}


def _plant_hub(tmp_path):
    e = _hub_env(tmp_path)
    Path(e["STRIH_HUB_UNIT_FILE"]).parent.mkdir(parents=True)
    Path(e["STRIH_HUB_UNIT_FILE"]).write_text("[Unit]\n")
    Path(e["STRIH_HUB_DROPIN_DIR"]).mkdir()
    (Path(e["STRIH_HUB_DROPIN_DIR"]) / "ndi-discovery.conf").write_text("[Service]\n")
    Path(e["STRIH_HUB_BIN"]).parent.mkdir(parents=True)
    Path(e["STRIH_HUB_BIN"]).write_text("bin")
    Path(e["STRIH_HUB_CONFIG_DIR"]).mkdir(parents=True)
    (Path(e["STRIH_HUB_CONFIG_DIR"]) / "intercom.toml").write_text("x")
    return e


def test_remove_stops_disables_then_moves_every_hub_file_aside(tmp_path):
    e = _plant_hub(tmp_path)
    stub, log = _fake_systemctl(tmp_path, "active", "enabled")
    r = _bash("strih_intercom_hub_remove", env=e, path_prefix=stub)
    assert r.returncode == 0, r.stderr
    calls = log.read_text().splitlines()
    assert calls.index("stop intercom-hub") < calls.index("disable intercom-hub") < calls.index("daemon-reload")
    for k in ("STRIH_HUB_UNIT_FILE", "STRIH_HUB_DROPIN_DIR", "STRIH_HUB_BIN", "STRIH_HUB_CONFIG_DIR"):
        assert not os.path.lexists(e[k]), k
    moved = list((tmp_path / "aside").glob("*" + str(tmp_path) + "/etc/intercom-hub/intercom.toml"))
    assert len(moved) == 1 and moved[0].read_text() == "x", "moved aside, never deleted"
    assert r.stdout.count("moved ") == 4


def test_remove_is_a_no_op_on_a_box_without_a_hub(tmp_path):
    e = _hub_env(tmp_path)
    stub, log = _fake_systemctl(tmp_path, "inactive", "disabled")
    r = _bash("strih_intercom_hub_remove", env=e, path_prefix=stub)
    assert r.returncode == 0, r.stderr
    assert "nothing to remove" in r.stdout
    calls = log.read_text().splitlines()
    assert not any(c.startswith(("stop", "disable", "daemon-reload")) for c in calls), calls
    assert not (tmp_path / "aside").exists()


def test_a_failed_stop_moves_nothing(tmp_path):
    e = _plant_hub(tmp_path)
    stub, _log = _fake_systemctl(tmp_path, "active", "enabled", fail="stop")
    r = _bash("strih_intercom_hub_remove || echo RC=$?", env=e, path_prefix=stub)
    assert "RC=1" in r.stdout and "could not stop" in r.stderr
    assert os.path.exists(e["STRIH_HUB_UNIT_FILE"])


def test_setup_step_13_removes_the_hub_on_the_stream_role(tmp_path):
    s = SETUP.read_text()
    a = s.index('if WHY="$(obs_box_step_applies 13)"; then')
    end = s.index("\nelse echo \"  SKIP: ${WHY}\"; strih_intercom_hub_remove", a)
    block = s[a:s.index("fi\n", end) + 3]
    e = _plant_hub(tmp_path)
    stub, log = _fake_systemctl(tmp_path, "active", "enabled")
    body = ('fail() { echo "FAIL $*"; exit 1; }\nwarn() { echo "WARN $*"; }\nstrih_box_load stream-pp\n'
            'HERE=/nonexistent\n' + block)
    r = _bash(body, env=e, path_prefix=stub)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "SKIP: the stream role runs no intercom hub" in r.stdout
    assert not os.path.lexists(e["STRIH_HUB_UNIT_FILE"])
    assert "enable intercom-hub" not in log.read_text()


def test_setup_writes_no_hub_dropin_on_the_stream_role():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    for write in ('ndi_discovery_dropin_content > "$NDI_DISCOVERY_INTERCOM_DROPIN"',
                  '> /etc/systemd/system/intercom-hub.service.d/10-local-audio.conf'):
        line = next(ln for ln in s.splitlines() if write in ln)
        assert line.startswith("if obs_box_step_applies 13 >/dev/null; then mkdir -p "), line
    step13 = s[s.index('step 13 "'):s.index('step 14 "')]
    assert step13.index('if WHY="$(obs_box_step_applies 13)"; then') \
        < step13.index('install -Dm644 "${HERE}/../systemd/intercom-hub.service"') \
        < step13.index("strih_intercom_hub_binary_install") < step13.index("strih_intercom_hub_remove ||")


def test_setup_installs_the_vban_module_next_to_the_seeder():
    s = SETUP.read_text()
    assert 'install -m 0644 "${HERE}/program_audio_vban.py" /usr/local/bin/program_audio_vban.py' in s


def test_the_role_table_skips_the_hub_on_the_stream_role_only():
    body = 'strih_box_load "$1"\nif WHY="$(obs_box_step_applies 13)"; then echo RUN; else echo "SKIP $WHY"; fi'
    assert _bash(body, "strih-snv").stdout.strip() == "RUN"
    assert _bash(body, "strih-pp").stdout.strip() == "RUN"
    out = _bash(body, "stream-pp").stdout.strip()
    assert out.startswith("SKIP the stream role runs no intercom hub") and "issue 1439" in out


# --- verify-strih item 9 on the stream role: the obs-vban source ---------------------------------------------

LINE = ("12:00:00.000: [obs-vban] obs-vban receive: received=%d lost=%d late=0 re_anchors=%d silence_ms=0.0 "
        "max_late_ms=-34.7 sender_ppm=+1.400 phase_ms=+0.01 fit=locked receive_buffer_ms=%d rate=48000 ch=2 "
        "port=%d stream='%s' source='%s'\n")


def _line(received, port=6980, stream="stream", r=40, lost=0, re_anchors=0, source="Zvuk z mixu"):
    return LINE % (received, lost, re_anchors, r, port, stream, source)


def _grade(tmp_path, log_text, kind=VBAN_KIND, append=None):
    log = tmp_path / "obs.txt"
    log.write_text(log_text)
    kf = tmp_path / "kind"
    kf.write_text(kind)
    later = ""
    if append is not None:
        (tmp_path / "later.txt").write_text(append)
        later = '( sleep 0.3; cat "$2" >> "$1" ) &\n'
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_box_load stream-pp\n' + later + 'strih_program_vban_grade /nonexistent 127.0.0.1 "$1"\nwait')
    r = _bash(body, log, tmp_path / "later.txt",
              env={"STRIH_PROGRAM_VBAN_KIND_FILE": str(kf), "STRIH_PROGRAM_VBAN_WAIT_S": "1" if append else "0"})
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def _rows(rows, verdict):
    return [r for r in rows if r.startswith(verdict)]


def test_a_live_vban_source_passes(tmp_path):
    rows = _grade(tmp_path, _line(100), append=_line(2000))
    assert not _rows(rows, "FAIL") and not _rows(rows, "NOTE"), rows
    assert any("is the obs-vban receive source" in r for r in rows)
    assert any("receiving stream 'stream' on UDP 6980: received 100 -> 2000" in r for r in rows), rows


def test_no_new_packet_is_a_note_never_a_pass(tmp_path):
    rows = _grade(tmp_path, _line(100))
    assert not _rows(rows, "FAIL"), rows
    assert any(r.startswith("NOTE") and "none in the last 0 s" in r for r in rows), rows
    assert not any("receiving stream" in r for r in rows)


def test_the_newest_line_of_this_source_is_read(tmp_path):
    text = _line(500) + _line(90000, source="Other VBAN") + _line(600)
    rows = _grade(tmp_path, text, append=_line(700, source="Other VBAN") + _line(800))
    assert any("received 600 -> 800" in r for r in rows), rows


def test_a_pulse_input_fails(tmp_path):
    rows = _grade(tmp_path, _line(100), kind="pulse_input_capture")
    assert any(r.startswith("FAIL") and "'pulse_input_capture', not the obs-vban receive source" in r
               for r in rows), rows


def test_no_status_line_fails_naming_the_port(tmp_path):
    rows = _grade(tmp_path, "12:00:00.000: something else\n")
    assert any(r.startswith("FAIL") and "no obs-vban receive status line" in r and ":6980" in r
               and "'stream'" in r for r in rows), rows


def test_a_bind_failure_fails_naming_the_hub(tmp_path):
    rows = _grade(tmp_path, "12:00:00.000: [obs-vban] Failed to bind port 6980\n")
    assert any(r.startswith("FAIL") and "could not bind UDP 6980" in r and "step 13" in r for r in rows), rows


def test_a_port_or_stream_off_the_facts_fails(tmp_path):
    rows = _grade(tmp_path, _line(100, port=6981, stream="strih"))
    fails = _rows(rows, "FAIL")
    assert any("UDP 6981" in r and "STRIH_PROGRAM_VBAN_PORT" in r for r in fails), rows
    assert any("'strih'" in r and "STRIH_PROGRAM_VBAN_STREAMS" in r for r in fails), rows


def test_an_operator_r_is_a_note(tmp_path):
    rows = _grade(tmp_path, _line(100, r=60), append=_line(200, r=60))
    assert not _rows(rows, "FAIL"), rows
    assert any(r.startswith("NOTE") and "receive_buffer_ms=60" in r and "fact says 40" in r for r in rows), rows


def test_lost_or_re_anchored_audio_is_named(tmp_path):
    rows = _grade(tmp_path, _line(100), append=_line(200, lost=3, re_anchors=1))
    assert any(r.startswith("NOTE") and "lost=3 late=0 re_anchors=1" in r for r in rows), rows


def test_verify_wiring_and_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    item9 = v[v.index("# 9) PipeWire program audio"):v.index("# 9b)")]
    assert 'if ! WHY="$(obs_box_step_applies 13)"; then strih_program_vban_grade "$SCN_BIN" "$WS_HOST" "$LOG"; else' \
        in item9
    assert item9.index("strih_program_vban_grade") < item9.index("AUDIO_SINK=0") < item9.index("esac; fi")
    item16 = v[v.index("# 16) intercom hub unit"):v.index("# 17)")]
    assert item16.index('if ! WHY="$(obs_box_step_applies 13)"; then strih_intercom_hub_absent_grade "$WHY"') \
        < item16.index("elif [ -f /etc/systemd/system/intercom-hub.service ]; then")
