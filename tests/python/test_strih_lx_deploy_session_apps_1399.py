"""Issue 1399 -- the strih-lx deploy's start step starts the session apps after OBS (main ROZHODNUTE
5978724027 point 2).

setup-strih.sh installs strih-browser-keeper.service + bkshading-panel-app.service ENABLE-ONLY (it never
starts anything on the production box), and the kiosk autostart starts them only at the next login. So
before this, the first deploy that installed them left both stopped until the next login, and verify-strih
items 37/38 (the deploy's own acceptance step) failed on them. The deploy's start step
(`strih_lx_remote_start_cmd`, `scripts/lib/strih-lx-deploy.sh`) now starts them right after
strih-obs.service, in the kiosk autostart's order.

What this pins, by RUNNING the emitted remote command under bash with stub `systemctl`/`id`:
  * the order: reset-failed + start strih-obs.service, THEN start the session apps;
  * the units started are exactly STRIH_SESSION_APP_UNITS (the ONE list setup-strih and verify-strih use);
  * the command's exit code is OBS's: a session app that does not start is a named WARNING with rc 0, and
    an OBS start failure keeps OBS's rc (the deploy exits 4 on it) while the apps are still tried;
  * the command stays ONE line and the --plan STEP 7 line prints the same command;
  * it carries none of the other deploy steps' distinguishing texts (the Rust exec test's ssh stub
    dispatches on them, first match wins).
setup-strih.sh stays enable-only: test_strih_session_apps_1399.py runs its install step and asserts the
systemctl calls it really made never start a unit (`_never_starts`).

Tier-0: bash + python stubs, no network, no box.
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FLEET = SCRIPTS / "deploy-genlock-fleet.sh"
UNITS = ["strih-browser-keeper.service", "bkshading-panel-app.service", "companion-satellite.service",
         "strih-satellite-watch.timer"]
STAGE = "/tmp/genlock-stage-abc123"


def _bash(body):
    """Source deploy-genlock-fleet.sh (its source-guard stops before main; it sources the strih-lx deploy
    lib) under the caller's set -euo pipefail, then run BODY."""
    script = 'set -euo pipefail\n. "%s"\n%s\n' % (FLEET, body)
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _start_cmd(stage=STAGE):
    return _bash('strih_lx_remote_start_cmd "%s"' % stage)


def test_the_units_started_are_the_one_session_app_list():
    units = _bash('printf "%s\\n" "${STRIH_SESSION_APP_UNITS[@]}"').split()
    assert units == UNITS


def test_the_start_command_is_one_line_obs_first_then_the_session_apps():
    cmd = _start_cmd()
    assert cmd.endswith("\n") and cmd.count("\n") == 1, "one remote line"
    obs = cmd.index("systemctl --user start strih-obs.service")
    apps = cmd.index("systemctl --user start %s" % " ".join(UNITS))
    assert cmd.index("touch '%s/obs-start.marker'" % STAGE) < cmd.index("reset-failed strih-obs.service") < obs < apps
    assert cmd.count("systemctl --user start ") == 2


def test_the_start_command_matches_no_other_deploy_step():
    # the Rust exec test's ssh stub dispatches on these texts in order; the start step must reach its
    # own `--user start` case, never an earlier one
    cmd = _start_cmd()
    for other in ("acceptance gate did not pass", "repo/scripts/verify-strih.sh", "pgrep -x setup-strih.sh",
                  "--local-sweep", "LEFTOVER", "setup-strih.rc", "run-setup.sh", ", then run verify-strih",
                  "setup-strih.log", "strih-obs-stop.sh", "GENLOCK_BUILD_SHA.txt"):
        assert other not in cmd, other


def test_the_plan_prints_the_same_start_command():
    plan = _bash('strih_lx_plan_steps /tmp/local-stage abc123 10.77.9.202 4242')
    line = [ln for ln in plan.splitlines() if ln.startswith("# STEP 7 (start): ")]
    assert line == ["# STEP 7 (start): " + _start_cmd("/tmp/genlock-stage-abc123").rstrip("\n")]


STUB = r'''#!%(py)s
import os, sys
with open(os.environ["STUB_LOG"], "a") as f:
    f.write("%(name)s " + " ".join(sys.argv[1:]) + "\n")
if "%(name)s" == "id":
    print("1000")
    sys.exit(0)
args = sys.argv[1:]
if args[:2] == ["--user", "start"]:
    if args[2:] == ["strih-obs.service"]:
        sys.exit(int(os.environ.get("OBS_RC", "0")))
    sys.exit(int(os.environ.get("APPS_RC", "0")))
sys.exit(0)
'''


def _run_start(tmp_path, obs_rc=0, apps_rc=0):
    stub = tmp_path / "bin"
    stub.mkdir()
    for name in ("systemctl", "id"):
        p = stub / name
        p.write_text(STUB % {"py": sys.executable, "name": name})
        p.chmod(0o755)
    os.symlink(shutil.which("touch"), stub / "touch")
    stage = tmp_path / "stage"
    stage.mkdir()
    log = tmp_path / "calls.log"
    r = subprocess.run(["/bin/bash", "-c", _start_cmd(str(stage))], capture_output=True, text=True, timeout=30,
                       env={"PATH": str(stub), "STUB_LOG": str(log), "OBS_RC": str(obs_rc), "APPS_RC": str(apps_rc)})
    calls = log.read_text().splitlines() if log.exists() else []
    return r, calls, stage


def test_running_it_starts_obs_then_every_app(tmp_path):
    r, calls, stage = _run_start(tmp_path)
    assert r.returncode == 0 and "WARNING" not in r.stderr
    assert calls == ["id -u", "systemctl --user reset-failed strih-obs.service",
                     "systemctl --user start strih-obs.service", "systemctl --user start " + " ".join(UNITS)]
    assert (stage / "obs-start.marker").exists()


def test_a_session_app_that_does_not_start_is_a_warning_not_a_failed_deploy(tmp_path):
    r, calls, _ = _run_start(tmp_path, apps_rc=5)
    assert r.returncode == 0, "the deploy's start rc is OBS's"
    assert ("WARNING: [strih-lx start] the session apps (%s) did not start -- verify-strih items 37-40 name why"
            % " ".join(UNITS)) in r.stderr


@pytest.mark.parametrize("obs_rc", [1, 5])
def test_an_obs_start_failure_keeps_obs_rc_and_still_tries_the_apps(tmp_path, obs_rc):
    r, calls, _ = _run_start(tmp_path, obs_rc=obs_rc)
    assert r.returncode == obs_rc
    assert calls[-1] == "systemctl --user start " + " ".join(UNITS)

