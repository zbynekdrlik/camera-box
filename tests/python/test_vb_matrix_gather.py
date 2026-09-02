"""#1227 — PURE gather-side parsers for the `vb_matrix_running` bundle-state facet
(scripts/bundle_state_gather.py).

The facet must distinguish THREE states so the dev1 watchdog never false-pages:
  running="1" -- a VBAudioMatrix* process is alive,
  running="0" -- the box HAS a VB-Matrix install on disk but the process is not running (ALERT),
  facet omitted ("") -- the box has NO VB-Matrix install (imag) -> never a false negative.

Process presence alone cannot tell "installed but dead" from "not installed", so the facet pairs a
native `tasklist` process parse with a disk install-present check. Both parsers are pure (Tier-0
pytest, #557 kills cargo). The real tasklist row here is the one observed live on the stream box
(VBAudioMatrix_x64.exe PID 8144) — note the comma INSIDE the quoted "18,236 K" Mem Usage field,
which is exactly why a csv.reader (not a naive split) is required.
"""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


# The live-observed stream-box tasklist row (comma inside the quoted Mem Usage field).
TASKLIST_STREAM = (
    '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
    '"System Idle Process","0","Services","0","8 K"\r\n'
    '"obs64.exe","4321","Console","1","512,000 K"\r\n'
    '"VBAudioMatrix_x64.exe","8144","Console","1","18,236 K"\r\n'
    '"notepad.exe","9999","Console","1","10,000 K"\r\n'
)

# strih runs the Coconut build under a different image name — the pattern must still match it.
TASKLIST_STRIH = (
    '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
    '"VBAudioMatrixCoconut_x64.exe","4210","Console","1","20,100 K"\r\n'
)

TASKLIST_NO_VBMATRIX = (
    '"Image Name","PID","Session Name","Session#","Mem Usage"\r\n'
    '"obs64.exe","4321","Console","1","512,000 K"\r\n'
)


# ------------------------------------------------------------------ vb_matrix_process_from_listing
def test_process_from_listing_finds_stream_x64():
    assert bsg.vb_matrix_process_from_listing(TASKLIST_STREAM) == ("VBAudioMatrix_x64", "8144")


def test_process_from_listing_finds_strih_coconut():
    assert bsg.vb_matrix_process_from_listing(TASKLIST_STRIH) == ("VBAudioMatrixCoconut_x64", "4210")


def test_process_from_listing_absent_is_empty():
    assert bsg.vb_matrix_process_from_listing(TASKLIST_NO_VBMATRIX) == ("", "")


def test_process_from_listing_empty_text_is_empty():
    assert bsg.vb_matrix_process_from_listing("") == ("", "")
    assert bsg.vb_matrix_process_from_listing(None) == ("", "")


def test_process_name_re_matches_both_builds_and_nothing_else():
    assert bsg.VB_MATRIX_PROCESS_NAME_RE.match("VBAudioMatrix_x64")
    assert bsg.VB_MATRIX_PROCESS_NAME_RE.match("VBAudioMatrixCoconut_x64")
    assert bsg.VB_MATRIX_PROCESS_NAME_RE.match("vbaudiomatrix_x64")  # case-insensitive
    assert not bsg.VB_MATRIX_PROCESS_NAME_RE.match("obs64")
    assert not bsg.VB_MATRIX_PROCESS_NAME_RE.match("NotVBAudioMatrix_x64")  # must anchor at start


# ------------------------------------------------------------------ vb_matrix_install_present_under
def test_install_present_true_when_exe_exists(tmp_path):
    d = tmp_path / "VB" / "VBAudioMatrix"
    d.mkdir(parents=True)
    (d / "VBAudioMatrix_x64.exe").write_bytes(b"fake exe")
    assert bsg.vb_matrix_install_present_under([str(tmp_path)]) is True


def test_install_present_true_for_coconut(tmp_path):
    d = tmp_path / "VB" / "VBAudioMatrix"
    d.mkdir(parents=True)
    (d / "VBAudioMatrixCoconut_x64.exe").write_bytes(b"fake exe")
    assert bsg.vb_matrix_install_present_under([str(tmp_path)]) is True


def test_install_present_false_when_no_exe(tmp_path):
    (tmp_path / "unrelated.exe").write_bytes(b"x")
    assert bsg.vb_matrix_install_present_under([str(tmp_path)]) is False


def test_install_present_false_for_missing_dir():
    assert bsg.vb_matrix_install_present_under(["/nonexistent/vb/dir"]) is False


# ------------------------------------------------------------------ vb_matrix_running_facet
def test_running_facet_no_install_omits_everything():
    # imag: no install -> running "" (omitted downstream), never a false "0" that would page.
    assert bsg.vb_matrix_running_facet(False, "VBAudioMatrix_x64", "8144") == ("", "", "")
    assert bsg.vb_matrix_running_facet(False, "", "") == ("", "", "")


def test_running_facet_install_and_process_is_one():
    assert bsg.vb_matrix_running_facet(True, "VBAudioMatrix_x64", "8144") == \
        ("1", "VBAudioMatrix_x64", "8144")


def test_running_facet_install_no_process_is_zero():
    # the 3-day outage shape: install present, process dead -> running "0" (present in JSON = DOWN).
    assert bsg.vb_matrix_running_facet(True, "", "") == ("0", "", "")


# ------------------------------------------------------------------ build_bundle_state wiring
def test_build_bundle_state_carries_running_facet():
    d = bsg.build_bundle_state(
        vb_matrix_running="1", vb_matrix_name="VBAudioMatrix_x64",
        vb_matrix_pid="8144", vb_matrix_start="2026-09-02T14:01:40",
    )
    assert d["vb_matrix_running"] == "1"
    assert d["vb_matrix_name"] == "VBAudioMatrix_x64"
    assert d["vb_matrix_pid"] == "8144"
    assert d["vb_matrix_start"] == "2026-09-02T14:01:40"


def test_build_bundle_state_keeps_running_zero():
    # "0" is a truthy string -> must NOT be dropped by the omit-when-empty filter (DOWN must surface).
    d = bsg.build_bundle_state(vb_matrix_running="0")
    assert d["vb_matrix_running"] == "0"


def test_build_bundle_state_omits_empty_running():
    d = bsg.build_bundle_state(vb_matrix_running="")
    assert "vb_matrix_running" not in d
    assert "vb_matrix_name" not in d


def test_build_bundle_state_facet_is_json_serialisable():
    d = bsg.build_bundle_state(vb_matrix_running="1", vb_matrix_pid="8144")
    json.dumps(d)  # must not raise
