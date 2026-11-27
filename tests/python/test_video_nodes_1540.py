"""Issue 1540 slice 3: the video node list dcontroller reads (`:8899/video-nodes.json`).

scripts/video_nodes.py generates scripts/bundle_state_video_nodes.py from the ONE set of repo
declarations (the strih fact files, camera-set.sh camera_resolve, OBS_FLEET); the server serves the
box's own venue from it. These tests run the real generator over the real repo facts and over a
temp fact dir, pin the committed module equal to a fresh generation, and read the route through the
real server on a temp port.
"""
import datetime as dt
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
SCRIPTS = REPO / "scripts"
sys.path.insert(0, str(SCRIPTS))
import bundle_state_video as bsv
import bundle_state_video_nodes as generated
import video_nodes

SNV_FACT = (SCRIPTS / "strih-boxes" / "strih-snv.env").read_text()


def _names(nodes):
    return [n["name"] for n in nodes]


def test_the_real_repo_facts_give_both_venues():
    venues = video_nodes.build()
    assert sorted(venues) == ["pp", "snv"]
    assert _names(venues["snv"]) == ["strih-snv", "stream-snv", "cam2", "cam3", "cam5"]
    assert _names(venues["pp"]) == ["strih-pp", "stream-pp", "cam1", "cam4", "cam6", "cam7"]
    snv = {n["name"]: n for n in venues["snv"]}
    assert snv["strih-snv"] == {"name": "strih-snv", "kind": "obs", "role": "strih",
                                "url": "http://10.77.9.202:8899/video-connections.json"}
    assert snv["cam3"] == {"name": "cam3", "kind": "cambox", "url": "http://10.77.9.63:8896/video.json"}
    # a PP camera keeps its venue-LAN address, never the dev1-side 10.76.9.x NAT
    pp = {n["name"]: n for n in venues["pp"]}
    assert pp["cam7"]["url"] == "http://10.77.9.67:8896/video.json"
    assert pp["strih-pp"]["url"] == "http://10.77.8.202:8899/video-connections.json"


def test_the_committed_module_equals_a_fresh_generation():
    assert (SCRIPTS / "bundle_state_video_nodes.py").read_text() == video_nodes.render(video_nodes.build()), (
        "scripts/bundle_state_video_nodes.py is stale: run python3 scripts/video_nodes.py --write")
    assert generated.VENUES == video_nodes.build()


def test_the_check_cli_passes_on_the_committed_module():
    out = subprocess.run([sys.executable, str(SCRIPTS / "video_nodes.py"), "--check"],
                         capture_output=True, text=True, check=False)
    assert out.returncode == 0, out.stderr


def _fact_dir(tmp_path, files):
    d = tmp_path / "boxes"
    d.mkdir()
    for name, text in files.items():
        (d / name).write_text(text)
    return d


def _with(text, key, value):
    return "\n".join(f"{key}={value}" if line.startswith(key + "=") else line for line in text.splitlines()) + "\n"


def test_a_box_absent_from_obs_fleet_refuses(tmp_path, monkeypatch):
    d = _fact_dir(tmp_path, {"x.env": _with(SNV_FACT, "STRIH_HOSTNAME", "strih-xyz")})
    monkeypatch.setenv("VIDEO_NODES_STRIH_BOXES_DIR", str(d))
    with pytest.raises(RuntimeError, match="no OBS_FLEET row"):
        video_nodes.build()


def test_a_retired_box_refuses(tmp_path, monkeypatch):
    d = _fact_dir(tmp_path, {"x.env": _with(SNV_FACT, "STRIH_HOSTNAME", "imag")})
    monkeypatch.setenv("VIDEO_NODES_STRIH_BOXES_DIR", str(d))
    with pytest.raises(RuntimeError, match="retired"):
        video_nodes.build()


def test_a_camera_listed_twice_refuses(tmp_path, monkeypatch):
    stream = (SCRIPTS / "strih-boxes" / "stream-snv.env").read_text()
    d = _fact_dir(tmp_path, {"a.env": SNV_FACT, "b.env": _with(stream, "STRIH_CAMERAS", "3")})
    monkeypatch.setenv("VIDEO_NODES_STRIH_BOXES_DIR", str(d))
    with pytest.raises(RuntimeError, match="cam3 is listed by"):
        video_nodes.build()


def test_a_camera_unknown_to_camera_resolve_refuses(tmp_path, monkeypatch):
    d = _fact_dir(tmp_path, {"a.env": _with(SNV_FACT, "STRIH_CAMERAS", "2 42")})
    monkeypatch.setenv("VIDEO_NODES_STRIH_BOXES_DIR", str(d))
    with pytest.raises(RuntimeError, match="cam42, unknown to camera_resolve"):
        video_nodes.build()


VENUES = {"snv": [{"name": "strih-snv", "kind": "obs", "role": "strih", "url": "http://a:8899/v"},
                  {"name": "cam2", "kind": "cambox", "url": "http://b:8896/video.json"}],
          "pp": [{"name": "strih-pp", "kind": "obs", "role": "strih", "url": "http://c:8899/v"}]}
NOW = dt.datetime(2026, 11, 27, 13, 0, 0, tzinfo=dt.timezone.utc)


def test_the_payload_is_the_boxs_own_venue():
    body = bsv.nodes_payload("strih-snv", VENUES, NOW)
    assert body == {"schema": 1, "venue": "snv", "node": "strih-snv", "read_at": "2026-11-27T13:00:00+00:00",
                    "nodes": VENUES["snv"]}
    assert bsv.nodes_payload("cam2", VENUES, NOW)["venue"] == "snv"
    assert bsv.nodes_payload("strih-pp", VENUES, NOW)["nodes"] == VENUES["pp"]


def test_an_unknown_box_has_no_payload():
    assert bsv.nodes_payload("resolume-snv", VENUES, NOW) is None


def test_the_payload_is_a_copy_not_the_module_state():
    body = bsv.nodes_payload("strih-snv", VENUES, NOW)
    body["nodes"][0]["url"] = "changed"
    assert VENUES["snv"][0]["url"] == "http://a:8899/v"


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _get(port, path):
    for _ in range(50):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=2) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, None
        except OSError:
            time.sleep(0.2)
    raise AssertionError("the server never answered")


def _serve(tmp_path, hostname):
    port = _free_port()
    env = dict(os.environ, BUNDLE_STATE_NODE_NAME=hostname)
    proc = subprocess.Popen([sys.executable, str(SCRIPTS / "bundle-state-server.py"), "--port", str(port),
                             "--obs-log-dir", str(tmp_path)],
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return port, proc


def test_the_server_serves_its_venue(tmp_path):
    port, proc = _serve(tmp_path, "stream-pp")
    try:
        status, body = _get(port, "/video-nodes.json")
        assert status == 200
        assert body["schema"] == 1 and body["venue"] == "pp" and body["node"] == "stream-pp"
        assert _names(body["nodes"]) == _names(video_nodes.build()["pp"])
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def test_the_server_answers_404_on_a_box_in_no_venue(tmp_path):
    port, proc = _serve(tmp_path, "resolume-snv")
    try:
        status, _ = _get(port, "/video-nodes.json")
        assert status == 404
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def test_the_generated_module_is_in_both_deployed_file_lists():
    assert "bundle_state_video_nodes.py" in (SCRIPTS / "lib" / "bundle-state-files.txt").read_text().split()
    assert "bundle_state_video_nodes.py" in (SCRIPTS / "lib" / "bundle-state-files.sh").read_text()
