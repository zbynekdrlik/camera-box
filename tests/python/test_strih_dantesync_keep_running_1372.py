"""Issue 1372: setup-strih must never restart a running, unchanged dantesync.

WHY: strih-lx is the fleet's dantesync DATE MASTER in daily mode. Its fleet line drifts ~0.7 s a day
against NTP and is meant to be stepped only in the nightly 02:00Z window. setup-strih step 2 runs on
every strih-lx genlock deploy, and it used to rewrite the unit, remove /var/run/dantesync.lock and
`systemctl restart dantesync` unconditionally. On 29.9.2026 00:35Z that restart made the master
re-derive the date at once, the followers joined, the whole rig took a 0.67 s date step and the cg OBS
program audio stayed broken until a relaunch (finding 5881495922, design 5881503623 Approach 1).

What this pins:
  * the pure decision `strih_dantesync_restart_decision UNIT_CHANGED BINARY_CHANGED ACTIVE PRESENT`
    (scripts/lib/strih-provision.sh): restart | start | keep | absent, fail-closed on a bad input;
  * the byte-exact unit comparison + drop-in probe shared by setup-strih and verify-strih;
  * the setup-strih caller `strih_dantesync_install`, run against a temp root through its path seams
    with a fake `systemctl` on PATH that logs its argv: a second run on an unchanged unit issues no
    restart and removes no lock; a changed unit restarts exactly once; a stopped daemon is started and
    only then is the stale lock cleared.

Tier-0: bash + pytest only (no cargo, no root, no rig). The caller is run by SOURCING setup-strih.sh,
whose source-guard stops before the provisioning flow, so the tests exercise the real function.
"""
import os
import re
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"

KEEP_LINE = "dantesync.service: kept running (unit unchanged) -- no restart, the fleet date is untouched"


def _bash(script, body, env=None):
    """Source `script` in a fresh bash, then run `body`. Returns the CompletedProcess."""
    harness = 'set -uo pipefail\n. "$SCRIPT"\n' + body
    full_env = dict(os.environ, SCRIPT=str(script))
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=full_env,
                          cwd=str(REPO), timeout=60)


# --- the pure decision ------------------------------------------------------------------------

def _expected(unit_changed, binary_changed, active, present):
    if present == 0:
        return "absent"
    if active == 0:
        return "start"
    if unit_changed or binary_changed:
        return "restart"
    return "keep"


@pytest.mark.parametrize("unit_changed", [0, 1])
@pytest.mark.parametrize("binary_changed", [0, 1])
@pytest.mark.parametrize("active", [0, 1])
@pytest.mark.parametrize("present", [0, 1])
def test_restart_decision_table(unit_changed, binary_changed, active, present):
    r = _bash(LIB, 'strih_dantesync_restart_decision "$U" "$B" "$A" "$P"',
              env={"U": str(unit_changed), "B": str(binary_changed), "A": str(active), "P": str(present)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == _expected(unit_changed, binary_changed, active, present)


def test_restart_decision_named_cases():
    # the four tokens the caller acts on, spelled out
    for args, want in (("0 0 1 1", "keep"), ("1 0 1 1", "restart"), ("0 1 1 1", "restart"),
                       ("0 0 0 1", "start"), ("1 1 0 1", "start"), ("1 1 1 0", "absent"),
                       ("0 0 0 0", "absent")):
        r = _bash(LIB, "strih_dantesync_restart_decision " + args)
        assert r.returncode == 0, (args, r.stderr)
        assert r.stdout.strip() == want, (args, r.stdout)


@pytest.mark.parametrize("args", ["", "0 0 1", "0 0 1 2", "x 0 1 1", "0 0 yes 1", "'' 0 1 1", "0 0 1 1 1"])
def test_restart_decision_refuses_a_bad_input(args):
    # A caller that cannot read a state must never guess a restart of the fleet date master:
    # anything that is not exactly four 0/1 values prints nothing and returns non-zero.
    r = _bash(LIB, "strih_dantesync_restart_decision " + args)
    assert r.returncode != 0, (args, r.stdout)
    assert r.stdout.strip() == "", (args, r.stdout)


# --- the unit comparison + drop-in probe -----------------------------------------------------

def _unit_text(role="server", args=""):
    r = _bash(LIB, 'strih_dantesync_unit_text "$R" "$A"', env={"R": role, "A": args})
    assert r.returncode == 0, r.stderr
    return r.stdout  # the exact bytes setup-strih writes (the heredoc ends with ONE newline)


def test_unit_matches_is_byte_exact(tmp_path):
    text = _unit_text()
    unit = tmp_path / "dantesync.service"
    body = 'strih_dantesync_unit_matches "$(strih_dantesync_unit_text server "")" "$U"'
    env = {"U": str(unit)}
    assert _bash(LIB, body, env).returncode == 1, "a missing unit never matches"
    unit.write_text(text)
    assert _bash(LIB, body, env).returncode == 0, "the written unit matches"
    unit.write_text(text.rstrip("\n"))
    assert _bash(LIB, body, env).returncode == 1, "a missing trailing newline is a difference"
    unit.write_text(text + "\n")
    assert _bash(LIB, body, env).returncode == 1, "an extra trailing newline is a difference"
    unit.write_text(text.replace("RestartSec=5", "RestartSec=6"))
    assert _bash(LIB, body, env).returncode == 1, "a changed line is a difference"
    unit.write_text(_unit_text("client", "--ntp-server strih.lan"))
    assert _bash(LIB, body, env).returncode == 1, "the other role's unit is a difference"


def test_dropins_present_only_counts_conf_files(tmp_path):
    d = tmp_path / "dantesync.service.d"
    body = 'strih_dantesync_dropins_present "$D"'
    env = {"D": str(d)}
    assert _bash(LIB, body, env).returncode == 1, "no directory = no drop-in"
    d.mkdir()
    assert _bash(LIB, body, env).returncode == 1, "an empty directory = no drop-in"
    (d / "README").write_text("x")
    assert _bash(LIB, body, env).returncode == 1, "only *.conf files are drop-ins"
    (d / "10-ntp-master.conf").write_text("[Service]\n")
    assert _bash(LIB, body, env).returncode == 0, "a *.conf file is a drop-in"


# --- the setup-strih caller, against a temp root + a fake systemctl --------------------------

class Box:
    """A temp root with the dantesync paths, a fake systemctl on PATH and its argv log."""

    def __init__(self, tmp_path):
        self.root = tmp_path
        self.unit = tmp_path / "etc" / "systemd" / "system" / "dantesync.service"
        self.dropin = tmp_path / "etc" / "systemd" / "system" / "dantesync.service.d"
        self.lock = tmp_path / "run" / "dantesync.lock"
        self.bin = tmp_path / "usr" / "local" / "bin" / "dantesync"
        self.state = tmp_path / "systemctl.state"
        self.log = tmp_path / "systemctl.log"
        fake_dir = tmp_path / "fakebin"
        for p in (self.unit.parent, self.lock.parent, self.bin.parent, fake_dir):
            p.mkdir(parents=True, exist_ok=True)
        sc = fake_dir / "systemctl"
        sc.write_text(
            "#!/usr/bin/env bash\n"
            'printf "%s\\n" "$*" >> "$FAKE_SC_LOG"\n'
            'case "$1" in\n'
            '  is-active) [ "$(cat "$FAKE_SC_STATE" 2>/dev/null)" = active ] && exit 0; exit 3 ;;\n'
            '  start|restart) echo active > "$FAKE_SC_STATE" ;;\n'
            "esac\n"
            "exit 0\n")
        sc.chmod(0o755)
        self.path = "%s:%s" % (fake_dir, os.environ.get("PATH", "/usr/bin:/bin"))

    def with_binary(self):
        self.bin.write_text("#!/bin/sh\nexit 0\n")
        self.bin.chmod(0o755)
        return self

    def set_active(self, active):
        self.state.write_text("active\n" if active else "inactive\n")
        return self

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def reset_log(self):
        if self.log.exists():
            self.log.unlink()

    def run(self, role="server", args=""):
        env = {
            "PATH": self.path,
            "FAKE_SC_LOG": str(self.log),
            "FAKE_SC_STATE": str(self.state),
            "STRIH_DANTESYNC_UNIT": str(self.unit),
            "STRIH_DANTESYNC_DROPIN_DIR": str(self.dropin),
            "STRIH_DANTESYNC_LOCK": str(self.lock),
            "STRIH_DANTESYNC_BIN": str(self.bin),
            "R": role,
            "A": args,
        }
        return _bash(SETUP, 'strih_dantesync_install "$(strih_dantesync_unit_text "$R" "$A")" "$R"', env)


def _count(calls, want):
    return sum(1 for c in calls if c == want)


def test_second_run_on_an_unchanged_unit_keeps_the_daemon_running(tmp_path):
    box = Box(tmp_path).with_binary().set_active(False)
    box.lock.write_text("")  # a stale lock a crashed daemon left behind
    first = box.run()
    assert first.returncode == 0, first.stdout + first.stderr
    calls = box.calls()
    assert box.unit.read_text() == _unit_text(), "the first run installs the unit byte-for-byte"
    assert _count(calls, "daemon-reload") == 1, calls
    assert _count(calls, "start dantesync") == 1, calls
    assert not any(c.startswith("restart") for c in calls), calls
    assert not box.lock.exists(), "a stopped daemon's stale lock is cleared before the start"

    # the daemon now runs and holds its lock; a redeploy on the unchanged unit must leave it alone
    box.lock.write_text("")
    mtime = box.unit.stat().st_mtime_ns
    box.reset_log()
    second = box.run()
    assert second.returncode == 0, second.stdout + second.stderr
    calls = box.calls()
    assert not any(c.startswith(("restart", "start", "stop", "daemon-reload")) for c in calls), calls
    assert box.lock.exists(), "the lock of a RUNNING daemon is never removed"
    assert box.unit.stat().st_mtime_ns == mtime, "an unchanged unit is not rewritten"
    assert KEEP_LINE in second.stdout, second.stdout


def test_a_changed_unit_restarts_exactly_once(tmp_path):
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text().replace("RestartSec=5", "RestartSec=6"))
    box.lock.write_text("")
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert box.unit.read_text() == _unit_text(), "the changed unit is rewritten"
    assert _count(calls, "daemon-reload") == 1, calls
    assert _count(calls, "restart dantesync") == 1, calls
    assert not any(c.startswith("start") for c in calls), calls
    assert box.lock.exists(), "a restart never removes the lock (only a start does)"
    assert "RESTARTED" in r.stdout and "unit changed" in r.stdout, r.stdout


def test_a_stopped_daemon_is_started_and_its_lock_cleared(tmp_path):
    box = Box(tmp_path).with_binary().set_active(False)
    box.unit.write_text(_unit_text())
    box.lock.write_text("")
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert _count(calls, "start dantesync") == 1, calls
    assert not any(c.startswith("restart") for c in calls), calls
    assert _count(calls, "daemon-reload") == 0, "an unchanged unit needs no reload: %s" % calls
    assert not box.lock.exists(), "the start clears the stale lock"
    assert "dantesync.service: started" in r.stdout, r.stdout


def test_a_removed_dropin_counts_as_a_change(tmp_path):
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text())
    box.dropin.mkdir()
    (box.dropin / "10-ntp-master.conf").write_text("[Service]\nExecStart=\nExecStart=/usr/local/bin/dantesync\n")
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert not box.dropin.exists(), "the stale drop-in (and its now-empty dir) is removed"
    assert _count(calls, "daemon-reload") == 1, calls
    assert _count(calls, "restart dantesync") == 1, calls
    assert "drop-in removed" in r.stdout, r.stdout


def test_an_empty_dropin_dir_is_not_a_change(tmp_path):
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text())
    box.dropin.mkdir()
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert not any(c.startswith(("restart", "start", "daemon-reload")) for c in calls), calls
    assert KEEP_LINE in r.stdout, r.stdout


def test_no_binary_installs_the_unit_but_starts_nothing(tmp_path):
    box = Box(tmp_path).set_active(False)
    box.lock.write_text("")
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert box.unit.read_text() == _unit_text()
    assert not any(c.startswith(("restart", "start")) for c in calls), calls
    assert box.lock.exists(), "no start, so no lock removal"
    assert "binary absent" in r.stdout + r.stderr


def test_the_client_role_follows_the_same_rule(tmp_path):
    args = "--ntp-server strih.lan"
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text("client", args))
    r = box.run("client", args)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not any(c.startswith(("restart", "start", "daemon-reload")) for c in box.calls())
    assert KEEP_LINE in r.stdout, r.stdout


# --- wiring: step 2 calls the caller, and nothing else restarts or clears the lock -----------

def _function_body(text, name):
    m = re.search(r"^%s\(\) \{\n(.*?)^\}\n" % re.escape(name), text, re.M | re.S)
    assert m, "%s() not found" % name
    return m.group(1)


def test_setup_strih_step2_goes_through_the_caller():
    s = SETUP.read_text()
    emit = s.index('DS_UNIT_TEXT="$(strih_dantesync_unit_text "$DS_ROLE" "$DS_ARGS")"')
    call = s.index('strih_dantesync_install "$DS_UNIT_TEXT" "$DS_ROLE"')
    guard = s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    assert emit < call, "step 2 emits the unit text, then hands it to the caller"
    assert s.index("strih_dantesync_install() {") < guard, \
        "the caller is defined before the source-guard, so a sourced setup (the tests) runs the real one"
    body = _function_body(s, "strih_dantesync_install")
    assert "strih_dantesync_restart_decision" in body
    # exactly one restart statement in the whole script, and it is the caller's `restart` arm
    assert s.count("systemctl restart dantesync") == 1, "the only restart is the decision's restart arm"
    assert "systemctl restart dantesync" in body
    assert "rm -f /var/run/dantesync.lock" not in s, "the unconditional lock removal is gone"
    assert s.count('rm -f "$lock"') == 1 and 'rm -f "$lock"' in body


def test_verify_strih_grades_the_unit_content_read_only():
    v = VERIFY.read_text()
    start = v.index("# 6c) dantesync UNIT content")
    item = v[start:v.index("# 30)", start)]
    assert "strih_dantesync_unit_matches" in item
    assert "strih_dantesync_dropins_present" in item
    assert "(dantesync-unit)" in item
    assert not re.search(r"systemctl\s+(restart|start|stop|daemon-reload)", item), \
        "the acceptance gate is read-only: it never restarts the date master"
