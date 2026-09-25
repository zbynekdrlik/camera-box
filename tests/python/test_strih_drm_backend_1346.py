"""issue 1346 -- the strih-lx BUILT-IN HDMI is driven by the NVIDIA GPU, so its fixed output uses the
second DRM-output backend, Vulkan direct display ("backend": "vk-direct" in ~/.camera-box/drm-output.json),
chosen by the per-box fact STRIH_HDMI_OUTPUT_BACKEND (owner rulings 5838578002 + 5838662632, main design
5838663570).

Tier-0 runnable (pytest on the pure helpers; no cargo, no rig, no OBS). The backend grammar table is SHARED
with the vendored C lift (tests/fixtures/drm_output_backend_parity.tsv, read by
tests/drm_output_vk_direct_1346.rs), so the Python mirror and the C module can never drift apart.
"""
import importlib.util
import json
from pathlib import Path

import pytest

HERE = Path(__file__).parent
REPO = HERE.parent.parent
SCRIPTS = REPO / "scripts"
PARITY_TSV = REPO / "tests" / "fixtures" / "drm_output_backend_parity.tsv"


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


def test_backend_of_matches_the_shared_c_grammar_table():
    for value, want in _parity_rows():
        assert _mod.drm_output_backend_of(value) == want, "backend %r" % (value,)


def test_backend_of_non_string_reads_as_absent_like_the_c():
    # obs_data_get_string() on a non-string item reads "" -> the C picks the lease default; mirror it.
    for value in (3, True, [], {}):
        assert _mod.drm_output_backend_of(value) == "lease"


def test_backend_token_of_a_config_text():
    base = {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "multiview"}
    assert _mod.drm_output_backend_token(json.dumps(base)) == "lease"
    assert _mod.drm_output_backend_token(json.dumps(dict(base, backend="vk-direct"))) == "vk-direct"
    assert _mod.drm_output_backend_token(json.dumps(dict(base, backend="lease"))) == "lease"
    assert _mod.drm_output_backend_token(json.dumps(dict(base, backend="vulkan"))) == "unknown"
    assert _mod.drm_output_backend_token("") == "lease"
    assert _mod.drm_output_backend_token("not json") == "lease"
    assert _mod.drm_output_backend_token("[1, 2]") == "lease"


def _config(tmp_path, cfg):
    p = tmp_path / "drm-output.json"
    p.write_text(json.dumps(cfg, separators=(",", ":")) + "\n")
    return p


def test_write_backend_keeps_the_operator_view_and_every_other_key(tmp_path):
    p = _config(tmp_path, {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "program"})
    _mod.write_drm_backend("vk-direct", str(p))
    text = p.read_text()
    assert text.count("\n") == 1 and text.endswith("\n"), "ONE compact machine-written line"
    assert json.loads(text) == {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "program",
                                "backend": "vk-direct"}
    assert text == '{"enabled":true,"connector":"HDMI-0","argb":2105376,"view":"program","backend":"vk-direct"}\n'


def test_write_backend_lease_drops_the_key_so_the_lease_config_shape_is_unchanged(tmp_path):
    p = _config(tmp_path, {"enabled": True, "connector": "HDMI-1", "argb": 2105376, "view": "program",
                           "backend": "vk-direct"})
    _mod.write_drm_backend("lease", str(p))
    assert p.read_text() == '{"enabled":true,"connector":"HDMI-1","argb":2105376,"view":"program"}\n'


def test_write_backend_refuses_an_unknown_backend_or_an_unprovisioned_config(tmp_path):
    p = _config(tmp_path, {"enabled": True, "connector": "HDMI-0"})
    with pytest.raises(ValueError):
        _mod.write_drm_backend("vulkan", str(p))
    with pytest.raises(ValueError):
        _mod.write_drm_backend("vk-direct", str(tmp_path / "absent.json"))
    bad = tmp_path / "bad.json"
    bad.write_text("{not json")
    with pytest.raises(ValueError):
        _mod.write_drm_backend("vk-direct", str(bad))
    assert bad.read_text() == "{not json", "a broken config is never rewritten"


def test_write_view_still_keeps_the_backend_key(tmp_path):
    # The OBS Tools-menu switch (the C) and its scripted twin rewrite ONLY "view" -- the box fact survives.
    p = _config(tmp_path, {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "multiview",
                           "backend": "vk-direct"})
    _mod.write_drm_view("program", str(p))
    assert json.loads(p.read_text())["backend"] == "vk-direct"


def test_an_unknown_backend_never_arms_the_wrapper():
    # The C stays dormant on an unknown backend, so strih-obs-start.sh must not take the connector out
    # of X for it (a black HDMI) -- review round 1.
    base = {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "multiview"}
    assert _mod.drm_output_lease_connector(json.dumps(dict(base, backend="vk-direct"))) == "HDMI-0"
    assert _mod.drm_output_lease_connector(json.dumps(base)) == "HDMI-0"
    assert _mod.drm_output_lease_connector(json.dumps(dict(base, backend="vulkan"))) == ""
    assert _mod.drm_output_lease_connector(json.dumps(dict(base, backend=7))) == "HDMI-0"

