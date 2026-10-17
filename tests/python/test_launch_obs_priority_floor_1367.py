"""issue 1367 -- a launched OBS never runs below Normal CPU priority.

17.10.2026: the stream OBS (pid 18900) ran at BelowNormal although its global.ini says
ProcessPriority=Normal. It inherited the priority from its launcher: the win-* MCP server runs as a
scheduled task at priority 7 (BelowNormal), and every deploy/relaunch program runs from that MCP
Shell. The soak's on-box decode (also BelowNormal, issue 1260) then starved the OBS audio thread for
up to 34 s (`audio-stall #1367 wake_late_max_ms=33970`). The launch program now raises a started
obs64 that sits at Idle/BelowNormal to Normal, leaves Normal/AboveNormal/High alone, and fails loud
when it cannot.
"""
import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
LAUNCH = REPO / "scripts" / "launch-obs-genlock.sh"


def _program(box="stream-snv"):
    r = subprocess.run(["bash", str(LAUNCH), "--box", box, "--force"], capture_output=True, text=True,
                       cwd=str(REPO), timeout=120)
    assert r.returncode == 0, r.stderr
    parts = re.split(r"(?m)^# -{6,}.*$", r.stdout)
    assert len(parts) >= 3, r.stdout[:400]
    return parts[1]


def _pwsh():
    for c in (os.environ.get("PWSH"), shutil.which("pwsh"), str(Path.home() / ".local/pwsh74/pwsh")):
        if c and Path(c).exists():
            return c
    pytest.fail("no pwsh found (PWSH env, PATH or ~/.local/pwsh74/pwsh) -- this test needs one")


def _function(prog):
    m = re.search(r"(?ms)^function Set-CbObsPriorityFloor.*?^}\s*$", prog)
    assert m, "the launch program defines no Set-CbObsPriorityFloor function"
    return m.group(0)


def test_the_program_raises_the_started_obs_after_the_session_check_and_before_the_log_verify():
    prog = _program()
    fn = prog.index("function Set-CbObsPriorityFloor")
    call = prog.index("Set-CbObsPriorityFloor (Get-Process -Id")
    assert prog.index("SESSION OK: obs64 PID") < call < prog.index("(4) VERIFY the fresh OBS log")
    assert fn < call


@pytest.mark.parametrize("before,after", [
    ("BelowNormal", "Normal"), ("Idle", "Normal"),
    ("Normal", "Normal"), ("AboveNormal", "AboveNormal"), ("High", "High"),
])
def test_the_floor_raises_only_a_priority_below_normal(tmp_path, before, after):
    script = tmp_path / "t.ps1"
    script.write_text(_function(_program()) + "\n"
                      "$p = [pscustomobject]@{ PriorityClass = '%s' }\n"
                      "$note = Set-CbObsPriorityFloor $p\n"
                      "Write-Output \"$($p.PriorityClass)|$note\"\n" % before)
    r = subprocess.run([_pwsh(), "-NoProfile", "-File", str(script)], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    got, note = r.stdout.strip().split("|", 1)
    assert got == after, r.stdout
    assert before in note and after in note, note


def test_a_priority_that_stays_below_normal_throws(tmp_path):
    script = tmp_path / "t.ps1"
    # a process object whose PriorityClass refuses the write (stays BelowNormal)
    script.write_text(_function(_program()) + "\n"
                      "$p = [pscustomobject]@{}\n"
                      "$p | Add-Member -MemberType ScriptProperty -Name PriorityClass -Value { 'BelowNormal' } "
                      "-SecondValue { param($v) }\n"
                      "try { Set-CbObsPriorityFloor $p; Write-Output 'NO-THROW' } catch { Write-Output \"THREW:$_\" }\n")
    r = subprocess.run([_pwsh(), "-NoProfile", "-File", str(script)], capture_output=True, text=True, timeout=60)
    assert r.stdout.startswith("THREW:") and "BelowNormal" in r.stdout, r.stdout + r.stderr
