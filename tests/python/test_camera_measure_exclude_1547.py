"""issue 1547 (owner 21.11.2026): cam3 has no picture, so it leaves the E2E measured set for now, but it
must stay asked: "nie ze zistim za mesiac ze vlastne sa to netestuje lebo si to vyradil".

`CAMERA_MEASURE_EXCLUDE` (space-separated camera names) drops cameras from the SNV DEFAULT measured set
only, prints a loud NOTE naming the ticket on every source, and is set in ONE place: the Full-path E2E
workflow, next to the ticket number. An explicit CAMERA_ACTIVE_SET is never filtered.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
CAMSET = REPO / "scripts" / "camera-set.sh"
WORKFLOW = REPO / ".github" / "workflows" / "full-path-e2e.yml"


def _src(**env):
    e = {k: v for k, v in os.environ.items() if k not in ("CAMERA_ACTIVE_SET", "CAMERA_TEST_CAMERA_BOX",
                                                           "CAMERA_MEASURE_EXCLUDE", "CAMERA_SET")}
    e.update(env)
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; printf "%s|%s|%s\\n" "$CAMERA_ACTIVE_SET" '
                        '"${CAMERA_TEST_CAMERA_BOX:-}" "$CAMERA_SET"', "h", str(CAMSET)],
                       capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip().split("|"), r.stderr


def test_without_an_exclusion_the_default_is_unchanged():
    (active, test_box, cset), err = _src()
    assert active == "cam2 cam3 cam5" and test_box == "cam5" and cset == active
    assert "1547" not in err


def test_an_excluded_camera_leaves_the_default_set_with_a_loud_note():
    (active, test_box, cset), err = _src(CAMERA_MEASURE_EXCLUDE="cam3")
    assert active == "cam2 cam5", active
    assert cset == "cam2 cam5"
    assert test_box == "cam5", "the test camera box stays when it is not excluded"
    assert "cam3" in err and "1547" in err and "NOTE" in err, err


def test_an_explicit_active_set_is_never_filtered():
    (active, _, _), _err = _src(CAMERA_ACTIVE_SET="cam2 cam3", CAMERA_MEASURE_EXCLUDE="cam3")
    assert active == "cam2 cam3"


def test_the_exclusion_lives_in_the_e2e_workflow_next_to_its_ticket():
    s = WORKFLOW.read_text()
    assert "CAMERA_MEASURE_EXCLUDE: cam3" in s
    i = s.index("CAMERA_MEASURE_EXCLUDE: cam3")
    assert "1547" in s[max(0, i - 400):i + 200], "the ticket is named next to the exclusion"
