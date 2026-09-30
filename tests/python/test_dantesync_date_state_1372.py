"""Issue 1372 -- dantesync 1.15.0 (dantesync issue 126): the date master's saved fleet date.

dantesync 1.15.0 persists the NTP master's date offset in date-offset.json and restores it at start.
A master taken back below 1.15.0 -- a VERIFY-failure rollback (main's design, issue comment
5908602207) or a forced downgrade (supervisor ROZHODNUTE, issue comment 5910080606) -- deletes it,
or a 1.15 reinstalled within a day restores the session from before (a stale D). Both emitted
programs of each path are RUN here (the Linux script with PATH stubs, the .ps1 under pwsh), not
only read as text, and the orchestrator is driven end to end with the shared sshpass stub.

Moved verbatim out of test_dantesync_fleet_upgrade_tray_1372.py, which had passed the ~1000-line
budget; the shared helpers are in dantesync_upgrade_harness_1372.py.
"""
import os
import pathlib
import shutil
import subprocess
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from dantesync_upgrade_harness_1372 import _ROOT, _UPGRADE, _at, _roll, _source  # noqa: E402


# ---------------------------------------------------------------------------------------------
# dantesync 1.15.0 (dantesync issue 126): a date master rolled back below 1.15.0 drops its saved
# fleet date. 1.15.0 persists the master's date offset in date-offset.json and restores it at start,
# so a master rolled back to an older build must delete it in the same step -- or a 1.15 reinstalled
# within a day restores the session from before the rollback (a stale D). Only the rollback, only the
# fleet ntp-master, only below 1.15.0: a rollback to 1.15.x keeps a valid saved date (deleting it
# would boot-step the fleet during the day), and an upgrade never touches the file.
# ---------------------------------------------------------------------------------------------

_DATE_LINE = "date-offset.json removed (rollback below 1.15.0, dantesync issue 126)"
_DATE_ABSENT_LINE = "date-offset.json absent, nothing to remove (rollback below 1.15.0, dantesync issue 126)"
_DATE_WARNING = "WARNING: date-offset.json could NOT be removed"


@pytest.mark.parametrize("role,version,clears", [
    ("ntp-master", "1.14.0", True),
    ("ntp-master", "1.15.0", False),
    ("ntp-master", "1.16.0", False),
    ("slave", "1.14.0", False),
    ("slave", "1.15.0", False),
    ("slave", "1.16.0", False),
    # semver through the script's own helper, never lexical ("1.9.0" > "1.15.0" as text)
    ("ntp-master", "1.9.0", True),
    ("ntp-master", "1.14.9", True),
    ("ntp-master", "1.15.1", False),
    # the fleet's other roles, an unread version, no role
    ("video", "1.14.0", False),
    ("audio", "1.14.0", False),
    ("ntp-master", "", False),
    ("", "1.14.0", False),
])
def test_rollback_clears_the_date_state_only_for_a_master_restored_below_1_15(tmp_path, role, version, clears):
    r = _source(tmp_path, f"dantesync_rollback_clears_date_state '{role}' '{version}'; echo rc=$?")
    assert r.stdout.strip() == ("rc=0" if clears else "rc=1"), r.stdout + r.stderr


def _emit(tmp_path, fn, *args):
    r = _source(tmp_path, fn + "".join(f" '{a}'" for a in args))
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_linux_rollback_of_a_master_below_1_15_deletes_the_date_state_between_stop_and_start(tmp_path):
    cmd = _emit(tmp_path, "dantesync_linux_rollback_cmd", "ntp-master", "1.14.0")
    rw = _at(cmd, "if [ \"$ro_root\" = 1 ]; then mount -o remount,rw /; fi")
    stop = _at(cmd, "systemctl stop dantesync", rw)
    restore = _at(cmd, 'cp -a "/usr/local/bin/dantesync.bak" /usr/local/bin/dantesync', stop)
    rm = _at(cmd, 'rm -f "/etc/dantesync/date-offset.json"', restore)
    start = _at(cmd, "systemctl restart dantesync", rm)
    assert rw < stop < restore < rm < start, cmd
    assert cmd.count(_DATE_LINE) == 1 and cmd.count("date-offset.json removed") == 1, cmd


@pytest.mark.parametrize("args", [(), ("slave", "1.14.0"), ("ntp-master", "1.15.0"), ("ntp-master", "1.16.0"),
                                  ("ntp-master", "")])
def test_linux_rollback_keeps_the_date_state_otherwise_and_is_the_plain_program(tmp_path, args):
    cmd = _emit(tmp_path, "dantesync_linux_rollback_cmd", *args)
    assert "date-offset" not in cmd, cmd
    assert cmd == _emit(tmp_path, "dantesync_linux_rollback_cmd")


def test_windows_rollback_of_a_master_below_1_15_deletes_the_date_state_between_stop_and_start(tmp_path):
    ps = _emit(tmp_path, "dantesync_windows_rollback_ps", "ntp-master", "1.14.0")
    assert "$dateState = 'C:\\ProgramData\\DanteSync\\date-offset.json'" in ps, ps
    stop = _at(ps, "Stop-Service dantesync")
    wait = _at(ps, "Wait-Process -Name dantesync", stop)
    restore = _at(ps, "Copy-Item -Force $bak $exe", wait)
    rm = _at(ps, "Remove-Item -LiteralPath $dateState -Force -ErrorAction SilentlyContinue", restore)
    start = _at(ps, "Start-Service dantesync", rm)
    assert stop < wait < restore < rm < start, ps
    assert ps.count(_DATE_LINE) == 1, ps
    # a Remove-Item that throws (e.g. a path it would have to prompt for) must never skip the start
    assert _at(ps, "try {", restore) < rm < _at(ps, "} catch {", rm) < start, ps


@pytest.mark.parametrize("args", [(), ("slave", "1.14.0"), ("ntp-master", "1.15.0"), ("ntp-master", "1.16.0"),
                                  ("ntp-master", "")])
def test_windows_rollback_keeps_the_date_state_otherwise_and_is_the_plain_program(tmp_path, args):
    ps = _emit(tmp_path, "dantesync_windows_rollback_ps", *args)
    assert "date-offset" not in ps and "$dateState" not in ps, ps
    assert ps == _emit(tmp_path, "dantesync_windows_rollback_ps")


@pytest.mark.parametrize("fn", ["dantesync_linux_upgrade_cmd", "dantesync_windows_upgrade_ps"])
@pytest.mark.parametrize("version,role", [
    ("1.14.0", None), ("1.15.0", None),              # no role given: the plain program
    ("1.14.0", "slave"), ("1.16.0", "slave"),         # a non-master downgrade or upgrade
    ("1.15.0", "ntp-master"), ("1.15.1", "ntp-master"), ("1.16.0", "ntp-master"),  # the master to >= 1.15.0
])
def test_an_upgrade_to_1_15_or_newer_or_a_non_master_roll_never_deletes_the_date_state(tmp_path, fn, version,
                                                                                         role):
    """Narrowed by the supervisor decision (issue comment 5910080606): the old claim "an upgrade
    never deletes", for target 1.14.0 on the master, WAS the defect -- a forced downgrade of the date
    master below 1.15.0 must delete like a rollback. What stays true is pinned here: an upgrade to
    1.15.0 or newer, and any non-master node, emit exactly the plain program."""
    text = _emit(tmp_path, fn, version, *(() if role is None else (role,)))
    assert "date-offset" not in text, text
    assert text == _emit(tmp_path, fn, version)


@pytest.mark.parametrize("version", ["1.14.0", "1.15.0"])
def test_the_tray_only_program_never_deletes_the_date_state(tmp_path, version):
    assert "date-offset" not in _emit(tmp_path, "dantesync_windows_tray_only_ps", version)


def _run_linux_rollback(tmp_path, role, version, state="file", ro=False):
    """Run the EMITTED Linux rollback for real. Its binary, .bak and date-state paths point into
    tmp_path, and PATH stubs stand in for systemctl / mount / findmnt / dantesync; each stub logs its
    call and whether the date state exists at that moment. STATE: file | absent | stuck (a non-empty
    directory, which `rm -f` cannot remove -- also as root)."""
    bindir, etc, stub = tmp_path / "bin", tmp_path / "etc", tmp_path / "stub"
    for d in (bindir, etc, stub):
        d.mkdir()
    (bindir / "dantesync").write_text("the 1.15.0 binary\n")
    (bindir / "dantesync.bak").write_text("the pre-upgrade binary\n")
    st = etc / "date-offset.json"
    if state == "file":
        st.write_text('{"version":1}\n')
    elif state == "stuck":
        st.mkdir()
        (st / "held").write_text("x\n")
    log = tmp_path / "calls.log"
    probe = (f'if [ -e "{st}" ]; then s=present; else s=absent; fi\n'
             f'echo "$(basename "$0") $* state=$s" >> "{log}"\n')
    for tool, reply in (("systemctl", ""), ("mount", ""), ("findmnt", "ro,relatime" if ro else "rw,relatime"),
                        ("dantesync", f"dantesync {version}")):
        p = stub / tool
        p.write_text("#!/bin/bash\n" + probe + (f'echo "{reply}"\n' if reply else ""))
        p.chmod(0o755)
    body = (f"DANTESYNC_LINUX_BIN='{bindir / 'dantesync'}'\n"
            f"DANTESYNC_LINUX_BAK='{bindir / 'dantesync.bak'}'\n"
            f"DANTESYNC_LINUX_DATE_STATE='{st}'\n"
            f"dantesync_linux_rollback_cmd '{role}' '{version}'")
    r = _source(tmp_path, body)
    assert r.returncode == 0, r.stderr
    script = tmp_path / "rollback.sh"
    script.write_text(r.stdout)
    run = subprocess.run(["bash", str(script)], capture_output=True, text=True,
                         env={"PATH": f"{stub}:/usr/bin:/bin", "HOME": str(tmp_path)})
    calls = [c for c in (log.read_text().splitlines() if log.exists() else []) if not c.startswith("findmnt")]
    return run, calls, st, bindir


def test_the_emitted_linux_rollback_removes_the_date_state_after_the_stop_inside_the_rw_window(tmp_path):
    run, calls, st, bindir = _run_linux_rollback(tmp_path, "ntp-master", "1.14.0", ro=True)
    assert run.returncode == 0, run.stdout + run.stderr
    assert _DATE_LINE in run.stdout.splitlines(), run.stdout
    assert not st.exists()
    assert (bindir / "dantesync").read_text() == "the pre-upgrade binary\n"
    assert calls == [
        "mount -o remount,rw / state=present",
        "systemctl stop dantesync state=present",
        "systemctl restart dantesync state=absent",
        "dantesync --version state=absent",
        "mount -o remount,ro / state=absent",
    ], calls


def test_a_date_state_that_cannot_be_removed_is_named_and_the_master_still_starts(tmp_path):
    run, calls, st, _ = _run_linux_rollback(tmp_path, "ntp-master", "1.14.0", state="stuck")
    assert run.returncode == 0, run.stdout + run.stderr
    assert st.exists()
    assert any(ln.startswith(_DATE_WARNING) for ln in run.stdout.splitlines()), run.stdout
    assert _DATE_LINE not in run.stdout
    assert "systemctl restart dantesync state=present" in calls, calls


def test_an_absent_date_state_is_named_never_claimed_removed(tmp_path):
    run, calls, _, _ = _run_linux_rollback(tmp_path, "ntp-master", "1.14.0", state="absent")
    assert run.returncode == 0, run.stdout + run.stderr
    assert _DATE_ABSENT_LINE in run.stdout.splitlines() and _DATE_LINE not in run.stdout, run.stdout
    assert "systemctl restart dantesync state=absent" in calls, calls


@pytest.mark.parametrize("role,version", [("slave", "1.14.0"), ("ntp-master", "1.15.0")])
def test_the_emitted_linux_rollback_keeps_the_date_state_of_a_slave_or_a_1_15_restore(tmp_path, role, version):
    run, calls, st, _ = _run_linux_rollback(tmp_path, role, version)
    assert run.returncode == 0, run.stdout + run.stderr
    assert st.is_file() and "date-offset" not in run.stdout, run.stdout
    assert "systemctl restart dantesync state=present" in calls, calls


def _pwsh():
    """ubuntu-latest ships pwsh; dev1 has a portable one under ~/.local/pwsh74. A missing pwsh FAILS,
    never skips (the issue-1389 LaptopScriptRun1389 shape)."""
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not pwsh and os.access(home_pwsh, os.X_OK):
        pwsh = home_pwsh
    if not pwsh:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the rollback .ps1 must really run)")
    return pwsh


@pytest.mark.parametrize("role,state,want_line,want_state", [
    ("ntp-master", "file", _DATE_LINE, "absent"),
    ("ntp-master", "absent", _DATE_ABSENT_LINE, "absent"),
    ("ntp-master", "stuck", _DATE_WARNING, "present"),
    ("slave", "file", None, "present"),
])
def test_the_emitted_windows_rollback_runs_and_always_starts_the_service(tmp_path, role, state, want_line,
                                                                         want_state):
    """RUN the emitted rollback .ps1 under pwsh: stub functions stand in for the service cmdlets and
    log whether the date state exists when each one runs; Test-Path / Remove-Item / Copy-Item are
    real, on tmp paths. `& $exe --version` is replaced (a tmp file cannot run); the Rust anchors pin
    that line. A non-empty directory makes Remove-Item THROW in a non-interactive session even with
    -ErrorAction SilentlyContinue, so "stuck" proves the service is still started."""
    exe, bak, st = tmp_path / "dantesync.exe", tmp_path / "dantesync.exe.bak", tmp_path / "date-offset.json"
    exe.write_text("the 1.15.0 exe\n")
    bak.write_text("the pre-upgrade exe\n")
    if state == "file":
        st.write_text('{"version":1}\n')
    elif state == "stuck":
        st.mkdir()
        (st / "held").write_text("x\n")
    body = (f"DANTESYNC_WIN_EXE='{exe}'\nDANTESYNC_WIN_BAK='{bak}'\nDANTESYNC_WIN_DATE_STATE='{st}'\n"
            f"dantesync_windows_rollback_ps '{role}' 1.14.0")
    r = _source(tmp_path, body)
    assert r.returncode == 0, r.stderr
    assert r.stdout.rstrip().endswith("& $exe --version"), r.stdout
    program = tmp_path / "rollback.ps1"
    program.write_text(r.stdout.replace("& $exe --version", "Write-Output 'version read'"))
    harness = tmp_path / "harness.ps1"
    harness.write_text(
        "$script:calls = New-Object System.Collections.Generic.List[string]\n"
        f"$script:state = '{st}'\n"
        "function Note([string]$what) {\n"
        "    $s = if (Test-Path -LiteralPath $script:state) { 'present' } else { 'absent' }\n"
        "    $script:calls.Add($what + ' state=' + $s)\n"
        "}\n"
        "function Stop-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name) Note ('Stop-Service ' + $Name) }\n"
        "function Start-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name) Note ('Start-Service ' + $Name) }\n"
        "function Wait-Process { [CmdletBinding()] param($Name, $Timeout, $Id) Note ('Wait-Process ' + $Name) }\n"
        "function Get-Process { [CmdletBinding()] param($Name) }\n"
        "function Stop-Process { [CmdletBinding()] param($Name, [switch]$Force) Note ('Stop-Process ' + $Name) }\n"
        f". '{program}'\n"
        "foreach ($c in $script:calls) { Write-Output ('CALL ' + $c) }\n")
    run = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(harness)],
                         capture_output=True, text=True, timeout=180)
    out = run.stdout.splitlines()
    assert run.returncode == 0, run.stdout + run.stderr
    assert bak.read_text() == exe.read_text() == "the pre-upgrade exe\n"
    calls = [ln[len("CALL "):] for ln in out if ln.startswith("CALL ")]
    assert calls[0] == ("Stop-Service dantesync state=present" if state != "absent"
                        else "Stop-Service dantesync state=absent"), calls
    assert calls[-1] == f"Start-Service dantesync state={want_state}", calls
    assert "version read" in out, run.stdout
    date_lines = [ln for ln in out if "date-offset.json" in ln]
    if want_line is None:
        assert date_lines == [], run.stdout
    else:
        assert len(date_lines) == 1 and date_lines[0].startswith(want_line), run.stdout


def test_rollback_node_threads_the_role_and_the_restored_version_to_both_programs():
    s = _UPGRADE.read_text()
    assert s.count('dantesync_linux_rollback_cmd "$role" "$restored"') == 2, "the local and the linux arm"
    assert s.count('dantesync_windows_rollback_ps "$role" "$restored"') == 1
    assert s.count('rollback_node "$name" "$kind" "$addr" "$cur"') == 1, \
        "the version the node ran before the swap is the version the rollback restores"
    node = s[_at(s, "rollback_node() {"):_at(s, "\n}\n", _at(s, "rollback_node() {"))]
    assert 'role="$(dantesync_date_role "$name" "$NTP_MASTER")"' in node, node
    assert 'dantesync_rollback_date_state_note "$name" "$out"' in node, node


def test_the_date_state_line_is_relayed_to_the_roll_log(tmp_path):
    out = f"x\r\n{_DATE_LINE}\r\ndantesync 1.14.0\r\n"
    (tmp_path / "out.txt").write_text(out)
    r = _source(tmp_path, f'dantesync_rollback_date_state_note strih-lx "$(cat "{tmp_path / "out.txt"}")"; echo rc=$?')
    assert r.stdout == f"[strih-lx] {_DATE_LINE}\nrc=0\n", repr(r.stdout)
    r = _source(tmp_path, 'set -e; dantesync_rollback_date_state_note strih-lx ""; echo rc=$?')
    assert r.stdout == "rc=0\n", repr(r.stdout + r.stderr)


def test_the_rollback_lib_holds_the_rule_and_the_programs():
    lib = (_ROOT / "scripts" / "lib" / "dantesync-rollback.sh").read_text()
    upgrade = _UPGRADE.read_text()
    for fn in ("dantesync_rollback_clears_date_state", "dantesync_date_role", "dantesync_linux_rollback_cmd",
               "dantesync_windows_rollback_ps", "dantesync_rollback_date_state_note"):
        assert f"{fn}() {{" in lib and f"{fn}() {{" not in upgrade, fn
    assert '. "$HERE/lib/dantesync-rollback.sh"' in upgrade
    # the same persisted paths dantesync 1.15.0 uses (src/main.rs DATE_STATE_PATH)
    assert "DANTESYNC_LINUX_DATE_STATE='/etc/dantesync/date-offset.json'" in lib
    assert "DANTESYNC_WIN_DATE_STATE='C:\\ProgramData\\DanteSync\\date-offset.json'" in lib
    # the comparison is the script's own version helper, never a second semver
    assert "dantesync_upgrade_status" in lib and "sort -V" not in lib


def test_roll_of_a_master_that_fails_verify_rolls_back_with_the_date_state_delete(tmp_path):
    """End to end through the orchestrator: stream is the NTP master here, its verify fails (the
    version never flips), so the canary rollback restores 1.11.0 -- below 1.15.0 -- and the uploaded
    rollback .ps1 deletes the date state; the program's date-state line reaches the roll log."""
    r = _roll(tmp_path, f"{_DATE_LINE}\n", flip=False, env_extra={"NTP_MASTER": "stream"})
    out = r.stdout + r.stderr
    assert r.returncode == 10, out
    assert "[stream] rolled back to the previous binary" in out, out
    assert f"[stream] {_DATE_LINE}" in out, out
    rollback = (tmp_path / "uploaded.ps1").read_text()
    assert "Copy-Item -Force $bak $exe" in rollback and "Remove-Item -LiteralPath $dateState" in rollback


def test_roll_of_a_slave_that_fails_verify_rolls_back_without_touching_the_date_state(tmp_path):
    r = _roll(tmp_path, "dantesync 1.11.0\n", flip=False)
    out = r.stdout + r.stderr
    assert r.returncode == 10, out
    assert "[stream] rolled back to the previous binary" in out, out
    rollback = (tmp_path / "uploaded.ps1").read_text()
    assert "Copy-Item -Force $bak $exe" in rollback and "date-offset" not in rollback, rollback


# ---------------------------------------------------------------------------------------------
# A FORCED DOWNGRADE of the date master below 1.15.0 is a rollback in all but path (supervisor
# ROZHODNUTE, issue comment 5910080606): `upgrade_node`'s OLDER + --force branch runs the UPGRADE
# program, so that program deletes date-offset.json too -- the same pure decision on (role, target).
# Placement (issue comment 5910147397): AFTER the downgraded service has started. The upgrade
# programs self-heal back to the `.bak` -- in a downgrade the 1.15 binary -- on a failed start, and
# that 1.15 master must come back WITH its saved date, never boot-step the fleet during the day.
# ---------------------------------------------------------------------------------------------

def test_date_role_is_the_ntp_master_the_way_verify_names_it(tmp_path):
    body = ("dantesync_date_role stream stream; echo\n"
            "dantesync_date_role stream strih-lx; echo\n"
            "dantesync_date_role stream ''; echo")
    r = _source(tmp_path, body)
    assert r.stdout.splitlines() == ["ntp-master", "slave", "slave"], r.stdout + r.stderr


def test_linux_forced_downgrade_of_the_master_deletes_the_date_state_after_a_good_start(tmp_path):
    cmd = _emit(tmp_path, "dantesync_linux_upgrade_cmd", "1.14.0", "ntp-master")
    rw = _at(cmd, "if [ \"$ro_root\" = 1 ]; then mount -o remount,rw /; fi")
    stop = _at(cmd, "\nsystemctl stop dantesync\n", rw)
    swap = _at(cmd, 'install -m 0755 "$tmp/dantesync" /usr/local/bin/dantesync', stop)
    start = _at(cmd, "\nsystemctl restart dantesync\n", swap)
    version = _at(cmd, "\ndantesync --version\n", start)
    rm = _at(cmd, 'rm -f "/etc/dantesync/date-offset.json"', version)
    assert rw < stop < swap < start < version < rm, cmd
    assert "systemctl" not in cmd[rm:], "the delete is the last step, after every self-heal path"
    assert cmd.count(_DATE_LINE) == 1, cmd


def test_windows_forced_downgrade_of_the_master_deletes_the_date_state_after_a_good_start(tmp_path):
    ps = _emit(tmp_path, "dantesync_windows_upgrade_ps", "1.14.0", "ntp-master")
    swap = _at(ps, "Copy-Item -Force $tmp $exe")
    start = _at(ps, "    Start-Service dantesync\n} catch {", swap)
    rethrow = _at(ps, "    throw\n}", start)
    rm = _at(ps, "Remove-Item -LiteralPath $dateState -Force -ErrorAction SilentlyContinue", rethrow)
    purge = _at(ps, 'cmd /c "schtasks /Delete', rm)
    tray = _at(ps, "# 5. the tray", purge)
    assert swap < start < rethrow < rm < purge < tray, ps
    assert _at(ps, "try {", rethrow) < rm < _at(ps, "} catch {", rm), "the delete never skips the rest"
    assert ps.count(_DATE_LINE) == 1, ps


@pytest.mark.parametrize("fn,block_fn", [
    ("dantesync_linux_upgrade_cmd", "_dantesync_linux_date_state_rm_sh"),
    ("dantesync_windows_upgrade_ps", "_dantesync_windows_date_state_rm_ps"),
])
def test_the_downgrade_program_is_the_plain_program_plus_the_one_delete_block(tmp_path, fn, block_fn):
    plain = _emit(tmp_path, fn, "1.14.0")
    master = _emit(tmp_path, fn, "1.14.0", "ntp-master")
    block = _emit(tmp_path, block_fn, "ntp-master", "1.14.0")
    assert block and master.count(block) == 1, master
    assert master.replace(block, "", 1) == plain


def _run_linux_downgrade(tmp_path, role, fail_start=False):
    """Run the EMITTED Linux upgrade program to 1.14.0 for real: the running binary is 1.15.0, the
    staged 1.14.0 binary sits at a tmp DANTESYNC_LINUX_STAGED with its sha256, PATH stubs stand in
    for systemctl / mount / findmnt / dantesync and log whether the date state exists when they run.
    FAIL_START makes the first `systemctl restart` fail, so the program's own ERR trap self-heals."""
    bindir, etc, stub = tmp_path / "bin", tmp_path / "etc", tmp_path / "stub"
    for d in (bindir, etc, stub):
        d.mkdir()
    (bindir / "dantesync").write_text("the 1.15.0 binary\n")
    staged = tmp_path / "staged"
    staged.write_text("the 1.14.0 binary\n")
    sha = subprocess.run(["sha256sum", str(staged)], capture_output=True, text=True).stdout.split()[0]
    (tmp_path / "staged.sha256").write_text(f"{sha}  dantesync-linux-amd64\n")
    st = etc / "date-offset.json"
    st.write_text('{"version":1}\n')
    log, fail = tmp_path / "calls.log", tmp_path / "fail-start"
    if fail_start:
        fail.write_text("")
    probe = (f'if [ -e "{st}" ]; then s=present; else s=absent; fi\n'
             f'echo "$(basename "$0") $* state=$s" >> "{log}"\n')
    stubs = {
        "systemctl": f'if [ "$1" = restart ] && [ -f "{fail}" ]; then rm -f "{fail}"; exit 1; fi\n',
        "mount": "", "findmnt": 'echo "ro,relatime"\n', "dantesync": 'echo "dantesync 1.14.0"\n',
    }
    for tool, extra in stubs.items():
        p = stub / tool
        p.write_text("#!/bin/bash\n" + probe + extra)
        p.chmod(0o755)
    body = (f"DANTESYNC_LINUX_BIN='{bindir / 'dantesync'}'\n"
            f"DANTESYNC_LINUX_BAK='{bindir / 'dantesync.bak'}'\n"
            f"DANTESYNC_LINUX_STAGED='{staged}'\n"
            f"DANTESYNC_LINUX_DATE_STATE='{st}'\n"
            f"dantesync_linux_upgrade_cmd 1.14.0 '{role}'")
    r = _source(tmp_path, body)
    assert r.returncode == 0, r.stderr
    script = tmp_path / "upgrade.sh"
    script.write_text(r.stdout)
    run = subprocess.run(["bash", str(script)], capture_output=True, text=True,
                         env={"PATH": f"{stub}:/usr/bin:/bin", "HOME": str(tmp_path)})
    calls = [c for c in (log.read_text().splitlines() if log.exists() else []) if not c.startswith("findmnt")]
    return run, calls, st, bindir


def test_the_emitted_linux_downgrade_of_the_master_deletes_after_the_start_inside_the_rw_window(tmp_path):
    run, calls, st, bindir = _run_linux_downgrade(tmp_path, "ntp-master")
    assert run.returncode == 0, run.stdout + run.stderr
    assert _DATE_LINE in run.stdout.splitlines(), run.stdout
    assert not st.exists()
    assert (bindir / "dantesync").read_text() == "the 1.14.0 binary\n"
    assert calls == [
        "mount -o remount,rw / state=present",
        "systemctl stop dantesync state=present",
        "systemctl restart dantesync state=present",
        "dantesync --version state=present",
        "mount -o remount,ro / state=absent",
    ], calls


def test_a_failed_start_of_the_downgrade_self_heals_with_the_saved_date_intact(tmp_path):
    """Why the delete waits for the start: the self-heal puts the 1.15 binary back, and it must find
    its saved date -- a missing file would boot-step the fleet during the day."""
    run, calls, st, bindir = _run_linux_downgrade(tmp_path, "ntp-master", fail_start=True)
    assert run.returncode != 0, run.stdout + run.stderr
    assert "SELF-HEAL: restored previous dantesync binary" in run.stderr, run.stderr
    assert st.is_file(), "the 1.15 binary came back without its saved fleet date"
    assert (bindir / "dantesync").read_text() == "the 1.15.0 binary\n"
    assert "date-offset" not in run.stdout, run.stdout
    assert calls[-2:] == ["systemctl restart dantesync state=present", "mount -o remount,ro / state=present"], calls


def test_the_emitted_linux_downgrade_of_a_slave_keeps_the_date_state(tmp_path):
    run, calls, st, _ = _run_linux_downgrade(tmp_path, "slave")
    assert run.returncode == 0, run.stdout + run.stderr
    assert st.is_file() and "date-offset" not in run.stdout, run.stdout
    assert calls[-1] == "mount -o remount,ro / state=present", calls


@pytest.mark.parametrize("role,fail_start,want_line,want_state,want_exe", [
    ("ntp-master", False, _DATE_LINE, "absent", "the 1.14.0 exe\n"),
    ("ntp-master", True, None, "present", "the 1.15.0 exe\n"),
    ("slave", False, None, "present", "the 1.14.0 exe\n"),
])
def test_the_emitted_windows_downgrade_runs_and_deletes_only_after_a_good_start(tmp_path, role, fail_start,
                                                                                 want_line, want_state, want_exe):
    """RUN the emitted upgrade .ps1 to 1.14.0 under pwsh: stub functions stand in for the service
    cmdlets, the download (a local 1.14.0 release file and its real hash), `cmd` and the tray is not
    installed (its arm notes a warning and never throws); Test-Path / Remove-Item / Copy-Item /
    Get-FileHash are real, on tmp paths. A failed Start-Service makes the program's own catch put
    the 1.15 exe back and rethrow, before the delete."""
    exe, bak = tmp_path / "dantesync.exe", tmp_path / "dantesync.exe.bak"
    st, rel, temp = tmp_path / "date-offset.json", tmp_path / "release.exe", tmp_path / "temp"
    temp.mkdir()
    exe.write_text("the 1.15.0 exe\n")
    rel.write_text("the 1.14.0 exe\n")
    st.write_text('{"version":1}\n')
    body = (f"DANTESYNC_WIN_EXE='{exe}'\nDANTESYNC_WIN_BAK='{bak}'\nDANTESYNC_WIN_DATE_STATE='{st}'\n"
            f"DANTESYNC_WIN_TRAY_EXE='{tmp_path / 'no-tray.exe'}'\n"
            f"dantesync_windows_upgrade_ps 1.14.0 '{role}'")
    r = _source(tmp_path, body)
    assert r.returncode == 0, r.stderr
    assert r.stdout.rstrip().endswith("& $exe --version"), r.stdout[-200:]
    program = tmp_path / "upgrade.ps1"
    program.write_text(r.stdout.replace("& $exe --version", "Write-Output 'version read'"))
    harness = tmp_path / "harness.ps1"
    harness.write_text(
        "$script:calls = New-Object System.Collections.Generic.List[string]\n"
        f"$script:state = '{st}'\n"
        f"$script:rel = '{rel}'\n"
        f"$script:failStart = ${'true' if fail_start else 'false'}\n"
        "$script:starts = 0\n"
        "function Note([string]$what) {\n"
        "    $s = if (Test-Path -LiteralPath $script:state) { 'present' } else { 'absent' }\n"
        "    $script:calls.Add($what + ' state=' + $s)\n"
        "}\n"
        "function Invoke-WebRequest { [CmdletBinding()] param([switch]$UseBasicParsing, $Uri, $OutFile)\n"
        "    if ($OutFile -like '*.sha256') {\n"
        "        Set-Content -NoNewline -LiteralPath $OutFile -Value ((Get-FileHash -Algorithm SHA256 -LiteralPath $script:rel).Hash + '  x.exe')\n"
        "    } else { Copy-Item -Force -LiteralPath $script:rel -Destination $OutFile } }\n"
        "function Stop-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name) Note ('Stop-Service ' + $Name) }\n"
        "function Start-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name)\n"
        "    $script:starts++; Note ('Start-Service ' + $Name)\n"
        "    if ($script:failStart -and $script:starts -eq 1) { throw 'the service did not start' } }\n"
        "function Wait-Process { [CmdletBinding()] param($Name, $Timeout, $Id) }\n"
        "function Get-Process { [CmdletBinding()] param($Name) }\n"
        "function Stop-Process { [CmdletBinding()] param($Name, [switch]$Force) }\n"
        "function cmd { }\n"
        f"try {{ . '{program}' }} catch {{ Write-Output ('PROGRAM THREW: ' + $_.Exception.Message) }}\n"
        "foreach ($c in $script:calls) { Write-Output ('CALL ' + $c) }\n")
    env = dict(os.environ, TEMP=str(temp))
    run = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(harness)],
                         capture_output=True, text=True, timeout=180, env=env)
    out = run.stdout.splitlines()
    assert run.returncode == 0, run.stdout + run.stderr
    calls = [ln[len("CALL "):] for ln in out if ln.startswith("CALL ")]
    assert calls[0] == "Stop-Service dantesync state=present", calls
    assert calls[-1] == "Start-Service dantesync state=present", calls
    assert (st.exists() and "present" or "absent") == want_state, run.stdout
    assert exe.read_text() == want_exe, run.stdout
    date_lines = [ln for ln in out if "date-offset.json" in ln]
    if want_line is None:
        assert date_lines == [], run.stdout
    else:
        assert date_lines == [want_line], run.stdout
    if fail_start:
        assert "PROGRAM THREW: the service did not start" in out, run.stdout
    else:
        assert "version read" in out, run.stdout


def test_run_upgrade_threads_the_role_and_upgrade_node_relays_the_line():
    s = _UPGRADE.read_text()
    assert s.count('dantesync_linux_upgrade_cmd "$TARGET" "$role"') == 2, "the local and the linux arm"
    assert s.count('dantesync_windows_upgrade_ps "$TARGET" "$role"') == 1
    assert s.count('role="$(dantesync_date_role "$name" "$NTP_MASTER")"') == 2, "run_upgrade + rollback_node"
    run = s[_at(s, "run_upgrade() {"):_at(s, "\n}\n", _at(s, "run_upgrade() {"))]
    assert 'role="$(dantesync_date_role "$name" "$NTP_MASTER")"' in run, run
    node = s[_at(s, "upgrade_node() {"):_at(s, "\n}\n", _at(s, "upgrade_node() {"))]
    relay = _at(node, 'dantesync_rollback_date_state_note "$name" "$REMOTE_OUT"')
    assert _at(node, 'if ! run_upgrade "$name"') < relay < _at(node, 'if verify_node "$name"'), node


def _uploads(tmp_path):
    return [p.read_text() for p in sorted(tmp_path.glob("upload-*.ps1"), key=lambda p: int(p.stem[7:]))]


def test_roll_forced_downgrade_of_the_master_below_1_15_deletes_and_logs_the_line(tmp_path):
    """End to end: stream is the NTP master, runs 1.12.0, and is forced down to 1.11.1. The upgrade
    program it is sent deletes the date state, and the program's line reaches the roll log."""
    r = _roll(tmp_path, f"TRAY OK: x\n{_DATE_LINE}\n", start="1.12.0", extra=("--force",),
              env_extra={"NTP_MASTER": "stream", "MASTER_GATE_WAIT_TRIES": "1", "MASTER_GATE_WAIT_SECS": "0"})
    out = r.stdout + r.stderr
    assert "[stream] --force: downgrading 1.12.0 -> 1.11.1" in out, out
    uploads = _uploads(tmp_path)
    assert "Copy-Item -Force $tmp $exe" in uploads[0], uploads[0][:300]
    assert "Remove-Item -LiteralPath $dateState" in uploads[0]
    relay = r.stdout.index(f"[stream] {_DATE_LINE}")
    after = [m for m in ("[stream] verified", "[stream] rolled back") if m in r.stdout]
    assert after and relay < r.stdout.index(after[0]), r.stdout


@pytest.mark.parametrize("start,target,env_extra", [
    ("1.12.0", None, {}),                                              # a slave forced down below 1.15
    ("1.16.0", "1.15.1", {"NTP_MASTER": "stream", "MASTER_GATE_WAIT_TRIES": "1",
                          "MASTER_GATE_WAIT_SECS": "0"}),              # the master forced down to 1.15.1
])
def test_roll_forced_downgrade_that_keeps_the_date_state_never_deletes(tmp_path, start, target, env_extra):
    extra = ("--force",) + (() if target is None else ("--target", target))
    r = _roll(tmp_path, "TRAY OK: x\n", start=start, extra=extra, env_extra=env_extra)
    out = r.stdout + r.stderr
    assert "--force: downgrading" in out, out
    uploads = _uploads(tmp_path)
    assert uploads and all("date-offset" not in u for u in uploads), out
