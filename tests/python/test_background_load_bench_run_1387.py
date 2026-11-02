"""Issue 1387: `background-load-bench.sh --run` driven end to end with fakes, never the rig.

A fake sshpass + ssh on PATH answer the box and cam2 reads and stand in for the load; a fake
obs_phase2.py behind BACKGROUND_BENCH_OBS_DIR answers rig-busy-check; RIG_LEASE_DIR is a temp dir.
"""
import json
import os
import site
import subprocess
import sys
from pathlib import Path

import pytest

BENCH = Path(__file__).resolve().parents[2] / "scripts" / "background-load-bench.sh"

FAKE_SSHPASS = """#!/bin/bash
[ "$1" = -e ] && [ -n "${SSHPASS:-}" ] || exit 97
shift
exec "$@"
"""

FAKE_SSH = """#!@PY@
import os, sys, time
cmd = sys.argv[-1]
d = os.environ["FAKE_DIR"]
with open(os.path.join(d, "calls"), "a") as f:
    f.write(cmd.replace("\\n", " | ") + "\\n")
def state(name, default=""):
    p = os.path.join(d, name)
    return open(p).read() if os.path.exists(p) else default
if "RIG_MODE_PROBE_OK" in cmd:
    on = "1" if os.environ.get("FAKE_MODE", "TEST") == "TEST" else "0"
    print("RIG_MODE_PROBE_OK\\nPID_PRESENT|0\\nPID_ALIVE|0\\nSVC_ENABLED|%s\\nSVC_ACTIVE|%s" % (on, on))
elif "sudo -n -l" in cmd:
    sys.exit(0)
elif "stat -c %s" in cmd:
    print("/log/one.txt\\t1000")
elif "Cpus_allowed_list" in cmd:
    print(os.environ.get("FAKE_MASK", "2-15"))
elif "pgrep -x sha256sum" in cmd:
    print("16 16" if state("phase") == "inside" else "16 0")
elif "program-render-audit" in cmd:
    n = int(state("reads", "0"))
    open(os.path.join(d, "reads"), "w").write(str(n + 1))
    lagged = (0, 0, 7)[n]
    line = "program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=23.00 lagged=%d total=150\\n" % lagged
    sys.stdout.write(line * 6)
elif "bg-load-1387" in cmd and "pkill" in cmd:
    pass
elif "bg-load-1387" in cmd:
    inside = "obs-box-background-run" in cmd
    open(os.path.join(d, "phase"), "w").write("inside" if inside else "outside")
    if inside and os.environ.get("FAKE_INSIDE_FAILS"):
        sys.exit(1)
    time.sleep(float(os.environ.get("FAKE_LOAD_S", "2")))
    sys.exit(124)
else:
    sys.exit(255)
"""

FAKE_OBS = """#!@PY@
import json, os, sys
live = os.path.exists(os.path.join(os.environ["FAKE_DIR"], "phase")) and os.environ.get("FAKE_LIVE_IN_LOAD")
print(json.dumps({"busy": bool(live), "diagnostics": [
    {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
    {"host": "stream", "streaming": bool(live), "recording": bool(live)}]}))
"""


def _exe(path, text):
    path.write_text(text.replace("@PY@", sys.executable))
    path.chmod(0o755)


@pytest.fixture
def rig(tmp_path):
    fake, stub, obs = tmp_path / "fake", tmp_path / "bin", tmp_path / "obs"
    for p in (fake, stub, obs, tmp_path / "home"):
        p.mkdir()
    _exe(stub / "sshpass", FAKE_SSHPASS)
    _exe(stub / "ssh", FAKE_SSH)
    _exe(obs / "obs_phase2.py", FAKE_OBS)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BACKGROUND_BENCH_", "FAKE_", "RIG_LEASE_", "AV_SOAK_"))}
    env.update({
        "PATH": f"{stub}:{os.environ['PATH']}", "HOME": str(tmp_path / "home"), "PYTHONUSERBASE": site.getuserbase(),
        "FAKE_DIR": str(fake), "RIG_LEASE_DIR": str(tmp_path / "lease"), "BACKGROUND_BENCH_OBS_DIR": str(obs),
        "BACKGROUND_BENCH_HOST": "192.0.2.10", "STREAM_HOST": "192.0.2.11", "PAINTER_IP": "192.0.2.12",
        "STRIH_USER": "u", "STRIH_PW": "not-a-password", "CAM_PW": "not-a-password",
        "BACKGROUND_BENCH_MIN_PHASE_S": "2", "BACKGROUND_BENCH_POLL_S": "1", "BACKGROUND_BENCH_SETTLE_S": "0",
        "AV_SOAK_BROADCAST_RETRY_S": "0", "RIG_LEASE_MAX_HOLD_SECS": "600",
    })

    class Rig:
        run_dir = tmp_path / "run"

        @staticmethod
        def run(**extra):
            return subprocess.run(
                ["bash", str(BENCH), "--run", "--phase-s", "2", "--run-dir", str(Rig.run_dir)],
                env={**env, **extra}, capture_output=True, text=True, timeout=120,
            )

        @staticmethod
        def calls():
            p = fake / "calls"
            return p.read_text() if p.exists() else ""

        @staticmethod
        def lease_left():
            return [p for p in (tmp_path / "lease").glob("*") if p.is_dir()] if (tmp_path / "lease").exists() else []

    return Rig


def test_a_clean_run_passes_and_releases_the_lease(rig):
    r = rig.run()
    assert r.returncode == 0, r.stdout + r.stderr
    verdict = json.loads((rig.run_dir / "verdict.json").read_text())
    assert verdict["verdict"] == "PASS"
    assert (rig.run_dir / "inside.load").read_text().strip() == "124:16:16"
    assert (rig.run_dir / "outside.load").read_text().strip() == "124:16:0"
    calls = rig.calls()
    assert "sudo -n /usr/local/sbin/obs-box-background-run timeout 2" in calls
    assert calls.index("obs-box-background-run timeout") < calls.index("nice -n 19 ionice -c 3 timeout"), "inside first"
    assert "tail -c +1001" in calls
    assert not rig.lease_left()


def test_an_inside_load_that_failed_to_start_is_unknown_never_a_pass(rig):
    r = rig.run(FAKE_INSIDE_FAILS="1")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "inside: the load did not run" in (rig.run_dir / "verdict.json").read_text()


def test_no_running_obs_refuses_before_any_load(rig):
    r = rig.run(FAKE_MASK="")
    assert r.returncode == 4, "an empty mask = OBS not running: refused before any load"
    assert "bg-load-1387" not in rig.calls()


def test_outside_test_mode_refuses_before_any_load(rig):
    r = rig.run(FAKE_MODE="EVENT")
    assert r.returncode == 4
    assert "bg-load-1387" not in rig.calls()
    assert not rig.lease_left()


def test_a_broadcast_during_a_loaded_phase_stops_the_load_and_releases(rig):
    r = rig.run(FAKE_LIVE_IN_LOAD="1", FAKE_LOAD_S="20")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "pkill -f '[b]g-load-1387'" in rig.calls()
    assert not rig.lease_left()
