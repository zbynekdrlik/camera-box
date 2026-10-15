"""The Windows dantesync upgrade's self-heal must ALWAYS start the service again.

Live 12.10.2026 (the 1.17.0 canary on stream): the swap `Copy-Item $tmp $exe` failed with "The process
cannot access the file ... because it is being used by another process", the catch's restore
`Copy-Item $bak $exe` hit the same lock and THREW (an IOException terminates under
`$ErrorActionPreference = 'Stop'` despite `-ErrorAction SilentlyContinue`), so the catch's
`Start-Service` never ran: the clock daemon sat STOPPED until a manual start.

The emitted upgrade .ps1 is RUN here under pwsh (the same harness shape as
test_dantesync_date_state_1372.py): service cmdlets and the download are stubs; Copy-Item is the real
cmdlet except that, once the service was stopped, a copy onto the installed exe throws the sharing
violation. A missing pwsh fails the test, never skips it.
"""

import os
import shutil
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(__file__))
from dantesync_upgrade_harness_1372 import _source  # noqa: E402


def _pwsh():
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not pwsh and os.access(home_pwsh, os.X_OK):
        pwsh = home_pwsh
    if not pwsh:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the upgrade .ps1 must really run)")
    return pwsh


def _run(tmp_path, locked_copies):
    """Run the upgrade program with the installed exe locked for the first LOCKED_COPIES copies onto
    it after the stop (0 = never locked). Returns (stdout lines, the CALL list, the exe text)."""
    exe, bak = tmp_path / "dantesync.exe", tmp_path / "dantesync.exe.bak"
    rel, temp = tmp_path / "release.exe", tmp_path / "temp"
    temp.mkdir()
    exe.write_text("the 1.16.0 exe\n")
    rel.write_text("the 1.17.0 exe\n")
    body = (f"DANTESYNC_WIN_EXE='{exe}'\nDANTESYNC_WIN_BAK='{bak}'\n"
            f"DANTESYNC_WIN_TRAY_EXE='{tmp_path / 'no-tray.exe'}'\n"
            "dantesync_windows_upgrade_ps 1.17.0")
    r = _source(tmp_path, body)
    assert r.returncode == 0, r.stderr
    program = tmp_path / "upgrade.ps1"
    program.write_text(r.stdout.replace("& $exe --version", "Write-Output 'version read'"))
    harness = tmp_path / "harness.ps1"
    harness.write_text(
        "$script:calls = New-Object System.Collections.Generic.List[string]\n"
        f"$script:rel = '{rel}'\n"
        f"$script:exe = '{exe}'\n"
        f"$script:locked = {locked_copies}\n"
        "$script:stopped = $false\n"
        "function Invoke-WebRequest { [CmdletBinding()] param([switch]$UseBasicParsing, $Uri, $OutFile)\n"
        "    if ($OutFile -like '*.sha256') {\n"
        "        Set-Content -NoNewline -LiteralPath $OutFile -Value ((Get-FileHash -Algorithm SHA256 -LiteralPath $script:rel).Hash + '  x.exe')\n"
        "    } else { Microsoft.PowerShell.Management\\Copy-Item -Force -LiteralPath $script:rel -Destination $OutFile } }\n"
        "function Copy-Item { [CmdletBinding()] param([Parameter(Position = 0)]$Path,\n"
        "        [Parameter(Position = 1)]$Destination, [switch]$Force)\n"
        "    if ($script:stopped -and \"$Destination\" -eq $script:exe -and $script:locked -gt 0) {\n"
        "        $script:locked--; $script:calls.Add('Copy-Item LOCKED ' + $Path)\n"
        "        throw [System.IO.IOException]::new(\"The process cannot access the file '$Destination' because it is being used by another process.\", -2147024864)\n"
        "    }\n"
        "    Microsoft.PowerShell.Management\\Copy-Item -Force -LiteralPath $Path -Destination $Destination }\n"
        "function Stop-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name)\n"
        "    $script:stopped = $true; $script:calls.Add('Stop-Service ' + $Name) }\n"
        "function Start-Service { [CmdletBinding()] param([Parameter(Position = 0)]$Name)\n"
        "    $script:calls.Add('Start-Service ' + $Name) }\n"
        "function Wait-Process { [CmdletBinding()] param($Name, $Timeout, $Id) }\n"
        "function Get-Process { [CmdletBinding()] param($Name) }\n"
        "function Stop-Process { [CmdletBinding()] param($Name, [switch]$Force) }\n"
        "function cmd { }\n"
        f"try {{ . '{program}' }} catch {{ Write-Output ('PROGRAM THREW: ' + $_.Exception.Message) }}\n"
        "foreach ($c in $script:calls) { Write-Output ('CALL ' + $c) }\n")
    env = dict(os.environ, TEMP=str(temp))
    run = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(harness)],
                         capture_output=True, text=True, timeout=180, env=env)
    assert run.returncode == 0, run.stdout + run.stderr
    out = run.stdout.splitlines()
    calls = [ln[len("CALL "):] for ln in out if ln.startswith("CALL ")]
    return out, calls, exe.read_text()


def test_a_locked_exe_whose_restore_also_fails_still_starts_the_service(tmp_path):
    out, calls, exe_text = _run(tmp_path, locked_copies=2)
    assert calls[0] == "Stop-Service dantesync", calls
    assert calls[-1] == "Start-Service dantesync", calls
    assert exe_text == "the 1.16.0 exe\n", "the locked exe was never written"
    assert any(ln.startswith("PROGRAM THREW: ") and "being used by another process" in ln for ln in out), out
    assert any("SELF-HEAL" in ln and "restore" in ln for ln in out), out


def test_a_locked_swap_with_a_good_restore_starts_the_service(tmp_path):
    out, calls, exe_text = _run(tmp_path, locked_copies=1)
    assert calls[-1] == "Start-Service dantesync", calls
    assert exe_text == "the 1.16.0 exe\n"
    assert any(ln.startswith("PROGRAM THREW: ") for ln in out), out


def test_an_unlocked_swap_upgrades_and_starts_once(tmp_path):
    out, calls, exe_text = _run(tmp_path, locked_copies=0)
    assert calls == ["Stop-Service dantesync", "Start-Service dantesync"], calls
    assert exe_text == "the 1.17.0 exe\n"
    assert "version read" in out, out
