"""issue 1483 -- a Linux OBS box never starts OBS on a wall clock dantesync will still step.

strih-pp's RTC runs ~3 s off. At boot dantesync waits for the first PTP lock, then restores the
saved fleet date or runs the boot step (25.10.2026: OBS 09:06:17, wall step +3.75 s 09:07:05;
26.10.2026: OBS 13:20:09, boot step +2.9 s 13:20:44), so every NDI stamp of a running OBS jumped.
strih-obs-start.sh now waits, bounded, until the local dantesync /status reports the date settled.
"""
import json
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
START = REPO / "scripts" / "strih-obs-start.sh"


def _run(body, *args, env=None, timeout=30):
    e = {"PATH": "/usr/bin:/bin", "HOME": "/tmp"}
    e.update(env or {})
    script = 'set -euo pipefail\n. "$1"\nshift\n' + body
    return subprocess.run(["bash", "-c", script, "harness", str(START), *args],
                          capture_output=True, text=True, env=e, timeout=timeout)


def _verdict(status):
    return _run('strih_date_settled_verdict "$1"', json.dumps(status)).stdout.strip()


SETTLED_MASTER = {"date_authority": "master", "date_step_pending_ns": None, "date_step_due_in_ms": None}


def test_a_master_or_follower_on_the_fleet_line_is_settled():
    assert _verdict(SETTLED_MASTER) == "settled"
    assert _verdict({"date_authority": "follower", "date_step_pending_ns": None}) == "settled"
    assert _verdict({"date_authority": "holding", "date_step_pending_ns": None}) == "settled"


def test_a_future_scheduled_step_is_settled_an_overdue_one_is_not():
    # the nightly step is announced ahead (positive due time); a master off the fleet line publishes
    # the way back as an OVERDUE step (negative due time) until it re-aligns (25.10.2026 09:07:01-05).
    assert _verdict({"date_authority": "follower", "date_step_pending_ns": 200000000,
                     "date_step_due_in_ms": 3600000}) == "settled"
    v = _verdict({"date_authority": "master", "date_step_pending_ns": -3528490000,
                  "date_step_due_in_ms": -1200})
    assert v.startswith("wait:") and "overdue" in v


def test_not_phase_locked_or_the_local_path_is_not_settled():
    for auth in ("", "local"):
        v = _verdict({"date_authority": auth, "date_step_pending_ns": None})
        assert v.startswith("wait:"), (auth, v)
    assert _verdict({}).startswith("wait:")


def test_an_unreadable_status_is_not_settled():
    assert _run('strih_date_settled_verdict ""').stdout.strip().startswith("wait:")
    assert _run('strih_date_settled_verdict "not json"').stdout.strip().startswith("wait:")


def _wait(tmp_path, status_text, budget="3"):
    st = tmp_path / "status.json"
    if status_text is not None:
        st.write_text(status_text)
    env = {"STRIH_DANTESYNC_STATUS_URL": st.as_uri(), "STRIH_OBS_DATE_WAIT_S": budget,
           "STRIH_OBS_DATE_POLL_S": "1"}
    t0 = time.monotonic()
    r = _run("strih_wait_for_settled_date", env=env)
    return r, time.monotonic() - t0


def test_the_wait_returns_at_once_on_a_settled_date(tmp_path):
    r, dt = _wait(tmp_path, json.dumps(SETTLED_MASTER))
    assert r.returncode == 0 and "date settled" in r.stdout and dt < 2


def test_the_wait_is_bounded_and_never_fails_the_launch(tmp_path):
    r, dt = _wait(tmp_path, json.dumps({"date_authority": "", "date_step_pending_ns": None}))
    assert r.returncode == 0, r.stderr
    assert "WARN issue 1483" in r.stdout and 3 <= dt < 8
    r, dt = _wait(tmp_path / "missing", None)
    assert r.returncode == 0 and "WARN issue 1483" in r.stdout and dt < 8


def test_the_launcher_waits_before_launching_obs():
    s = START.read_text()
    main = s[s.index('exec >>"$LOG" 2>&1'):]
    call = "\nstrih_wait_for_settled_date\n"
    assert main.count(call) == 1
    assert main.index('pgrep -x obs >/dev/null') < main.index(call) < main.index('echo "launching:')
