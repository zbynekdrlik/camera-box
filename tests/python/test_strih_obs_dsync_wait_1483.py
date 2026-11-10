"""issue 1483 (25.10.2026 strih-pp, again on the 9.11.2026 16:59 cold start): strih-obs.service started OBS
at 16:59:39, then dsync's boot NTP step moved the wall clock back -2.207 s at 16:59:51 (the saved fleet
date offset was 27 h old, so it was not restored), under a running OBS.

Pinned here:
  * the pure decision `strih_dsync_date_settled JSON`: settled = `is_locked` true and no pending date
    step; anything else (unlocked, a pending step, unreadable JSON) is not settled;
  * `strih_wait_dsync_date BOUND`: polls dsync's own /status once a second, bounded, rc 0 settled /
    1 not settled after the bound / a non-number bound = 90;
  * the launcher runs the wait AFTER the rig-address wait and BEFORE the OBS launch, fail-open
    (a timeout is one named WARN and OBS still starts).

Every bash call runs under the callers' `set -euo pipefail`; values are passed as ARGUMENTS.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
START = REPO / "scripts" / "strih-obs-start.sh"

LOCKED = {"is_locked": True, "mode": "NANO", "date_authority": "master", "date_step_pending_ns": None}
BOOTING = {"is_locked": False, "mode": "HOLD", "date_authority": "master", "date_step_pending_ns": None}
PENDING = {"is_locked": True, "mode": "LOCK", "date_authority": "master", "date_step_pending_ns": -2207162000}


def _run(script, *args, env=None):
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *map(str, args)],
                          capture_output=True, text=True, cwd=str(REPO), env=e, timeout=60)


def _settled(text):
    return _run(f'. "{START}"\nset +e\nstrih_dsync_date_settled "$1"\necho "RC=$?"', text)


def _rc(r):
    """The harness's own exact `RC=` line (never a substring: RC=1 is inside RC=127)."""
    return [ln for ln in r.stdout.splitlines() if ln.startswith("RC=")][-1]


def test_a_locked_status_with_no_pending_step_is_settled():
    r = _settled(json.dumps(LOCKED))
    assert _rc(r) == "RC=0", r.stdout + r.stderr


def test_an_unlocked_status_is_not_settled_the_boot_step_comes_before_the_first_lock():
    r = _settled(json.dumps(BOOTING))
    assert _rc(r) == "RC=1", r.stdout + r.stderr


def test_a_pending_date_step_is_not_settled():
    r = _settled(json.dumps(PENDING))
    assert _rc(r) == "RC=1", r.stdout + r.stderr


def test_an_unreadable_status_is_not_settled():
    for text in ("", "not json", "[]", json.dumps({"is_locked": "true"})):
        r = _settled(text)
        assert _rc(r) == "RC=1", (text, r.stdout + r.stderr)


def _fake_tools(tmp_path, locked_after):
    """`curl` prints the BOOTING status until its Nth call, then LOCKED (-1 = never, -2 = no dsync:
    curl fails); `sleep` only logs."""
    bin_ = tmp_path / "bin"
    bin_.mkdir(parents=True, exist_ok=True)
    (tmp_path / "calls").write_text("0")
    (tmp_path / "booting.json").write_text(json.dumps(BOOTING))
    (tmp_path / "locked.json").write_text(json.dumps(LOCKED))
    (bin_ / "curl").write_text(
        "#!/bin/bash\n"
        f'c=$(cat "{tmp_path}/calls"); echo $((c + 1)) > "{tmp_path}/calls"\n'
        f'echo "$*" >> "{tmp_path}/curl_args"\n'
        f'[ {locked_after} -eq -2 ] && exit 7\n'
        f'if [ {locked_after} -ge 0 ] && [ "$c" -ge {locked_after} ]; then cat "{tmp_path}/locked.json";\n'
        f'else cat "{tmp_path}/booting.json"; fi\n')
    (bin_ / "sleep").write_text(f'#!/bin/bash\necho "$*" >> "{tmp_path}/slept"\n')
    for f in ("curl", "sleep"):
        (bin_ / f).chmod(0o755)
    return {"PATH": f"{bin_}:{os.environ['PATH']}"}


def _wait(tmp_path, bound, locked_after):
    return _run(f'. "{START}"\nset +e\nstrih_wait_dsync_date "$1"\necho "RC=$?"', bound,
                env=_fake_tools(tmp_path, locked_after))


def test_the_wait_returns_once_dsync_locks_and_names_how_long_it_took(tmp_path):
    r = _wait(tmp_path, 90, 3)
    assert _rc(r) == "RC=0", r.stdout + r.stderr
    assert "dsync date settled after 3 s" in r.stdout
    assert (tmp_path / "calls").read_text().strip() == "4"
    assert "127.0.0.1:8898/status" in (tmp_path / "curl_args").read_text()


def test_an_already_locked_dsync_costs_no_wait(tmp_path):
    r = _wait(tmp_path, 90, 0)
    assert _rc(r) == "RC=0" and "after 0 s" in r.stdout, r.stdout + r.stderr
    assert not (tmp_path / "slept").exists()


def test_the_wait_is_bounded_and_returns_1_when_dsync_never_settles(tmp_path):
    r = _wait(tmp_path, 5, -1)
    assert _rc(r) == "RC=1", r.stdout + r.stderr
    assert (tmp_path / "calls").read_text().strip() == "6"
    assert len((tmp_path / "slept").read_text().split()) == 5


def test_a_dsync_that_does_not_answer_is_bounded_too(tmp_path):
    r = _wait(tmp_path, 3, -2)
    assert _rc(r) == "RC=1", r.stdout + r.stderr


def test_a_non_number_bound_falls_back_to_90(tmp_path):
    r = _wait(tmp_path, "abc", -1)
    assert _rc(r) == "RC=1", r.stdout + r.stderr
    assert (tmp_path / "calls").read_text().strip() == "91"


def test_the_launcher_waits_for_dsync_after_the_rig_address_and_before_obs_fail_open():
    s = START.read_text()
    flow = s[s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]'):]
    rig = flow.index("strih_wait_rig_address ")
    ds = flow.index("strih_wait_dsync_date ")
    launch = flow.index('"$OBS_BIN"')
    assert rig < ds < launch
    block = flow[ds:launch]
    assert "WARN issue 1483" in block and "starting OBS anyway" in block
    assert "exit" not in block.split("WARN issue 1483")[0]
