"""issue 1424 part 3: verify-strih grades the intercom hub's HDMI audio leg (the strih-pp vestibule).

The grade reads the hub's /api/state (a fixture file through the STRIH_HUB_HDMI_STATE_FILE seam here),
and for every `alsa_hdmi` participant PASSes a leg that is resolved to its hw PCM with aplay running
and blocks written, FAILs one that is silent and names why. A hub with no such participant prints no
row; an unreadable hub is a NOTE (item 16 already FAILs a dead hub on the strih role).

The real lib runs under the callers' `set -euo pipefail`; values reach bash as arguments only.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"
VERIFY = REPO / "scripts" / "verify-strih.sh"


def _grade(tmp_path, state):
    f = tmp_path / "state.json"
    if state is not None:
        f.write_text(state if isinstance(state, str) else json.dumps(state))
    script = ('set -euo pipefail\n. "$1"\n'
              'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              'strih_intercom_hub_hdmi_leg_grade "http://192.0.2.1:8790/api/state"\necho END')
    # the grade also checks the box carries aplay + amixer; stub both so the result never depends on
    # whether the test host has alsa-utils (the CI runner does not)
    tools = tmp_path / "tools"
    tools.mkdir(exist_ok=True)
    for t in ("aplay", "amixer"):
        (tools / t).write_text("#!/bin/sh\nexit 0\n")
        (tools / t).chmod(0o755)
    env = dict(os.environ, STRIH_HUB_HDMI_STATE_FILE=str(f), PATH=str(tools) + os.pathsep + os.environ["PATH"])
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB)], capture_output=True, text=True,
                       env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    lines = r.stdout.strip().splitlines()
    assert lines[-1] == "END", r.stdout
    return lines[:-1]


def _vestibule(**hdmi):
    h = {"connector": "HDMI-1", "resolved": True, "hw": "hw:0,7", "reason": "matched", "running": True,
         "resolves": 1, "xruns": 0}
    h.update(hdmi)
    return {"name": "vestibule", "role": "program_monitor", "adapter": "alsa_hdmi", "hdmi": h,
            "local_audio": {"tx_blocks": 4000, "pipe_depth_frames": 640, "pipe_refills": 0}}


def _state(*parts):
    lv1 = {"name": "lv1", "role": "program_ref", "adapter": "vban", "rx_packets": 10}
    return {"version": "v", "participants": [lv1, *parts]}


def test_a_resolved_running_leg_passes_naming_its_hw(tmp_path):
    rows = _grade(tmp_path, _state(_vestibule()))
    assert len(rows) == 1 and rows[0].startswith("PASS"), rows
    assert "vestibule" in rows[0] and "HDMI-1" in rows[0] and "hw:0,7" in rows[0], rows


def test_a_silent_leg_fails_and_names_why(tmp_path):
    rows = _grade(tmp_path, _state(_vestibule(resolved=False, hw=None, running=False,
                                              reason="no HDMI/DP PCM reports a monitor (no ELD)")))
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    assert "no ELD" in rows[0] and "HDMI-1" in rows[0], rows


def test_a_resolved_leg_whose_aplay_is_not_running_fails(tmp_path):
    # a busy PCM: the matcher found it, aplay could not open it
    rows = _grade(tmp_path, _state(_vestibule(running=False)))
    assert rows[0].startswith("FAIL") and "aplay" in rows[0], rows


def test_a_leg_that_wrote_nothing_fails(tmp_path):
    v = _vestibule()
    v["local_audio"]["tx_blocks"] = 0
    rows = _grade(tmp_path, _state(v))
    assert rows[0].startswith("FAIL"), rows


def test_a_hub_without_an_hdmi_leg_prints_no_row(tmp_path):
    assert _grade(tmp_path, _state()) == []


def test_an_unreadable_hub_is_a_note_and_garbage_a_fail(tmp_path):
    rows = _grade(tmp_path, None)
    assert len(rows) == 1 and rows[0].startswith("NOTE"), rows
    rows = _grade(tmp_path, "{not json")
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows


def test_verify_strih_grades_the_leg_and_stays_in_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    assert v.count('strih_intercom_hub_hdmi_leg_grade "http://${WS_HOST}:8790/api/state"') == 1
    # inside item 9 (program audio), after the program-audio verdict
    i = v.index('strih_intercom_hub_hdmi_leg_grade "http')
    assert v.index('AUDIO_VERDICT="$(strih_lx_program_audio_verdict') < i < v.index("# 9b) MiniFuse")


def test_a_box_without_aplay_or_amixer_fails_the_leg(tmp_path):
    # the leg runs aplay + amixer (alsa-utils, setup-strih step 4b); review finding: a box without them
    # must FAIL by name, not read as a silent leg
    import shutil
    bindir = tmp_path / "bin"
    bindir.mkdir()
    for tool in ("python3", "cat"):
        (bindir / tool).symlink_to(shutil.which(tool))
    f = tmp_path / "state.json"
    f.write_text(json.dumps(_state(_vestibule())))
    script = ('set -euo pipefail\n. "$1"\n'
              'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              'PATH="$2"\nstrih_intercom_hub_hdmi_leg_grade "http://192.0.2.1:8790/api/state"\necho END')
    env = dict(os.environ, STRIH_HUB_HDMI_STATE_FILE=str(f))
    r = subprocess.run(["/bin/bash", "-c", script, "harness", str(LIB), str(bindir)], capture_output=True,
                       text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert any(ln.startswith("FAIL") and "alsa-utils" in ln for ln in r.stdout.splitlines()), r.stdout
    assert r.stdout.strip().splitlines()[-1] == "END"
