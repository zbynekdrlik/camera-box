"""issue 1423 (owner 13.10.2026: the PP vestibule TV hangs on a ZowieBox, which decodes NDI|HX only,
never our full NDI; ROZHODNUTÉ: "tak to nahod na srt"): a strih box whose fact STRIH_PROGRAM_SRT_URL
names an SRT listener streams its OBS program there -- setup writes the profile's custom SRT service +
an NVENC H.264 stream encoder, the launcher starts OBS with --startstreaming, verify grades both."""
import configparser
import importlib.util
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
LAUNCHER = SCRIPTS / "strih-obs-start.sh"
URL = "srt://10.77.8.141:9009?streamid=pusher/live/av"

_spec = importlib.util.spec_from_file_location("obs_box_stream_profile_1423", SCRIPTS / "obs_box_stream_profile.py")
prof = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(prof)


def _fact(box, key):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\nstrih_box_fact "$3"', "h",
                        str(FACTS), box, key], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_strih_pp_streams_its_program_to_the_vestibule_zowiebox():
    assert _fact("strih-pp", "STRIH_PROGRAM_SRT_URL") == URL
    assert _fact("strih-snv", "STRIH_PROGRAM_SRT_URL") == "none"
    assert _fact("stream-pp", "STRIH_PROGRAM_SRT_URL") == "none"


def test_the_fact_loader_refuses_a_url_that_is_not_srt(tmp_path):
    src = (SCRIPTS / "strih-boxes" / "strih-pp.env").read_text()
    for bad in ("http://10.77.8.141:9009", "srt://10.77.8.141", "srt://10.77.8.141:9009?x=$(id)"):
        boxes = tmp_path / bad.replace("/", "_").replace(":", "_").replace("?", "_").replace("$", "_")[:40]
        boxes.mkdir()
        (boxes / "strih-pp.env").write_text(src.replace(URL, bad))
        r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_box_load strih-pp', "h", str(FACTS)],
                           capture_output=True, text=True, timeout=60,
                           env=dict(os.environ, STRIH_BOXES_DIR=str(boxes)))
        assert r.returncode != 0 and "STRIH_PROGRAM_SRT_URL" in r.stderr, (bad, r.stderr)


def test_apply_strih_writes_the_srt_service_and_the_nvenc_stream_encoder(tmp_path):
    d = tmp_path / "strih-pp"
    prof.apply_strih(str(d), "strih-pp", srt_url=URL)
    svc = json.loads((d / "service.json").read_text())
    assert svc["type"] == "rtmp_custom" and svc["settings"]["server"] == URL
    cp = configparser.RawConfigParser(strict=False)
    cp.optionxform = str
    cp.read(d / "basic.ini")
    assert cp.get("AdvOut", "Encoder") == "obs_nvenc_h264_tex"
    enc = json.loads((d / "streamEncoder.json").read_text())
    assert enc["rate_control"] == "CBR" and enc["bitrate"] == 6000 and enc["keyint_sec"] == 1 and enc["bf"] == 0
    assert prof.grade_strih(str(d), srt_url=URL) == []


def test_apply_strih_without_a_url_writes_no_stream_service(tmp_path):
    d = tmp_path / "strih-snv"
    prof.apply_strih(str(d), "strih-snv")
    assert not (d / "service.json").exists() and not (d / "streamEncoder.json").exists()
    assert prof.grade_strih(str(d)) == []


def test_grade_strih_names_a_missing_srt_service(tmp_path):
    d = tmp_path / "strih-pp"
    prof.apply_strih(str(d), "strih-pp")
    drift = " ".join(prof.grade_strih(str(d), srt_url=URL))
    assert "service.json" in drift and "Encoder" in drift


def test_the_cli_passes_the_url(tmp_path):
    d = tmp_path / "strih-pp"
    r = subprocess.run(["python3", str(SCRIPTS / "obs_box_stream_profile.py"), "--strih", "--profile-dir", str(d),
                        "--profile-name", "strih-pp", "--srt-url", URL], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    r = subprocess.run(["python3", str(SCRIPTS / "obs_box_stream_profile.py"), "--grade-strih", str(d),
                        "--srt-url", URL], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0 and r.stdout.strip() == "", r.stdout


def _launcher_check(profile_dir):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nif strih_srt_stream_on_start "$2"; then echo YES; '
                        'else echo NO; fi', "h", str(LAUNCHER), str(profile_dir)],
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_launcher_starts_streaming_only_for_an_srt_service(tmp_path):
    d = tmp_path / "p"
    d.mkdir()
    assert _launcher_check(d) == "NO"
    (d / "service.json").write_text(json.dumps({"type": "rtmp_custom", "settings": {"server": URL}}))
    assert _launcher_check(d) == "YES"
    (d / "service.json").write_text(json.dumps({"type": "rtmp_custom", "settings": {"server": "rtmp://x/live"}}))
    assert _launcher_check(d) == "NO"
    (d / "service.json").write_text("{not json")
    assert _launcher_check(d) == "NO"


def test_the_launcher_passes_startstreaming():
    s = LAUNCHER.read_text()
    assert 'if strih_srt_stream_on_start "${OBS_CFG}/basic/profiles/${STRIH_OBS_PROFILE}"; then' in s
    assert "OBS_ARGS+=(--startstreaming)" in s


def test_setup_and_verify_pass_the_fact():
    setup = (SCRIPTS / "setup-strih.sh").read_text()
    assert 'obs_box_strih_profile_provision "$OBS_CFG" "$DESKTOP_USER" "$(strih_lx_obs_profile)" "$(strih_lx_program_srt_url)"' in setup
    verify = (SCRIPTS / "verify-strih.sh").read_text()
    assert 'obs_box_strih_profile_grade "${USER_HOME}/.config/obs-studio/basic/profiles/$(strih_lx_obs_profile)" "$(strih_lx_program_srt_url)"' in verify
    assert len(setup.splitlines()) <= 1000 and len(verify.splitlines()) <= 1000
