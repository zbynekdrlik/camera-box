"""issue 1424 -- strih_operator_hdmi_shows_multiview reads the "outputs" array form too.

strih-pp's drm-output.json is `{"enabled":true,"outputs":[{"connector":"HDMI-0",...,"view":"multiview"},
{"connector":"HDMI-1",...,"view":"program"}]}` (issue 1424 part 2). The predicate read only the single-object
`view` key, so on strih-pp it said "no Multiview on HDMI": the projector seed then put a duplicate fullscreen
Multiview window on the operator screen and the (mv-click) grading took the wrong branch.
"""
import json
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "strih-operator.sh"


def _shows(tmp_path, conf):
    f = tmp_path / "drm-output.json"
    f.write_text(conf if isinstance(conf, str) else json.dumps(conf))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; strih_operator_hdmi_shows_multiview "$2" && echo yes || echo no',
                        "h", str(LIB), str(f)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip() == "yes"


def _out(conn, view=None):
    e = {"connector": conn, "argb": 2105376, "backend": "vk-direct"}
    if view is not None:
        e["view"] = view
    return e


def test_the_strih_pp_outputs_array_with_a_multiview_entry_shows_the_multiview(tmp_path):
    conf = {"enabled": True, "outputs": [_out("HDMI-0", "multiview"), _out("HDMI-1", "program")]}
    assert _shows(tmp_path, conf)


def test_an_outputs_array_with_program_only_does_not(tmp_path):
    assert not _shows(tmp_path, {"enabled": True, "outputs": [_out("HDMI-0", "program"), _out("HDMI-1")]})


def test_a_disabled_outputs_array_does_not(tmp_path):
    assert not _shows(tmp_path, {"enabled": False, "outputs": [_out("HDMI-0", "multiview")]})


def test_a_malformed_outputs_array_never_reads_as_a_multiview(tmp_path):
    # the C starts NO output on a malformed array (all or nothing), so nothing shows
    for outputs in ("x", [], ["HDMI-0"], [_out("HDMI-0", "multiview"), _out("HDMI-0", "program")],
                    [_out("HDMI-0", "multiview")] * 3, [_out("HDMI-0", "grid")]):
        assert not _shows(tmp_path, {"enabled": True, "outputs": outputs}), outputs


def test_the_single_object_form_is_unchanged(tmp_path):
    assert _shows(tmp_path, {"enabled": True, "connector": "HDMI-0", "argb": 2105376, "view": "multiview"})
    assert not _shows(tmp_path, {"enabled": True, "connector": "HDMI-0", "argb": 2105376})
    assert not _shows(tmp_path, "{not json")
