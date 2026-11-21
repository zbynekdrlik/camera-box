"""Issue 1543: the netcfg watchdog logs in with the key-only read-only MikroTik user, not admin.

`admin` is disabled on every SNV device by design (mikrotik, network#3); the mikrotik project set up
`camerabox` (read-only, from dev1 only) for this key. With no NETCFG_SWITCH_PW the orchestrator must
call plain ssh with the key and that user; with neither a password nor a readable key it refuses.
"""
import os
import subprocess
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "netcfg-audit.sh"


def _env(tmp_path, **extra):
    env = {"PATH": f"{tmp_path / 'bin'}:/usr/bin:/bin", "HOME": str(tmp_path),
           "NETCFG_SSH_TIMEOUT": "2"}
    env.update(extra)
    return env


def _fake_ssh(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "ssh.log"
    fake = bindir / "ssh"
    fake.write_text(f'#!/bin/bash\nprintf "%s\\n" "$*" >> "{log}"\nexit 255\n')
    fake.chmod(0o755)
    return log


def test_without_a_password_it_uses_the_key_and_the_camerabox_user(tmp_path):
    log = _fake_ssh(tmp_path)
    key = tmp_path / "k"
    key.write_text("not a real key\n")
    r = subprocess.run(["bash", str(SCRIPT), "--json"], capture_output=True, text=True, timeout=120,
                       env=_env(tmp_path, NETCFG_SSH_KEY=str(key)))
    calls = log.read_text().splitlines()
    assert calls, r.stderr
    assert all(f"-i {key}" in c and "BatchMode=yes" in c and "IdentitiesOnly=yes" in c for c in calls)
    assert any("camerabox@10.77.9.2 " in c for c in calls)
    assert not any("admin@" in c for c in calls)


def test_the_user_and_key_can_be_overridden(tmp_path):
    log = _fake_ssh(tmp_path)
    key = tmp_path / "other"
    key.write_text("x\n")
    subprocess.run(["bash", str(SCRIPT), "--json"], capture_output=True, text=True, timeout=120,
                   env=_env(tmp_path, NETCFG_SSH_KEY=str(key), NETCFG_SSH_USER="ro2"))
    assert any(f"-i {key}" in c and "ro2@10.77.8.1 " in c for c in log.read_text().splitlines())


def test_no_password_and_no_readable_key_refuses(tmp_path):
    log = _fake_ssh(tmp_path)
    r = subprocess.run(["bash", str(SCRIPT), "--check"], capture_output=True, text=True, timeout=60,
                       env=_env(tmp_path, NETCFG_SSH_KEY=str(tmp_path / "missing")))
    assert r.returncode == 2
    assert "no credential" in r.stderr
    assert not log.exists()
