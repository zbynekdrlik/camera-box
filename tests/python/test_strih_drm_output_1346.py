"""issue 1346 -- the strih-lx HDMI output is the issue-1152 in-OBS DRM-lease output with a Program /
Multiview VIEW (owner ROZHODNUTE 24.9.2026, design comment 5810067589). It is never an OBS projector
window and never the desktop. This file replaces the 19.9. projector-window tests: the
seed_projector / strih-lx-projector.json path is removed.

Tier-0 runnable (pytest on the pure helpers + script-text anchors; no cargo, no rig, no OBS). The view
grammar table is SHARED with the vendored C lift (tests/fixtures/drm_output_view_parity.tsv, read by
tests/drm_output_view_1346.rs), so the Python mirror and the C module can never drift apart.
"""
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).parent
REPO = HERE.parent.parent
SCRIPTS = REPO / "scripts"
PARITY_TSV = REPO / "tests" / "fixtures" / "drm_output_view_parity.tsv"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / ("%s.py" % name))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load("strih_scenes")


def _parity_rows():
    rows = []
    for line in PARITY_TSV.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        value, expected = line.split("\t")
        value = {"<absent>": None, "<empty>": ""}.get(value, value)
        rows.append((value, None if expected == "unknown" else expected))
    assert len(rows) >= 8
    return rows


# --- the view grammar: ONE table with the C -------------------------------------------------------

def test_view_of_matches_the_shared_c_grammar_table():
    for value, want in _parity_rows():
        assert _mod.drm_output_view_of(value) == want, "view %r" % (value,)


def test_view_of_non_string_fails_open_to_program_like_the_c():
    # obs_data_get_string() on a non-string item reads "" -> the C runs the Program; mirror it.
    for value in (3, True, [], {}):
        assert _mod.drm_output_view_of(value) == "program"


def test_view_token_of_a_config_text():
    cfg = lambda view: json.dumps({"enabled": True, "connector": "HDMI-0", "view": view})
    assert _mod.drm_output_view_token(cfg("multiview")) == "multiview"
    assert _mod.drm_output_view_token(cfg("program")) == "program"
    assert _mod.drm_output_view_token(cfg("Preview")) == "unknown"
    assert _mod.drm_output_view_token('{"enabled":true,"connector":"HDMI-0"}') == "program"
    assert _mod.drm_output_view_token("") == "program"
    assert _mod.drm_output_view_token("not json") == "program"


# --- the lease connector: the SAME grammar imag uses (the C module's own contract) ----------------

def test_lease_connector_agrees_with_imag_on_every_vector():
    imag = _load("imag_scenes")
    for text in (
        "",
        "garbage",
        "[]",
        '{"enabled":true,"connector":"HDMI-0","view":"multiview"}',
        '{"enabled":true,"connector":"HDMI-1"}',
        '{"enabled":false,"connector":"HDMI-1"}',
        '{"enabled":"true","connector":"HDMI-1"}',
        '{"enabled":true,"connector":""}',
        '{"enabled":true,"connector":7}',
        '{"enabled":true}',
    ):
        assert _mod.drm_output_lease_connector(text) == imag.drm_output_lease_connector(text), text


# --- read / write the persisted view (the --projector CLI twin of the OBS Tools menu) -------------

def _write(path, obj_or_text):
    path.write_text(obj_or_text if isinstance(obj_or_text, str) else json.dumps(obj_or_text))


def test_write_view_keeps_every_other_key_on_one_compact_line(tmp_path):
    p = tmp_path / "drm-output.json"
    _write(p, '{"enabled":true,"connector":"HDMI-0","argb":2105376,"view":"multiview"}\n')
    _mod.write_drm_view("program", str(p))
    text = p.read_text()
    assert text == '{"enabled":true,"connector":"HDMI-0","argb":2105376,"view":"program"}\n'
    assert _mod.read_drm_view(str(p)) == "program"


def test_write_view_adds_the_key_when_absent(tmp_path):
    p = tmp_path / "drm-output.json"
    _write(p, {"enabled": True, "connector": "HDMI-1"})
    _mod.write_drm_view("multiview", str(p))
    assert json.loads(p.read_text()) == {"enabled": True, "connector": "HDMI-1", "view": "multiview"}


def test_write_view_refuses_an_unprovisioned_or_broken_config(tmp_path):
    with pytest.raises(ValueError):
        _mod.write_drm_view("program", str(tmp_path / "absent.json"))
    bad = tmp_path / "bad.json"
    _write(bad, "not json")
    with pytest.raises(ValueError):
        _mod.write_drm_view("program", str(bad))
    assert bad.read_text() == "not json", "a config it cannot parse is never rewritten"
    arr = tmp_path / "arr.json"
    _write(arr, "[1]")
    with pytest.raises(ValueError):
        _mod.write_drm_view("program", str(arr))


def test_write_view_rejects_an_unknown_view(tmp_path):
    p = tmp_path / "drm-output.json"
    _write(p, {"enabled": True, "connector": "HDMI-0"})
    for bad in ("", "preview", "Program", None):
        with pytest.raises(ValueError):
            _mod.write_drm_view(bad, str(p))


def test_read_view_absent_is_none(tmp_path):
    assert _mod.read_drm_view(str(tmp_path / "none.json")) is None


def _cli(args, home):
    # A fake HOME must not also hide the user site-packages (python3-websocket may live there, and
    # strih_scenes imports it at module load) -- pin PYTHONUSERBASE to the REAL one first.
    import site
    env = dict(os.environ, HOME=str(home), PYTHONUSERBASE=site.getuserbase())
    return subprocess.run([sys.executable, str(SCRIPTS / "strih_scenes.py")] + args,
                          capture_output=True, text=True, env=env, timeout=30)


def test_projector_cli_writes_the_view_without_touching_obs(tmp_path):
    cfg_dir = tmp_path / ".camera-box"
    cfg_dir.mkdir()
    p = cfg_dir / "drm-output.json"
    _write(p, '{"enabled":true,"connector":"HDMI-0","view":"multiview"}')
    # an unreachable WS port proves the CLI never connects to OBS for this mode
    r = _cli(["--projector", "program", "--port", "1"], tmp_path)
    assert r.returncode == 0, r.stderr
    assert json.loads(p.read_text())["view"] == "program"
    assert "next OBS start" in r.stdout


def test_projector_cli_fails_loud_when_the_output_is_not_provisioned(tmp_path):
    r = _cli(["--projector", "multiview", "--port", "1"], tmp_path)
    assert r.returncode != 0
    assert "drm-output.json" in (r.stdout + r.stderr)


def test_projector_cli_rejects_bad_value():
    r = subprocess.run([sys.executable, str(SCRIPTS / "strih_scenes.py"), "--projector", "bogus"],
                       capture_output=True, text=True)
    assert r.returncode != 0
    assert "projector" in r.stderr.lower()


# --- the retired projector-window seed ------------------------------------------------------------

def test_projector_window_seed_is_removed():
    s = (SCRIPTS / "strih_scenes.py").read_text()
    for gone in ("def seed_projector", "OpenVideoMixProjector", "strih-lx-projector.json",
                 "seed_projector(obs)"):
        assert gone not in s, "issue 1346 (24.9.): `%s` must be removed -- the HDMI output is the " \
                              "DRM lease, a projector must never target it" % gone


def test_setup_strih_preseeds_saveprojectors_before_the_obs_enable():
    # The operator's LAPTOP projector keeps working and persisting (the design keeps it).
    s = (SCRIPTS / "setup-strih.sh").read_text()
    save = s.find('for kv in ("SaveProjectors=true", "ProjectorAlwaysOnTop=false")')
    enable = s.find("systemctl --user enable strih-obs.service")
    assert save != -1 and enable != -1
    assert save < enable
