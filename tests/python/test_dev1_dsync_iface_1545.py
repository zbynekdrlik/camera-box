"""issue 1545 -- dev1's dsync NIC is a declared fleet fact, installed as a drop-in, and graded.

20.11.2026: dev1's dsync ran NTP-only. Its unit (written at the dsync 1.8.0 install) has no
`--interface`, so dsync 1.35.16 picked the link-DOWN enp39s0 instead of the rig NIC enp38s0. It was
fixed by hand with the drop-in `dsync.service.d/zz-camera-box-interface.conf`; nothing in the repo
declared the NIC, so a reinstall would lose it silently.

What this pins:
  * the fact: `DANTESYNC_FLEET_PTP_NICS` (`dev1=enp38s0`), read by `dantesync_fleet_ptp_nic NAME`
    and its python twin `dantesync_fleet.ptp_nic`, both agreeing; a malformed entry fails by name;
  * `dsync_local_iface_dropin_text NIC` renders the drop-in (ExecStart reset + `--interface NIC`);
  * `dsync_local_iface_install NIC` writes it only when it differs, then daemon-reload + restart;
    unchanged = no write, no restart; a failed write never restarts;
  * `dsync_local_iface_check NIC` grades /status through dsync_iface_grade: exit 0 ok, 1 wrong or
    missing --interface, 2 unreadable;
  * `scripts/dev1-dsync-iface.sh` takes the local node + its declared NIC from the fleet;
  * every bash call runs under the callers' `set -euo pipefail`; no test reads the live :8898.
"""
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
FLEET_LIB = REPO / "scripts" / "lib" / "dantesync-fleet.sh"
IFACE_LIB = REPO / "scripts" / "lib" / "dsync-iface.sh"
LOCAL_LIB = REPO / "scripts" / "lib" / "dsync-local-iface.sh"
CLI = REPO / "scripts" / "dev1-dsync-iface.sh"

_spec = importlib.util.spec_from_file_location("dantesync_fleet", REPO / "scripts" / "dantesync_fleet.py")
fleet = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fleet)

DROPIN = """\
# camera-box issue 1545: dsync joins PTP on the NIC the dantesync fleet declares for this node
# (DANTESYNC_FLEET_PTP_NICS in scripts/lib/dantesync-fleet.sh), never a NIC it picks by itself.
# Written by scripts/dev1-dsync-iface.sh --apply.
[Service]
ExecStart=
ExecStart=/usr/local/bin/dsync --interface enp38s0
"""


def _env(extra=None, drop=("DANTESYNC_FLEET_PTP_NICS",)):
    env = {k: v for k, v in os.environ.items() if k not in drop}
    env.update(extra or {})
    return env


def _bash(script, *args, env=None):
    """Run SCRIPT under the callers' set -euo pipefail; values only as positional arguments."""
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *args],
                          capture_output=True, text=True, cwd=str(REPO), timeout=60, env=_env(env))


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


def _stubs(tmp_path, status=None, curl_fails=False):
    """A stub bin dir: `curl` answers /status with STATUS (a dict) or fails, `sudo` logs + runs the
    rest, `systemctl` logs. Returns (bindir, log)."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "calls.log"
    body = tmp_path / "status.json"
    body.write_text(json.dumps(status) if status is not None else "")
    if curl_fails:
        _exe(bindir / "curl", "#!/usr/bin/env bash\nexit 7\n")
    else:
        _exe(bindir / "curl", f'#!/usr/bin/env bash\necho "curl $*" >>"{log}"\ncat "{body}"\n')
    _exe(bindir / "sudo", f'#!/usr/bin/env bash\necho "sudo $*" >>"{log}"\n'
                          '[ "${1:-}" = -n ] && shift\nexec "$@"\n')
    _exe(bindir / "systemctl", f'#!/usr/bin/env bash\necho "systemctl $*" >>"{log}"\n')
    return bindir, log


def _calls(log):
    return log.read_text().splitlines() if log.exists() else []


# --- the fleet fact ------------------------------------------------------------------------------
def test_dev1_declares_enp38s0_in_the_fleet_facts():
    r = _bash('. "$1"\ndantesync_fleet_ptp_nic dev1', str(FLEET_LIB))
    assert (r.returncode, r.stdout) == (0, "enp38s0\n"), r.stderr
    assert fleet.ptp_nic("dev1") == "enp38s0"


def test_a_node_with_no_declared_nic_is_rc_1_and_none():
    r = _bash('. "$1"\ndantesync_fleet_ptp_nic cam2', str(FLEET_LIB))
    assert (r.returncode, r.stdout) == (1, ""), r.stderr
    assert fleet.ptp_nic("cam2") is None


@pytest.mark.parametrize("value,name,nic", [
    ("dev1=enp38s0 strih-snv=enp5s0", "strih-snv", "enp5s0"),
    ("strih-snv=enp5s0 dev1=eno1", "dev1", "eno1"),
    ("", "dev1", None),
])
def test_bash_and_python_read_the_same_table(value, name, nic, monkeypatch):
    r = _bash('. "$1"\ndantesync_fleet_ptp_nic "$2"', str(FLEET_LIB), name,
              env={"DANTESYNC_FLEET_PTP_NICS": value})
    monkeypatch.setenv("DANTESYNC_FLEET_PTP_NICS", value)
    if value == "":
        # `${VAR:-default}`: an EMPTY value is the default in bash and in the python twin alike
        assert r.stdout == "enp38s0\n" and fleet.ptp_nic(name) == "enp38s0"
        return
    assert (r.returncode, r.stdout) == (0, nic + "\n"), r.stderr
    assert fleet.ptp_nic(name) == nic


@pytest.mark.parametrize("bad", ["dev1", "dev1=", "dev1=enp 38", "dev1=enp38s0;reboot",
                                 "dev1=abcdefghijklmnop", "dev1=٥٣", "dev1=-x"])
def test_a_malformed_entry_fails_by_name_in_both(bad, monkeypatch):
    r = _bash('. "$1"\ndantesync_fleet_ptp_nic dev1', str(FLEET_LIB),
              env={"DANTESYNC_FLEET_PTP_NICS": bad, "LC_ALL": "C.UTF-8"})
    assert r.returncode == 1 and r.stdout == ""
    assert "DANTESYNC_FLEET_PTP_NICS" in r.stderr, r.stderr
    monkeypatch.setenv("DANTESYNC_FLEET_PTP_NICS", bad)
    with pytest.raises(RuntimeError, match="DANTESYNC_FLEET_PTP_NICS"):
        fleet.ptp_nic("dev1")


def test_the_python_cli_prints_the_nic():
    r = subprocess.run([sys.executable, str(REPO / "scripts" / "dantesync_fleet.py"), "ptp-nic", "dev1"],
                       capture_output=True, text=True, env=_env())
    assert (r.returncode, r.stdout) == (0, "enp38s0\n"), r.stderr
    r = subprocess.run([sys.executable, str(REPO / "scripts" / "dantesync_fleet.py"), "ptp-nic", "cam2"],
                       capture_output=True, text=True, env=_env())
    assert (r.returncode, r.stdout) == (1, ""), r.stderr


def test_the_row_table_is_unchanged_seven_fields():
    # The fact lives beside the table, never as an 8th column every row parser would have to learn.
    r = _bash('. "$1"\ndantesync_fleet_rows | grep "^dev1|"', str(FLEET_LIB))
    assert r.stdout == "dev1|local|linux|video|local|-|\n", r.stderr


# --- the drop-in render --------------------------------------------------------------------------
def test_the_dropin_resets_execstart_and_names_the_nic():
    r = _bash('. "$1"\ndsync_local_iface_dropin_text enp38s0', str(LOCAL_LIB))
    assert r.returncode == 0, r.stderr
    assert r.stdout == DROPIN


@pytest.mark.parametrize("bad", ["", "enp38s0 --ntp-master x", "enp38s0,enp39s0", "a$(id)"])
def test_the_render_refuses_a_bad_nic(bad):
    r = _bash('. "$1"\ndsync_local_iface_dropin_text "$2"', str(LOCAL_LIB), bad)
    assert r.returncode == 1 and r.stdout == ""
    assert "dsync_local_iface" in r.stderr


# --- the install ---------------------------------------------------------------------------------
def _install(tmp_path, nic="enp38s0", pre=None, extra=None, status="locked"):
    # the live dsync reads LOCKED on the declared NIC unless a test says otherwise: an UNCHANGED
    # install re-checks /status and restarts only a dsync that is not running the file (review 1)
    bindir, log = _stubs(tmp_path, status=LOCKED_OK if status == "locked" else status,
                         curl_fails=status is None)
    dropin = tmp_path / "etc" / "dsync.service.d" / "zz-camera-box-interface.conf"
    if pre is not None:
        dropin.parent.mkdir(parents=True)
        dropin.write_text(pre)
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "DSYNC_LOCAL_IFACE_DROPIN": str(dropin),
           "TMPDIR": str(tmp_path)}
    env.update(extra or {})
    r = _bash('. "$1"\ndsync_local_iface_install "$2"\necho AFTER', str(LOCAL_LIB), nic, env=env)
    return r, dropin, _calls(log)


def test_a_missing_dropin_is_written_then_dsync_restarted(tmp_path):
    r, dropin, calls = _install(tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.endswith("AFTER\n")
    assert dropin.read_text() == DROPIN
    sysctl = [c for c in calls if c.startswith("systemctl ")]
    assert sysctl == ["systemctl daemon-reload", "systemctl restart dsync"], calls
    # every root step goes through sudo -n (never an interactive prompt on an unattended run)
    assert all(c.startswith("sudo -n ") for c in calls if c.startswith("sudo ")), calls
    assert any(c.startswith("sudo -n systemctl restart dsync") for c in calls), calls
    assert "WROTE" in r.stdout


def test_an_identical_dropin_is_never_rewritten_nor_restarted(tmp_path):
    r, dropin, calls = _install(tmp_path, pre=DROPIN)
    assert r.returncode == 0, r.stderr
    assert dropin.read_text() == DROPIN
    assert calls == [], calls
    assert "UNCHANGED" in r.stdout and r.stdout.endswith("AFTER\n")


def test_a_differing_dropin_is_replaced_and_dsync_restarted_once(tmp_path):
    old = "[Service]\nExecStart=\nExecStart=/usr/local/bin/dsync --interface enp39s0\n"
    r, dropin, calls = _install(tmp_path, pre=old)
    assert r.returncode == 0, r.stderr
    assert dropin.read_text() == DROPIN
    assert [c for c in calls if c.startswith("systemctl ")] == ["systemctl daemon-reload",
                                                               "systemctl restart dsync"]
    assert not list(dropin.parent.glob("*.new")), "no temp file left beside the drop-in"


def test_a_comment_only_difference_is_rewritten_without_a_restart(tmp_path):
    # the 20.11 hand drop-in carries other comment text than the rendered one; the first --apply on
    # dev1 must not cost dsync's ~95 s cold lock for a comment
    hand = ("[Service]\n# camera-box 20.11.2026: bind PTP to the rig NIC (hand fix)\nExecStart=\n"
            "ExecStart=/usr/local/bin/dsync --interface enp38s0\n")
    r, dropin, calls = _install(tmp_path, pre=hand)
    assert r.returncode == 0, r.stderr
    assert dropin.read_text() == DROPIN
    sysctl = [c for c in calls if c.startswith("systemctl ")]
    assert sysctl == ["systemctl daemon-reload"], calls
    assert "not restarted" in r.stdout


def test_a_restart_that_failed_last_time_is_retried_on_the_next_apply(tmp_path):
    # run 1 wrote the file but the restart failed; dsync still runs WITHOUT --interface. Run 2 finds
    # the file identical and must still restart, because /status shows dsync is not on it.
    stale = {"is_locked": False, "ptp_interface_configured": None, "ptp_interface": "enp39s0"}
    r, dropin, calls = _install(tmp_path, pre=DROPIN, status=stale)
    assert r.returncode == 0, r.stderr
    assert [c for c in calls if c.startswith("systemctl ")] == ["systemctl daemon-reload",
                                                               "systemctl restart dsync"], calls
    assert "UNCHANGED" in r.stdout and "restarted" in r.stdout


def test_an_unchanged_file_with_a_dead_dsync_is_restarted(tmp_path):
    r, _dropin, calls = _install(tmp_path, pre=DROPIN, status=None)
    assert r.returncode == 0, r.stderr
    assert "systemctl restart dsync" in calls, calls


def test_an_unchanged_file_whose_dsync_joined_elsewhere_is_not_restarted(tmp_path):
    # the unit IS on the declared NIC; a restart cannot fix a link that is down
    joined = {"is_locked": False, "ptp_interface_configured": "enp38s0", "ptp_interface": "enp39s0"}
    r, _dropin, calls = _install(tmp_path, pre=DROPIN, status=joined)
    assert r.returncode == 0, r.stderr
    assert calls == [] or not any(c.startswith("systemctl") for c in calls), calls


def test_a_failed_write_never_restarts_dsync(tmp_path):
    # the drop-in dir is a FILE: the install cannot create it
    bindir, log = _stubs(tmp_path)
    blocker = tmp_path / "etc"
    blocker.write_text("not a dir")
    dropin = blocker / "dsync.service.d" / "zz.conf"
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "DSYNC_LOCAL_IFACE_DROPIN": str(dropin),
           "TMPDIR": str(tmp_path)}
    r = _bash('. "$1"\nif dsync_local_iface_install "$2"; then echo RC=0; else echo RC=$?; fi',
              str(LOCAL_LIB), "enp38s0", env=env)
    assert "RC=1" in r.stdout, (r.stdout, r.stderr)
    assert not any(c.startswith("systemctl ") for c in _calls(log)), _calls(log)
    assert "dsync_local_iface_install" in r.stderr


def test_a_bad_nic_installs_nothing(tmp_path):
    bindir, log = _stubs(tmp_path)
    dropin = tmp_path / "zz.conf"
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "DSYNC_LOCAL_IFACE_DROPIN": str(dropin)}
    r = _bash('. "$1"\nif dsync_local_iface_install "$2"; then echo RC=0; else echo RC=$?; fi',
              str(LOCAL_LIB), "enp38s0 x", env=env)
    assert "RC=1" in r.stdout
    assert not dropin.exists() and _calls(log) == []


# --- the grade -----------------------------------------------------------------------------------
LOCKED_OK = {"is_locked": True, "ptp_interface_configured": "enp38s0", "ptp_interface": "enp38s0"}


@pytest.mark.parametrize("status,curl_fails,rc,verdict", [
    (LOCKED_OK, False, 0, "ok:enp38s0"),
    ({"is_locked": False, "ptp_interface_configured": None, "ptp_interface": "enp39s0"}, False, 1,
     "not-configured"),
    ({"is_locked": True, "ptp_interface_configured": "enp39s0", "ptp_interface": "enp39s0"}, False, 1,
     "configured-elsewhere:enp39s0:enp38s0"),
    ({"is_locked": False, "ptp_interface_configured": "enp38s0", "ptp_interface": "enp39s0"}, False, 1,
     "joined-elsewhere:enp39s0:enp38s0"),
    (None, True, 2, "unreadable"),
])
def test_the_check_grades_status_through_the_one_verdict(tmp_path, status, curl_fails, rc, verdict):
    bindir, log = _stubs(tmp_path, status=status, curl_fails=curl_fails)
    env = {"PATH": f"{bindir}:{os.environ['PATH']}"}
    r = _bash('. "$1"\n. "$2"\nif dsync_local_iface_check dev1 "$3"; then echo RC=0; else echo RC=$?; fi',
              str(IFACE_LIB), str(LOCAL_LIB), "enp38s0", env=env)
    assert f"RC={rc}" in r.stdout, (r.stdout, r.stderr)
    assert f"node=dev1 declared=enp38s0 verdict={verdict}" in r.stdout, r.stdout
    if verdict.startswith("joined-elsewhere"):
        # the unit is right: the fix is the link (or the lock time), never --apply
        assert "--apply" not in r.stdout and "link" in r.stdout
    elif rc == 1:
        assert "scripts/dev1-dsync-iface.sh --apply" in r.stdout
    # read-only: the check never writes or restarts anything
    assert not any(c.startswith(("sudo", "systemctl")) for c in _calls(log)), _calls(log)


def test_the_check_reuses_dsync_iface_grade_never_a_copy():
    code = "\n".join(ln for ln in LOCAL_LIB.read_text().splitlines() if not ln.lstrip().startswith("#"))
    assert "dsync_iface_grade" in code
    assert "ptp_interface_configured" not in code, "the /status fields are read by dsync-iface.sh only"
    assert "curl" not in code and "8898" not in code


# --- the CLI -------------------------------------------------------------------------------------
def _cli(tmp_path, *args, status=LOCKED_OK, extra=None, pre=None):
    bindir, log = _stubs(tmp_path, status=status)
    dropin = tmp_path / "d" / "zz.conf"
    if pre is not None:
        dropin.parent.mkdir(parents=True)
        dropin.write_text(pre)
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "DSYNC_LOCAL_IFACE_DROPIN": str(dropin),
           "TMPDIR": str(tmp_path)}
    env.update(extra or {})
    r = subprocess.run(["bash", str(CLI), *args], capture_output=True, text=True, timeout=60,
                       env=_env(env))
    return r, dropin, _calls(log)


def test_the_cli_checks_by_default_with_the_declared_nic(tmp_path):
    r, dropin, calls = _cli(tmp_path)
    assert r.returncode == 0, r.stderr
    assert "node=dev1 declared=enp38s0 verdict=ok:enp38s0" in r.stdout
    assert not dropin.exists()
    assert not any(c.startswith(("sudo", "systemctl")) for c in calls)


def test_the_cli_check_names_a_wrong_nic(tmp_path):
    r, _d, _c = _cli(tmp_path, "--check", status={"is_locked": False, "ptp_interface_configured": None,
                                                  "ptp_interface": "enp39s0"})
    assert r.returncode == 1
    assert "verdict=not-configured" in r.stdout


def test_the_cli_apply_installs_the_declared_nic(tmp_path):
    r, dropin, calls = _cli(tmp_path, "--apply")
    assert r.returncode == 0, r.stderr
    assert dropin.read_text() == DROPIN
    assert "systemctl restart dsync" in calls


def test_the_cli_apply_is_idempotent(tmp_path):
    r, dropin, calls = _cli(tmp_path, "--apply", pre=DROPIN)
    assert r.returncode == 0, r.stderr
    assert calls == [] and "UNCHANGED" in r.stdout


def test_the_cli_refuses_a_node_with_no_declared_nic(tmp_path):
    r, dropin, calls = _cli(tmp_path, "--apply", extra={"DANTESYNC_FLEET_PTP_NICS": "strih-snv=enp5s0"})
    assert r.returncode == 2
    assert "dev1" in r.stderr and "DANTESYNC_FLEET_PTP_NICS" in r.stderr
    assert not dropin.exists() and calls == []


def test_the_cli_refuses_two_local_rows(tmp_path):
    table = "dev1|local|linux|video|local|-|\ndev9|local|linux|video|local|-|"
    r, dropin, calls = _cli(tmp_path, "--apply", extra={"DANTESYNC_FLEET": table})
    assert r.returncode == 2
    assert "exactly one" in r.stderr and "dev1 dev9" in r.stderr
    assert not dropin.exists() and calls == []


def test_the_cli_rejects_an_unknown_argument(tmp_path):
    r, _d, calls = _cli(tmp_path, "--bogus")
    assert r.returncode == 2 and calls == []
