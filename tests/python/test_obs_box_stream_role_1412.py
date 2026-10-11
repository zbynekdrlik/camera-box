"""issue 1412, slice B: the STREAM role on a Linux OBS box (stream-pp) + the PP venue's NDI sender list.

Covers:
  * the facts STRIH_PROGRAM_SENDER / STRIH_VENUE and the stream-only relaxations (cameras none, HDMI none);
  * scripts/lib/obs-box-role.sh: the role -> step table, obs_box_role_apply (keeper-only session apps, the
    PP sender list for step 4b / verify item 34), the stream seed manifest, the [NDIPlugin] output set,
    the encoder by GPU + the profile (scripts/obs_box_stream_profile.py);
  * scripts/strih_scenes.py: the "program" class (genlock, the latency pin never written) and the
    manifest's program_audio_input;
  * the session-apps grader's row count for a keeper-only box;
  * intercom/intercom.stream-pp.toml vs the hub load rules (the real loader: intercom/hub/tests/
    deployed_matrix_pp_1412.rs at CI);
  * the setup-strih / verify-strih wiring.

Tier-0: bash + pytest only (no cargo, no root, no rig; box tools are fakes on PATH).
"""
import configparser
import importlib.util
import json
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
NDI = SCRIPTS / "lib" / "ndi-discovery.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"

LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "RIG_GRANDMASTER_IP", "GH_TOKEN", "NDI_DISCOVERY_SENDER_HOSTS",
         "OBS_BOX_STREAM_RTMP_SERVER")


def _env(extra=None, path_prefix=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        e["PATH"] = "%s:%s" % (path_prefix, e["PATH"])
    if extra:
        e.update(extra)
    return e


def _bash(body, env=None, sources=(FACTS, PROVISION), path_prefix=None, strict=False):
    harness = ("set -euo pipefail\n" if strict else "set -uo pipefail\n") + \
        "".join('. "%s"\n' % s for s in sources) + body
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True,
                          env=_env(env, path_prefix), cwd=str(REPO), timeout=60)


@pytest.fixture
def boxes(tmp_path):
    """The real fact files with the one open fact (the NDI runtime peer) answered."""
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    return d


def _scenes():
    spec = importlib.util.spec_from_file_location("strih_scenes_1412", SCRIPTS / "strih_scenes.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _profile_mod():
    spec = importlib.util.spec_from_file_location("obs_box_stream_profile", SCRIPTS / "obs_box_stream_profile.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


# --- facts ------------------------------------------------------------------------------------------

def test_the_program_sender_and_venue_close_the_fact_list():
    r = _bash("strih_box_fact_keys", sources=(FACTS,))
    assert r.stdout.split()[-2:] == ["STRIH_PROGRAM_SENDER", "STRIH_VENUE"]


def test_stream_pp_loads_as_the_stream_role(boxes):
    r = _bash('strih_box_load stream-pp || exit 9\n'
              'printf "%s|%s|%s|%s|%s|[%s]\\n" "$(strih_box_role)" "$(strih_lx_program_sender)" "$(strih_box_venue)" '
              '"$(strih_lx_hdmi_output_backend)" "$(strih_lx_dantesync_args)" "$(strih_lx_cameras)"',
              env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "stream|STRIH-PP (2ME PGM)|pp|none|--ntp-server strih-pp.lan|[]"


def test_stream_pp_loads_with_every_fact_answered():
    """11.10.2026: the last open fact (the NDI runtime peer) is answered -- SNV cam5, the fleet runtime."""
    r = _bash("strih_box_load stream-pp || exit 9\nstrih_lx_ndi_runtime_peer", sources=(FACTS,))
    assert r.returncode == 0, r.stderr
    assert "TODO_OWNER" not in r.stderr
    assert r.stdout.strip() == "10.77.9.65"


@pytest.mark.parametrize("box,old,new,word", [
    ("strih-lx", "STRIH_PROGRAM_SENDER=none", "STRIH_PROGRAM_SENDER=STRIH-PP (2ME PGM)", "STRIH_PROGRAM_SENDER"),
    ("stream-pp", "STRIH_PROGRAM_SENDER=STRIH-PP (2ME PGM)", "STRIH_PROGRAM_SENDER=none", "STRIH_PROGRAM_SENDER"),
    ("stream-pp", "STRIH_PROGRAM_SENDER=STRIH-PP (2ME PGM)", "STRIH_PROGRAM_SENDER=strih-pp", "STRIH_PROGRAM_SENDER"),
    ("strih-lx", "STRIH_CAMERAS=1 2 3 4 5 6 7", "STRIH_CAMERAS=none", "STRIH_CAMERAS"),
    ("strih-lx", "STRIH_HDMI_OUTPUT_BACKEND=vk-direct", "STRIH_HDMI_OUTPUT_BACKEND=none", "STRIH_HDMI_OUTPUT_BACKEND"),
    ("strih-lx", "STRIH_VENUE=snv", "STRIH_VENUE=SNV", "STRIH_VENUE"),
])
def test_a_role_mismatched_fact_refuses_by_name(boxes, box, old, new, word):
    f = boxes / ("%s.env" % box)
    body = f.read_text()
    assert body.count(old + "\n") == 1, old
    f.write_text(body.replace(old + "\n", new + "\n"))
    r = _bash("strih_box_load %s" % box, env={"STRIH_BOXES_DIR": str(boxes)}, sources=(FACTS,))
    assert r.returncode != 0
    assert word in r.stderr, r.stderr


# --- the role -> step table ---------------------------------------------------------------------------

def _steps(boxes, box):
    body = ('strih_box_load %s || exit 9\nfor s in 3-nvenc 6-hdmi 11b 14 16 16c 16e 4b; do '
            'if WHY="$(obs_box_step_applies $s)"; then echo "$s RUN"; else echo "$s SKIP"; fi; done' % box)
    r = _bash(body, env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    return dict(ln.split() for ln in r.stdout.splitlines())


def test_the_stream_role_skips_the_strih_only_steps(boxes):
    assert _steps(boxes, "stream-pp") == {"3-nvenc": "SKIP", "6-hdmi": "SKIP", "11b": "RUN", "14": "SKIP",
                                         "16": "SKIP", "16c": "SKIP", "16e": "SKIP", "4b": "RUN"}


def test_the_strih_role_runs_them(boxes):
    assert _steps(boxes, "strih-pp") == {"3-nvenc": "RUN", "6-hdmi": "RUN", "11b": "SKIP", "14": "RUN",
                                        "16": "RUN", "16c": "RUN", "16e": "SKIP", "4b": "RUN"}
    # issue 1411: strih-lx's rig NIC is PCIe (the 10 GbE Thunderbolt Aquantia), so only 11b skips.
    assert _steps(boxes, "strih-lx") == {"3-nvenc": "RUN", "6-hdmi": "RUN", "11b": "SKIP", "14": "RUN",
                                        "16": "RUN", "16c": "RUN", "16e": "RUN", "4b": "RUN"}


# --- obs_box_role_apply: session apps + the venue's NDI sender list ------------------------------------

def _applied(boxes, box):
    body = ('strih_box_load %s || exit 9\nobs_box_role_apply || exit 8\n'
            'echo "UNITS=${STRIH_SESSION_APP_UNITS[*]}"\necho "ROWS=$(strih_session_apps_row_count)"\n'
            'echo "PINNED=$(ndi_discovery_sender_ips pinned)"\necho "RESOLVE=$(ndi_discovery_sender_ips resolve 2>/dev/null)"\n'
            'echo "AUTOSTART=$(strih_session_apps_autostart_lines | wc -l)"' % box)
    r = _bash(body, env={"STRIH_BOXES_DIR": str(boxes)}, sources=(FACTS, PROVISION, NDI))
    assert r.returncode == 0, r.stderr
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines())


def test_a_stream_box_runs_the_keeper_alone_and_lists_the_pp_boxes(boxes):
    a = _applied(boxes, "stream-pp")
    assert a["UNITS"] == "strih-browser-keeper.service"
    assert a["ROWS"] == "2", "one unit row + the keeper's last-pass row"
    assert a["AUTOSTART"] == "1"
    assert a["PINNED"] == a["RESOLVE"] == "10.77.8.204,10.77.8.202", "fact-file name order (C collation)"


def test_strih_pp_keeps_every_session_app_and_lists_the_pp_boxes(boxes):
    a = _applied(boxes, "strih-pp")
    assert a["UNITS"].split() == ["strih-browser-keeper.service", "bkshading-panel-app.service",
                                  "companion-satellite.service", "strih-satellite-watch.timer"]
    assert a["ROWS"] == "7"
    assert a["PINNED"] == "10.77.8.204,10.77.8.202"


def test_strih_lx_keeps_the_snv_fleet_list(boxes):
    a = _applied(boxes, "strih-lx")
    assert a["ROWS"] == "7"
    assert a["PINNED"] == "10.77.9.202,10.77.9.204"


# --- the stream seed manifest + strih_scenes -----------------------------------------------------------

def _manifest(boxes, box):
    r = _bash("strih_box_load %s || exit 9\nobs_box_seed_manifest_json" % box, env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_stream_manifest_is_one_genlocked_program_input(boxes):
    text = _manifest(boxes, "stream-pp")
    d = json.loads(text)
    assert d["mode"] == "update-only"
    assert d["inputs"] == [{"sender": "STRIH-PP (2ME PGM)", "input": "NDI 2ME PGM", "scene": "PRO", "class": "program"}]
    assert d["outputs"] == ["STREAM-PP (stream)"]
    s = _scenes()
    inputs, _outs, latency = s.parse_seed_manifest(text)
    plan = s.seed_inputs(inputs, latency)
    assert plan == [{"scene": "PRO", "input": "NDI 2ME PGM", "ndi_source_name": "STRIH-PP (2ME PGM)",
                     "settings": {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2}}]
    assert "genlock_latency_ms_src" not in plan[0]["settings"], "the stream A/V pin is the operator's knob"
    assert s.parse_program_audio_input(text) == "Zvuk z mixu"
    assert s.input_classes_summary(inputs) == "0 camera, 0 feedback, 1 program (STRIH-PP (2ME PGM)=program)"


def test_a_strih_manifest_is_unchanged_by_the_class_key(boxes):
    s = _scenes()
    text = _manifest(boxes, "strih-lx")
    assert '"class"' not in text and "program_audio_input" not in text
    inputs, _o, latency = s.parse_seed_manifest(text)
    assert s.parse_program_audio_input(text) == "ASIO zvuk"
    classes = [p["settings"].get("genlock_fifo") for p in s.seed_inputs(inputs, latency)]
    assert classes.count(False) == 2 and classes.count(True) == 9


def test_the_program_audio_input_name_reaches_the_seeder():
    s = _scenes()

    class FakeObs:
        def __init__(self):
            self.calls = []

        def req(self, t, d=None, ignore_err=False):
            self.calls.append((t, d))
            if t == "GetInputList":
                return {"inputs": [{"inputName": "Zvuk z mixu", "inputKind": "asio_input_capture"}]}
            if t == "GetSceneList":
                return {"currentProgramSceneName": "PRO", "scenes": [{"sceneName": "PRO"}]}
            if t == "GetSceneItemList":
                return {"sceneItems": []}
            return {}

    obs = FakeObs()
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="Zvuk z mixu")
    created = [d for t, d in obs.calls if t == "CreateInput"]
    assert created and created[0]["inputName"] == "Zvuk z mixu" and created[0]["inputKind"] == "pulse_input_capture"
    assert ("RemoveInput", {"inputName": "Zvuk z mixu"}) in obs.calls
    assert "Zvuk z mixu" in res and "ASIO zvuk" not in res


def test_the_program_class_is_healed_without_touching_the_pin():
    s = _scenes()
    desired = s.program_settings()
    effective = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "genlock_latency_ms_src": 963}
    assert not s.settings_update_needed(effective, desired)


# --- the [NDIPlugin] output identity --------------------------------------------------------------------

OLD_STRIH_LINE = ('for kv in ("MainOutputName=2ME PGM", "PreviewOutputName=2ME PVW", "MainOutputEnabled=true", '
                  '"PreviewOutputEnabled=true"):')


def test_the_strih_ini_printer_is_byte_identical_and_the_stream_one_has_one_output(tmp_path):
    r = _bash("strih_lx_ndi_output_ini_cmds /x/user.ini; echo ---; strih_lx_ndi_output_ini_cmds /x/user.ini strih")
    a, b = r.stdout.split("---\n")
    assert OLD_STRIH_LINE in a and a.strip() == b.strip()
    ini = tmp_path / "user.ini"
    ini.write_text("[NDIPlugin]\nPreviewOutputEnabled=true\n[Other]\nkeep=1\n")
    r = _bash('eval "$(strih_lx_ndi_output_ini_cmds "%s" stream)"' % ini)
    assert r.returncode == 0, r.stderr
    cp = configparser.RawConfigParser()
    cp.optionxform = str
    cp.read(ini)
    assert dict(cp["NDIPlugin"]) == {"PreviewOutputEnabled": "false", "MainOutputName": "stream",
                                     "MainOutputEnabled": "true"}
    assert cp["Other"]["keep"] == "1"


# --- the encoder + the profile ---------------------------------------------------------------------------

def test_the_profile_module_writes_vaapi_cbr_and_keeps_a_stream_key(tmp_path):
    m = _profile_mod()
    assert m.encoder_for(False) == "ffmpeg_vaapi_tex" and m.encoder_for(True) == "obs_nvenc_h264_tex"
    d = tmp_path / "prof"
    d.mkdir()
    (d / "basic.ini").write_text("[General]\nName=old\n[Hotkeys]\nX=1\n")
    (d / "service.json").write_text(json.dumps({"type": "rtmp_custom",
                                                "settings": {"server": "rtmp://old/x", "key": "SECRET"}}))
    m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://127.0.0.1:1234/live", "stream-pp")
    cp = configparser.RawConfigParser()
    cp.optionxform = str
    cp.read(d / "basic.ini")
    assert cp["AdvOut"]["Encoder"] == "ffmpeg_vaapi_tex" and cp["Output"]["Mode"] == "Advanced"
    assert cp["Video"]["ColorFormat"] == "NV12" and cp["Video"]["FPSCommon"] == "30"
    assert cp["Hotkeys"]["X"] == "1" and cp["General"]["Name"] == "stream-pp"
    enc = json.loads((d / "streamEncoder.json").read_text())
    assert enc == {"vaapi_device": "/dev/dri/renderD128", "rate_control": "CBR", "bitrate": 19000,
                   "keyint_sec": 2, "bf": 0, "profile": 100}
    svc = json.loads((d / "service.json").read_text())
    assert svc["type"] == "rtmp_custom" and svc["settings"]["server"] == "rtmp://127.0.0.1:1234/live"
    assert svc["settings"]["key"] == "SECRET"
    assert oct((d / "service.json").stat().st_mode & 0o777) == "0o600"


def test_a_new_service_json_never_carries_a_key(tmp_path):
    m = _profile_mod()
    svc = m.service_json(None, "rtmp://127.0.0.1:1234/live")
    assert svc == {"type": "rtmp_custom", "settings": {"server": "rtmp://127.0.0.1:1234/live", "use_auth": False}}
    other = m.service_json({"type": "rtmp_common", "settings": {"service": "YouTube", "server": "auto", "key": "K"}}, "rtmp://s")
    assert other == {"type": "rtmp_custom", "settings": {"key": "K", "server": "rtmp://s", "use_auth": False}}


def _box_bin(tmp_path, nvidia=False, obs_running=False):
    b = tmp_path / "boxbin"
    b.mkdir()
    log = tmp_path / "box.log"
    gpu = "01:00.0 VGA compatible controller: NVIDIA Corporation GA106" if nvidia else \
        "04:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Barcelo"
    for name, body in {
        "lspci": "echo '%s'" % gpu,
        "apt-get": 'echo "apt-get $*" >> %s' % log,
        "usermod": 'echo "usermod $*" >> %s' % log,
        "vainfo": "printf '      VAProfileH264High               :\\tVAEntrypointEncSlice\\n'",
        "pgrep": "exit %d" % (0 if obs_running else 1),
        "chown": "exit 0",
    }.items():
        (b / name).write_text("#!/bin/bash\n%s\n" % body)
        (b / name).chmod(0o755)
    return b, log


def test_the_stream_encoder_step_installs_vaapi_and_writes_the_profile(tmp_path, boxes):
    b, log = _box_bin(tmp_path)
    cfg = tmp_path / "obs-studio"
    r = _bash('strih_box_load stream-pp\nobs_box_stream_encoder_provision "%s" newlevel stream-pp' % cfg,
              env={"STRIH_BOXES_DIR": str(boxes)}, path_prefix=b, strict=True)
    assert r.returncode == 0, r.stderr
    assert "apt-get install -y mesa-va-drivers vainfo" in log.read_text()
    assert "usermod -aG render newlevel" in log.read_text()
    ini = (cfg / "basic" / "profiles" / "stream-pp" / "basic.ini").read_text()
    assert "Encoder=ffmpeg_vaapi_tex" in ini


def test_the_stream_encoder_step_never_writes_under_a_running_obs(tmp_path, boxes):
    b, _log = _box_bin(tmp_path, nvidia=True, obs_running=True)
    cfg = tmp_path / "obs-studio"
    r = _bash('strih_box_load stream-pp\nobs_box_stream_encoder_provision "%s" newlevel stream-pp' % cfg,
              env={"STRIH_BOXES_DIR": str(boxes)}, path_prefix=b, strict=True)
    assert r.returncode == 0, r.stderr
    assert "NVENC" in r.stdout and "OBS is running" in r.stderr
    assert not cfg.exists()


def test_the_stream_encoder_grade_reads_the_written_profile(tmp_path, boxes):
    b, _log = _box_bin(tmp_path)
    m = _profile_mod()
    d = tmp_path / "p"
    m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://x", "stream-pp")
    r = _bash('ok(){ echo "OK $*"; }\nbad(){ echo "BAD $*"; }\nobs_box_stream_encoder_grade "%s"' % d,
              path_prefix=b, sources=(FACTS, PROVISION, ROLE), strict=True)
    assert r.returncode == 0, r.stderr
    assert [ln.split()[0] for ln in r.stdout.splitlines()] == ["OK", "OK"]


# --- the stream-pp hub matrix ------------------------------------------------------------------------------

def test_the_stream_pp_matrix_satisfies_the_hub_load_rules():
    m = tomllib.loads((REPO / "intercom" / "intercom.stream-pp.toml").read_text())
    assert "janus" not in m and "video" not in m and "point" not in m
    parts = m["participant"]
    assert not [p for p in parts if p["role"] == "cambox" or p.get("out_stream")]
    o = next(p for p in parts if p["role"] == "program_out")
    assert o["adapter"] == "pipewire" and o["pipewire_target"] == "strih-program"
    assert o["source_streams"] == ["pp-stream"]
    assert {p["in_stream"] for p in parts if p["adapter"] == "vban"} == {"pp-stream"}


# --- the setup / verify wiring ---------------------------------------------------------------------------

def test_setup_strih_wires_the_stream_role_within_budget():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    guard = s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    assert s.index("obs_box_role_apply ||") < guard
    for gate, step in (("3-nvenc", "step 3 "), ("14", "step 14 "), ("16", "step 16 "), ("16c", 'step "16c"')):
        g = s.index('if WHY="$(obs_box_step_applies %s)"; then' % gate)
        assert s.index(step) < g, gate
    assert 'obs_box_stream_encoder_provision "$OBS_CFG" "$DESKTOP_USER" "$(strih_lx_obs_profile)"' in s
    assert "obs_box_seed_manifest_json > /opt/camera-box/strih-lx-seed.json" in s
    assert 'strih_lx_ndi_output_ini_cmds "$USER_INI" "$(strih_box_role)"' in s


def test_verify_strih_gates_the_strih_only_items():
    v = VERIFY.read_text()
    assert v.index("obs_box_role_apply ||") < v.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]') \
        if 'if [ "${BASH_SOURCE[0]}" != "${0}" ]' in v else True
    for gate in ("3-nvenc", "16", "16c"):
        assert 'if WHY="$(obs_box_step_applies %s)"; then' % gate in v
    assert "obs_box_stream_encoder_grade" in v
    assert "mapfile -t NDI_KVS < <(obs_box_ndi_output_kvs)" in v
    assert "skip-no-output" in v


# --- review round 1 (slice B) ------------------------------------------------------------------------------

def test_a_replace_keeps_the_audio_input_in_every_scene_and_its_sync_offset():
    s = _scenes()

    class FakeObs:
        def __init__(self):
            self.calls = []

        def req(self, t, d=None, ignore_err=False):
            self.calls.append((t, d))
            if t == "GetInputList":
                return {"inputs": [{"inputName": "Zvuk z mixu", "inputKind": "wasapi_input_capture"}]}
            if t == "GetSceneList":
                return {"currentProgramSceneName": "PRO",
                        "scenes": [{"sceneName": "POST"}, {"sceneName": "PRO"}, {"sceneName": "PRE"}]}
            if t == "GetSceneItemList":
                held = d["sceneName"] in ("POST", "PRE")
                return {"sceneItems": [{"sourceName": "Zvuk z mixu"}] if held else []}
            if t == "GetInputAudioSyncOffset":
                return {"inputAudioSyncOffset": 250}
            return {}

    obs = FakeObs()
    res = s.seed_program_audio_input(obs, "Zvuk z mixu")
    kinds = [t for t, _d in obs.calls]
    assert kinds.index("GetSceneItemList") < kinds.index("RemoveInput") < kinds.index("CreateInput")
    created = [d for t, d in obs.calls if t == "CreateInput"][0]
    assert created["sceneName"] == "POST" and created["inputKind"] == "pulse_input_capture"
    assert ("CreateSceneItem", {"sceneName": "PRE", "sourceName": "Zvuk z mixu"}) in obs.calls
    assert ("SetInputAudioSyncOffset", {"inputName": "Zvuk z mixu", "inputAudioSyncOffset": 250}) in obs.calls
    assert "kept in 2 scene(s)" in res


def test_a_bom_profile_is_read_and_a_bad_service_json_leaves_every_file_untouched(tmp_path):
    m = _profile_mod()
    d = tmp_path / "prof"
    d.mkdir()
    (d / "basic.ini").write_bytes(b"\xef\xbb\xbf[General]\nName=old\n[Hotkeys]\nX=1\n")
    (d / "service.json").write_bytes(b"\xef\xbb\xbf" + json.dumps(
        {"type": "rtmp_custom", "settings": {"server": "rtmp://old", "key": "K"}}).encode())
    m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://new", "stream-pp")
    assert not (d / "basic.ini").read_bytes().startswith(b"\xef\xbb\xbf")
    assert json.loads((d / "service.json").read_text())["settings"]["key"] == "K"
    bad = tmp_path / "bad"
    bad.mkdir()
    (bad / "basic.ini").write_text("[General]\nName=keep\n")
    (bad / "service.json").write_text("{not json")
    with pytest.raises(ValueError):
        m.apply(str(bad), "ffmpeg_vaapi_tex", "rtmp://new", "stream-pp")
    assert (bad / "basic.ini").read_text() == "[General]\nName=keep\n"
    assert not (bad / "streamEncoder.json").exists()


def test_service_json_is_never_written_world_readable(tmp_path):
    m = _profile_mod()
    d = tmp_path / "p"
    old = os.umask(0)
    try:
        m.apply(str(d), "ffmpeg_vaapi_tex", "rtmp://x", "stream-pp")
    finally:
        os.umask(old)
    assert oct((d / "service.json").stat().st_mode & 0o777) == "0o600"
    src = (SCRIPTS / "obs_box_stream_profile.py").read_text()
    assert "os.O_EXCL" in src and "os.open(tmp" in src


def test_an_inherited_sender_list_never_reaches_an_snv_box(boxes):
    r = _bash('strih_box_load strih-lx || exit 9\nobs_box_role_apply || exit 8\n'
              'echo "SET=${NDI_DISCOVERY_SENDER_HOSTS+yes}"\necho "PINNED=$(ndi_discovery_sender_ips pinned)"',
              env={"STRIH_BOXES_DIR": str(boxes), "NDI_DISCOVERY_SENDER_HOSTS": "10.77.8.204"},
              sources=(FACTS, PROVISION, NDI))
    assert r.returncode == 0, r.stderr
    assert "SET=\n" in r.stdout and "PINNED=10.77.9.202,10.77.9.204" in r.stdout


def _autostart(boxes, box):
    r = subprocess.run(["bash", str(REPO / "tests/fixtures/strih_box_1361/render.sh"), str(REPO), box],
                       capture_output=True, text=True, env=_env({"STRIH_BOXES_DIR": str(boxes)}),
                       cwd=str(REPO), timeout=120)
    assert r.returncode == 0, r.stderr
    return r.stdout.split("=== openbox-autostart ===\n", 1)[1].split("\n=== ", 1)[0]


@pytest.mark.parametrize("verdict,dgpu,want", [
    ("nvenc-ok", "1", "note"),
    ("nvenc-unsupported", "1", "bad"),
    ("nvenc-unsupported", "0", "note"),
    ("nvenc-unknown", "1", "note"),
    ("nvenc-unknown", "0", "note"),
])
def test_nvenc_unsupported_fails_only_on_a_box_with_an_nvidia_gpu(verdict, dgpu, want):
    """11.10.2026 stream-pp (AMD Barcelo iGPU, no NVIDIA): OBS logs 'NVENC not supported' by design,
    and verify-strih failed the box for it. The missing-helper FAIL only means something where NVENC
    can exist, a box with a discrete NVIDIA GPU."""
    r = _bash('strih_nvenc_grade "%s" "%s"' % (verdict, dgpu))
    assert r.stdout.split("|", 1)[0] == want, (r.stdout, r.stderr)


def test_the_stream_box_autostart_starts_the_restreamer(boxes):
    """11.10.2026 (restreamer agreement on issue 1412): the stream box runs the restreamer headless as
    the --user unit restreamer.service, which its .deb ships; the kiosk has no tray and no XDG
    autostart, so the openbox autostart starts it. A strih box never does."""
    line = "systemctl --user start restreamer.service || true"
    assert line in _autostart(boxes, "stream-pp").splitlines()
    assert "restreamer" not in _autostart(boxes, "strih-pp")
    assert "restreamer" not in _autostart(boxes, "strih-lx")


def test_the_stream_box_autostart_keeps_hdmi_to_x(boxes):
    assert 'xrandr --output "$PROJ" --off' not in _autostart(boxes, "stream-pp")
    assert 'xrandr --output "$PROJ" --off' in _autostart(boxes, "strih-pp")


def _item9_check(state):
    v = VERIFY.read_text()
    line = next(ln for ln in v.splitlines() if 'p.get("role")=="program_ref"' in ln)
    prog = line.split("python3 -c '", 1)[1].split("'", 1)[0]
    return subprocess.run(["python3", "-c", prog], input=json.dumps(state), text=True,
                          capture_output=True).returncode


def test_verify_item_9_reads_the_program_ref_when_there_is_no_fohabl():
    pp = {"participants": [{"name": "resolume-pp", "role": "program_ref", "rx_packets": 12},
                           {"name": "strih-program", "role": "program_out", "rx_packets": 0}]}
    assert _item9_check(pp) == 0
    pp["participants"][0]["rx_packets"] = 0
    assert _item9_check(pp) == 1
    snv = {"participants": [{"name": "fohabl", "role": "program_ref", "rx_packets": 0},
                            {"name": "lv1", "role": "program_ref", "rx_packets": 9}]}
    assert _item9_check(snv) == 1, "strih-lx keeps grading fohabl exactly"
