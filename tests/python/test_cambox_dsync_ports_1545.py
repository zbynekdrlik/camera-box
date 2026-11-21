"""issue 1545 -- a cambox's dsync lists BOTH onboard ports; the joined one must be the cabled one.

19.11.2026: cam3 ran the whole SNV service on NTP fallback because its cable sat in the other
onboard port than the one `--interface` named (issue 1439 pinned ONE port; dsync never falls back).
dsync v1.35.16 (dsync#212) takes a list (`--interface enp1s0,enp3s0`): it joins the listed NIC that
has an IPv4, re-joins on another listed one when PTP goes silent, never an unlisted NIC, and keeps
`ptp_interface_configured` as the list string. An older dsync would read the list as ONE name and
never join, so the list is written only for a dsync that reports >= 1.35.16.

What this pins:
  * `cambox_lan_nics SYSROOT` = every onboard `enp*` with a `device` link, carrier not required,
    sorted, comma-joined; none = rc 1 by name;
  * `cambox_dsync_list_capable VERSION` = rc 0 iff VERSION >= 1.35.16;
  * `cambox_dsync_iface_arg SYSROOT VERSION` = the list on a capable dsync, else the cabled port;
  * `cambox_dsync_unit_text` takes the list (each element checked);
  * `dsync_iface_verdict` takes the port it must have joined as a 5th argument (4 arguments =
    unchanged, the OBS boxes' item 6d);
  * verify-device (ba): the unit carries the list, /status configured == the list, joined == the
    cabled port; a capable dsync still pinned to one port FAILs as unit-not-bound;
  * the live apply writes the list on a capable box, the single cabled port on an older one.
"""
import json
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import log, root, run_text  # noqa: E402
from test_cambox_dsync_interface_1439 import (  # noqa: E402
    IFACE_LIB, OLD_UNIT, UNIT_LIB, _bash, _dsync_box, _program, _sysfs)

TWO_PORTS_CAM3 = {"lo": False, "enp1s0": True, "enp3s0": "nocarrier", "wlp2s0": False, "enx00e04c680185": True}


def _list(sysroot):
    return _bash('. "$1"\ncambox_lan_nics "$2"', str(UNIT_LIB), str(sysroot))


def test_the_port_list_is_every_onboard_port_sorted_whatever_the_carrier(tmp_path):
    r = _list(_sysfs(tmp_path, TWO_PORTS_CAM3))
    assert r.returncode == 0, r.stderr
    assert r.stdout == "enp1s0,enp3s0\n"


def test_one_port_is_a_list_of_one_and_no_port_fails_by_name(tmp_path):
    a = tmp_path / "a"
    a.mkdir()
    r = _list(_sysfs(a, {"enp2s0": True, "enx00e04c680185": True}))
    assert (r.returncode, r.stdout) == (0, "enp2s0\n"), r.stderr
    b = tmp_path / "b"
    b.mkdir()
    r = _list(_sysfs(b, {"lo": False, "enx00e04c680185": True, "enp3s0": False}))
    assert r.returncode == 1 and r.stdout == ""
    assert "cambox_lan_nics" in r.stderr, r.stderr


@pytest.mark.parametrize("ver,capable", [
    ("1.35.16", True), ("1.35.17", True), ("1.36.0", True), ("2.0.0", True),
    ("1.35.15", False), ("1.35.7", False), ("1.9.99", False), ("", False), ("garbage", False),
    ("1.35", False), ("1.35.16-rc1", False),
])
def test_only_a_dsync_from_1_35_16_takes_a_list(ver, capable):
    r = _bash('. "$1"\ncambox_dsync_list_capable "$2" && echo yes || echo no', str(UNIT_LIB), ver)
    assert r.stdout.strip() == ("yes" if capable else "no"), (ver, r.stdout, r.stderr)


def test_the_interface_argument_follows_the_dsync_version(tmp_path):
    sysroot = _sysfs(tmp_path, TWO_PORTS_CAM3)
    new = _bash('. "$1"\ncambox_dsync_iface_arg "$2" 1.35.16', str(UNIT_LIB), str(sysroot))
    old = _bash('. "$1"\ncambox_dsync_iface_arg "$2" 1.35.7', str(UNIT_LIB), str(sysroot))
    unknown = _bash('. "$1"\ncambox_dsync_iface_arg "$2" ""', str(UNIT_LIB), str(sysroot))
    assert (new.returncode, new.stdout) == (0, "enp1s0,enp3s0\n"), new.stderr
    assert (old.returncode, old.stdout) == (0, "enp1s0\n"), old.stderr
    assert (unknown.returncode, unknown.stdout) == (0, "enp1s0\n"), unknown.stderr


def test_a_capable_dsync_gets_the_list_even_with_no_cable_at_write_time(tmp_path):
    sysroot = _sysfs(tmp_path, {"enp1s0": "nocarrier", "enp3s0": "nocarrier"})
    r = _bash('. "$1"\ncambox_dsync_iface_arg "$2" 1.35.16', str(UNIT_LIB), str(sysroot))
    assert (r.returncode, r.stdout) == (0, "enp1s0,enp3s0\n"), r.stderr


def test_the_unit_carries_the_list_and_refuses_a_bad_element():
    good = _bash('. "$1"\ncambox_dsync_unit_text enp1s0,enp3s0', str(UNIT_LIB))
    assert good.returncode == 0, good.stderr
    assert good.stdout == OLD_UNIT.replace("--ntp-server strih.lan\n",
                                           "--ntp-server strih.lan --interface enp1s0,enp3s0\n")
    for bad in ("enp1s0,", ",enp1s0", "enp1s0,,enp3s0", "enp1s0,-x", "enp1s0,a b", "enp1s0;rm"):
        r = _bash('. "$1"\ncambox_dsync_unit_text "$2"', str(UNIT_LIB), bad)
        assert r.returncode == 1 and r.stdout == "", (bad, r.stdout)


def test_the_shared_verdict_takes_the_port_it_must_have_joined():
    def v(*a):
        return _bash('. "$1"\nshift\ndsync_iface_verdict "$@"', str(IFACE_LIB), *a).stdout.strip()
    assert v("1", "enp1s0,enp3s0", "enp1s0", "enp1s0,enp3s0", "enp1s0") == "ok:enp1s0"
    assert v("1", "enp1s0,enp3s0", "enp3s0", "enp1s0,enp3s0", "enp1s0") == "joined-elsewhere:enp3s0:enp1s0"
    assert v("1", "enp3s0", "enp3s0", "enp1s0,enp3s0", "enp1s0") == "configured-elsewhere:enp3s0:enp1s0,enp3s0"
    # four arguments: the issue-1439 grade, byte for byte (the OBS boxes' item 6d)
    assert v("1", "enp7s0", "enp7s0", "enp7s0") == "ok:enp7s0"
    assert v("1", "enp7s0", "rig0", "enp7s0") == "joined-elsewhere:rig0:enp7s0"


def _status(conf, joined):
    return json.dumps({"is_locked": True, "ptp_interface_configured": conf, "ptp_interface": joined})


def _gather(tmp_path, nics, unit_exec, body, version):
    sysroot = _sysfs(tmp_path, nics)
    unitf = tmp_path / "dsync.service"
    unitf.write_text("[Service]\nExecStart=" + unit_exec + "\n")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    (tmp_path / "body.json").write_text(body)
    (bindir / "curl").write_text('#!/bin/sh\ncat "%s"\n' % (tmp_path / "body.json"))
    (bindir / "curl").chmod(0o755)
    dsync = tmp_path / "dsync"
    dsync.write_text('#!/bin/sh\necho "dsync %s"\n' % version)
    dsync.chmod(0o755)
    r = _bash('. "$1"\nbash -c "$(cambox_dsync_gather_remote_snippet "$2" "$3" "$4")"',
              str(UNIT_LIB), str(sysroot), str(unitf), str(dsync),
              env={"PATH": "%s:%s" % (bindir, os.environ["PATH"])})
    assert r.returncode == 0, r.stderr
    v = _bash('. "$1"\ncambox_dsync_verify_verdict "$2"', str(UNIT_LIB), r.stdout)
    return v.stdout.strip(), r.stdout


LIST_EXEC = "/usr/local/bin/dsync --ntp-server strih.lan --interface enp1s0,enp3s0"


def test_ba_passes_a_listed_unit_joined_on_the_cabled_port(tmp_path):
    v, block = _gather(tmp_path, TWO_PORTS_CAM3, LIST_EXEC, _status("enp1s0,enp3s0", "enp1s0"), "1.35.16")
    assert v == "ok:enp1s0", block


@pytest.mark.parametrize("unit_exec,body,ver,want", [
    # a capable dsync still pinned to the one port: the 19.11 trap is still armed
    ("/usr/local/bin/dsync --ntp-server strih.lan --interface enp1s0", _status("enp1s0", "enp1s0"),
     "1.35.16", "unit-not-bound"),
    (LIST_EXEC, _status("enp1s0,enp3s0", "enp3s0"), "1.35.16", "joined-elsewhere:enp3s0:enp1s0"),
    (LIST_EXEC, _status("enp1s0", "enp1s0"), "1.35.16", "configured-elsewhere"),
])
def test_ba_fails_every_other_listed_state(tmp_path, unit_exec, body, ver, want):
    v, block = _gather(tmp_path, TWO_PORTS_CAM3, unit_exec, body, ver)
    assert v.startswith(want), (v, block)


def test_ba_keeps_the_single_port_grade_for_an_older_dsync(tmp_path):
    v, block = _gather(tmp_path, TWO_PORTS_CAM3, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp1s0",
                       _status("enp1s0", "enp1s0"), "1.35.7")
    assert v == "ok:enp1s0", block


def _capable_box(tmp_path, version):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp1s0": True, "enp3s0": "nocarrier"},
                                   status=_status("enp1s0,enp3s0", "enp1s0"))
    binp = fs / "usr" / "local" / "bin"
    binp.mkdir(parents=True, exist_ok=True)
    (binp / "dsync").write_text('#!/bin/sh\necho "dsync %s"\n' % version)
    (binp / "dsync").chmod(0o755)
    return box, unit_dir, fs


def test_apply_writes_the_list_on_a_capable_box_and_reads_the_cabled_join_back(tmp_path):
    box, unit_dir, fs = _capable_box(tmp_path, "1.35.16")
    proc = run_text(box, _program(unit_dir, fs), strict="set -euo pipefail")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "--interface enp1s0,enp3s0\n" in unit_dir.joinpath("dsync.service").read_text()
    assert "dsync PTP NIC enp1s0,enp3s0" in proc.stdout, proc.stdout
    assert "OK: dsync locked on enp1s0" in proc.stdout, proc.stdout
    assert root(box) == "ro"


def test_apply_keeps_the_single_cabled_port_on_an_older_dsync(tmp_path):
    box, unit_dir, fs = _capable_box(tmp_path, "1.35.7")
    proc = run_text(box, _program(unit_dir, fs), strict="set -euo pipefail")
    assert "--interface enp1s0\n" in unit_dir.joinpath("dsync.service").read_text()
    assert "--interface enp1s0,enp3s0" not in unit_dir.joinpath("dsync.service").read_text()
    assert "dsync PTP NIC enp1s0;" in proc.stdout or "dsync PTP NIC enp1s0 " in proc.stdout, proc.stdout


def test_step17_writes_both_ports_for_the_pinned_dsync(tmp_path):
    from test_cambox_dsync_interface_1439 import _step17
    dsync = tmp_path / "dsync-bin"
    dsync.write_text('#!/bin/sh\necho "dsync 1.35.16"\n')
    dsync.chmod(0o755)
    r, out = _step17(tmp_path, {"enp1s0": "nocarrier", "enp3s0": True, "enx00e04c680185": True}, dsync_bin=dsync)
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    assert "--ntp-server strih.lan --interface enp1s0,enp3s0\n" in out.read_text()
