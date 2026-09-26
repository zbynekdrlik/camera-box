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
    fetch = ps[_at(ps, "$trayUrl = "):stop]
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
    replace = _at(swap, "Copy-Item -Force $trayTmp $trayExe")
    verify = _at(swap, "(Get-FileHash -Algorithm SHA256 $trayExe).Hash")
    assert backup < replace < verify, swap
    # a failed replace or a wrong installed sha restores the previous tray
    assert _at(swap, "Copy-Item -Force $trayPre $trayExe", verify) > verify


def test_the_tray_is_relaunched_through_a_temporary_builtin_users_task(ps):
    swap = ps[_at(ps, "# 5. the tray"):]
    principal = _at(swap, "New-ScheduledTaskPrincipal -GroupId 'BUILTIN\\Users' -RunLevel Limited")
    register = _at(swap, "Register-ScheduledTask -TaskName $trayTask")
    start = _at(swap, "Start-ScheduledTask -TaskName $trayTask")
    fin = _at(swap, "} finally {", start)
    unregister = _at(swap, "Unregister-ScheduledTask -TaskName $trayTask -Confirm:$false", fin)
    assert principal < register < start < fin < unregister, swap
    assert "-Execute $trayExe" in swap
    assert "-Password" not in swap and "-User " not in swap


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


# ---------------------------------------------------------------------------------------------
# the orchestrator, end to end: a stateful sshpass stub stands in for the Windows node
# ---------------------------------------------------------------------------------------------

def _stubs(tmp_path, program_out):
    b = tmp_path / "bin"
    b.mkdir()
    state = tmp_path / "version"
    state.write_text("1.11.0")
    (tmp_path / "program_out.txt").write_text(program_out)
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\n"
        "shift 2\n"
        'tool="$1"; shift\n'
        'last="${!#}"\n'
        'if [ "$tool" = scp ]; then cp "${@: -2:1}" "' + str(tmp_path / "uploaded.ps1") + '"; exit 0; fi\n'
        'case "$last" in\n'
        '  *-File*) echo "' + _TARGET + '" > "' + str(state) + '"; cat "' + str(tmp_path / "program_out.txt") + '" ;;\n'
        '  *--version*) echo "dantesync $(cat "' + str(state) + '")" ;;\n'
        "  *) exit 255 ;;\n"
        "esac\n")
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


def _roll(tmp_path, program_out):
    b = _stubs(tmp_path, program_out)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "RIG_GRANDMASTER", "GATE_"))}
    env.update({
        "PATH": f"{b}:/usr/bin:/bin",
        "SSH_PASS": "stub",
        "CAMBOX_OFFLINE_ACK": "",
        "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env"),
        "GATE_LINUX": "",
        "GATE_WAIT_TRIES": "1",
        "RIG_GRANDMASTER_IP": "10.77.9.230",
        "DANTESYNC_GATE_WIN_HTTP_STREAM": str(_fresh_slave(tmp_path)),
        "DANTESYNC_SAMPLE_COUNT": "1",
        "DANTESYNC_SAMPLE_WINDOW_S": "0",
        "DANTESYNC_SAMPLE_MIN_DISTINCT": "1",
    })
    return subprocess.run(["bash", str(_UPGRADE), "--win", "stream=user@10.77.9.204", "--target", _TARGET],
                          capture_output=True, text=True, env=env)


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
