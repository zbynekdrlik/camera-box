"""issue 1470 -- scripts/obs_dock_state.py: the A/V-sync dock in an OBS user.ini [BasicWindow] DockState.

OBS hides a dock a plugin adds (OBSStudioAPI.cpp obs_frontend_add_dock_by_id: setVisible(false) +
setFloating(true)); the saved DockState, restored after the plugin load, decides. The fixtures are the
DockState lines read off the two Linux stream boxes on 23.10.2026 (read-only):
  * stream-snv: ported from the Windows stream PC, it already holds the dock docked + visible;
  * stream-pp: no entry for the dock, so OBS would load it hidden + floating.
These tests pin the pure parser, the three seed actions (insert / redock / keep), the user.ini line
edit (only the DockState value changes, mode kept, never through a symlink) and the CLI tokens. The
same seeds restored by the REAL Qt: tests/qt/run_dock_state_restore_1470.sh (linux-genlock.yml).
"""

import base64
import importlib.util
import os
import re
import struct
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "scripts" / "obs_dock_state.py"
FIX = Path(__file__).resolve().parent / "fixtures" / "obs_dock_state_1470"
DOCK = "net.nagater.obs-audio-video-sync-dock..main"


def _load():
    spec = importlib.util.spec_from_file_location("obs_dock_state_1470", SCRIPT)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


M = _load()


def _blob(box):
    line = [l for l in (FIX / f"{box}.user.ini").read_text().splitlines() if l.startswith("DockState=")][0]
    return base64.b64decode(line.split("=", 1)[1])


def _names(blob):
    return [(i["name"], i["flags"]) for i in M.parse(blob)["items"]]


def _cli(*args):
    return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True, timeout=30)


# --- the parser over the live captures --------------------------------------------------------------

def test_the_stream_snv_capture_parses_into_its_two_dock_areas():
    st = M.parse(_blob("stream-snv"))
    assert [(a["pos"], a["width"], a["height"]) for a in st["areas"]] == [(1, 352, 431), (3, 1920, 263)]
    names = _names(_blob("stream-snv"))
    assert names[0] == (DOCK, 1)
    assert ("scenesDock", 1) in names and ("transitionsDock", 0) in names and ("advss-status-dock", 2) in names
    assert len(names) == 13


def test_the_stream_pp_capture_has_no_right_area_and_no_dock():
    st = M.parse(_blob("stream-pp"))
    assert [a["pos"] for a in st["areas"]] == [3]
    assert DOCK not in [n for n, _ in _names(_blob("stream-pp"))]
    assert M.dock_state(_blob("stream-pp"), DOCK) == "absent"


def test_dock_state_reads_each_placement():
    assert M.dock_state(_blob("stream-snv"), DOCK) == "visible"
    assert M.dock_state(_blob("stream-snv"), "transitionsDock") == "hidden"            # closed while docked
    assert M.dock_state(_blob("stream-snv"), "advss-status-dock") == "default-hidden"  # hidden + floating


@pytest.mark.parametrize("cut", [3, 9, 40, 200])
def test_a_truncated_blob_is_refused(cut):
    with pytest.raises(M.DockStateError):
        M.parse(_blob("stream-pp")[:cut])


def test_a_blob_without_the_qmainwindow_markers_is_refused():
    b = bytearray(_blob("stream-pp"))
    b[3] = 0x7F  # the version marker
    with pytest.raises(M.DockStateError):
        M.parse(bytes(b))
    b = bytearray(_blob("stream-pp"))
    b[8] = 0x00  # the dock-widget state marker
    with pytest.raises(M.DockStateError):
        M.parse(bytes(b))


def test_a_corner_value_qt_never_writes_is_refused():
    st = M.parse(_blob("stream-pp"))
    b = bytearray(_blob("stream-pp"))
    corner = st["areas_end"] + 8
    b[corner:corner + 4] = struct.pack(">i", 3)
    with pytest.raises(M.DockStateError):
        M.parse(bytes(b))


# --- the seed ---------------------------------------------------------------------------------------

def test_an_absent_dock_is_inserted_as_a_new_right_area_before_the_bottom_one():
    old = _blob("stream-pp")
    new = M.seed(old, DOCK)
    st_old, st_new = M.parse(old), M.parse(new)
    assert [a["pos"] for a in st_new["areas"]] == [1, 3]
    assert M.dock_state(new, DOCK) == "visible"
    # every other dock keeps its flags and order; the bytes after the dock areas are untouched
    assert [x for x in _names(new) if x[0] != DOCK] == _names(old)
    assert new[st_new["areas_end"]:] == old[st_old["areas_end"]:]
    assert new[:st_old["areas_count_offset"]] == old[:st_old["areas_count_offset"]]


def test_an_absent_dock_joins_an_existing_right_area_at_its_end():
    old = _blob("stream-snv")
    new = M.seed(old, "camera-box.test.dock")
    st = M.parse(new)
    assert [a["pos"] for a in st["areas"]] == [1, 3]
    right = [n for n, _ in _names(new)][:5]
    assert right == [DOCK, "advss-status-dock", "DownstreamKeyerDock", "distroav-sync-dock", "camera-box.test.dock"]
    assert M.dock_state(new, "camera-box.test.dock") == "visible"


def test_the_obs_default_entry_is_redocked_in_place():
    old = _blob("stream-snv")
    assert M.seed_action(old, "advss-status-dock") == "redock"
    new = M.seed(old, "advss-status-dock")
    assert len(new) == len(old)
    assert M.dock_state(new, "advss-status-dock") == "visible"
    assert [x for x in _names(new) if x[0] != "advss-status-dock"] == \
        [x for x in _names(old) if x[0] != "advss-status-dock"]
    item = [i for i in M.parse(new)["items"] if i["name"] == "advss-status-dock"][0]
    at = item["flags_offset"]
    assert new[at:at + 17] == bytes([1]) + struct.pack(">iiii", 0, -1, 0, 0)


@pytest.mark.parametrize("name", [DOCK, "transitionsDock"])
def test_an_operator_placement_is_kept(name):
    blob = _blob("stream-snv")
    assert M.seed_action(blob, name) == "keep"
    with pytest.raises(M.DockStateError):
        M.seed(blob, name)


def test_a_name_outside_the_parsed_items_is_refused():
    # a QString of the name appended after the parsed part: present, but nowhere the parser read it
    blob = _blob("stream-pp") + M._qstring_bytes(DOCK)
    with pytest.raises(M.DockStateError):
        M.dock_state(blob, DOCK)


# --- user.ini -----------------------------------------------------------------------------------------

def _copy(tmp_path, box, text=None):
    p = tmp_path / "user.ini"
    p.write_text(text if text is not None else (FIX / f"{box}.user.ini").read_text())
    os.chmod(p, 0o640)
    return p


def test_seeding_stream_pp_changes_only_the_dockstate_value_and_keeps_the_mode(tmp_path):
    p = _copy(tmp_path, "stream-pp")
    before = p.read_text().splitlines()
    assert M.seed_ini(str(p), DOCK) == "seeded"
    after = p.read_text().splitlines()
    # the first-seed marker is appended as its own section (round 2); every other line kept in place
    assert after[len(before):] == ["", "[CameraBox]", "AvsyncDockSeeded=true"]
    after = after[:len(before)]
    diff = [i for i, (a, b) in enumerate(zip(before, after)) if a != b]
    assert len(diff) == 1 and after[diff[0]].startswith("DockState=")
    assert M.check_ini(str(p), DOCK) == "visible"
    assert os.stat(p).st_mode & 0o777 == 0o640
    # a second run changes nothing
    snap = p.read_bytes()
    assert M.seed_ini(str(p), DOCK) == "present" and p.read_bytes() == snap


def test_seeding_stream_snv_keeps_its_layout_and_only_adds_the_marker(tmp_path):
    p = _copy(tmp_path, "stream-snv")
    before = p.read_text().splitlines()
    assert M.seed_ini(str(p), DOCK) == "present"
    assert p.read_text().splitlines() == before + ["", "[CameraBox]", "AvsyncDockSeeded=true"]
    snap = p.read_bytes()
    assert M.seed_ini(str(p), DOCK) == "present" and p.read_bytes() == snap


def test_crlf_line_endings_are_kept(tmp_path):
    text = (FIX / "stream-pp.user.ini").read_text().replace("\n", "\r\n")
    p = tmp_path / "user.ini"
    p.write_bytes(text.encode())
    assert M.seed_ini(str(p), DOCK) == "seeded"
    raw = p.read_bytes()
    # the original lines + the marker section's blank line, header and key
    assert raw.count(b"\r\n") == text.count("\r\n") + 3 and b"\n" not in raw.replace(b"\r\n", b"")


def test_a_dockstate_outside_basicwindow_is_not_read(tmp_path):
    line = [l for l in (FIX / "stream-pp.user.ini").read_text().splitlines() if l.startswith("DockState=")][0]
    p = _copy(tmp_path, None, "[General]\n" + line + "\n[BasicWindow]\nSaveProjectors=true\n")
    assert M.check_ini(str(p), DOCK) == "no-state"
    snap = p.read_bytes()
    assert M.seed_ini(str(p), DOCK) == "no-state" and p.read_bytes() == snap


def test_two_dockstate_lines_are_refused_untouched(tmp_path):
    text = (FIX / "stream-pp.user.ini").read_text()
    line = [l for l in text.splitlines() if l.startswith("DockState=")][0]
    p = _copy(tmp_path, None, text.replace(line, line + "\n" + line))
    snap = p.read_bytes()
    with pytest.raises(M.DockStateError):
        M.seed_ini(str(p), DOCK)
    assert p.read_bytes() == snap
    assert M.check_ini(str(p), DOCK) == "unreadable"


def test_a_symlinked_user_ini_is_refused(tmp_path):
    real = _copy(tmp_path, "stream-pp")
    link = tmp_path / "link.ini"
    link.symlink_to(real)
    snap = real.read_bytes()
    with pytest.raises(PermissionError):
        M.seed_ini(str(link), DOCK)
    assert real.read_bytes() == snap


def test_a_non_base64_dockstate_is_refused(tmp_path):
    p = _copy(tmp_path, None, "[BasicWindow]\nDockState=not*base64!\n")
    with pytest.raises(M.DockStateError):
        M.seed_ini(str(p), DOCK)
    assert M.check_ini(str(p), DOCK) == "unreadable"


def test_no_ini_and_no_state(tmp_path):
    assert M.check_ini(str(tmp_path / "missing.ini"), DOCK) == "no-ini"
    assert M.seed_ini(str(tmp_path / "missing.ini"), DOCK) == "no-ini"
    p = _copy(tmp_path, None, "[BasicWindow]\nSaveProjectors=true\n")
    assert M.check_ini(str(p), DOCK) == "no-state"


# --- the CLI ------------------------------------------------------------------------------------------

def test_the_cli_tokens_and_exit_codes(tmp_path):
    p = _copy(tmp_path, "stream-pp")
    r = _cli("check", "--ini", str(p), "--dock", DOCK)
    assert (r.returncode, r.stdout.strip()) == (0, "absent")
    r = _cli("seed", "--ini", str(p), "--dock", DOCK)
    assert (r.returncode, r.stdout.strip()) == (0, "seeded")
    r = _cli("check", "--ini", str(p), "--dock", DOCK)
    assert (r.returncode, r.stdout.strip()) == (0, "visible")
    bad = _copy(tmp_path, None, "[BasicWindow]\nDockState=AAAA\n")
    r = _cli("seed", "--ini", str(bad), "--dock", DOCK)
    assert r.returncode == 3 and "left untouched" in r.stderr
    link = tmp_path / "l.ini"
    link.symlink_to(p)
    r = _cli("seed", "--ini", str(link), "--dock", DOCK)
    assert r.returncode == 4


# --- the first-seed marker (review rounds 1-2) ------------------------------------------------------
# OBS saves the dock hidden + floating both before any seed AND when the operator floats it and closes
# it. The first seed decision therefore writes `[CameraBox] AvsyncDockSeeded=true` INTO user.ini, in the
# same atomic rewrite: a replaced / restored user.ini loses it together with the layout (round 2), and
# OBS's own config save keeps an unknown section.

MARK = "AvsyncDockSeeded=true"


def _default_hidden_ini(tmp_path):
    # stream-snv's advss-status-dock is hidden + floating: use it as the dock under test
    return _copy(tmp_path, "stream-snv"), "advss-status-dock"


def _marked(p):
    lines = p.read_text().splitlines()
    return "[CameraBox]" in lines and MARK in lines[lines.index("[CameraBox]") + 1:]


def test_without_the_marker_the_obs_default_is_redocked_and_marked(tmp_path):
    p, name = _default_hidden_ini(tmp_path)
    assert M.check_ini(str(p), name) == "default-hidden"
    assert M.seed_ini(str(p), name) == "redocked"
    assert _marked(p) and M.check_ini(str(p), name) == "visible"


def test_with_the_marker_the_obs_default_is_the_operators_and_kept(tmp_path):
    p, name = _default_hidden_ini(tmp_path)
    p.write_text(p.read_text() + "\n[CameraBox]\n" + MARK + "\n")
    snap = p.read_bytes()
    assert M.check_ini(str(p), name) == "hidden"
    assert M.seed_ini(str(p), name) == "present"
    assert p.read_bytes() == snap


@pytest.mark.parametrize("box,want", [("stream-pp", "seeded"), ("stream-snv", "present")])
def test_a_seed_decision_marks_user_ini_once(tmp_path, box, want):
    p = _copy(tmp_path, box)
    before = p.read_text().splitlines()
    assert M.seed_ini(str(p), DOCK) == want and _marked(p)
    after = p.read_text().splitlines()
    # only the DockState value (seeded) changed and the marker section was appended; the rest kept in order
    assert after[len(before):] == ["", "[CameraBox]", MARK]
    assert [l for l in after[:len(before)] if not l.startswith("DockState=")] == \
        [l for l in before if not l.startswith("DockState=")]
    snap = p.read_bytes()
    assert M.seed_ini(str(p), DOCK) == "present" and p.read_bytes() == snap


def test_a_replaced_user_ini_loses_the_marker_and_is_redocked(tmp_path):
    # round 2: the stream box seeded once, then user.ini restored from a backup without the dock, then
    # OBS ran once with the plugin and saved its hidden + floating default: the next seed re-docks it.
    p, name = _default_hidden_ini(tmp_path)
    assert M.seed_ini(str(p), name) == "redocked" and _marked(p)
    p.write_text((FIX / "stream-snv.user.ini").read_text())  # the restore: layout + marker both gone
    assert M.check_ini(str(p), name) == "default-hidden"
    assert M.seed_ini(str(p), name) == "redocked"


def test_an_existing_camerabox_section_gains_the_key_in_place(tmp_path):
    p = _copy(tmp_path, None, (FIX / "stream-pp.user.ini").read_text() + "\n[CameraBox]\nOther=1\n")
    assert M.seed_ini(str(p), DOCK) == "seeded"
    lines = p.read_text().splitlines()
    assert lines.count("[CameraBox]") == 1
    i = lines.index("[CameraBox]")
    assert MARK in lines[i + 1:] and "Other=1" in lines[i + 1:]


def test_the_marker_follows_the_files_line_endings(tmp_path):
    text = (FIX / "stream-snv.user.ini").read_text().replace("\n", "\r\n")
    p = tmp_path / "user.ini"
    p.write_bytes(text.encode())
    assert M.seed_ini(str(p), DOCK) == "present"
    raw = p.read_bytes()
    assert b"[CameraBox]\r\n" + MARK.encode() + b"\r\n" in raw
    assert b"\n" not in raw.replace(b"\r\n", b"")


def test_no_saved_layout_is_never_marked(tmp_path):
    p = _copy(tmp_path, None, "[BasicWindow]\nSaveProjectors=true\n")
    snap = p.read_bytes()
    assert M.seed_ini(str(p), DOCK) == "no-state" and p.read_bytes() == snap


def test_a_false_marker_value_is_not_a_marker(tmp_path):
    p, name = _default_hidden_ini(tmp_path)
    p.write_text(p.read_text() + "\n[CameraBox]\nAvsyncDockSeeded=false\n")
    assert M.check_ini(str(p), name) == "default-hidden"


def test_an_existing_marker_key_with_another_value_is_replaced_not_duplicated(tmp_path):
    # round-2 review nit: a second copy of the key reads differently in libobs (a hash) and configparser
    # (the last copy), so the one existing line is replaced.
    p, name = _default_hidden_ini(tmp_path)
    p.write_text(p.read_text() + "\n[CameraBox]\nAvsyncDockSeeded=false\nOther=1\n")
    assert M.seed_ini(str(p), name) == "redocked"
    lines = p.read_text().splitlines()
    keys = [l for l in lines if l.startswith("AvsyncDockSeeded=")]
    assert keys == [MARK] and "Other=1" in lines
    assert M.check_ini(str(p), name) == "visible"


def test_a_rewrite_as_root_keeps_the_files_owner(tmp_path, monkeypatch):
    # round-2 review nit: the rewritten user.ini keeps its owner in the same rename, never only through a
    # later chown. Non-root here, so the root branch is driven through the module's own os handle.
    p = _copy(tmp_path, "stream-pp")
    st = os.stat(p)
    calls = []
    monkeypatch.setattr(M.os, "geteuid", lambda: 0)
    monkeypatch.setattr(M.os, "chown", lambda path, uid, gid: calls.append((os.path.dirname(path), uid, gid)))
    assert M.seed_ini(str(p), DOCK) == "seeded"
    assert calls == [(str(tmp_path), st.st_uid, st.st_gid)]


def test_a_rewrite_as_a_user_never_chowns(tmp_path, monkeypatch):
    p = _copy(tmp_path, "stream-pp")
    monkeypatch.setattr(M.os, "geteuid", lambda: 1000)
    monkeypatch.setattr(M.os, "chown", lambda *a: (_ for _ in ()).throw(AssertionError("chown as a user")))
    assert M.seed_ini(str(p), DOCK) == "seeded"


# --- the names are the dock's own --------------------------------------------------------------------

def test_the_dock_object_name_and_module_follow_the_vendored_plugin():
    cm = (REPO / "vendor" / "av-sync-dock" / "CMakeLists.txt").read_text()
    project = re.search(r"^project\(([\w-]+) ", cm, re.M).group(1)
    prefix = re.search(r'^set\(ID_PREFIX "([^"]+)"\)', cm, re.M).group(1)
    main = (REPO / "vendor" / "av-sync-dock" / "src" / "plugin-main.c").read_text()
    assert 'obs_frontend_add_dock_by_id(ID_PREFIX ".main",' in main
    lib = (REPO / "scripts" / "lib" / "obs-box-avsync-dock.sh").read_text()
    assert f'OBS_BOX_AVSYNC_DOCK_MODULE="{project}"' in lib
    assert f'OBS_BOX_AVSYNC_DOCK_ID="{prefix}.main"' in lib
    assert prefix + ".main" == DOCK


def test_obs_hides_a_newly_added_plugin_dock_and_restores_the_saved_state_after_the_plugins():
    # The premise of the seed, read from the vendored OBS: the dock is added hidden + floating, and the
    # saved DockState is restored after the plugins' post-load (where the dock adds itself).
    api = (REPO / "vendor" / "obs-studio" / "frontend" / "OBSStudioAPI.cpp").read_text()
    body = api[api.index("bool OBSStudioAPI::obs_frontend_add_dock_by_id("):]
    body = body[:body.index("\n}\n")]
    assert body.index("dock->setVisible(false);") < body.index("dock->setFloating(true);")
    basic = (REPO / "vendor" / "obs-studio" / "frontend" / "widgets" / "OBSBasic.cpp").read_text()
    assert basic.index("App()->loadAppModules(mfi);") < basic.index('"BasicWindow", "DockState");')
