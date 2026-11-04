"""issue 1485: verify-strih item 16 grades the intercom hub's Dante channels one by one.

On 3.11.2026 the hub's FOH flow was lost at 03:18 while the hub kept counting pump blocks, and the
one-level `foh_dante` view could not tell. intercom 1.9.9 publishes participants[].dlink.rx[] per
channel (state, written). The grade reads /api/state twice: every channel `connected` with `written`
growing PASSes; a channel not connected, or connected with `written` flat (the flow is lost), FAILs; a
hub without rx[] FAILs; a box without a hub Dante device prints nothing. Fixture files go in through
the STRIH_HUB_DANTE_STATE_FILE1/2 seam; the lib runs under the callers' `set -euo pipefail`.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"
VERIFY = REPO / "scripts" / "verify-strih.sh"


def _state(written, state="connected", rx=True):
    chans = [{"rx": i, "name": n, "tx_device": d, "tx_channel": c, "state": state, "flow": f,
              "packets": 10, "written": written}
             for i, n, d, c, f in [(1, "REPRO L", "master-asio-dante", "Repro 0db L", 1),
                                   (2, "REPRO R", "master-asio-dante", "Repro 0db R", 1),
                                   (3, "CLICK 3doba", "band-asio-dante", "Click 3doba", 2),
                                   (4, "GUIDE", "band-asio-dante", "Guide 2 iem", 2)]]
    d = {"device": "strih-snv-intercom", "running": True}
    if rx:
        d["rx"] = chans
    return {"version": "1.9.9", "participants": [{"name": "fohabl"}, {"name": "foh_dante", "dlink": d}]}


def _grade(tmp_path, s1, s2, box="strih-snv"):
    f1, f2 = tmp_path / "s1.json", tmp_path / "s2.json"
    for f, s in ((f1, s1), (f2, s2)):
        f.write_text(s if isinstance(s, str) else json.dumps(s))
    script = ('set -euo pipefail\n. "$1"\nstrih_box_load "$3" >/dev/null\n'
              'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              'strih_intercom_hub_dante_rx_grade "$2"\necho END')
    env = dict(os.environ, STRIH_HUB_DANTE_STATE_FILE1=str(f1), STRIH_HUB_DANTE_STATE_FILE2=str(f2))
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB), "http://192.0.2.1:8790/api/state", box],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "command not found" not in r.stderr and "unbound variable" not in r.stderr, r.stderr
    lines = r.stdout.strip().splitlines()
    assert lines[-1] == "END", r.stdout
    return lines[:-1]


def test_every_channel_connected_and_writing_passes(tmp_path):
    out = _grade(tmp_path, _state(2189608), _state(2669840))
    assert out == ["PASS (intercom-dante) foh_dante: 4 channel(s) connected, written growing (rx1, rx2, rx3, rx4)"], out


def test_a_connected_channel_that_writes_nothing_fails_as_a_lost_flow(tmp_path):
    out = _grade(tmp_path, _state(100), _state(100))
    assert len(out) == 1 and out[0].startswith("FAIL (intercom-dante) foh_dante: rx1 master-asio-dante/Repro 0db L "
                                               "connected but written did not grow (100 -> 100): the flow is lost"), out


def test_a_channel_not_connected_fails_with_its_state(tmp_path):
    out = _grade(tmp_path, _state(1, state="failed"), _state(2, state="failed"))
    assert len(out) == 1 and "rx3 band-asio-dante/Click 3doba state=failed" in out[0] and out[0].startswith("FAIL"), out


def test_a_hub_without_per_channel_rx_fails(tmp_path):
    out = _grade(tmp_path, _state(1, rx=False), _state(2, rx=False))
    assert len(out) == 1 and out[0].startswith("FAIL") and "before intercom 1.9.9" in out[0], out


def test_an_unreadable_or_garbage_state_fails(tmp_path):
    assert _grade(tmp_path, "", "")[0].startswith("FAIL (intercom-dante) the hub state at")
    assert _grade(tmp_path, "{", "{")[0].startswith("FAIL (intercom-dante) the hub state is not JSON")


def test_a_box_without_a_hub_dante_device_prints_nothing(tmp_path):
    assert _grade(tmp_path, "", "", box="strih-pp") == []


def test_verify_item_16_calls_the_grade_on_the_hub_line():
    text = VERIFY.read_text()
    assert text.count('strih_intercom_hub_dante_rx_grade "http://${WS_HOST}:8790/api/state"') == 1
