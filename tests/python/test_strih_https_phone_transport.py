"""verify-strih item 45 `(https-upstream) interkom` follows the hub's phone transport (intercom#42 phase 4).

From intercom v1.9.32 the strih-snv phones join the hub's own WebRTC leg; the hub then answers `GET /janus`
with 503 ("this hub has no Janus room"), so the /janus 101 check would FAIL a healthy box. The row reads
the hub's `GET /api/phone` (`{"transport":"webrtc"}` | `{"transport":"janus"}`):
  * webrtc -> the /janus upgrade is not graded (named in the row), /api/state through nginx still must be
    200 and Janus (kept running as the fallback) must still listen on loopback only;
  * janus, or an unreadable / unknown answer -> today's check (the /janus upgrade must be 101).
"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_strih_https_1399 import Rig, SNV_INTERKOM, _healthy, _rows  # noqa: E402

HUB = "127.0.0.1:8790"


def _interkom_row(rows):
    row = [(st, t) for st, t in rows if t.startswith("(https-upstream) interkom")]
    assert len(row) == 1, rows
    return row[0]


def test_webrtc_phones_skip_the_janus_upgrade_and_name_it(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    rig.http(HUB, "/api/phone", '{"transport":"webrtc"}')
    rig.http(SNV_INTERKOM[0], "/janus", "503")
    rows = _rows(rig)
    assert all(st == "OK" for st, _ in rows), rows
    st, text = _interkom_row(rows)
    assert "webrtc" in text and "/janus" in text and "not graded" in text, text
    assert "loopback" in text, text


def test_webrtc_phones_still_need_the_hub_state_through_nginx(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    rig.http(HUB, "/api/phone", '{"transport":"webrtc"}')
    rig.http(SNV_INTERKOM[0], "/api/state", "502")
    st, text = _interkom_row(_rows(rig))
    assert st == "FAIL" and "502" in text, text


def test_webrtc_phones_still_need_janus_on_loopback_only(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    rig.http(HUB, "/api/phone", '{"transport":"webrtc"}')
    (rig.state / "listen-8188").write_text("0.0.0.0:8188\n")
    st, text = _interkom_row(_rows(rig))
    assert st == "FAIL" and "0.0.0.0:8188" in text, text


def test_janus_phones_keep_todays_check(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    rig.http(HUB, "/api/phone", '{"transport":"janus"}')
    rig.http(SNV_INTERKOM[0], "/janus", "503")
    st, text = _interkom_row(_rows(rig))
    assert st == "FAIL" and "503" in text, text


@pytest.mark.parametrize("body", [None, '{"transport":"carrier-pigeon"}', "garbage"])
def test_an_unreadable_transport_keeps_todays_check(tmp_path, body):
    """No /api/phone answer (an older hub) or a value we do not know = the janus check, never a skip."""
    rig = Rig(tmp_path)
    _healthy(rig)
    if body is not None:
        rig.http(HUB, "/api/phone", body)
    rig.http(SNV_INTERKOM[0], "/janus", "503")
    st, text = _interkom_row(_rows(rig))
    assert st == "FAIL" and "503" in text, (body, text)
