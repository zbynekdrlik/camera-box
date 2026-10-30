"""issue 1474 item 4 -- the Windows-only planners refuse a linux-genlock box by class.

Since issue 1462 the stream box is the Linux stream-snv (obs-fleet row `linux-genlock`), restarted by
its `strih-obs.service` user unit. Before this change `launch-obs-genlock.sh --box stream`,
`obs-self-heal-install.sh --box stream` and `avsync-watchdog-install.sh` (install and `--retire`)
still printed Windows programs for it, and `scripts/lib/genlock-fleet-boxes.sh` still mapped it to
the `win-stream-snv` MCP. Each now refuses through the ONE class gate
`obs_fleet_refuse_linux_target` (rc 2 for the CLIs, nothing printed on stdout), and `fleet_box_mcp`
names a win-* MCP only for a windows-genlock row. On a fleet whose stream box is Windows (the
`OBS_FLEET` override) every planner prints its program exactly as before.
"""
import os
import pathlib
import subprocess
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCRIPTS = REPO / "scripts"
WIN_STREAM_FLEET = ("strih-snv|10.77.9.202|linux-genlock|always\n"
                    "stream-snv|10.77.9.204|windows-genlock|always\n"
                    "resolume-snv|resolume.lan|windows-genlock|traveling")


def _env(fleet=None):
    env = {k: v for k, v in os.environ.items() if k != "OBS_FLEET"}
    if fleet is not None:
        env["OBS_FLEET"] = fleet
    return env


def _run(script, *args, fleet=None):
    return subprocess.run(["bash", str(SCRIPTS / script), *args], capture_output=True, text=True,
                          cwd=REPO, env=_env(fleet), timeout=120)


PLANNERS = [
    ("launch-obs-genlock.sh", ("--box", "stream")),
    ("launch-obs-genlock.sh", ("--box", "stream-snv", "--force")),
    ("obs-self-heal-install.sh", ("--box", "stream")),
    ("avsync-watchdog-install.sh", ()),
    ("avsync-watchdog-install.sh", ("--retire",)),
]


@pytest.mark.parametrize("script,args", PLANNERS)
def test_a_planner_refuses_the_linux_stream_box_by_class(script, args):
    r = _run(script, *args)
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert r.stdout == "", r.stdout
    assert f"ERROR: {script} is Windows-only" in r.stderr, r.stderr
    assert "linux-genlock fleet box" in r.stderr, r.stderr


@pytest.mark.parametrize("script,args", PLANNERS)
def test_a_planner_still_prints_its_program_for_a_windows_stream_box(script, args):
    r = _run(script, *args, fleet=WIN_STREAM_FLEET)
    assert r.returncode == 0, r.stderr
    assert "win-stream-snv" in r.stdout, r.stdout
    assert "Windows-only" not in r.stderr, r.stderr


def test_the_resolume_launch_plan_is_unchanged_on_the_real_fleet():
    r = _run("launch-obs-genlock.sh", "--box", "resolume")
    assert r.returncode == 0, r.stderr
    assert "box=resolume (win-resolume, resolume.lan)" in r.stdout, r.stdout


def _lib(body, fleet=None):
    return subprocess.run(["bash", "-c", '. "$1"; ' + body, "harness",
                           str(SCRIPTS / "lib" / "genlock-fleet-boxes.sh")],
                          capture_output=True, text=True, env=_env(fleet), timeout=60)


def test_fleet_box_mcp_names_no_win_mcp_for_a_linux_row():
    r = _lib("fleet_box_mcp stream-snv; echo rc=$?; fleet_box_mcp stream; echo rc=$?")
    assert r.stdout.splitlines() == ["rc=2", "rc=2"], r.stdout


def test_fleet_box_mcp_keeps_the_windows_rows():
    r = _lib("fleet_box_mcp stream-snv; fleet_box_mcp resolume-snv", fleet=WIN_STREAM_FLEET)
    assert r.stdout.splitlines() == ["win-stream-snv", "win-resolume"], r.stdout
    real = _lib("fleet_box_mcp resolume-snv")
    assert real.stdout.splitlines() == ["win-resolume"], real.stdout


def test_the_deploy_execute_never_prints_a_windows_program_for_the_linux_stream_box(tmp_path, monkeypatch):
    # The execute arm printed the Windows paste-program for EVERY stream-snv|resolume-snv in the
    # list once a Windows box was requested, even when stream-snv had just taken the Linux arm.
    sys.path.insert(0, str(HERE))
    import test_deploy_genlock_fleet_pp_1426 as pp  # noqa: E402
    stub = '[ "$h" = 10.77.9.202 ] && h=strih-snv;'
    assert pp.SSH.count(stub) == 1
    # the stub box at 10.77.9.204 answers `hostname` as stream-snv, as the real one does
    monkeypatch.setattr(pp, "SSH", pp.SSH.replace(stub, stub + ' [ "$h" = 10.77.9.204 ] && h=stream-snv;'))
    r, calls, _log = pp._execute(tmp_path, "stream-snv,resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr + calls
    assert "newlevel@10.77.9.204" in calls, calls
    out = r.stdout + r.stderr
    assert "FLEET PLAN: box=resolume-snv" in out, out + calls
    assert "FLEET PLAN: box=stream-snv" not in out, out
    assert "win-stream-snv" not in out, out
