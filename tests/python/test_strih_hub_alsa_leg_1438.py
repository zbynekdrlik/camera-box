"""issue 1438: verify-strih item 9 grades the intercom hub's card leg (the cutters' USB card, opened
directly through ALSA: strih-snv's MiniFuse 4, strih-pp's MG-XU).

The grade reads the hub's /api/state (a fixture file through the STRIH_HUB_ALSA_STATE_FILE seam here).
For every `alsa` participant it PASSes a card that is on the box with every side the participant uses
moving audio (aplay playing, arecord delivering), naming the format and the xruns; it FAILs a card that
is not on the box, and a side that is down or not moving audio, naming why. A participant still on the
`pipewire` adapter (the rollback path) is a NOTE. An unreadable hub is a NOTE (item 16 FAILs a dead hub).

The real lib runs under the callers' `set -euo pipefail`; values reach bash as arguments only.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"
VERIFY = REPO / "scripts" / "verify-strih.sh"


def _grade(tmp_path, state, tools=("aplay", "arecord")):
    f = tmp_path / "state.json"
    if state is None:
        f.unlink(missing_ok=True)
    else:
        f.write_text(state if isinstance(state, str) else json.dumps(state))
    script = ('set -euo pipefail\n. "$1"\n'
              'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              'strih_intercom_hub_alsa_leg_grade "http://192.0.2.1:8790/api/state"\necho END')
    # the grade also checks the box carries aplay + arecord: stub them on a PATH that holds only the
    # stubs and the basic tools the lib needs, so the result never depends on the test host
    bindir = tmp_path / "tools"
    bindir.mkdir(exist_ok=True)
    for t in tools:
        (bindir / t).write_text("#!/bin/sh\nexit 0\n")
        (bindir / t).chmod(0o755)
    for t in ("python3", "cat", "curl", "dirname", "sed", "grep", "awk", "tr", "id", "hostname"):
        for d in ("/usr/local/bin", "/usr/bin", "/bin"):
            src = Path(d) / t
            if src.exists() and not (bindir / t).exists():
                (bindir / t).symlink_to(src)
                break
    env = dict(os.environ, STRIH_HUB_ALSA_STATE_FILE=str(f), PATH=str(bindir))
    r = subprocess.run(["/bin/bash", "-c", script, "harness", str(LIB)], capture_output=True,
                       text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    lines = r.stdout.strip().splitlines()
    assert lines[-1] == "END", r.stdout + r.stderr
    return lines[:-1]


def _side(**kw):
    s = {"state": "ready", "format": "S16_LE", "hw_channels": 6, "running": True, "xruns": 0, "spawns": 1}
    s.update(kw)
    return s


def _cutters(alsa=None, **top):
    a = {"card": "M4", "pcm": "hw:CARD=M4,DEV=0", "present": True,
         "playback": _side(), "capture": _side(format="S32_LE", xruns=2),
         "alsa_period_frames": 256, "alsa_buffer_frames": 1024}
    a.update(alsa or {})
    p = {"name": "cutters", "role": "cutters", "adapter": "alsa", "alsa": a,
         "local_audio": {"tx_blocks": 4000, "rx_blocks": 4000, "pipe_depth_frames": 640}}
    p.update(top)
    return p


def _state(*parts):
    cam = {"name": "cam1", "role": "cambox", "adapter": "vban", "rx_packets": 10}
    return {"version": "v", "participants": [cam, *parts]}


def test_a_card_with_both_sides_moving_audio_passes_naming_the_card_and_xruns(tmp_path):
    rows = _grade(tmp_path, _state(_cutters()))
    assert len(rows) == 1 and rows[0].startswith("PASS"), rows
    for want in ("cutters", "hw:CARD=M4,DEV=0", "S16_LE", "S32_LE", "xruns 0", "xruns 2"):
        assert want in rows[0], (want, rows)


def test_a_card_that_is_not_on_the_box_fails(tmp_path):
    down = _side(state="card not present", format=None, running=False)
    rows = _grade(tmp_path, _state(_cutters({"present": False, "playback": down, "capture": down})))
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    assert "M4" in rows[0] and "not on the box" in rows[0], rows


def test_a_side_that_is_not_moving_audio_fails_and_names_the_tool(tmp_path):
    # a PCM another process holds: the card is there, aplay cannot open it
    rows = _grade(tmp_path, _state(_cutters({"playback": _side(running=False)})))
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    assert "aplay" in rows[0] and "playback" in rows[0], rows
    rows = _grade(tmp_path, _state(_cutters({"capture": _side(running=False, state="the card offers no "
                                                               "S16_LE/S32_LE/S24_3LE at this rate and channel count",
                                                               format=None)})))
    assert rows[0].startswith("FAIL") and "arecord" in rows[0] and "offers no" in rows[0], rows


def test_an_unused_side_is_not_graded(tmp_path):
    rows = _grade(tmp_path, _state(_cutters({"playback": None})))
    assert len(rows) == 1 and rows[0].startswith("PASS"), rows


def test_a_hub_without_the_facet_fails_as_a_binary_from_before_the_card_leg(tmp_path):
    p = _cutters()
    del p["alsa"]
    rows = _grade(tmp_path, _state(p))
    assert rows[0].startswith("FAIL") and "no alsa facet" in rows[0], rows


def test_the_pipewire_rollback_is_a_note(tmp_path):
    p = {"name": "cutters", "role": "cutters", "adapter": "pipewire", "local_audio": {"rx_blocks": 1}}
    rows = _grade(tmp_path, _state(p))
    assert len(rows) == 1 and rows[0].startswith("NOTE") and "pipewire" in rows[0], rows


def test_a_hub_with_no_card_leg_prints_nothing_and_an_unreadable_hub_is_a_note(tmp_path):
    assert _grade(tmp_path, _state()) == []
    rows = _grade(tmp_path, None)
    assert len(rows) == 1 and rows[0].startswith("NOTE"), rows
    rows = _grade(tmp_path, "not json")
    assert rows[0].startswith("FAIL") and "not JSON" in rows[0], rows


def test_missing_aplay_or_arecord_fails(tmp_path):
    rows = _grade(tmp_path, _state(_cutters()), tools=("aplay",))
    assert any(r.startswith("FAIL") and "arecord" in r for r in rows), rows


def test_verify_item_9_calls_the_grade_in_place_of_the_minifuse_note():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    item9 = v[v.index("# 9) program audio"):v.index("# 9b)")]
    assert item9.count('strih_intercom_hub_alsa_leg_grade "http://${WS_HOST}:8790/api/state"') == 1
    assert "talkback: MiniFuse" not in item9
    assert item9.index("strih_intercom_hub_hdmi_leg_grade") \
        < item9.index("strih_intercom_hub_alsa_leg_grade") < item9.index("strih_vban_leg_grade")
