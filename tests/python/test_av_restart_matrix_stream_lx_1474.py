"""issue 1474 -- the restart matrix's stream-obs kind on the LINUX stream box stream-snv.

The main's decision (relayed in the lane dispatch, recorded on issue 1474): on a linux-genlock stream
box the stream-obs kind is AUTOMATIC -- the same blocking `strih-obs.service` restart the matrix
already uses for strih-snv (mv_reverify_obs_restart_linux_cmd) over plain ssh with the stream
credentials -- and the bounded healthy wait stays the Development program scene. No supervisor step,
no confirm file. Cleanup leaves the unit running (`is-active || start`) and, when the OBS still does
not answer, names the Linux restart, never the Windows planner (which refuses a Linux box, exit 2).
A Windows stream box keeps the supervisor step byte for byte: the whole
test_av_restart_matrix_orchestrator_1367.py suite runs on a Windows OBS_FLEET override.

The rig is that suite's fully faked one (fake soak window, OBS, sshpass, curl; a tmp lease). Tier-0:
no rig, no cargo, no network.
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from test_av_restart_matrix_orchestrator_1367 import (  # noqa: E402,F401  (rig is a fixture)
    REPO, STREAM, _events, _matrix, _steps, _windows, rig)

STREAM_BOX = os.path.join(REPO, "scripts", "lib", "stream-box.sh")
LINUX_RESTART = "systemctl --user restart strih-obs.service"


def _linux(env, **extra):
    """The default obs-fleet list: stream-snv (10.77.9.204) is linux-genlock."""
    e = {k: v for k, v in env.items() if k != "OBS_FLEET"}
    # a supervisor wait, should one ever happen on Linux, must not hang the suite for 30 min
    e["AV_MATRIX_SUPERVISOR_TIMEOUT_S"] = "2"
    e.update(extra)
    return e


def _stream_ssh(p):
    return [text for k, (host, text) in ((k, d) for k, d in _events(p["log"]) if k == "ssh")
            if host == STREAM]


def test_a_linux_stream_obs_restart_is_automatic_and_graded(rig):
    env, p = rig
    r = _matrix(_linux(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1",
                       AV_MATRIX_SUPERVISOR_TIMEOUT_S="2"), "--run")
    assert r.returncode == 0, r.stdout + r.stderr
    sent = _stream_ssh(p)
    restarts = [t for t in sent if LINUX_RESTART in t]
    assert len(restarts) == 1, sent
    # the ONE existing headless restart: the unit-installed guard first, then the blocking restart
    assert "list-unit-files" in restarts[0] and "--no-block" not in restarts[0], restarts[0]
    assert "powershell" not in "".join(sent).lower(), "no PowerShell is sent to the Linux stream box"
    assert not list(p["run"].glob("supervisor-step-stream-obs-*")), "no supervisor step on Linux"
    assert "SUPERVISOR STEP" not in r.stdout and "win-stream-snv" not in r.stdout
    steps = _steps(p["run"])
    assert steps[-1]["kind"] == "stream-obs" and steps[-1]["outcome"] == "measured", steps
    assert len(_windows(p["log"])) == 2


def test_a_linux_stream_relaunch_on_another_scene_fails_and_is_never_switched_back(rig):
    env, p = rig
    r = _matrix(_linux(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1",
                       FAKE_STREAM_RELAUNCH_SCENE="PRO", AV_MATRIX_HEALTHY_TIMEOUT_S="20"), "--run")
    assert r.returncode == 1, r.stdout + r.stderr
    steps = _steps(p["run"])
    assert steps[-1]["outcome"] == "not_healthy" and "Development" in steps[-1]["note"], steps
    assert len(_windows(p["log"])) == 1, "no window, no settle on a wrong program scene"
    assert not [d for k, d in _events(p["log"]) if k == "obs" and d[:1] == ["switch"]]


def test_a_linux_stream_box_without_the_unit_is_not_performed(rig):
    env, p = rig
    r = _matrix(_linux(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1",
                       FAKE_STRIH_NO_UNIT="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _steps(p["run"])[-1]["outcome"] == "not_performed"
    # not performed because the unit guard refused, never because a supervisor never confirmed
    assert any(LINUX_RESTART in t for t in _stream_ssh(p)), _stream_ssh(p)
    assert "not installed" in _steps(p["run"])[-1]["note"], _steps(p["run"])[-1]


def test_cleanup_on_a_linux_stream_box_starts_the_unit_and_names_the_linux_restart(rig):
    env, p = rig
    r = _matrix(_linux(env, AV_MATRIX_KINDS="stream-obs", AV_MATRIX_REPEATS="1",
                       FAKE_STREAM_DEAD_AT_CLEANUP="1"), "--run")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    sent = _stream_ssh(p)
    assert any("start strih-obs.service" in t and "is-active" in t for t in sent), sent
    warn = [ln for ln in out.splitlines() if "does NOT answer" in ln]
    assert warn, out
    assert f"ssh u@{STREAM} '{LINUX_RESTART}'" in warn[0], warn
    assert "launch-obs-genlock" not in warn[0] and "win-stream-snv" not in warn[0], warn


def test_the_linux_plan_prints_the_automatic_restart_and_no_supervisor_step(rig):
    env, p = rig
    r = _matrix(_linux(env, AV_MATRIX_KINDS="strih-obs stream-obs"))
    assert r.returncode == 0, r.stderr
    assert p["log"].read_text() == "", "plan mode must not call OBS, ssh, curl or a window"
    out = r.stdout
    assert f"stream-obs: ssh <STREAM_USER>@{STREAM}:" in out, out
    assert out.count(LINUX_RESTART) >= 2, "strih-obs AND stream-obs restart the unit"
    for banned in ("SUPERVISOR STEP", "launch-obs-genlock", "win-stream-snv", "confirm-stream-obs"):
        assert banned not in out, banned
    # the lease's hold ceiling counts no supervisor wait on Linux
    win = _matrix(dict(env, AV_MATRIX_KINDS="strih-obs stream-obs")).stdout
    ceil = [int(re.search(r"hold ceiling (\d+) s", t).group(1)) for t in (out, win)]
    assert ceil[1] - ceil[0] == 3 * 1800, ceil


def _hint(*args, fleet=None):
    env = {k: v for k, v in os.environ.items() if k != "OBS_FLEET"}
    if fleet is not None:
        env["OBS_FLEET"] = fleet
    script = f'set -euo pipefail; . "{STREAM_BOX}"; stream_box_obs_restart_hint "$@"'
    return subprocess.run(["bash", "-c", script, "harness", *args], env=env,
                          capture_output=True, text=True, timeout=30)


def test_the_shared_restart_hint_follows_the_fleet_class():
    lx = _hint("newlevel", STREAM, "WINDOWS TEXT")
    assert lx.returncode == 0, lx.stderr
    assert lx.stdout == f"ssh newlevel@{STREAM} '{LINUX_RESTART}'", lx.stdout
    win_fleet = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"
    w = _hint("newlevel", STREAM, "bash x/launch-obs-genlock.sh --box stream --force", fleet=win_fleet)
    assert w.returncode == 0 and w.stdout == "bash x/launch-obs-genlock.sh --box stream --force", w
    # an address the fleet list does not know is no Linux box: the caller's Windows text
    assert _hint("newlevel", "192.0.2.7", "W").stdout == "W"
