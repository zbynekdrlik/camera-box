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
  * the byte-exact unit comparison, the drop-in probe and the unit verdict verify-strih item 6c grades
    (scripts/lib/strih-dantesync.sh, shared by setup-strih and verify-strih);
  * the setup-strih step-2 caller `strih_dantesync_install` (scripts/lib/strih-dantesync.sh, its own
    lib so setup-strih.sh stays under its 1000-line budget), run against a temp root through its path
    seams with a fake `systemctl` on PATH that logs its argv: a second run on an unchanged unit issues
    no restart and removes no lock; a changed unit restarts exactly once, AFTER the reload of the
    written unit; a unit on disk that systemd has not loaded counts as a change; a stopped daemon is
    started and only then is the stale lock cleared, and never while a process holds it; an unreadable
    unit or a missing helper fails closed, never towards a restart.

Tier-0: bash + pytest only (no cargo, no root, no rig). The caller is run by SOURCING setup-strih.sh
(which sources the lib), whose source-guard stops before the provisioning flow, so the tests
exercise the real function through the real wiring.
"""
import os
import re
import subprocess
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
DS_LIB = SCRIPTS / "lib" / "strih-dantesync.sh"

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
    body = 'strih_dantesync_unit_matches "$T" "$U"'
    env = {"U": str(unit), "T": text.rstrip("\n")}
    assert _bash(DS_LIB, body, env).returncode == 1, "a missing unit never matches"
    unit.write_text(text)
    assert _bash(DS_LIB, body, env).returncode == 0, "the written unit matches"
    unit.write_text(text.rstrip("\n"))
    assert _bash(DS_LIB, body, env).returncode == 1, "a missing trailing newline is a difference"
    unit.write_text(text + "\n")
    assert _bash(DS_LIB, body, env).returncode == 1, "an extra trailing newline is a difference"
    unit.write_text(text.replace("RestartSec=5", "RestartSec=6"))
    assert _bash(DS_LIB, body, env).returncode == 1, "a changed line is a difference"
    unit.write_text(_unit_text("client", "--ntp-server strih.lan"))
    assert _bash(DS_LIB, body, env).returncode == 1, "the other role's unit is a difference"


def test_unit_matches_reports_an_unreadable_unit_apart_from_a_difference(tmp_path):
    # A read error must never look like "changed": the caller would then restart a running master.
    unit = tmp_path / "dantesync.service"
    unit.write_text(_unit_text())
    r = _bash(DS_LIB, 'cmp() { return 2; }\nrc=0; strih_dantesync_unit_matches "x" "$U" || rc=$?; echo "rc=$rc"',
              env={"U": str(unit)})
    assert "rc=2" in r.stdout, r.stdout + r.stderr


def test_dropins_present_only_counts_conf_files(tmp_path):
    d = tmp_path / "dantesync.service.d"
    body = 'strih_dantesync_dropins_present "$D"'
    env = {"D": str(d)}
    assert _bash(DS_LIB, body, env).returncode == 1, "no directory = no drop-in"
    d.mkdir()
    assert _bash(DS_LIB, body, env).returncode == 1, "an empty directory = no drop-in"
    (d / "README").write_text("x")
    assert _bash(DS_LIB, body, env).returncode == 1, "only *.conf files are drop-ins"
    (d / "10-ntp-master.conf").write_text("[Service]\n")
    assert _bash(DS_LIB, body, env).returncode == 0, "a *.conf file is a drop-in"


def _verdict(want, unit, dropin, need_reload):
    r = _bash(DS_LIB, 'rc=0; strih_dantesync_unit_verdict "$W" "$U" "$D" "$N" || rc=$?; echo; echo "rc=$rc"',
              env={"W": want, "U": str(unit), "D": str(dropin), "N": need_reload})
    lines = r.stdout.splitlines()
    return lines[0].strip() if lines else "", lines[-1] if lines else ""


def test_unit_verdict_grades_content_dropin_and_loaded_state(tmp_path):
    want = _unit_text().rstrip("\n")
    unit = tmp_path / "dantesync.service"
    dropin = tmp_path / "dantesync.service.d"
    assert _verdict(want, unit, dropin, "no") == ("differs", "rc=1"), "a missing unit differs"
    unit.write_text(_unit_text())
    assert _verdict(want, unit, dropin, "no") == ("ok", "rc=0")
    assert _verdict(want, unit, dropin, "") == ("ok", "rc=0"), "an unread NeedDaemonReload is not a fault"
    assert _verdict(want, unit, dropin, "yes") == ("not-loaded", "rc=1"), "a pending daemon-reload"
    dropin.mkdir()
    (dropin / "10-x.conf").write_text("[Service]\n")
    assert _verdict(want, unit, dropin, "no") == ("dropin", "rc=1")
    unit.write_text(_unit_text().replace("RestartSec=5", "RestartSec=6"))
    assert _verdict(want, unit, dropin, "yes") == ("differs", "rc=1"), "content is graded first"
    r = _bash(DS_LIB, 'cmp() { return 2; }\nrc=0; strih_dantesync_unit_verdict "$W" "$U" "$D" no || rc=$?; '
              'echo; echo "rc=$rc"', env={"W": want, "U": str(unit), "D": str(dropin)})
    assert r.stdout.splitlines()[0].strip() == "unreadable", r.stdout


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
        self.enabled = tmp_path / "systemctl.enabled"
        self.snap = tmp_path / "unit-at-daemon-reload"
        self.need_reload = tmp_path / "systemctl.need-reload"
        fake_dir = tmp_path / "fakebin"
        for p in (self.unit.parent, self.lock.parent, self.bin.parent, fake_dir):
            p.mkdir(parents=True, exist_ok=True)
        sc = fake_dir / "systemctl"
        sc.write_text(
            "#!/usr/bin/env bash\n"
            'printf "%s\\n" "$*" >> "$FAKE_SC_LOG"\n'
            'case "$1" in\n'
            '  is-active) [ "$(cat "$FAKE_SC_STATE" 2>/dev/null)" = active ] && exit 0; exit 3 ;;\n'
            '  is-enabled) [ -e "$FAKE_SC_ENABLED" ] && exit 0; exit 1 ;;\n'
            '  enable) : > "$FAKE_SC_ENABLED" ;;\n'
            '  start|restart) echo active > "$FAKE_SC_STATE" ;;\n'
            '  daemon-reload) cp "$FAKE_SC_UNIT" "$FAKE_SC_SNAP" 2>/dev/null; rm -f "$FAKE_SC_NEED_RELOAD" ;;\n'
            '  show) cat "$FAKE_SC_NEED_RELOAD" 2>/dev/null ;;\n'
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
        if active:
            self.enabled.write_text("")  # a running provisioned daemon is enabled too
        return self

    def set_need_reload(self):
        self.need_reload.write_text("yes\n")
        return self

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def reset_log(self):
        if self.log.exists():
            self.log.unlink()

    def run(self, role="server", args="", pre=""):
        env = {
            "PATH": self.path,
            "FAKE_SC_LOG": str(self.log),
            "FAKE_SC_STATE": str(self.state),
            "FAKE_SC_ENABLED": str(self.enabled),
            "FAKE_SC_UNIT": str(self.unit),
            "FAKE_SC_SNAP": str(self.snap),
            "FAKE_SC_NEED_RELOAD": str(self.need_reload),
            "STRIH_DANTESYNC_UNIT": str(self.unit),
            "STRIH_DANTESYNC_DROPIN_DIR": str(self.dropin),
            "STRIH_DANTESYNC_LOCK": str(self.lock),
            "STRIH_DANTESYNC_BIN": str(self.bin),
            "R": role,
            "A": args,
        }
        return _bash(SETUP, pre + 'strih_dantesync_install "$(strih_dantesync_unit_text "$R" "$A")" "$R"', env)


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
    assert _count(calls, "enable dantesync") == 1, "a fresh box enables the unit: %s" % calls
    assert not any(c.startswith("restart") for c in calls), calls
    assert not box.lock.exists(), "a stopped daemon's stale lock is cleared before the start"

    # the daemon now runs and holds its lock; a redeploy on the unchanged unit must leave it alone
    box.lock.write_text("")
    mtime = box.unit.stat().st_mtime_ns
    box.reset_log()
    second = box.run()
    assert second.returncode == 0, second.stdout + second.stderr
    calls = box.calls()
    assert not any(c.startswith(("restart", "start", "stop", "daemon-reload", "enable")) for c in calls), \
        "a kept daemon sees only reads (enable reloads the manager): %s" % calls
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
    assert calls.index("daemon-reload") < calls.index("restart dantesync"), \
        "the restart must run on the RELOADED unit: %s" % calls
    assert box.snap.read_text() == _unit_text(), "the new unit is on disk before the reload"
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
    assert calls.index("daemon-reload") < calls.index("restart dantesync"), calls
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


def test_a_unit_systemd_has_not_loaded_counts_as_a_change(tmp_path):
    # A run killed (or a daemon-reload that failed) after the write leaves the new unit on disk but
    # not loaded; the next run must not read that as "unchanged" and keep the old config running.
    box = Box(tmp_path).with_binary().set_active(True).set_need_reload()
    box.unit.write_text(_unit_text())
    mtime = box.unit.stat().st_mtime_ns
    r = box.run()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert _count(calls, "daemon-reload") == 1, calls
    assert _count(calls, "restart dantesync") == 1, calls
    assert calls.index("daemon-reload") < calls.index("restart dantesync"), calls
    assert box.unit.stat().st_mtime_ns == mtime, "the matching unit on disk is not rewritten"
    assert "unit not loaded" in r.stdout, r.stdout


def test_an_unreadable_unit_fails_closed_never_towards_a_restart(tmp_path):
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text())
    mtime = box.unit.stat().st_mtime_ns
    r = box.run(pre='cmp() { return 2; }\n')
    assert r.returncode != 0, r.stdout
    assert not any(c.startswith(("restart", "start", "daemon-reload", "enable")) for c in box.calls()), box.calls()
    assert box.unit.stat().st_mtime_ns == mtime, "an unreadable unit is never overwritten"
    assert "cannot read" in r.stderr, r.stderr


def test_a_missing_decision_helper_fails_closed(tmp_path):
    box = Box(tmp_path).with_binary().set_active(True)
    box.unit.write_text(_unit_text().replace("RestartSec=5", "RestartSec=6"))
    r = box.run(pre="unset -f strih_dantesync_restart_decision\n")
    assert r.returncode != 0, r.stdout
    assert box.calls() == [], "nothing is touched without the decision: %s" % box.calls()
    assert "RestartSec=6" in box.unit.read_text(), "the unit is left as it was"


def test_a_lock_a_process_still_holds_is_never_removed(tmp_path):
    # A stray dantesync outside the unit holds the flock while systemd reads the unit inactive:
    # removing the file would let a second instance lock a new inode.
    box = Box(tmp_path).with_binary().set_active(False)
    box.unit.write_text(_unit_text())
    box.lock.write_text("")
    holder = subprocess.Popen(["flock", str(box.lock), "sleep", "30"])
    try:
        deadline = time.time() + 5
        while time.time() < deadline:
            probe = subprocess.run(["flock", "-n", str(box.lock), "true"])
            if probe.returncode != 0:
                break
            time.sleep(0.05)
        r = box.run()
    finally:
        holder.kill()
        holder.wait()
    assert r.returncode == 0, r.stdout + r.stderr
    assert box.lock.exists(), "a held lock is kept"
    assert "holds" in r.stdout + r.stderr, r.stdout + r.stderr
    assert _count(box.calls(), "start dantesync") == 1, box.calls()


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
    lib = DS_LIB.read_text()
    src = s.index('. "${HERE}/lib/strih-dantesync.sh"')
    guard = s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    emit = s.index('DS_UNIT_TEXT="$(strih_dantesync_unit_text "$DS_ROLE" "$DS_ARGS")"')
    call = s.index('strih_dantesync_install "$DS_UNIT_TEXT" "$DS_ROLE"')
    assert src < guard, "the lib is sourced before the source-guard, so a sourced setup (the tests) has it"
    assert guard < emit < call, "step 2 emits the unit text, then hands it to the caller"
    body = _function_body(lib, "strih_dantesync_install")
    assert "strih_dantesync_restart_decision" in body
    # setup-strih itself never restarts dantesync; the ONE restart statement is the caller's `restart` arm
    assert "systemctl restart dantesync" not in s, "setup-strih.sh must not restart dantesync itself"
    assert lib.count("systemctl restart dantesync") == 1, "the only restart is the decision's restart arm"
    assert "systemctl restart dantesync" in body
    assert "rm -f /var/run/dantesync.lock" not in s + lib, "the unconditional lock removal is gone"
    assert lib.count('rm -f "$lock"') == 1 and 'rm -f "$lock"' in body
    assert 'flock -n "$lock"' in body, "the lock is removed only when no process holds it"
    assert "NeedDaemonReload" in body


def test_verify_strih_sources_the_dantesync_lib_before_its_guard():
    v = VERIFY.read_text()
    src = v.index('. "${HERE}/lib/strih-dantesync.sh"')
    guard = v.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    assert src < guard


def test_verify_strih_grades_the_unit_content_read_only():
    v = VERIFY.read_text()
    start = v.index("# 6c) dantesync UNIT content")
    item = v[start:v.index("# 30)", start)]
    assert "strih_dantesync_unit_verdict" in item
    assert "NeedDaemonReload" in item
    assert "(dantesync-unit)" in item
    assert not re.search(r"systemctl\s+(restart|start|stop|daemon-reload)", item), \
        "the acceptance gate is read-only: it never restarts the date master"


def _verify_item(tmp_path, unit, need_reload):
    """Run verify-strih item 6c's REAL text (only its unit path moved to UNIT) under the caller's
    set -euo pipefail, with a fake systemctl answering `show`. Returns the PASS/FAIL line."""
    v = VERIFY.read_text()
    start = v.index("# 6c) dantesync UNIT content")
    block = v[start:v.index("# 30)", start)]
    literal = 'DS_UNIT_PATH_V="/etc/systemd/system/dantesync.service"'
    assert block.count(literal) == 1
    block = block.replace(literal, 'DS_UNIT_PATH_V="%s"' % unit)
    fake = tmp_path / "fakebin"
    fake.mkdir(exist_ok=True)
    sc = fake / "systemctl"
    sc.write_text('#!/usr/bin/env bash\n[ "$1" = show ] && printf "%%s\\n" "%s"\nexit 0\n' % need_reload)
    sc.chmod(0o755)
    harness = ('set -euo pipefail\n. "$LIBP"\n. "$DSLIB"\n'
               'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nDS_ROLE_V=server\n'
               + block + '\necho END\n')
    r = subprocess.run(["bash", "-c", harness], capture_output=True, text=True, cwd=str(REPO), timeout=60,
                       env=dict(os.environ, LIBP=str(LIB), DSLIB=str(DS_LIB),
                                PATH="%s:%s" % (fake, os.environ.get("PATH", "/usr/bin:/bin"))))
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    return r.stdout.strip().splitlines()[0]


def test_verify_item_6c_runs_under_set_e_and_grades_each_state(tmp_path):
    unit = tmp_path / "dantesync.service"
    assert _verify_item(tmp_path, unit, "no").startswith("FAIL (dantesync-unit)"), "missing"
    unit.write_text(_unit_text())
    assert _verify_item(tmp_path, unit, "no").startswith("PASS (dantesync-unit)"), "a kept daemon passes"
    assert "not loaded" in _verify_item(tmp_path, unit, "yes")
    (tmp_path / "dantesync.service.d").mkdir()
    (tmp_path / "dantesync.service.d" / "10-x.conf").write_text("[Service]\n")
    assert "drop-in" in _verify_item(tmp_path, unit, "no")
