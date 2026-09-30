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

The last section pins the ROLLBACK date-state rule of the dantesync 1.15.0 slice (main's design,
issue comment 5908602207): a date master rolled back below 1.15.0 deletes date-offset.json. Both
emitted rollback programs are RUN there (the Linux script with PATH stubs, the .ps1 under pwsh), not
only read as text.
"""
import json
import os
import pathlib
import re
import shutil
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
    # race-fix review round 2: a restore is never "proven" by two unreadable (null) hashes
    assert "$trayRestoredHash = (Get-FileHash -Algorithm SHA256 $trayExe -ErrorAction Stop).Hash" in swap
    assert ("$trayRestored = ($trayRestoredHash -and $trayRestoredHash -eq "
            "(Get-FileHash -Algorithm SHA256 $trayPre).Hash)") in swap


# ---------------------------------------------------------------------------------------------
# a tray that re-spawns during the swap (the 1.12.0 roll, 27.9.2026): the arm reported "the running
# tray did not exit" once each on stream, mbc and fohabl, then OK on a re-run. The kill worked; the
# wait and the re-check went by NAME, so a tray started meanwhile (the Task Scheduler, the HKLM Run
# `DanteSyncTray` entry) read as the killed one still running.
# ---------------------------------------------------------------------------------------------

def _swap(ps):
    return ps[_at(ps, "# 5. the tray"):_at(ps, "# 6. relaunch the tray")]


def test_the_killed_tray_is_waited_on_by_its_pids_never_by_name(ps):
    swap = _swap(ps)
    assert "Wait-Process -Name dantesync-tray" not in ps, "a wait by NAME also waits for a relaunched tray"
    capture = _at(swap, "$trayKilled = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue)")
    kill = _at(swap, "$trayKilled | Stop-Process -Force", capture)
    pids = _at(swap, "$trayPids = @($trayKilled | ForEach-Object { $_.Id })", kill)
    wait = _at(swap, "Wait-Process -Id $trayPids -Timeout 15", pids)
    # race-fix review round 1: a reused PID is not the tray -- only a still-alive dantesync-tray counts
    check = _at(swap, "@(Get-Process -Id $trayPids -ErrorAction SilentlyContinue | Where-Object "
                      "{ $_.ProcessName -eq 'dantesync-tray' }).Count -gt 0", wait)
    assert "did not exit" in swap[check:check + 200], swap[check:check + 200]
    # nothing between the wait and the backup re-reads the tray by name and throws on it
    backup = _at(swap, "Copy-Item -Force $trayExe $trayPre", check)
    assert "throw" not in swap[check + 200:backup], swap[check:backup]


def test_a_relaunched_tray_is_killed_again_right_before_the_replace(ps):
    """Bounded to 3 attempts, and nothing but the `try {` of the replace runs after the loop."""
    swap = _swap(ps)
    backup = _at(swap, "Copy-Item -Force $trayExe $trayPre")
    loop = _at(swap, "for ($trayTry = 1; $trayTry -le 3; $trayTry++) {", backup)
    replace = _at(swap, "Copy-Item -Force $trayTmp $trayExe", loop)
    body = swap[loop:replace]
    assert "$trayFresh = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue)" in body
    assert "if ($trayFresh.Count -eq 0) { break }" in body
    assert "$trayRespawnedBy += (Get-TrayParent $trayP.Id)" in body, "who relaunched it is recorded"
    assert "$trayFresh | Stop-Process -Force" in body
    tail = body[body.rindex("Wait-Process -Id"):]
    assert re.fullmatch(r"Wait-Process -Id [^\n]*\n\s*\}\n\s*try \{\n\s*", tail), repr(tail)


def test_the_parent_of_a_relaunched_tray_is_named_never_thrown(ps):
    """The parent (svchost -s Schedule, explorer for the Run key) is what the warning names."""
    fn = _at(ps, "function Get-TrayParent(")
    assert fn < _at(ps, "Get-TrayParent $trayP.Id"), "defined before its first use"
    body = ps[fn:_at(ps, "\n}\n", fn)]
    assert "Get-CimInstance Win32_Process -Filter ('ProcessId=' + $trayChildId)" in body
    assert "ParentProcessId" in body and "CommandLine" in body
    catch = body[body.index("} catch {"):]
    assert "return (" in catch and "throw" not in catch, catch


def test_a_copy_blocked_by_a_relaunched_tray_is_a_named_warning_without_a_restore(ps):
    """A copy that fails because a fresh tray holds the exe never wrote the file, so the previous
    tray is untouched; a restore would hit the same lock and wrongly report a partial exe."""
    swap = _swap(ps)
    replace = _at(swap, "Copy-Item -Force $trayTmp $trayExe")
    catch = swap[_at(swap, "} catch {", replace):]
    in_use = _at(catch, "$trayInUse = ")
    assert "-band 0xFFFF" in catch[in_use:in_use + 300], catch[in_use:in_use + 300]
    assert "32" in catch[in_use:in_use + 300] and "33" in catch[in_use:in_use + 300]
    warn = _at(catch, "'a tray keeps relaunching: '")
    restore = _at(catch, "Copy-Item -Force $trayPre $trayExe")
    assert in_use < warn < restore, catch
    assert "} else {" in catch[warn:restore], "the restore runs only when the file was not in use"
    assert "the previous tray exe is untouched" in catch[warn:restore]


def test_untouched_is_proven_by_the_hash_never_assumed(ps):
    """Race-fix review round 1: error 32 (a running exe) fails before the file is opened for write, but 33
    (lock violation) can come after truncation. So "untouched" compares the exe's hash with the one
    read before the copy; anything else restores."""
    swap = _swap(ps)
    backup = _at(swap, "Copy-Item -Force $trayExe $trayPre")
    before = _at(swap, "$trayBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath $trayExe -ErrorAction Stop).Hash",
                 backup)
    # race-fix review round 2: on Windows PowerShell 5.1 Get-FileHash is a script function whose
    # read failure is a non-terminating error -> a null hash; two null reads must never compare equal
    unreadable = _at(swap, "if (-not $trayBefore) { throw 'could not hash the tray exe before the replace' }", before)
    assert unreadable < _at(swap, "for ($trayTry = 1;", backup)
    catch = swap[_at(swap, "} catch {", _at(swap, "Copy-Item -Force $trayTmp $trayExe")):]
    now = _at(catch, "$trayNowHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $trayExe -ErrorAction Stop).Hash")
    proof = _at(catch, "$trayUntouched = ($trayNowHash -and $trayNowHash -eq $trayBefore)", now)
    assert _at(catch, "$trayInUse = ") < proof
    first_branch = _at(catch, "if ($trayUntouched -and $trayNew.Count -gt 0) {", proof)
    assert first_branch < _at(catch, "'a tray keeps relaunching: '") < _at(catch, "Copy-Item -Force $trayPre $trayExe")


def test_a_tray_that_did_not_die_is_named_apart_from_a_relaunch(ps):
    """Race-fix review round 1: "keeps relaunching" only for a NEW PID holding the exe; a tray the arm already
    tried to kill is "did not die"; nothing holding it is "locked by another process"."""
    swap = _swap(ps)
    loop = swap[_at(swap, "for ($trayTry = 1;"):_at(swap, "Copy-Item -Force $trayTmp $trayExe")]
    record = _at(loop, "if ($trayTried -notcontains $trayP.Id) {")
    assert record < _at(loop, "$trayRespawnedBy += (Get-TrayParent $trayP.Id)", record)
    assert "$trayTried += $trayP.Id" in loop
    assert "$trayTried = @($trayPids)" in swap
    catch = swap[_at(swap, "} catch {", _at(swap, "Copy-Item -Force $trayTmp $trayExe")):]
    assert "$trayNew = @($trayHolders | Where-Object { $trayTried -notcontains $_.Id })" in catch
    assert "$trayStuck = @($trayHolders | Where-Object { $trayTried -contains $_.Id })" in catch
    relaunching = _at(catch, "'a tray keeps relaunching: '")
    stuck = _at(catch, "'a tray did not die (pid '", relaunching)
    locked = _at(catch, "'the tray exe is locked by another process", stuck)
    assert locked < _at(catch, "Copy-Item -Force $trayPre $trayExe", locked)


def test_the_parent_command_line_is_cut(ps):
    """Race-fix review round 1: the parent's command line lands in the roll summary; an unexpected parent's
    arguments must not be printed in full."""
    fn = _at(ps, "function Get-TrayParent(")
    body = ps[fn:_at(ps, "\n}\n", fn)]
    assert "if ($trayCmd.Length -gt 120) { $trayCmd = $trayCmd.Substring(0, 120) + '...' }" in body


def test_tray_ok_says_when_a_running_tray_was_kept(ps):
    """Race-fix review round 1: TRAY OK is true when the swap succeeded (any tray kept started from the new
    exe), but it names that the tray was kept, not launched."""
    relaunch = ps[_at(ps, "# 6. relaunch the tray"):]
    guard = _at(relaunch, "if ($trayProcs.Count -eq 0) {")
    kept = _at(relaunch, "} else {\n            $trayKept = $true\n        }", guard)
    assert kept < _at(relaunch, "$trayProcs.Count -ne 1", kept)
    report = relaunch[_at(relaunch, "if ($trayNotes.Count -gt 0) {\n    Write-Output ('TRAY-WARNING: '"):]
    assert "if ($trayKept) {" in report and "not launched" in report


def test_the_relaunch_never_starts_a_second_tray(ps):
    """A tray relaunched meanwhile (by the scheduler or the Run entry) is kept; starting another
    would leave two and fail the one-tray check."""
    relaunch = ps[_at(ps, "# 6. relaunch the tray"):]
    running = _at(relaunch, "$trayProcs = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue "
                            "| Where-Object { $_.SessionId -ge 1 })")
    guard = _at(relaunch, "if ($trayProcs.Count -eq 0) {", running)
    assert running < guard < _at(relaunch, "Register-ScheduledTask"), relaunch[:800]


def test_tray_ok_names_a_relaunched_tray_that_was_killed_first(ps):
    report = ps[_at(ps, "if ($trayNotes.Count -gt 0) {\n    Write-Output ('TRAY-WARNING: '"):]
    assert "$trayRespawnedBy.Count -gt 0" in report
    assert "killed a relaunched tray before the swap" in report


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
          hosts=None, env_extra=None):
    b = _stubs(tmp_path, program_out, hosts or {"10.77.9.204": (start, flip)})
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "RIG_GRANDMASTER", "GATE_",
                                "NTP_MASTER"))}
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
    env.update(env_extra or {})
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


def test_roll_names_a_relaunching_tray_and_keeps_the_service(tmp_path):
    """The stubbed node plays a tray that re-spawned during the swap: the roll exits 0, the service
    is verified, and the summary names who keeps relaunching the tray."""
    parent = "svchost.exe (C:\\Windows\\system32\\svchost.exe -k netsvcs -p -s Schedule)"
    r = _roll(tmp_path, f"TRAY-WARNING: a tray keeps relaunching: {parent}; the previous tray exe is "
                        "untouched: The process cannot access the file because it is being used by "
                        f"another process.\ndantesync {_TARGET}\n")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "[stream] verified" in out and "rolled back" not in out
    assert "WARNING: dantesync-tray was NOT refreshed on 1 node" in out, out
    assert f"stream: a tray keeps relaunching: {parent}" in out, out


def test_roll_reports_a_tray_that_was_killed_once_before_the_swap(tmp_path):
    """The one-time respawn: the stubbed node reports TRAY OK naming the relaunch it killed, and the
    roll prints it as OK with no warning. Like every roll case here, this tests how the outcome line
    is read; the emitted PowerShell itself is run against stubbed cmdlets by
    tests/pwsh/run_dantesync_tray_swap_1372.sh (needs pwsh, not on dev1)."""
    ok = ("TRAY OK: dantesync-tray.exe sha256 93748C27 running in session 1 (killed a relaunched tray "
          "before the swap, started by: svchost.exe (C:\\Windows\\system32\\svchost.exe -k netsvcs -p -s Schedule))")
    r = _roll(tmp_path, f"{ok}\ndantesync {_TARGET}\n")
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "[stream] tray OK: dantesync-tray.exe sha256 93748C27 running in session 1 (killed a relaunched" in out
    assert "NOT refreshed" not in out


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
    assert 'if dantesync_is_ntp_master "$name" "$NTP_MASTER"; then role=ntp-master; fi' in node, node
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
    for fn in ("dantesync_rollback_clears_date_state", "dantesync_linux_rollback_cmd",
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
