"""issue 1418 -- the (mv-projector) row is not a gate item on the stream role.

stream-pp has one screen carrying the stream OBS and the Restreamer dashboard; a fullscreen Multiview
projector there would cover them, so verify-strih FAILed the box on 14.10.2026 with nothing wrong.
The row grades the strih operator surface (the cutter's Multiview); the strih role keeps its verdict.
"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_strih_operator_1418 import Op  # noqa: E402  (the item-43 operator fixture)


@pytest.fixture
def op(tmp_path):
    o = Op(tmp_path)
    o.healthy()
    yield o
    o.close()


def test_the_stream_role_passes_with_no_multiview_projector(op):
    (op.st / "wmctrl").write_text("0x01  0 obs.obs stream-pp OBS Studio 32.2.0\n")
    st, d = op.row("mv-projector", box="stream-pp")
    assert st == "OK", d
    assert "stream role" in d, d


def test_the_stream_role_fails_a_multiview_projector_window_1469(op):
    # issue 1469 (owner 23.10.2026, stream-snv): a fullscreen Multiview projector covered the stream
    # OBS and the Restreamer dashboard, and verify only printed a NOTE.
    (op.st / "wmctrl").write_text("0x01  0 obs.obs stream-snv OBS Studio 32.2.0\n"
                                  "0x02  0 obs.obs stream-snv Projector - Multiview\n")
    st, d = op.row("mv-projector", box="stream-snv")
    assert st == "FAIL", d
    assert "stream role" in d and "Projector - Multiview" in d, d


def test_the_strih_role_still_fails_a_missing_multiview_projector(op):
    (op.st / "wmctrl").write_text("0x01  0 obs.obs strih-pp OBS Studio 32.2.0\n")
    st, d = op.row("mv-projector", box="strih-pp")
    assert st == "FAIL" and "Projector - Multiview" in d, d


# --- issue 1469: the setup half never seeds the Multiview projector on the stream role --------------

def _seed_projector(tmp_path, box):
    from test_strih_operator_1418 import _bash, _obs, ME  # noqa: E402
    import json
    coll = tmp_path / "coll.json"
    coll.write_text(json.dumps({"name": box, "saved_projectors": [], "sources": []}))
    r = _bash('strih_operator_projector_seed "$1" "$2" "$3"', box=box,
              args=(str(coll), ME, str(tmp_path / "no-drm-output.json")),
              env={"STRIH_OPERATOR_OBS_RUNNING_CMD": str(_obs(tmp_path, False))})
    assert r.returncode == 0, r.stderr
    return json.loads(coll.read_text())["saved_projectors"], r.stdout


def test_the_seed_skips_the_stream_role_1469(tmp_path):
    saved, out = _seed_projector(tmp_path, "stream-snv")
    assert saved == [], saved
    assert "SKIP" in out and "stream role" in out, out


def test_the_seed_still_seeds_the_strih_role_1469(tmp_path):
    saved, out = _seed_projector(tmp_path, "strih-pp")
    assert [p["type"] for p in saved] == [4], (saved, out)
