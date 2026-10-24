"""issue 1401 (design 6265012279): verify-strih item 16 grades the intercom hub's memory lock.

On 14.10.2026 stream-pp ran out of memory and the hub's SCHED_FIFO hub-mix thread stalled in page
faults (711 lost ticks in 6 s): the hub held no memory lock. The hub now locks its memory before it
starts hub-mix and publishes the outcome as `memory_locked` on /api/state. The grade reads that field
(a fixture file through the STRIH_HUB_MEMLOCK_STATE_FILE seam here): true PASSes, false FAILs naming
the unit grant, a hub that does not report it FAILs (a binary from before the lock), an unreadable hub
is a NOTE (item 16 already FAILs a dead hub where the role runs it), garbage is a FAIL.

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
              'strih_intercom_hub_memlock_grade "$2"\necho END')
    env = dict(os.environ, STRIH_HUB_MEMLOCK_STATE_FILE=str(f))
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB), "http://192.0.2.1:8790/api/state"],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "command not found" not in r.stderr, r.stderr
    lines = r.stdout.strip().splitlines()
    assert lines[-1] == "END", r.stdout
    return lines[:-1]


def _state(**extra):
    s = {"version": "v", "participants": [], "mix_thread_sched": "SCHED_FIFO 10"}
    s.update(extra)
    return s


def test_a_locked_hub_passes(tmp_path):
    rows = _grade(tmp_path, _state(memory_locked=True))
    assert len(rows) == 1 and rows[0].startswith("PASS"), rows
    assert "memory locked" in rows[0], rows


def test_an_unlocked_hub_fails_naming_the_unit_grant(tmp_path):
    rows = _grade(tmp_path, _state(memory_locked=False))
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    assert "LimitMEMLOCK=infinity" in rows[0], rows
    assert "journalctl -u intercom-hub" in rows[0], "it says where the errno is: %s" % rows


def test_a_hub_that_does_not_report_the_lock_fails_as_an_old_binary(tmp_path):
    rows = _grade(tmp_path, _state())
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    assert "memory_locked" in rows[0] and "intercom-hub" in rows[0], rows


def test_a_non_boolean_lock_value_never_passes(tmp_path):
    # only a JSON true is a lock: a string, a number or null is not
    for value in ("true", 1, None):
        rows = _grade(tmp_path, _state(memory_locked=value))
        assert len(rows) == 1 and rows[0].startswith("FAIL"), (value, rows)


def test_an_unreadable_hub_is_a_note_and_garbage_a_fail(tmp_path):
    rows = _grade(tmp_path, None)
    assert len(rows) == 1 and rows[0].startswith("NOTE"), rows
    rows = _grade(tmp_path, "{not json")
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows
    rows = _grade(tmp_path, "[1, 2]")
    assert len(rows) == 1 and rows[0].startswith("FAIL"), rows


def test_verify_strih_grades_the_lock_in_item_16_and_stays_in_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    call = 'strih_intercom_hub_memlock_grade "http://${WS_HOST}:8790/api/state"'
    assert v.count(call) == 1
    item = v[v.index("# 16) intercom hub unit"):v.index("# 17)")]
    assert call in item, "the memory-lock row belongs to the hub item"
    # graded inside the installed-unit branch, next to the credential grade
    assert item.index("if [ -f /etc/systemd/system/intercom-hub.service ]") < item.index(call)
    assert item.index(call) < item.index("elif WHY=\"$(obs_box_step_applies 13-hub-live)\"; then bad \"intercom-hub.service not installed")
