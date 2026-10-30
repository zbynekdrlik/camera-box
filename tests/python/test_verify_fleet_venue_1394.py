"""Issue 1394: the fleet read-back reaches EVERY venue's camboxes.

A closed ticket whose fix is provisioning stays incomplete until scripts/verify-fleet.sh passes on
every box (the provisioning rule). verify-fleet.sh dialled only the camera_resolve addresses
(10.77.9.6x), so the Poprad camboxes (10.76.9.x through companion-pp) were unreachable from it.
`--venue <name>` reads the venue's cameras from the ONE venue table (dantesync_fleet_venue_camera_rows,
the same rows `cambox-ro-units-apply.sh --venue` dials) and hands verify-device.sh the dial address in
VERIFY_DEVICE_IP; the camera NAME still drives every per-box fact.
"""

import os
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FLEET = REPO / "scripts" / "verify-fleet.sh"
DEVICE = REPO / "scripts" / "verify-device.sh"
DS_FLEET = REPO / "scripts" / "lib" / "dantesync-fleet.sh"


def _exec(path, body):
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def _venue_rows(venue):
    r = subprocess.run(["bash", "-c", '. "$1"; dantesync_fleet_venue_camera_rows "$2"', "h",
                        str(DS_FLEET), venue], capture_output=True, text=True, timeout=60, cwd=REPO)
    assert r.returncode == 0, r.stderr
    rows = [l.split("|")[:2] for l in r.stdout.splitlines() if l]
    assert rows, "fixture premise: the venue lists cameras"
    return rows


def _fleet(tmp, *args, offline=()):
    tmp = Path(tmp)
    bindir = tmp / "bin"
    bindir.mkdir()
    probe_log = tmp / "probe.log"
    verify_log = tmp / "verify.log"
    off = " ".join(offline)
    _exec(bindir / "sshpass",
          "#!/usr/bin/env bash\n"
          f'host="${{@: -2:1}}"; echo "$host" >> "{probe_log}"\n'
          f'for o in {off}; do case "$host" in *"@$o") exit 255 ;; esac; done\nexit 0\n')
    stub = tmp / "verify-device-stub.sh"
    _exec(stub, f'#!/usr/bin/env bash\necho "$1 ${{VERIFY_DEVICE_IP:-<unset>}}" >> "{verify_log}"\nexit 0\n')
    env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"], VERIFY_CMD=str(stub))
    env.pop("CAMERA_SET", None)
    r = subprocess.run(["bash", str(FLEET), *args], capture_output=True, text=True, timeout=120, env=env)
    read = lambda p: p.read_text().splitlines() if p.exists() else []
    return r, read(probe_log), read(verify_log)


def test_venue_dials_every_camera_of_the_venue_at_its_venue_address(tmp_path):
    rows = _venue_rows("pp")
    r, probes, verifies = _fleet(tmp_path, "--venue", "pp")
    assert r.returncode == 0, r.stdout + r.stderr
    assert verifies == [f"{name} {addr}" for name, addr in rows], verifies
    assert probes == [f"root@{addr}" for _, addr in rows], probes
    assert "FLEET CONVERGED" in r.stdout + r.stderr


def test_venue_reports_an_offline_box_skipped_and_never_verifies_it(tmp_path):
    rows = _venue_rows("pp")
    off_name, off_addr = rows[0]
    r, _, verifies = _fleet(tmp_path, "--venue", "pp", offline=(off_addr,))
    assert r.returncode == 0, r.stdout + r.stderr
    assert all(not v.startswith(off_name + " ") for v in verifies), verifies
    assert f"[{off_name}] OFFLINE" in r.stdout + r.stderr


def test_an_unknown_or_missing_venue_verifies_nothing(tmp_path):
    for i, args in enumerate((("--venue", "nosuchvenue"), ("--venue",), ("--venue", ""))):
        d = tmp_path / f"c{i}"
        d.mkdir()
        r, probes, verifies = _fleet(d, *args)
        assert r.returncode == 2, (args, r.stdout + r.stderr)
        assert probes == [] and verifies == [], (args, probes, verifies)


def test_without_venue_the_set_mode_passes_no_dial_override(tmp_path):
    env_set = "cam2"
    d = Path(tmp_path)
    bindir = d / "bin"
    bindir.mkdir()
    log = d / "v.log"
    _exec(bindir / "sshpass", "#!/usr/bin/env bash\nexit 0\n")
    stub = d / "stub.sh"
    _exec(stub, f'#!/usr/bin/env bash\necho "$1 ${{VERIFY_DEVICE_IP:-<unset>}}" >> "{log}"\n')
    env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"], VERIFY_CMD=str(stub),
               CAMERA_SET=env_set, VERIFY_DEVICE_IP="10.99.99.99")
    r = subprocess.run(["bash", str(FLEET)], capture_output=True, text=True, timeout=60, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    # an inherited override must never re-point the camera_resolve addresses
    assert log.read_text().splitlines() == ["cam2 <unset>"], log.read_text()


# --- verify-device.sh: the dial override ---------------------------------------------------------

def test_verify_device_refuses_a_dial_override_that_is_not_ipv4(tmp_path):
    for bad in ("nope", "10.76.9", "10.76.9.300", "10.76.9.61;id", " 10.76.9.61"):
        r = subprocess.run(["bash", str(DEVICE), "cam1"], capture_output=True, text=True, timeout=60,
                           env=dict(os.environ, VERIFY_DEVICE_IP=bad))
        assert r.returncode == 1, (bad, r.stdout + r.stderr)
        assert "VERIFY_DEVICE_IP" in r.stderr and "issue 1394" in r.stderr, (bad, r.stderr)
        assert "== verify-device" not in r.stdout, (bad, r.stdout)


def test_verify_device_dials_the_override_address(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "ssh.log"
    # every ssh fails at once: only the banner and the dialled host matter here
    _exec(bindir / "sshpass", f'#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "{log}"\nexit 255\n')
    env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"],
               VERIFY_DEVICE_IP="10.76.9.61")
    r = subprocess.run(["bash", str(DEVICE), "cam1"], capture_output=True, text=True, timeout=300, env=env)
    assert "CAM1 @ 10.76.9.61" in r.stdout, r.stdout[:2000]
    dialled = log.read_text()
    assert "@10.76.9.61" in dialled and "@10.77.9.61" not in dialled, dialled[:2000]
