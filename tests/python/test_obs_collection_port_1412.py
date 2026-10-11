"""issue 1412, slice C: scripts/obs_collection_port.py -- port an OBS scene collection to a Linux OBS box.

The tool rewrites OBS's own collection JSON once, before the import (OBS stopped). Two uses:
  * stream-pp: the Windows stream collection -> the Linux stream box (keep PRO/Development/PRE/POST,
    the program ASIO input -> pulse on strih-program-source, the YouTube player and the Windows-only
    inputs/filters/modules dropped, text_gdiplus -> ft2, images under /srv/obs-media/stream-pp/);
  * strih-pp: the strih-lx collection -> strih-pp (STRIH-LX -> STRIH-PP, the CG sender, the presenter
    and fohabl hosts of the browser URLs as parameters).

Every fixture here is SYNTHETIC and small. A real collection is never committed (it carries stream keys
and API keys); the fake key below must never reach the output or the report.

Tier-0: pytest only, no OBS, no rig.
"""
import copy
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
TOOL = REPO / "scripts" / "obs_collection_port.py"
FAKE_KEY = "FAKE-gemini-key-not-a-secret"


def _mod():
    spec = importlib.util.spec_from_file_location("obs_collection_port_1412", TOOL)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _src(name, sid, settings=None, filters=None, versioned=None, uuid=None):
    return {"id": sid, "versioned_id": versioned or sid, "name": name, "uuid": uuid or ("u-" + name),
            "settings": settings or {}, "filters": filters or [], "volume": 1.0, "sync": 0, "mixers": 255}


def _scene(name, items):
    return _src(name, "scene", {"items": [{"name": n, "source_uuid": "u-" + n, "visible": True, "id": i + 1}
                                          for i, n in enumerate(items)], "id_counter": len(items)})


def _filter(name, fid, settings=None):
    return {"id": fid, "versioned_id": fid, "name": name, "settings": settings or {}}


def stream_collection():
    """A small Windows stream-pp shape (names from the read-only recon on the ticket)."""
    srcs = [
        _scene("PRE", ["pre-bannery", "pre-videa"]),
        _scene("testing", ["test", "NDIAr 2ME PGM", "Zvuk z mixu"]),
        _scene("PRO", ["Zvuk z mixu", "NDIAr 2ME PGM", "NDIAr Source"]),
        _scene("POST", ["ytfast", "BG", "POST graphic"]),
        _scene("pre-videa", ["ytfast", "BG", "PRE graphic", "timer", "Reklamy"]),
        _scene("pre-bannery", ["ytfast", "BG", "PRE graphic", "NDIAr odpocty", "Bannery", "timer"]),
        _scene("ytfast", ["ytfast_video", "ytfast_title"]),
        _scene("test video", ["dron video"]),
        _src("NDIAr 2ME PGM", "ndi_source", {"ndi_source_name": "OBS-PP (2ME PGM)", "latency": 2, "ndi_sync": 2}),
        _src("NDIAr Source", "ndi_source", {"ndi_source_name": "RESOLUME-PP (cg-obs)"}),
        _src("NDIAr odpocty", "ndi_source", {"ndi_source_name": "PROPRESENTER-PP.LOCAL (Odpocty)"},
             filters=[_filter("Shadow", "obs_composite_blur"), _filter("Crop", "crop_filter", {"top": 4})]),
        _src("Zvuk z mixu", "asio_input_capture", {"device_id": "VB-Matrix VASIO-8"}),
        _src("test", "asio_input_capture", {"device_id": "VB-Matrix VASIO-8"}),
        _src("BG", "image_source", {"file": "C:/Users/interkom/Documents/_stream-pp/STREAM_graphic/Poprad_KS_1.png"}),
        _src("PRE graphic", "image_source", {"file": "C:\\Users\\interkom\\Documents\\_stream-pp\\STREAM_graphic\\PRE.png"}),
        _src("POST graphic", "image_source", {"file": "C:/Users/interkom/Pictures/Saved Pictures/Bohosluzba_jar.png"}),
        _src("Bannery", "slideshow", {"files": [{"value": "H:/My Drive/KS POPRAD/Reklamy - Bannery", "hidden": False}]},
             versioned="slideshow_v2"),
        _src("Reklamy", "media_playlist_source_codeyan", {"playlist": [{"value": "H:/My Drive/x.mp4"}]}),
        _src("ytfast_video", "ffmpeg_source", {"local_file": "C:/Users/interkom/Documents/OBS-YouTube-Player/c.mp4"}),
        _src("ytfast_title", "text_gdiplus", {"text": "song"}, versioned="text_gdiplus_v3"),
        _src("dron video", "ffmpeg_source", {"local_file": "C:/Users/interkom/Documents/_stream-pp/Testing videos/dji.mp4"}),
        _src("timer", "browser_source", {"url": "http://presenter.lan/overlays/timer?room=x&password=hidden1",
                                         "width": 1920, "height": 1080}),
    ]
    return {
        "name": "stream-pp",
        "current_scene": "PRO", "current_program_scene": "ytfast",
        "scene_order": [{"name": s["name"]} for s in srcs if s["id"] == "scene"],
        "sources": srcs,
        "groups": [],
        "DesktopAudioDevice1": _src("Desktop Audio", "wasapi_output_capture", {"device_id": "default"}),
        "modules": {
            "scripts-tool": [{"path": "C:/Users/interkom/Documents/OBS-YouTube-Player/ytfast.py",
                              "settings": {"gemini_api_key": FAKE_KEY}}],
            "advanced-scene-switcher": {"macros": []},
            "captions": {"source": ""},
            "output-timer": {"streamTimerHours": 0},
            "auto-scene-switcher": {"active": False},
        },
        "transitions": [], "current_transition": "Fade",
    }


def strih_collection():
    """A small strih-lx shape: already Linux, SNV names and hosts."""
    srcs = [
        _scene("Cam 1", ["NDI cam1"]),
        _scene("2ME PVW", ["NDI 2ME PVW"]),
        _scene("CG", ["cg"]),
        _scene("Interkom", ["interkom web", "ASIO zvuk"]),
        _scene("Grafika", ["lower third", "foh meter"]),
        _src("NDI cam1", "ndi_source", {"ndi_source_name": "CAM1 (usb)", "genlock_fifo": True}),
        _src("NDI 2ME PVW", "ndi_source", {"ndi_source_name": "STRIH-LX (2ME PVW)", "ndi_sync": 1}),
        _src("cg", "ndi_source", {"ndi_source_name": "RESOLUME-SNV (SP-program)"}),
        _src("ASIO zvuk", "pulse_input_capture", {"device_id": "strih-program-source"}),
        _src("interkom web", "browser_source", {"url": "https://vdo.ninja/?room=snv&password=hidden2"},
             filters=[_filter("User-defined shader", "shader_filter"),
                      _filter("NDI out", "ndi_filter", {"ndi_filter_ndiname": "interkom"})]),
        _src("lower third", "browser_source", {"url": "http://presenter.lan:8080/overlays/lt"}),
        _src("foh meter", "browser_source", {"url": "http://fohabl.lan/meter"}),
    ]
    return {
        "name": "strih-lx", "current_scene": "Cam 1", "current_program_scene": "2ME PVW",
        "scene_order": [{"name": s["name"]} for s in srcs if s["id"] == "scene"],
        "sources": srcs, "groups": [],
        "modules": {"scripts-tool": [{"path": "D:/_APPS/vban-output.lua", "settings": {}}],
                    "output-timer": {}},
        "transitions": [], "current_transition": "Fade",
    }


def _stream_opts(m, **over):
    o = m.preset_options("stream-pp")
    o.update({"url_hosts": {"presenter.lan": "presenter-pp.lan"}})
    o.update(over)
    return o


def _names(coll):
    return {s["name"] for s in coll["sources"] + coll.get("groups", [])}


def _by_name(coll, name):
    return next(s for s in coll["sources"] + coll.get("groups", []) if s["name"] == name)


# --- stream-pp ----------------------------------------------------------------------------------------

def test_the_stream_port_keeps_the_four_scenes_and_what_they_use():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    scenes = [s["name"] for s in out["sources"] if s["id"] == "scene"]
    assert scenes == ["PRE", "PRO", "POST", "pre-videa", "pre-bannery"], "nested pre-* kept, ytfast/testing dropped"
    assert [e["name"] for e in out["scene_order"]] == scenes
    assert "testing" not in _names(out) and "test video" not in _names(out) and "dron video" not in _names(out)
    assert any("Development" in ln and "absent" in ln for ln in rep), "a missing keep scene is reported"
    assert out["current_program_scene"] == "PRO" and out["current_scene"] == "PRO"


def test_the_youtube_player_and_the_windows_only_kinds_are_dropped_with_a_reason():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    for gone in ("ytfast", "ytfast_video", "ytfast_title", "Reklamy", "test", "Desktop Audio"):
        assert gone not in _names(out), gone
        assert any(('"%s"' % gone) in ln and ln.startswith("DROP") for ln in rep), gone
    assert "DesktopAudioDevice1" not in out
    for s in out["sources"]:
        if s["id"] == "scene":
            assert all(i["name"] in _names(out) for i in s["settings"]["items"]), s["name"]
    post = [i["name"] for i in _by_name(out, "POST")["settings"]["items"]]
    assert post == ["BG", "POST graphic"]


def test_the_program_asio_input_becomes_pulse_on_the_program_source():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    z = _by_name(out, "Zvuk z mixu")
    assert z["id"] == z["versioned_id"] == "pulse_input_capture"
    assert z["settings"] == {"device_id": "strih-program-source"}
    assert z["uuid"] == "u-Zvuk z mixu" and z["mixers"] == 255
    assert any(ln.startswith("CONVERT") and "Zvuk z mixu" in ln for ln in rep)


def test_the_program_audio_device_matches_the_seeder():
    m = _mod()
    pytest.importorskip("websocket")
    spec = importlib.util.spec_from_file_location("strih_scenes_1412c", REPO / "scripts" / "strih_scenes.py")
    s = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(s)
    assert m.PROGRAM_AUDIO_KIND == s.AUDIO_INPUT_KIND
    assert m.PROGRAM_AUDIO_SETTINGS == s.program_audio_input_settings()


def test_the_strih_program_sender_and_the_stream_input_name_follow_the_manifest():
    m = _mod()
    out, _rep = m.port(stream_collection(), _stream_opts(m))
    assert "NDIAr 2ME PGM" not in _names(out)
    pgm = _by_name(out, "NDI 2ME PGM")
    assert pgm["settings"]["ndi_source_name"] == "STRIH-PP (2ME PGM)"
    pro = [i["name"] for i in _by_name(out, "PRO")["settings"]["items"]]
    assert pro == ["Zvuk z mixu", "NDI 2ME PGM", "NDIAr Source"]


def test_images_point_to_the_media_dir_and_every_remap_is_reported():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    assert _by_name(out, "BG")["settings"]["file"] == "/srv/obs-media/stream-pp/STREAM_graphic/Poprad_KS_1.png"
    assert _by_name(out, "PRE graphic")["settings"]["file"] == "/srv/obs-media/stream-pp/STREAM_graphic/PRE.png"
    assert _by_name(out, "POST graphic")["settings"]["file"] == \
        "/srv/obs-media/stream-pp/Saved Pictures/Bohosluzba_jar.png"
    assert _by_name(out, "Bannery")["settings"]["files"][0]["value"] == \
        "/srv/obs-media/stream-pp/KS POPRAD/Reklamy - Bannery"
    assert sum(1 for ln in rep if ln.startswith("PATH")) == 4
    assert "C:" not in json.dumps(out) and "H:/" not in json.dumps(out)


def test_filters_of_a_plugin_the_linux_bundle_lacks_are_dropped():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    f = _by_name(out, "NDIAr odpocty")["filters"]
    assert [x["name"] for x in f] == ["Crop"]
    assert any(ln.startswith("DROP filter") and "Shadow" in ln for ln in rep)


def test_windows_modules_and_scripts_are_dropped_and_the_key_never_leaks():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    assert set(out["modules"]) == {"output-timer", "auto-scene-switcher", "scripts-tool"}
    assert out["modules"]["scripts-tool"] == []
    assert FAKE_KEY not in json.dumps(out) and FAKE_KEY not in "\n".join(rep)
    assert "hidden1" not in "\n".join(rep), "a URL query (room passwords) is never printed"


def test_the_presenter_host_is_a_parameter():
    m = _mod()
    out, rep = m.port(stream_collection(), _stream_opts(m))
    assert _by_name(out, "timer")["settings"]["url"] == \
        "http://presenter-pp.lan/overlays/timer?room=x&password=hidden1"
    with pytest.raises(m.PortError, match="presenter.lan"):
        m.port(stream_collection(), _stream_opts(m, url_hosts={}))


def test_gdiplus_text_converts_to_ft2_when_it_is_kept():
    m = _mod()
    c = stream_collection()
    c["sources"].append(_src("titel", "text_gdiplus", {"text": "Ahoj", "font": {"face": "Arial", "size": 64},
                                                       "color": 0x00FF00, "opacity": 50, "outline": True,
                                                       "extents": True, "extents_cx": 800, "extents_wrap": True},
                             versioned="text_gdiplus_v3"))
    c["sources"][2]["settings"]["items"].append({"name": "titel", "source_uuid": "u-titel", "id": 9})
    out, rep = m.port(c, _stream_opts(m))
    t = _by_name(out, "titel")
    assert t["id"] == "text_ft2_source" and t["versioned_id"] == "text_ft2_source_v2"
    s = t["settings"]
    assert s["text"] == "Ahoj" and s["font"] == {"face": "Arial", "size": 64} and s["outline"] is True
    assert s["color1"] == s["color2"] == (127 << 24) | 0x00FF00
    assert s["word_wrap"] is True and s["custom_width"] == 800
    assert any(ln.startswith("CONVERT") and "titel" in ln for ln in rep)


def test_the_input_is_never_modified():
    m = _mod()
    c = stream_collection()
    before = copy.deepcopy(c)
    m.port(c, _stream_opts(m))
    assert c == before


def test_a_missing_program_audio_input_refuses():
    m = _mod()
    c = stream_collection()
    c["sources"] = [s for s in c["sources"] if s["name"] != "Zvuk z mixu"]
    with pytest.raises(m.PortError, match="Zvuk z mixu"):
        m.port(c, _stream_opts(m))


# --- strih-lx -> strih-pp -------------------------------------------------------------------------------

def _strih_opts(m, **over):
    o = m.preset_options("strih-pp")
    o.update({"url_hosts": {"presenter.lan": "presenter-pp.lan", "fohabl.lan": "fohabl-pp.lan"}})
    o.update(over)
    return o


def test_the_strih_port_renames_the_self_loop_and_the_cg_sender():
    m = _mod()
    out, rep = m.port(strih_collection(), _strih_opts(m))
    assert _by_name(out, "NDI 2ME PVW")["settings"]["ndi_source_name"] == "STRIH-PP (2ME PVW)"
    assert _by_name(out, "cg")["settings"]["ndi_source_name"] == "RESOLUME-PP (SP-program)"
    assert _by_name(out, "NDI cam1")["settings"]["ndi_source_name"] == "CAM1 (usb)"
    assert sum(1 for ln in rep if ln.startswith("SENDER")) == 2
    assert [e["name"] for e in out["scene_order"]] == ["Cam 1", "2ME PVW", "CG", "Interkom", "Grafika"]


def test_the_strih_port_maps_the_presenter_and_fohabl_hosts_by_parameter():
    m = _mod()
    out, _rep = m.port(strih_collection(), _strih_opts(m))
    assert _by_name(out, "lower third")["settings"]["url"] == "http://presenter-pp.lan:8080/overlays/lt"
    assert _by_name(out, "foh meter")["settings"]["url"] == "http://fohabl-pp.lan/meter"
    assert _by_name(out, "interkom web")["settings"]["url"].startswith("https://vdo.ninja/")
    with pytest.raises(m.PortError, match="fohabl.lan"):
        m.port(strih_collection(), _strih_opts(m, url_hosts={"presenter.lan": "presenter-pp.lan"}))
    kept, _r = m.port(strih_collection(), _strih_opts(m, url_hosts={"presenter.lan": "p.lan",
                                                                     "fohabl.lan": "fohabl.lan"}))
    assert _by_name(kept, "foh meter")["settings"]["url"] == "http://fohabl.lan/meter"


def test_the_strih_port_keeps_every_scene_and_drops_the_shader_and_the_dead_lua():
    m = _mod()
    out, rep = m.port(strih_collection(), _strih_opts(m))
    assert [x["name"] for x in _by_name(out, "interkom web")["filters"]] == ["NDI out"]
    assert out["modules"]["scripts-tool"] == []
    assert out["name"] == "strih-pp"
    assert _by_name(out, "ASIO zvuk")["settings"] == {"device_id": "strih-program-source"}


# --- the CLI -------------------------------------------------------------------------------------------

def _cli(*args):
    return subprocess.run([sys.executable, str(TOOL), *map(str, args)], capture_output=True, text=True, timeout=60)


def test_the_cli_writes_once_and_prints_the_report(tmp_path):
    src = tmp_path / "in.json"
    src.write_bytes(b"\xef\xbb\xbf" + json.dumps(stream_collection()).encode())
    dst = tmp_path / "stream-pp.json"
    r = _cli("--preset", "stream-pp", "--presenter-host", "presenter-pp.lan", src, dst)
    assert r.returncode == 0, r.stderr
    out = json.loads(dst.read_text())
    assert out["name"] == "stream-pp" and "NDI 2ME PGM" in _names(out)
    assert "DROP" in r.stderr and FAKE_KEY not in r.stderr + r.stdout
    again = _cli("--preset", "stream-pp", "--presenter-host", "presenter-pp.lan", src, dst)
    assert again.returncode == 2 and "exists" in again.stderr, "an imported collection is never rewritten"
    assert json.loads(dst.read_text()) == out


def test_the_cli_strih_preset_takes_both_hosts(tmp_path):
    src = tmp_path / "strih-lx.json"
    src.write_text(json.dumps(strih_collection()))
    dst = tmp_path / "strih-pp.json"
    r = _cli("--preset", "strih-pp", "--presenter-host", "presenter-pp.lan", "--fohabl-host", "fohabl-pp.lan",
             src, dst)
    assert r.returncode == 0, r.stderr
    assert "STRIH-PP (2ME PVW)" in dst.read_text()
    bad = _cli("--preset", "strih-pp", "--presenter-host", "p.lan", src, tmp_path / "x.json")
    assert bad.returncode == 2 and "fohabl.lan" in bad.stderr
    assert not (tmp_path / "x.json").exists()


def test_no_real_collection_is_committed():
    tracked = subprocess.run(["git", "-C", str(REPO), "ls-files"], capture_output=True, text=True).stdout.split()
    for p in tracked:
        if p.endswith(".json") and ("strih-pp" in p or "stream-pp" in p or "streampp" in p):
            pytest.fail("a real collection must never be committed: %s" % p)
