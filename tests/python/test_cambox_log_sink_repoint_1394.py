"""Issue 1394: re-point a LIVE cambox's off-box logging at its venue strih, without setup-device.

The owner ruled (9.10.2026) that the cambox log sink is the venue strih, never dev1. setup-device writes
the new files on a box it provisions; the seven live boxes get them through the existing issue-1394
read-only-root apply (scripts/lib/cambox-ro-units.sh + scripts/cambox-ro-units-apply.sh): given the
box's sink, the SAME verified rw window rewrites the netconsole setup script (0755), journal-upload.conf
and the upload drop-in where issue 1311 put them, and systemd-journal-upload restarts after the close.
The CLI resolves every box's sink before it touches the first box, and `--venue <name>` dials a venue's
cameras at their dev1 address (the Poprad boxes are 10.76.9.x through companion-pp).

The program runs on the issue-1407 fake read-only-root box (tests/python/ro_window_fakes_1407.py).
"""

import os
import re
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import build, index, log, root, run_text, starts_on_rw  # noqa: E402
from test_cambox_ro_units_1394 import UNITS, _box, _cli  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
REMOTE_LOG = REPO / "scripts" / "lib" / "remote-logging.sh"
SINK = "10.77.9.202"
SCRIPT = "/usr/local/sbin/cambox-netconsole-setup.sh"
CONF = "/etc/systemd/journal-upload.conf"
DROPIN_REL = "systemd-journal-upload.service.d/10-cambox-rostate.conf"


def _gen(fn, *args):
    return build(f'. "{REMOTE_LOG}"\n{fn} "$@"', *args)


def _program(unit_dir, fs, sink=SINK):
    return build(f'. "{UNITS}"\ncambox_ro_units_apply_program "$1"', sink,
                 env={"CAMBOX_RO_UNITS_UNIT_DIR": str(unit_dir), "CAMBOX_RO_UNITS_FS_ROOT": str(fs)})


def _fs(box):
    return box["state"] / "fs"


def _plant_old_logging(box, unit_dir, sink="10.77.9.200"):
    """The live state: the issue-1311 files pointing at dev1's old address."""
    fs = _fs(box)
    (fs / SCRIPT[1:]).parent.mkdir(parents=True, exist_ok=True)
    (fs / SCRIPT[1:]).write_text(_gen("remote_log_netconsole_setup_script_content", "10.77.9.202")
                                 .replace('SINK_IP="10.77.9.202"', f'DEV1_IP="{sink}"'))
    (fs / SCRIPT[1:]).chmod(0o755)
    (fs / CONF[1:]).parent.mkdir(parents=True, exist_ok=True)
    (fs / CONF[1:]).write_text(f"[Upload]\nURL=http://{sink}:19532\n")
    (unit_dir / DROPIN_REL).parent.mkdir(parents=True, exist_ok=True)
    (unit_dir / DROPIN_REL).write_text("[Service]\n# the pre-1394 drop-in\n")


def test_the_program_rewrites_the_logging_files_inside_the_window_and_restarts_after_it(tmp_path):
    box, unit_dir = _box(tmp_path)
    _plant_old_logging(box, unit_dir)
    proc = run_text(box, _program(unit_dir, _fs(box)), strict="set -euo pipefail")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    fs = _fs(box)
    assert (fs / SCRIPT[1:]).read_text() == _gen("remote_log_netconsole_setup_script_content", SINK)
    assert os.access(fs / SCRIPT[1:], os.X_OK)
    assert (fs / CONF[1:]).read_text() == f"[Upload]\nURL=http://{SINK}:19532\n"
    assert (unit_dir / DROPIN_REL).read_text() == _gen("remote_log_journal_upload_dropin_content")
    rw = index(calls, "mount -o remount,rw / root=ro")
    close = index(calls, "mount -o remount,ro / root=rw")
    assert all(rw < i < close for i, c in enumerate(calls) if c.startswith("mv ")), "\n".join(calls)
    mvs = [c for c in calls if c.startswith("mv ")]
    assert any(c.endswith("cambox-netconsole-setup.sh root=rw") for c in mvs), mvs
    assert any(c.endswith("journal-upload.conf root=rw") for c in mvs), mvs
    assert any(c.endswith("10-cambox-rostate.conf root=rw") for c in mvs), mvs
    nc = index(calls, "systemctl restart cambox-netconsole.service root=ro", close)
    ju = index(calls, "systemctl restart systemd-journal-upload.service root=ro", close)
    assert close < nc and close < ju
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert root(box) == "ro"
    assert f"off-box log sink {SINK}" in proc.stdout and "write: script conf dropin" in proc.stdout, proc.stdout


def test_a_second_run_finds_everything_in_place_and_opens_no_window(tmp_path):
    box, unit_dir = _box(tmp_path)
    _plant_old_logging(box, unit_dir)
    first = run_text(box, _program(unit_dir, _fs(box)))
    assert first.returncode == 0, first.stderr
    box["state"].joinpath("log").write_text("")
    again = run_text(box, _program(unit_dir, _fs(box)))
    calls = log(box)
    assert again.returncode == 0, again.stderr
    assert not any(c.startswith("mount ") for c in calls), "\n".join(calls)
    assert not any("systemd-journal-upload" in c and "restart" in c for c in calls), "\n".join(calls)
    assert "write: none (in place)" in again.stdout, again.stdout


def test_only_the_differing_file_is_written(tmp_path):
    box, unit_dir = _box(tmp_path)
    _plant_old_logging(box, unit_dir)
    assert run_text(box, _program(unit_dir, _fs(box))).returncode == 0
    fs = _fs(box)
    (fs / SCRIPT[1:]).chmod(0o644)  # a script that lost its executable bit
    box["state"].joinpath("log").write_text("")
    proc = run_text(box, _program(unit_dir, _fs(box)))
    assert proc.returncode == 0, proc.stderr
    assert "write: script" in proc.stdout and "conf" not in proc.stdout.split("write:")[1].split("\n")[0]
    assert os.access(fs / SCRIPT[1:], os.X_OK)
    calls = log(box)
    assert not any("systemd-journal-upload" in c and "restart" in c for c in calls), "\n".join(calls)


def test_a_box_without_the_1311_unit_gets_no_logging_file(tmp_path):
    box, unit_dir = _box(tmp_path, nc_present=False)
    proc = run_text(box, _program(unit_dir, _fs(box)))
    assert proc.returncode == 0, proc.stderr
    assert not (_fs(box) / SCRIPT[1:]).exists() and not (_fs(box) / CONF[1:]).exists()
    assert not any("systemd-journal-upload" in c for c in log(box))


def test_a_bad_sink_builds_no_program():
    import subprocess
    r = subprocess.run(["/bin/bash", "-c", f'set -euo pipefail\n. "{UNITS}"\ncambox_ro_units_apply_program "$1"',
                        "harness", "dev1.lan"], capture_output=True, text=True, timeout=60)
    assert r.returncode == 1 and r.stdout == "", r.stdout
    assert "issue 1394" in r.stderr


def test_without_a_sink_the_program_touches_no_logging_file(tmp_path):
    box, unit_dir = _box(tmp_path)
    _plant_old_logging(box, unit_dir)
    text = build(f'. "{UNITS}"\ncambox_ro_units_apply_program',
                 env={"CAMBOX_RO_UNITS_UNIT_DIR": str(unit_dir), "CAMBOX_RO_UNITS_FS_ROOT": str(_fs(box))})
    proc = run_text(box, text)
    assert proc.returncode == 0, proc.stderr
    assert "10.77.9.200" in (_fs(box) / CONF[1:]).read_text()
    assert "off-box log sink" not in proc.stdout


# --- the CLI ---------------------------------------------------------------------------------------------

def test_cli_plan_names_each_boxs_own_venue_sink(tmp_path):
    r, sshdir = _cli(tmp_path, "--plan", "--box", "cam1", "--box", "cam5")
    assert r.returncode == 0, r.stderr
    heads = re.findall(r"(?m)^== (cam\d) \(([\d.]+)\): .*log sink ([\d.]+);", r.stdout)
    assert heads == [("cam1", "10.77.9.61", "10.77.8.202"), ("cam5", "10.77.9.65", "10.77.9.202")], r.stdout
    assert "_rou_rl_sink=10.77.8.202" in r.stdout and "_rou_rl_sink=10.77.9.202" in r.stdout
    assert os.listdir(sshdir) == []


def test_cli_venue_dials_the_venue_cameras_at_their_dev1_address(tmp_path):
    r, _ = _cli(tmp_path, "--plan", "--venue", "pp")
    assert r.returncode == 0, r.stderr
    heads = re.findall(r"(?m)^== (cam\d) \(([\d.]+)\): .*log sink ([\d.]+);", r.stdout)
    assert heads == [("cam1", "10.76.9.61", "10.77.8.202"), ("cam4", "10.76.9.64", "10.77.8.202"),
                     ("cam6", "10.76.9.66", "10.77.8.202"), ("cam7", "10.76.9.67", "10.77.8.202")], r.stdout
    r, sshdir = _cli(tmp_path, "--apply", "--venue", "pp")
    assert r.returncode == 0, r.stdout + r.stderr
    hosts = sorted(next(a for a in (sshdir / f).read_text().splitlines() if a.startswith("root@"))
                   for f in os.listdir(sshdir) if f.startswith("argv-"))
    assert hosts == ["root@10.76.9.61", "root@10.76.9.64", "root@10.76.9.66", "root@10.76.9.67"], hosts


def test_cli_a_box_named_by_box_and_venue_is_applied_once(tmp_path):
    r, _ = _cli(tmp_path, "--plan", "--box", "cam4", "--venue", "pp")
    assert r.returncode == 0, r.stderr
    assert re.findall(r"(?m)^== (cam\d) ", r.stdout) == ["cam4", "cam1", "cam6", "cam7"], r.stdout
    assert "cam4 is named more than once" in r.stderr


def test_cli_refuses_an_unknown_venue_or_a_box_with_no_sink_before_touching_anything(tmp_path):
    r, sshdir = _cli(tmp_path, "--apply", "--venue", "nowhere")
    assert r.returncode == 2 and "lists no camera" in r.stderr, r.stderr
    assert os.listdir(sshdir) == []
    facts = tmp_path / "facts"
    facts.mkdir()
    for name in ("strih-snv.env",):
        text = (REPO / "scripts" / "strih-boxes" / name).read_text()
        facts.joinpath(name).write_text(re.sub(r"(?m)^STRIH_CAMERAS=.*$", "STRIH_CAMERAS=2 3", text))
    r, sshdir = _cli(tmp_path, "--apply", "--box", "cam2", "--box", "cam5", env={"STRIH_BOXES_DIR": str(facts)})
    assert r.returncode == 2, r.stdout + r.stderr
    assert "no log sink for cam5" in r.stderr, r.stderr
    assert os.listdir(sshdir) == [], "nothing is touched when one box has no sink"
