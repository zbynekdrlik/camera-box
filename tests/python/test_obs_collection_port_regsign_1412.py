"""issue 1412: obs_collection_port.py --preset stream-pp must rename the REAL Windows program input.

The owner's Windows stream collection names the program input `NDI® 2ME PGM` (a real UTF-8 registered
sign, U+00AE). The preset used to key on `NDIAr 2ME PGM`, a mangled spelling from the recon report, so
on stream-pp (11.10.2026) the input was imported as `NDI® 2ME PGM`, the seed manifest's `NDI 2ME PGM`
was MISSING (verify item 4b) and the launch seed could not heal its program class.

Synthetic fixtures only (the shapes of tests/python/test_obs_collection_port_1412.py). Tier-0: pytest.
"""
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
TOOL = REPO / "scripts" / "obs_collection_port.py"
REG = "®"
REAL = "NDI" + REG + " 2ME PGM"

sys.path.insert(0, str(HERE))
import test_obs_collection_port_1412 as base  # noqa: E402  (the shared synthetic fixtures)


def _mod():
    spec = importlib.util.spec_from_file_location("obs_collection_port_regsign_1412", TOOL)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _real_names(coll):
    """The fixture with every `NDIAr` spelled the way the Windows collection really spells it."""
    return json.loads(json.dumps(coll).replace("NDIAr", "NDI" + REG))


def test_the_preset_keys_on_the_real_registered_sign_name():
    m = _mod()
    assert REAL in m.PRESETS["stream-pp"]["source_renames"]
    assert m.PRESETS["stream-pp"]["source_renames"][REAL] == "NDI 2ME PGM"


def test_the_real_windows_name_is_renamed_everywhere():
    m = _mod()
    out, rep = m.port(_real_names(base.stream_collection()), base._stream_opts(m))
    names = base._names(out)
    assert REAL not in names
    pgm = base._by_name(out, "NDI 2ME PGM")
    assert pgm["settings"]["ndi_source_name"] == "STRIH-PP (2ME PGM)"
    pro = [i["name"] for i in base._by_name(out, "PRO")["settings"]["items"]]
    assert pro == ["Zvuk z mixu", "NDI 2ME PGM", "NDI" + REG + " Source"]
    assert any(line.startswith("RENAME source") and REAL in line for line in rep), rep


def test_the_cli_renames_the_real_name_from_a_utf8_bom_export(tmp_path):
    src = tmp_path / "in.json"
    text = json.dumps(_real_names(base.stream_collection()), ensure_ascii=False)
    assert REAL in text
    src.write_bytes(b"\xef\xbb\xbf" + text.encode("utf-8"))
    dst = tmp_path / "stream-pp.json"
    r = subprocess.run([sys.executable, str(TOOL), "--preset", "stream-pp", "--presenter-host",
                        "presenter-pp.lan", str(src), str(dst)], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    out = json.loads(dst.read_text(encoding="utf-8"))
    assert "NDI 2ME PGM" in base._names(out) and REAL not in base._names(out)


@pytest.mark.parametrize("spelling", ["NDIAr 2ME PGM", "NDIÂ® 2ME PGM", "NDI(R) 2ME PGM"])
def test_the_mangled_spellings_of_the_same_name_are_renamed_too(spelling):
    m = _mod()
    coll = json.loads(json.dumps(base.stream_collection()).replace("NDIAr 2ME PGM", spelling))
    out, rep = m.port(coll, base._stream_opts(m))
    assert spelling not in base._names(out)
    assert base._by_name(out, "NDI 2ME PGM")["settings"]["ndi_source_name"] == "STRIH-PP (2ME PGM)"


def test_two_spellings_of_the_program_input_in_one_collection_refuse():
    m = _mod()
    coll = base.stream_collection()
    twin = json.loads(json.dumps(base._by_name(coll, "NDIAr 2ME PGM")))
    twin["name"], twin["uuid"] = REAL, "u-real"
    coll["sources"].append(twin)
    # referenced from a kept scene, or the keep-set drops it before any rename runs
    pro = base._by_name(coll, "PRO")["settings"]
    pro["items"].append({"name": REAL, "source_uuid": "u-real", "visible": True, "id": 99})
    with pytest.raises(m.PortError, match="spelling"):
        m.port(coll, base._stream_opts(m))
