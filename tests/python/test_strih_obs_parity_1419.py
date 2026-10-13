"""issue 1419 -- strih-pp's OBS carries the full strih-snv OBS setup (owner 13.10.2026, verbatim:
"ale ved tam malo byt cely obs setup skopirovany aky mame v strih snv").

The live diff of both boxes' ~/.config/obs-studio (13.10.2026): the scene collection and the plugin set
were already equal; missing on strih-pp were (A) the profile's record / output / video keys, (B) four
user.ini [BasicWindow] keys, (C) the saved fullscreen Multiview projector. These tests pin:
  * (A) `obs_box_stream_profile.py --strih` writes every declared key + recordEncoder.json, the record
    keys equal the `strih_lx_profile_facts` declaration, `--grade-strih` names each missing key;
  * (B) STRIH_OPERATOR_INI_KEYS carries the four keys, the (operator-ini) row grades them;
  * (C) `scripts/strih_projector_seed.py` seeds the Multiview projector into an EMPTY saved list only,
    the (mv-projector) row reads the live window;
  * the setup / verify wiring, both scripts under their 1000-line budget.
"""
import configparser
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
OPERATOR = SCRIPTS / "lib" / "strih-operator.sh"
NIC_LIB = SCRIPTS / "lib" / "strih-nic-driver.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_strih_operator_1418 import Op, _exec  # noqa: E402  (the item-43 operator fixture)


def _mod(name):
    spec = importlib.util.spec_from_file_location(name + "_1419", SCRIPTS / (name + ".py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _bash(body, *args):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load strih-pp\n. "$2"\n. "$3"\n. "$4"\nshift 4\n' + body
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), str(PROVISION), str(NIC_LIB), str(OPERATOR),
                           *args], capture_output=True, text=True, timeout=120)


def _ini(path):
    cp = configparser.RawConfigParser()
    cp.optionxform = str
    cp.read(path)
    return cp


# --- (A) the strih profile ---------------------------------------------------------------------------

STRIH_KEYS = {
    ("Output", "Mode"): "Advanced",
    ("Output", "Reconnect"): "true",
    ("Output", "RetryDelay"): "2",
    ("Output", "MaxRetries"): "25",
    ("Output", "FilenameFormatting"): "%CCYY-%MM-%DD %hh-%mm-%ss",
    ("AdvOut", "RecType"): "Standard",
    ("AdvOut", "RecEncoder"): "obs_nvenc_hevc_tex",
    ("AdvOut", "RecFilePath"): "/srv/_REC",
    ("AdvOut", "RecFormat2"): "mkv",
    ("AdvOut", "RecSplitFileType"): "Time",
    ("AdvOut", "RecSplitFileTime"): "15",
    ("AdvOut", "RecTracks"): "1",
    ("Video", "BaseCX"): "1920",
    ("Video", "OutputCX"): "1920",
    ("Video", "FPSCommon"): "30",
    ("Video", "FPSInt"): "30",
    ("Video", "FPSNum"): "30",
    ("Video", "FPSDen"): "1",
    ("Video", "ScaleType"): "bicubic",
    ("Video", "ColorFormat"): "NV12",
    ("Audio", "MonitoringDeviceId"): "default",
}


def test_the_strih_profile_writes_the_strih_snv_record_output_and_video_keys(tmp_path):
    m = _mod("obs_box_stream_profile")
    d = tmp_path / "strih-pp"
    m.apply_strih(str(d), "strih-pp")
    cp = _ini(d / "basic.ini")
    got = {k: cp.get(*k, fallback=None) for k in STRIH_KEYS}
    assert got == STRIH_KEYS
    assert json.loads((d / "recordEncoder.json").read_text()) == {"rate_control": "CBR", "bitrate": 20000,
                                                                  "preset": "p5"}
    assert not (d / "service.json").exists() and not (d / "streamEncoder.json").exists()


def test_the_record_keys_equal_the_declared_strih_profile_facts():
    r = _bash("strih_lx_profile_facts")
    facts = dict(ln.split("=", 1) for ln in r.stdout.split())
    m = _mod("obs_box_stream_profile")
    adv = m.STRIH_PROFILE["AdvOut"]
    assert adv["RecEncoder"] == facts["rec_encoder"]
    assert adv["RecFilePath"] == facts["rec_path"]
    assert adv["RecFormat2"] == facts["rec_format"]
    assert adv["RecSplitFileTime"] == facts["rec_split_min"]
    assert "%sx%s" % (m.BASIC_INI["Video"]["OutputCX"], m.BASIC_INI["Video"]["OutputCY"]) == facts["output_res"]
    assert m.BASIC_INI["Video"]["FPSCommon"] == facts["fps"]


@pytest.mark.parametrize("sec,key", sorted(STRIH_KEYS))
def test_the_strih_grade_names_each_missing_key(tmp_path, sec, key):
    m = _mod("obs_box_stream_profile")
    d = tmp_path / "p"
    m.apply_strih(str(d), "strih-pp")
    assert m.grade_strih(str(d)) == []
    cp = _ini(d / "basic.ini")
    cp.remove_option(sec, key)
    with open(d / "basic.ini", "w") as fh:
        cp.write(fh, space_around_delimiters=False)
    drift = m.grade_strih(str(d))
    assert len(drift) == 1 and key in drift[0], drift


def test_the_strih_grade_names_a_changed_record_encoder_json(tmp_path):
    m = _mod("obs_box_stream_profile")
    d = tmp_path / "p"
    m.apply_strih(str(d), "strih-pp")
    (d / "recordEncoder.json").write_text(json.dumps({"rate_control": "CQP"}))
    assert any("recordEncoder.json" in t for t in m.grade_strih(str(d)))


# --- (B) user.ini [BasicWindow] ------------------------------------------------------------------------

NEW_INI = {"CloseExistingProjectors": "true", "WarnBeforeStartingStream": "false",
           "WarnBeforeStoppingStream": "false", "WarnBeforeStoppingRecord": "false"}


def test_the_operator_ini_keys_carry_the_strih_snv_basic_window_keys():
    r = _bash('printf "%s\\n" "${STRIH_OPERATOR_INI_KEYS[@]}"')
    keys = dict(ln.split("=", 1) for ln in r.stdout.split())
    for k, v in NEW_INI.items():
        assert keys.get(k) == v, keys


@pytest.fixture
def op(tmp_path):
    o = Op(tmp_path)
    o.healthy()
    yield o
    o.close()


def test_the_operator_ini_row_names_a_missing_key(op):
    ini = op.home / ".config" / "obs-studio" / "user.ini"
    ini.write_text(ini.read_text().replace("WarnBeforeStoppingRecord=false\n", ""))
    st, d = op.row("operator-ini")
    assert st == "FAIL" and "WarnBeforeStoppingRecord" in d, d


def test_the_operator_ini_row_passes_the_seeded_file(op):
    st, d = op.row("operator-ini")
    assert st == "OK", d


# --- (C) the saved Multiview projector ----------------------------------------------------------------

def test_the_projector_seed_fills_an_empty_saved_list(tmp_path):
    m = _mod("strih_projector_seed")
    p = tmp_path / "strih-pp.json"
    p.write_text(json.dumps({"name": "strih-pp", "saved_projectors": [], "sources": [{"name": "x"}]}))
    assert m.main(["--collection", str(p)]) == 0
    d = json.loads(p.read_text())
    assert d["saved_projectors"] == [{"monitor": 0, "type": 4, "geometry": "", "alwaysOnTopOverridden": False}]
    assert d["sources"] == [{"name": "x"}] and d["name"] == "strih-pp"


def test_the_projector_seed_never_touches_an_existing_list(tmp_path):
    m = _mod("strih_projector_seed")
    p = tmp_path / "strih-snv.json"
    keep = [{"monitor": 1, "type": 0, "geometry": "AdnQ", "alwaysOnTopOverridden": True}]
    p.write_text(json.dumps({"saved_projectors": keep}))
    before = p.read_bytes()
    assert m.main(["--collection", str(p)]) == 0
    assert p.read_bytes() == before


def test_the_projector_seed_refuses_a_missing_collection(tmp_path):
    m = _mod("strih_projector_seed")
    assert m.main(["--collection", str(tmp_path / "none.json")]) != 0


def test_the_mv_projector_row_reads_the_live_window(op):
    st, d = op.row("mv-projector")
    assert st == "OK", d
    (op.st / "wmctrl").write_text("0x01  0 obs.obs strih-pp OBS Studio 32.2.0\n")
    st, d = op.row("mv-projector")
    assert st == "FAIL" and "Projector - Multiview" in d, d


def test_the_mv_projector_row_is_a_note_while_obs_is_down(op):
    (op.st / "obs-up").unlink()
    st, d = op.row("mv-projector")
    assert st == "NOTE", d


# --- wiring ----------------------------------------------------------------------------------------

def test_setup_and_verify_wire_the_parity_pieces():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert 'strih_operator_install "${OBS_CFG}/user.ini" "$DESKTOP_USER" ' \
           '"${OBS_CFG}/basic/scenes/$(strih_lx_obs_collection).json"' in s
    assert "obs_box_strih_profile_grade" in v
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
