"""Shared harness for the issue-1372 dantesync fleet-upgrade pytest files (not collected: no
test_ prefix). `_source` sources scripts/dantesync-fleet-upgrade.sh (its source-guard stops
before the flow) and runs a body; `_roll` drives the whole orchestrator with a stateful
`sshpass` stub on PATH that plays each Windows node and keeps every uploaded .ps1 in order.
Used by test_dantesync_fleet_upgrade_tray_1372.py and test_dantesync_date_state_1372.py."""
import json
import os
import pathlib
import stat
import subprocess
import time

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


def _at(text, needle, start=0):
    i = text.find(needle, start)
    assert i >= 0, f"missing: {needle}"
    return i


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
        "    shutil.copy(args[-2], d / f'upload-{len(list(d.glob(\"upload-*.ps1\")))}.ps1')\n"
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
