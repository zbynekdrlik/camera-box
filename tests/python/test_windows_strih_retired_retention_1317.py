"""issue 1317 part 3 -- the two retention drivers no longer default to the RETIRED Windows strih.

The Windows strih PC (STRIH-SNV) was retired at the M4 cut-over (20.9.2026); its address 10.77.9.202
is the Linux strih-lx now (the ONE fleet list, scripts/lib/obs-fleet.sh). Both dev1 retention
drivers used to default HOST=10.77.9.202 and scp a PowerShell .ps1 at it:

  * scripts/strih-recordings-retention.sh -- Windows-only (.ps1 via `powershell -File`). strih-lx
    records to /srv/_REC on Linux (its OBS profile RecFilePath) and has no .ps1 executor, so the
    default is DROPPED: --host is required and a linux-genlock fleet address is refused.
  * scripts/obs-backup-retention.sh -- gains `--box <fleet-name>`, which dispatches on the fleet
    CLASS (windows-genlock -> the .ps1 driver, linux-genlock -> ssh + the bash --local-sweep, the
    same leg imag uses). No default box; `--host` refuses a linux-genlock fleet address.

Tier-0: bash only, with a fake `sshpass` first on PATH that logs its argv -- no network, no rig.
"""

import os
import stat
import subprocess
import tempfile

_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
REC = os.path.join(_ROOT, "scripts", "strih-recordings-retention.sh")
BAK = os.path.join(_ROOT, "scripts", "obs-backup-retention.sh")


def _fake_sshpass(tmp):
    """A fake sshpass on PATH: logs its argv (and drains stdin) into calls.log, exits 0."""
    bindir = os.path.join(tmp, "bin")
    os.makedirs(bindir)
    log = os.path.join(tmp, "calls.log")
    path = os.path.join(bindir, "sshpass")
    with open(path, "w") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "%s\\n" "$*" >> "' + log + '"\n'
            "cat >/dev/null 2>&1 || true\n"
            "exit 0\n"
        )
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)
    env = dict(os.environ)
    env["PATH"] = bindir + os.pathsep + env.get("PATH", "")
    for k in ("OBS_FLEET", "STRIH_LX_HOST"):
        env.pop(k, None)
    return env, log


def _run(script, args, env):
    return subprocess.run(["bash", script] + args, capture_output=True, text=True, env=env,
                          stdin=subprocess.DEVNULL, timeout=60)


def _calls(log):
    return open(log).read() if os.path.exists(log) else ""


# --- strih-recordings-retention.sh ---------------------------------------------------------------

def test_recordings_retention_has_no_default_box():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(REC, [], env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert "--host" in r.stderr and "RETIRED" in r.stderr, r.stderr
        assert _calls(log) == "", "no scp/ssh without a named box"


def test_recordings_retention_refuses_the_linux_strih_lx():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        for host in ("10.77.9.202", "strih-lx"):
            r = _run(REC, ["--host", host], env)
            assert r.returncode == 2, (host, r.stdout + r.stderr)
            assert "linux-genlock" in r.stderr, r.stderr
        assert _calls(log) == "", "a Windows .ps1 is never scp'd at the Linux strih-lx"


def test_recordings_retention_still_drives_an_explicit_windows_box():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(REC, ["--host", "10.77.9.204", "--record-dir", "C:\\Users\\newlevel\\Videos"], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "scp -O" in calls and "newlevel@10.77.9.204:" in calls, calls
        assert "powershell -NoProfile -ExecutionPolicy Bypass -File" in calls, calls
        assert "-Execute" not in calls, "dry-run by default"


# --- obs-backup-retention.sh ---------------------------------------------------------------------

def test_backup_retention_has_no_default_box():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(BAK, [], env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert "--box" in r.stderr and "RETIRED" in r.stderr, r.stderr
        assert _calls(log) == ""


def test_backup_retention_host_refuses_the_linux_strih_lx():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(BAK, ["--host", "10.77.9.202"], env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert "linux-genlock" in r.stderr and "--box" in r.stderr, r.stderr
        assert _calls(log) == "", "the .ps1 driver never targets the Linux strih-lx"


def test_backup_retention_box_strih_lx_runs_the_bash_local_sweep_over_ssh():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(BAK, ["--box", "strih-lx"], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "newlevel@10.77.9.202" in calls, calls
        assert "bash -s -- --local-sweep" in calls, calls
        assert "--backup-root '/opt/obs-backup' --stage-parent '/tmp'" in calls, calls
        assert "--execute" not in calls, "dry-run by default"
        assert "powershell" not in calls and "scp" not in calls, "no Windows driver for Linux: " + calls


def test_backup_retention_box_stream_uses_the_windows_driver():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(BAK, ["--box", "stream"], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _calls(log)
        assert "scp -O" in calls and "newlevel@10.77.9.204:" in calls, calls
        assert "powershell -NoProfile -ExecutionPolicy Bypass -File" in calls, calls


def test_backup_retention_unknown_box_is_a_usage_error():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _fake_sshpass(tmp)
        r = _run(BAK, ["--box", "strih"], env)
        assert r.returncode == 2, r.stdout + r.stderr
        assert "not in the fleet list" in r.stderr, r.stderr
        assert _calls(log) == ""


def test_backup_retention_local_sweep_is_unchanged():
    # the on-box leg (fed via `bash -s`) needs no fleet lib and no target -- it sweeps local dirs.
    with tempfile.TemporaryDirectory() as tmp:
        env, _log = _fake_sshpass(tmp)
        root = os.path.join(tmp, "root")
        stage = os.path.join(tmp, "stage")
        os.makedirs(os.path.join(root, "2026-01-01T00-00-00-789"))
        os.makedirs(os.path.join(stage, "genlock-stage-abc123"))
        os.makedirs(os.path.join(stage, "operator-dir"))
        r = _run(BAK, ["--local-sweep", "--backup-root", root, "--stage-parent", stage], env)
        assert r.returncode == 0, r.stdout + r.stderr
        assert "DRY-RUN" in r.stdout and "genlock-stage-abc123" in r.stdout, r.stdout
