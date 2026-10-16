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


def test_the_stream_role_does_not_grade_the_multiview_projector(op):
    (op.st / "wmctrl").write_text("0x01  0 obs.obs stream-pp OBS Studio 32.2.0\n")
    st, d = op.row("mv-projector", box="stream-pp")
    assert st == "NOTE", d
    assert "stream role" in d, d


def test_the_strih_role_still_fails_a_missing_multiview_projector(op):
    (op.st / "wmctrl").write_text("0x01  0 obs.obs strih-pp OBS Studio 32.2.0\n")
    st, d = op.row("mv-projector", box="strih-pp")
    assert st == "FAIL" and "Projector - Multiview" in d, d
