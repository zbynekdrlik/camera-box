"""Issue 1436 -- the stream-role headphone monitor plays the REAL outgoing stream: the provisioning half.

WHY (owner 15.10.2026): "a spomen si ako to mame riesene v snv ze sa pocuva cez vlc ktore ma nacitany
stream rtmp z obs alebo z restreamera" + "pozadujem aby sa budovalo jednotne riesenie pp/snv". The
stream-pp headphones played ONE OBS source (`Zvuk z mixu` monitored to the jack), not the stream. The
main's design (comment 6189806622): setup-strih gives the stream OBS an explicit key derived from the
box name, the seeder turns the source monitoring off, and a session app plays the restreamer's RTMP.

What this pins (the player itself: test_strih_stream_monitor_1436.py):
  * scripts/obs_box_stream_profile.py -- service.json gets the box key when it carries none (operator
    wins: an existing key is kept), and `--grade-key` reads the drift;
  * scripts/lib/obs-box-role.sh -- the stream step 3 passes the box name as the key; the stream seed
    manifest declares `program_audio_monitor: off`; a strih manifest does not;
  * scripts/strih_scenes.py -- the seeder sets `Zvuk z mixu` to Monitor Off (update-only, named in the
    seed log), and `--audio-monitor-type` reads it for verify.

Tier-0: bash + pytest only (no cargo, no root, no rig).
"""
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_BOXES_DIR", "OBS_FLEET", "OBS_BOX_STREAM_RTMP_SERVER",
         "NDI_DISCOVERY_SENDER_HOSTS")


def _bash(body, env=None, sources=(FACTS, PROVISION), path_prefix=None):
    harness = "set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        e["PATH"] = "%s:%s" % (path_prefix, e["PATH"])
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=e, cwd=str(REPO),
                          timeout=60)


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _profile():
    return _load("obs_box_stream_profile_1436", SCRIPTS / "obs_box_stream_profile.py")


def _scenes():
    return _load("strih_scenes_1436", SCRIPTS / "strih_scenes.py")


@pytest.fixture
def boxes(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    return d


# --- service.json: the box key ---------------------------------------------------------------------------

def test_a_new_service_json_carries_the_box_key():
    m = _profile()
    svc = m.service_json(None, "rtmp://127.0.0.1:1234/live", key="stream-pp")
    assert svc == {"type": "rtmp_custom",
                   "settings": {"server": "rtmp://127.0.0.1:1234/live", "use_auth": False, "key": "stream-pp"}}


@pytest.mark.parametrize("existing", [
    {"type": "rtmp_custom", "settings": {"server": "rtmp://127.0.0.1:1234/live", "use_auth": False}},
    {"type": "rtmp_custom", "settings": {"server": "rtmp://127.0.0.1:1234/live", "key": ""}},
    {"type": "rtmp_custom", "settings": {"server": "rtmp://127.0.0.1:1234/live", "key": "   "}},
])
def test_an_empty_or_missing_key_gets_the_box_key(existing):
    m = _profile()
    svc = m.service_json(existing, "rtmp://127.0.0.1:1234/live", key="stream-pp")
    assert svc["settings"]["key"] == "stream-pp"


def test_the_operator_key_wins():
    m = _profile()
    existing = {"type": "rtmp_custom", "settings": {"server": "rtmp://127.0.0.1:1234/live", "key": "obs-e2e-test"}}
    assert m.service_json(existing, "rtmp://127.0.0.1:1234/live", key="stream-pp")["settings"]["key"] == "obs-e2e-test"


def test_apply_writes_the_key_and_logs_it(tmp_path):
    m = _profile()
    d = tmp_path / "prof"
    lines = m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://127.0.0.1:1234/live", "stream-pp", key="stream-pp")
    svc = json.loads((d / "service.json").read_text())
    assert svc["settings"]["key"] == "stream-pp"
    assert oct((d / "service.json").stat().st_mode & 0o777) == "0o600"
    assert any("stream key stream-pp" in ln for ln in lines), lines


def test_apply_without_a_key_keeps_the_old_contract(tmp_path):
    # strih role / a caller that passes no key: nothing is invented
    m = _profile()
    d = tmp_path / "prof"
    m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://127.0.0.1:1234/live", "stream-pp")
    assert "key" not in json.loads((d / "service.json").read_text())["settings"]


@pytest.mark.parametrize("key,ok", [("stream-pp", True), ("x", False), (None, False), ("", False)])
def test_grade_key(tmp_path, key, ok):
    m = _profile()
    d = tmp_path / "prof"
    d.mkdir()
    settings = {"server": "rtmp://127.0.0.1:1234/live"}
    if key is not None:
        settings["key"] = key
    (d / "service.json").write_text(json.dumps({"type": "rtmp_custom", "settings": settings}))
    drift = m.grade_key(str(d), "stream-pp")
    assert (drift == []) is ok, drift
    if not ok:
        assert "want 'stream-pp'" in drift[0]


def test_grade_key_names_a_missing_or_unreadable_service_json(tmp_path):
    m = _profile()
    assert "service.json unreadable" in m.grade_key(str(tmp_path / "none"), "stream-pp")[0]
    d = tmp_path / "bad"
    d.mkdir()
    (d / "service.json").write_text("{not json")
    assert "service.json unreadable" in m.grade_key(str(d), "stream-pp")[0]


def test_grade_key_names_a_server_that_is_not_the_local_restreamer(tmp_path):
    m = _profile()
    d = tmp_path / "prof"
    d.mkdir()
    (d / "service.json").write_text(json.dumps({"type": "rtmp_custom", "settings": {
        "server": "rtmp://a.rtmp.youtube.com/live2", "key": "stream-pp"}}))
    drift = m.grade_key(str(d), "stream-pp")
    assert drift and "server" in drift[0]


def test_the_cli_grade_key_prints_nothing_on_a_match(tmp_path):
    m = _profile()
    d = tmp_path / "prof"
    m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://127.0.0.1:1234/live", "stream-pp", key="stream-pp")
    r = subprocess.run([sys.executable, str(SCRIPTS / "obs_box_stream_profile.py"), "--grade-key", str(d),
                        "--key", "stream-pp"], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout == "", (r.stdout, r.stderr)
    r = subprocess.run([sys.executable, str(SCRIPTS / "obs_box_stream_profile.py"), "--grade-key", str(d),
                        "--key", "other"], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and "want 'other'" in r.stdout


# --- step 3: the stream role passes the box name as the key -------------------------------------------------

def _box_bin(tmp_path):
    b = tmp_path / "boxbin"
    b.mkdir()
    for name, body in {
        "lspci": "echo '04:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Barcelo'",
        "apt-get": "exit 0", "usermod": "exit 0", "chown": "exit 0", "pgrep": "exit 1",
        "vainfo": "printf '      VAProfileH264High               :\\tVAEntrypointEncSlice\\n'",
    }.items():
        (b / name).write_text("#!/bin/bash\n%s\n" % body)
        (b / name).chmod(0o755)
    return b


def test_the_stream_encoder_step_writes_the_box_key(tmp_path, boxes):
    cfg = tmp_path / "obs-studio"
    r = _bash('strih_box_load stream-pp\nobs_box_stream_encoder_provision "%s" newlevel stream-pp' % cfg,
              env={"STRIH_BOXES_DIR": str(boxes)}, path_prefix=_box_bin(tmp_path))
    assert r.returncode == 0, r.stderr
    svc = json.loads((cfg / "basic" / "profiles" / "stream-pp" / "service.json").read_text())
    assert svc["settings"]["key"] == "stream-pp"
    assert "stream key stream-pp" in r.stdout


def test_the_stream_key_is_the_box_hostname(boxes):
    r = _bash('strih_box_load stream-pp\nprintf "%s|%s" "$(obs_box_stream_key)" "$(strih_lx_hostname)"',
              env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    assert r.stdout == "stream-pp|stream-pp"


# --- the seed manifest + the seeder: Monitor Off ----------------------------------------------------------------

def _manifest(boxes, box):
    r = _bash("strih_box_load %s\nobs_box_seed_manifest_json" % box, env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_stream_manifest_declares_the_program_audio_monitor_off(boxes):
    text = _manifest(boxes, "stream-pp")
    d = json.loads(text)
    assert d["program_audio_monitor"] == "off"
    assert _scenes().parse_program_audio_monitor(text) == "off"


def test_a_strih_manifest_leaves_the_monitoring_alone(boxes):
    text = _manifest(boxes, "strih-snv")
    assert "program_audio_monitor" not in text
    assert _scenes().parse_program_audio_monitor(text) is None


@pytest.mark.parametrize("text,want", [("{not json", None), ('{"program_audio_monitor": 5}', None),
                                       ('{"program_audio_monitor": "on"}', None), ("[]", None),
                                       ('{"program_audio_monitor": "off"}', "off")])
def test_parse_program_audio_monitor_is_lenient(text, want):
    assert _scenes().parse_program_audio_monitor(text) == want


class _FakeObs:
    def __init__(self, monitor, accept=True):
        self.monitor = monitor
        self.accept = accept
        self.calls = []

    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if t == "GetInputList":
            return {"inputs": [{"inputName": "Zvuk z mixu", "inputKind": "pulse_input_capture"}]}
        if t == "GetSceneList":
            return {"currentProgramSceneName": "PRO", "scenes": [{"sceneName": "PRO"}]}
        if t == "GetSceneItemList":
            return {"sceneItems": [{"sourceName": "Zvuk z mixu"}]}
        if t == "GetInputAudioMonitorType":
            return {"monitorType": self.monitor} if self.monitor else {}
        if t == "SetInputAudioMonitorType" and self.accept:
            self.monitor = d["monitorType"]
        return {}


def test_the_seeder_turns_a_monitored_program_audio_off():
    s = _scenes()
    obs = _FakeObs("OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT")
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu", audio_monitor="off")
    assert ("SetInputAudioMonitorType", {"inputName": "Zvuk z mixu",
                                         "monitorType": "OBS_MONITORING_TYPE_NONE"}) in obs.calls
    assert obs.monitor == "OBS_MONITORING_TYPE_NONE"
    assert "monitor off (was OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT)" in res["Zvuk z mixu"]


def test_the_seeder_leaves_an_unmonitored_input_alone():
    s = _scenes()
    obs = _FakeObs("OBS_MONITORING_TYPE_NONE")
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu", audio_monitor="off")
    assert not [c for c in obs.calls if c[0] == "SetInputAudioMonitorType"]
    assert res["Zvuk z mixu"].endswith("monitor off")


def test_the_seeder_names_a_refused_monitor_set():
    s = _scenes()
    obs = _FakeObs("OBS_MONITORING_TYPE_MONITOR_ONLY", accept=False)
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu", audio_monitor="off")
    assert "still OBS_MONITORING_TYPE_MONITOR_ONLY" in res["Zvuk z mixu"]


def test_the_seeder_never_touches_monitoring_without_the_manifest_key():
    s = _scenes()
    obs = _FakeObs("OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT")
    s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu")
    assert not [c for c in obs.calls if c[0] in ("GetInputAudioMonitorType", "SetInputAudioMonitorType")]


def test_the_seeder_cli_passes_the_manifest_monitor_key():
    src = (SCRIPTS / "strih_scenes.py").read_text()
    assert "audio_monitor=parse_program_audio_monitor(manifest_text)" in src


def test_audio_monitor_type_mode_reports_unreachable_obs_with_rc_2(tmp_path):
    man = tmp_path / "seed.json"
    man.write_text('{"program_audio_input": "Zvuk z mixu"}')
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()  # nothing listens there
    r = subprocess.run([sys.executable, str(SCRIPTS / "strih_scenes.py"), "--audio-monitor-type",
                        "--manifest", str(man), "--port", str(port)], capture_output=True, text=True, timeout=60)
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert r.stdout.startswith("unreachable:")


def test_monitor_type_of_reads_the_named_input():
    s = _scenes()
    assert s.audio_monitor_type_of(_FakeObs("OBS_MONITORING_TYPE_NONE"), "Zvuk z mixu") == "OBS_MONITORING_TYPE_NONE"

    class Absent(_FakeObs):
        def req(self, t, d=None, ignore_err=False):
            if t == "GetInputList":
                return {"inputs": []}
            return super().req(t, d, ignore_err)
    assert s.audio_monitor_type_of(Absent("x"), "Zvuk z mixu") == "absent"
