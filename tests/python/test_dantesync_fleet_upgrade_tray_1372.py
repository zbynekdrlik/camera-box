"""Issue 1372 part a -- a dantesync fleet roll carries the TRAY too.

WHY: `scripts/dantesync-fleet-upgrade.sh` swapped only the dantesync SERVICE binary on a Windows
node. `dantesync-tray.exe` -- the version the operator actually sees -- stayed behind on every
roll, and the version gate's tray sha-pin ALARMed until someone swapped it by hand (after both the
1.11.0 and the 1.11.1 rolls on 26.9.2026, issue comment 5847559578).

The fix (main's design, issue comment 5847562945, Approach 1 part a) is a tray arm inside the SAME
emitted Windows `.ps1` (sent as a file, run with `powershell -File`):
  * download + sha256-verify the tray asset of the SAME pinned release BEFORE anything is stopped;
  * stop the tray, back it up to `.pre-<version>`, replace it, verify the installed sha;
  * relaunch it through a temporary `BUILTIN\\Users` (Limited) scheduled task -- it starts the tray
    in the logged-on user's session with no password -- and unregister the task;
  * verify one tray process in an interactive session.
A tray failure is a named WARNING in the roll summary, never a service rollback: the tray is UI,
the service is the clock.

Tier-0: the emitted program is read as text (no Windows here); the orchestrator is driven end to end
with PATH stubs for sshpass (ssh + scp) and the gate's own /status fixture seam -- no box, no network.
"""
import json
import os
import pathlib
import re
import stat
import subprocess
import time

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_UPGRADE = _ROOT / "scripts" / "dantesync-fleet-upgrade.sh"
_STATUS = _ROOT / "tests" / "fixtures" / "dantesync_status_1372"
_TARGET = "1.11.1"


def _source(tmp_path, body, **env):
    script = tmp_path / "src.sh"
    script.write_text(f"set -euo pipefail\n. '{_UPGRADE}'\nset +e\n{body}\n")
    e = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "OBS_FLEET", "RIG_GRANDMASTER"))}
    e.update(env)
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=e)


@pytest.fixture()
def ps(tmp_path):
    r = _source(tmp_path, f"dantesync_windows_upgrade_ps {_TARGET}")
    assert r.returncode == 0, r.stderr
    return r.stdout


def _at(text, needle, start=0):
    i = text.find(needle, start)
    assert i >= 0, f"missing: {needle}"
    return i


def test_tray_release_url_is_the_pinned_tag_asset(tmp_path):
    r = _source(tmp_path, "dantesync_release_url_windows_tray 1.11.1")
    assert r.stdout.strip() == ("https://github.com/zbynekdrlik/dantesync/releases/download/"
                                "v1.11.1/dantesync-tray-windows-amd64.exe")


def test_the_tray_is_fetched_and_verified_before_anything_is_stopped(ps):
    stop = _at(ps, "Stop-Service dantesync")
    url = _at(ps, "releases/download/v1.11.1/dantesync-tray-windows-amd64.exe")
    sha = _at(ps, "(Get-FileHash -Algorithm SHA256 $trayTmp).Hash")
    assert url < stop and sha < stop, ps
    assert _at(ps, "'.sha256'", url) < stop
    # nothing is stopped before the service stop -- the tray included
    assert "Stop-Process" not in ps[:stop], ps[:stop]


def test_a_failed_tray_fetch_is_a_warning_never_an_abort(ps):
    """The fetch sits in its own try/catch that records a warning -- it must not throw, or the
    service upgrade (which follows) would be skipped for a UI binary."""
    stop = _at(ps, "Stop-Service dantesync")
    start = _at(ps, "$trayUrl = ")
    # the fetch block ends where the service backup begins (the service's own self-heal comment,
    # which says "rethrow", follows it and is not part of the tray fetch)
    end = _at(ps, "# 2. back up the current exe", start)
    assert end < stop
    fetch = ps[start:end]
    catch = fetch[fetch.index("} catch {"):]
    assert "$trayNotes +=" in catch and "throw" not in catch, catch


def test_the_tray_swap_runs_after_the_service_is_back(ps):
    service_start = _at(ps, "    Start-Service dantesync\n} catch {")
    service_catch_end = _at(ps, "    throw\n}", service_start)
    tray_stop = _at(ps, "Stop-Process -Force", service_catch_end)
    assert "-Name dantesync-tray" in ps[tray_stop - 120:tray_stop], ps[tray_stop - 120:tray_stop]
    assert tray_stop > service_catch_end


def test_the_tray_is_backed_up_replaced_and_its_installed_sha_verified(ps):
    swap = ps[_at(ps, "# 5. the tray"):]
    assert "$trayPre = 'C:\\Program Files\\DanteSync\\dantesync-tray.exe.pre-1.11.1'" in ps
    backup = _at(swap, "Copy-Item -Force $trayExe $trayPre")
    # review round 1: a re-run on the same target never overwrites the original pre-roll backup
    assert _at(swap, "if (-not (Test-Path $trayPre)) {") < backup
    replace = _at(swap, "Copy-Item -Force $trayTmp $trayExe")
    verify = _at(swap, "(Get-FileHash -Algorithm SHA256 $trayExe).Hash")
    assert backup < replace < verify, swap
    # a failed replace or a wrong installed sha restores the previous tray
    assert _at(swap, "Copy-Item -Force $trayPre $trayExe", verify) > verify


def test_the_tray_is_relaunched_through_a_temporary_builtin_users_task(ps):
    relaunch = ps[_at(ps, "# 6. relaunch the tray"):]
    # BUILTIN\Users by its well-known SID: the account NAME is localized (review round 1)
    assert "BUILTIN\\Users" in relaunch
    principal = _at(relaunch, "New-ScheduledTaskPrincipal -GroupId 'S-1-5-32-545' -RunLevel Limited")
    # Task Scheduler defaults would refuse a laptop on battery and time the tray out (review round 1)
    settings = _at(relaunch, "New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries "
                             "-DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)")
    register = _at(relaunch, "Register-ScheduledTask -TaskName $trayTask -Action $trayAction "
                             "-Principal $trayPrincipal -Settings $traySettings")
    start = _at(relaunch, "Start-ScheduledTask -TaskName $trayTask")
    fin = _at(relaunch, "} finally {", start)
    unregister = _at(relaunch, "Unregister-ScheduledTask -TaskName $trayTask -Confirm:$false -ErrorAction Stop", fin)
    assert principal < register and settings < register < start < fin < unregister, relaunch
    noted = relaunch[unregister:]
    assert "$trayNotes += ('the temporary task " in noted[:400], noted[:400]
    assert "-Execute $trayExe" in relaunch
    assert "-Password" not in relaunch and "-User " not in relaunch


def test_the_tray_is_relaunched_whenever_it_was_stopped(ps):
    """Review round 1: a failed backup, replace or sha check must never leave the operator without
    a tray -- the relaunch is its own step, run whenever the running tray was stopped."""
    swap_start = _at(ps, "# 5. the tray")
    relaunch = _at(ps, "# 6. relaunch the tray", swap_start)
    stop = _at(ps, "Stop-Process -Force", swap_start)
    flag = _at(ps, "$trayStopped = $true", stop)
    assert stop < flag < relaunch
    swap = ps[swap_start:relaunch]
    assert "} catch {" in swap and "Register-ScheduledTask" not in swap
    # review round 2: the same step also launches a current tray that is not running
    assert _at(ps, "if ($trayStopped -or $trayLaunch) {", relaunch) < _at(ps, "Register-ScheduledTask", relaunch)


def test_a_current_tray_that_is_not_running_is_relaunched(ps):
    """Review round 2: the likeliest warning (a relaunch that found nobody logged on) leaves the tray
    exe current but not running; a re-run must relaunch it, never report it OK unread."""
    swap_start = _at(ps, "# 5. the tray")
    relaunch = _at(ps, "# 6. relaunch the tray", swap_start)
    swap = ps[swap_start:relaunch]
    running = _at(swap, "$trayRunning = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue "
                        "| Where-Object { $_.SessionId -ge 1 })")
    assert running < _at(swap, "$trayLaunch = $true", running)
    assert _at(ps, "if ($trayStopped -or $trayLaunch) {", relaunch) < _at(ps, "Register-ScheduledTask", relaunch)
    report = ps[_at(ps, "already on sha256", relaunch) - 200:]
    assert "running in session ' + $trayRunning[0].SessionId" in report, report[:400]


def test_the_one_tray_check_is_read_after_the_task_is_unregistered(ps):
    """Review round 2: the count proves the tray outlives the task deletion."""
    relaunch = ps[_at(ps, "# 6. relaunch the tray"):]
    unregister = _at(relaunch, "Unregister-ScheduledTask -TaskName $trayTask")
    reread = _at(relaunch, "$trayProcs = @(Get-Process -Name dantesync-tray", unregister)
    assert reread < _at(relaunch, "$trayProcs.Count -ne 1", unregister)


def test_a_failed_restore_is_named_not_claimed(ps):
    """Review round 2: the restore after a failed swap is checked by hash before it is reported."""
    swap = ps[_at(ps, "# 5. the tray"):_at(ps, "# 6. relaunch the tray")]
    assert "(Get-FileHash -Algorithm SHA256 $trayPre).Hash" in swap
    assert "the previous tray restored" in swap and "may be partial" in swap


def test_the_tray_download_is_cleaned_up(ps):
    tail = ps[_at(ps, "# 6. relaunch the tray"):]
    assert "Remove-Item -Force -ErrorAction SilentlyContinue $trayTmp, ($trayTmp + '.sha256')" in tail


def test_a_tray_already_on_the_release_is_left_running(ps):
    """Review round 1: only the small .sha256 is fetched first; a tray already on the release is
    neither downloaded again nor restarted, so a re-run (or a SAME node) costs nothing."""
    fetch = ps[_at(ps, "$trayUrl = "):_at(ps, "# 2. back up the current exe")]
    sha_dl = _at(fetch, "($trayUrl + '.sha256')")
    current = _at(fetch, "(Get-FileHash -Algorithm SHA256 $trayExe).Hash -eq $trayExpected")
    exe_dl = _at(fetch, "-Uri $trayUrl -OutFile $trayTmp")
    assert sha_dl < current < exe_dl, fetch
    swap = ps[_at(ps, "# 5. the tray"):]
    assert "-not $trayCurrent" in swap and "already on sha256" in swap


def test_the_relaunch_is_verified_as_one_tray_process_in_an_interactive_session(ps):
    swap = ps[_at(ps, "# 5. the tray"):]
    assert "Where-Object { $_.SessionId -ge 1 }" in swap
    assert "$trayProcs.Count -ne 1" in swap
    assert "TRAY OK:" in swap and "TRAY-WARNING:" in swap


def test_a_tray_failure_never_throws_out_of_the_program(ps):
    """Every `throw` in the tray swap is caught by the tray arm's own catch, which only records a
    warning; nothing after it throws, so the program exits 0 when the service swap succeeded."""
    swap = ps[_at(ps, "# 5. the tray"):]
    outer_catch = swap.rindex("} catch {")
    assert "throw" not in swap[outer_catch:], swap[outer_catch:]
    assert swap.rstrip().endswith("& $exe --version")


def test_the_service_self_heal_and_the_dead_task_purge_are_unchanged(ps):
    assert ps.find("Copy-Item -Force $exe $bak") < ps.find("Copy-Item -Force $tmp $exe")
    assert "Copy-Item -Force $bak $exe -ErrorAction SilentlyContinue" in ps
    assert 'cmd /c "schtasks /Delete /TN \\"DanteSyncUpdate\\" /F >nul 2>&1"' in ps


@pytest.mark.parametrize("out,want", [
    ("TRAY OK: dantesync-tray.exe sha256 AB running in session 1\ndantesync 1.11.1\n",
     "OK dantesync-tray.exe sha256 AB running in session 1"),
    ("TRAY OK: first\nTRAY-WARNING: later\n", "WARNING later"),
    ("TRAY-WARNING: tray not fetched: 404\ndantesync 1.11.1\n", "WARNING tray not fetched: 404"),
    ("dantesync 1.11.1\n", "WARNING no tray report in the upgrade output"),
    ("", "WARNING no tray report in the upgrade output"),
])
def test_tray_outcome_is_read_from_the_program_output(tmp_path, out, want):
    (tmp_path / "out.txt").write_text(out)
    r = _source(tmp_path, f'dantesync_tray_outcome "$(cat "{tmp_path / "out.txt"}")"; echo')
    assert r.stdout.strip() == want, r.stdout + r.stderr


def test_help_documents_the_tray_arm(tmp_path):
    r = subprocess.run(["bash", str(_UPGRADE), "--help"], capture_output=True, text=True)
    assert r.returncode == 0
    assert "dantesync-tray" in r.stdout and "WARNING" in r.stdout
    assert "Exit codes" in r.stdout


def test_help_never_prints_the_default_ssh_password(tmp_path):
    """Review round 1: --help prints the whole header now, so the header must not carry the value."""
    default = re.search(r'SSH_PASS="\$\{SSH_PASS:-([^}]*)\}"', _UPGRADE.read_text()).group(1)
    r = subprocess.run(["bash", str(_UPGRADE), "--help"], capture_output=True, text=True)
    # the value may coincide with a rig LOGIN name shown in the usage examples, so the check is
    # scoped to the line that documents the password itself
    line = next(ln for ln in r.stdout.splitlines() if "SSH_PASS (default" in ln)
    assert default not in line, line


def test_the_tray_only_program_fetches_and_swaps_without_touching_the_service(tmp_path):
    r = _source(tmp_path, "dantesync_windows_tray_only_ps 1.11.1")
    prog = r.stdout
    assert prog.startswith("$ErrorActionPreference = 'Stop'"), prog[:80]
    assert "v1.11.1/dantesync-tray-windows-amd64.exe" in prog
    assert "# 5. the tray" in prog and "# 6. relaunch the tray" in prog and "TRAY-WARNING:" in prog
    for service in ("Stop-Service", "Start-Service", "dantesync-windows-amd64.exe", "--version"):
        assert service not in prog, service


def test_the_tray_arm_lives_in_its_own_lib():
    """Review round 1: the upgrade script stays under the ~1000-line budget; the emitted tray
    program and its outcome parser are their own sourced lib."""
    lib = (_ROOT / "scripts" / "lib" / "dantesync-tray-upgrade.sh").read_text()
    upgrade = _UPGRADE.read_text()
    for fn in ("dantesync_windows_tray_fetch_ps", "dantesync_windows_tray_swap_ps",
               "dantesync_windows_tray_only_ps", "dantesync_tray_outcome"):
        assert f"{fn}() {{" in lib and f"{fn}() {{" not in upgrade, fn
    assert '. "$HERE/lib/dantesync-tray-upgrade.sh"' in upgrade
    assert len(upgrade.splitlines()) <= 1000, len(upgrade.splitlines())


# ---------------------------------------------------------------------------------------------
# the orchestrator, end to end: a stateful sshpass stub stands in for the Windows node
# ---------------------------------------------------------------------------------------------

def _stubs(tmp_path, program_out, hosts):
    """A python `sshpass` on PATH that plays each Windows HOST (hosts: ip -> (start version, whether
    the -File program flips it to the target)): scp saves the .ps1 as uploaded-<ip>.ps1 (and
    uploaded.ps1, the last one), `--version` answers from version-<ip>, `-File` prints the stubbed
    program output."""
    b = tmp_path / "bin"
    b.mkdir()
    for ip, (start, flip) in hosts.items():
        (tmp_path / f"version-{ip}").write_text(start)
        if flip:
            (tmp_path / f"flip-{ip}").write_text("")
    (tmp_path / "program_out.txt").write_text(program_out)
    (b / "sshpass").write_text(
        "#!/usr/bin/env python3\n"
        "import pathlib, shutil, sys\n"
        f"d = pathlib.Path({str(tmp_path)!r})\n"
        "args = sys.argv[3:]\n"
        "tool = args[0]\n"
        "host = next(a for a in args[1:] if '@' in a).split('@', 1)[1].split(':', 1)[0]\n"
        "state = d / f'version-{host}'\n"
        "if tool == 'scp':\n"
        "    shutil.copy(args[-2], d / f'uploaded-{host}.ps1')\n"
        "    shutil.copy(args[-2], d / 'uploaded.ps1')\n"
        "    sys.exit(0)\n"
        "cmd = args[-1]\n"
        "if '-File' in cmd:\n"
        f"    if (d / f'flip-{{host}}').exists(): state.write_text({_TARGET!r})\n"
        "    sys.stdout.write((d / 'program_out.txt').read_text())\n"
        "    sys.exit(0)\n"
        "if '--version' in cmd:\n"
        "    print('dantesync ' + state.read_text().strip())\n"
        "    sys.exit(0)\n"
        "sys.exit(255)\n")
    for f in b.iterdir():
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return b


def _fresh_slave(tmp_path):
    s = json.loads((_STATUS / "stream-slave-1.11.1.json").read_text())
    now = int(time.time())
    s["updated_ts"] = now
    s["ntp_updated_ts"] = now - 1
    s["ntp_age_s"] = 1
    p = tmp_path / "stream.json"
    p.write_text(json.dumps(s))
    return p


def _roll(tmp_path, program_out, start="1.11.0", flip=True, extra=(), win="stream=user@10.77.9.204",
          hosts=None):
    b = _stubs(tmp_path, program_out, hosts or {"10.77.9.204": (start, flip)})
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "RIG_GRANDMASTER", "GATE_"))}
    env.update({
        "PATH": f"{b}:/usr/bin:/bin",
        "SSH_PASS": "stub",
        "CAMBOX_OFFLINE_ACK": "",
        "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env"),
        # The gate treats an EMPTY GATE_LINUX as unset (`${GATE_LINUX:-cam1=... cam2=...}`), so ""
        # never disabled its default cam nodes: on dev1 the live cams answered and hid that the
        # verify of ONE Windows node also graded cam1/cam2. Unreachable TEST-NET defaults make that
        # coupling fail here as it fails on a CI runner (no rig network).
        "GATE_LINUX": "cam1=192.0.2.1 cam2=192.0.2.2",
        "GATE_WAIT_TRIES": "1",
        "RIG_GRANDMASTER_IP": "10.77.9.230",
        "DANTESYNC_GATE_WIN_HTTP_STREAM": str(_fresh_slave(tmp_path)),
        "DANTESYNC_SAMPLE_COUNT": "1",
        "DANTESYNC_SAMPLE_WINDOW_S": "0",
        "DANTESYNC_SAMPLE_MIN_DISTINCT": "1",
    })
    return subprocess.run(["bash", str(_UPGRADE), "--win", win, "--target", _TARGET,
                           *extra], capture_output=True, text=True, env=env)


def test_roll_reports_a_tray_warning_by_name_and_keeps_the_service(tmp_path):
    r = _roll(tmp_path, "TRAY-WARNING: tray not fetched: The remote server returned an error: (404)\n"
                        f"dantesync {_TARGET}\n")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "rolled back" not in out and "ROLLBACK" not in out
    assert "[stream] verified" in out
    assert re.search(r"WARNING: dantesync-tray was NOT refreshed on 1 node", out), out
    assert "stream: tray not fetched: The remote server returned an error: (404)" in out
    assert "tray-windows-amd64.exe" in (tmp_path / "uploaded.ps1").read_text()


def test_roll_with_a_refreshed_tray_prints_no_warning(tmp_path):
    r = _roll(tmp_path, "TRAY OK: dantesync-tray.exe sha256 3C37CB51A064 running in session 1\n"
                        f"dantesync {_TARGET}\n")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "[stream] tray OK: dantesync-tray.exe sha256 3C37CB51A064 running in session 1" in out
    assert "NOT refreshed" not in out


def test_roll_reports_tray_warnings_on_the_canary_abort_too(tmp_path):
    """Review round 1: the summary is printed before EVERY exit after the roll started -- here the
    service verify fails (the version never flips), the canary aborts with exit 10."""
    r = _roll(tmp_path, "TRAY-WARNING: tray: expected one tray process in an interactive session\n",
              flip=False)
    out = r.stdout + r.stderr
    assert r.returncode == 10, out
    assert "WARNING: dantesync-tray was NOT refreshed on 1 node" in out, out
    assert "stream: tray: expected one tray process" in out


def test_roll_refreshes_the_tray_of_a_node_already_on_the_target(tmp_path):
    """Review round 1: a TRAY-WARNING on an earlier roll is repaired by simply re-running it -- a
    Windows node whose service is already on the target gets the tray-only program."""
    r = _roll(tmp_path, "TRAY OK: dantesync-tray.exe sha256 3C37CB51A064 running in session 1\n",
              start=_TARGET)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "[stream] tray OK: dantesync-tray.exe sha256 3C37CB51A064 running in session 1" in out
    uploaded = (tmp_path / "uploaded.ps1").read_text()
    assert "dantesync-tray-windows-amd64.exe" in uploaded and "Stop-Service" not in uploaded


def test_dry_run_names_the_tray_check_and_uploads_nothing(tmp_path):
    r = _roll(tmp_path, "TRAY OK: x\n", start=_TARGET, extra=("--dry-run",))
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "DRY-RUN" in out and "tray" in out and "stream" in out
    assert not (tmp_path / "uploaded.ps1").exists()


def test_roll_refreshes_the_tray_of_a_current_node_in_a_mixed_fleet(tmp_path):
    """Review round 2: after the roll, a Windows node already on the target gets the tray-only
    program too (not only on the all-current early exit)."""
    r = _roll(tmp_path, "TRAY OK: dantesync-tray.exe sha256 3C37CB51A064 running in session 1\n",
              win="stream=user@10.77.9.204 mbc=user@10.77.7.232",
              hosts={"10.77.9.204": ("1.11.0", True), "10.77.7.232": (_TARGET, False)})
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "[stream] verified" in out and "[mbc] tray OK:" in out, out
    tray_only = (tmp_path / "uploaded-10.77.7.232.ps1").read_text()
    assert "dantesync-tray-windows-amd64.exe" in tray_only and "Stop-Service" not in tray_only
    assert "Stop-Service" in (tmp_path / "uploaded-10.77.9.204.ps1").read_text()
