"""Issue 1439 part 2, ROZHODNUTE 1.11.2026: a cambox's dsync binds PTP to its LAN NIC, never by chance.

WHY: setup-device STEP 17 wrote `ExecStart=/usr/local/bin/dsync --ntp-server strih.lan`, so dsync
joined PTP on the first IPv4 interface it listed. The 20.8.2026 cam1 incident is exactly that: the
bkshading USB camera link (CDC-NCM, `enx*`) took the multicast join and the box went PTP-deaf for
~5 h. Read live 9.11.2026 on cam2: `ptp_interface_configured=null`, `ptp_interface=enp3s0`.

What this pins (the main's design, issue comment 6736485213):
  * `cambox_lan_nic SYSROOT` = the ONE `enp*` under SYSROOT/class/net that has a `device` link (the
    netplan #1155 name rule); none or two = rc 1 + a named stderr line, never a guess;
  * `cambox_dsync_unit_text [NIC]` = the STEP 17 unit, byte-identical to the old heredoc without a
    NIC, ` --interface <NIC>` after `--ntp-server strih.lan` with one; a malformed NIC emits nothing;
  * setup-device STEP 17 renders the unit through both and fails by name on an unresolved NIC;
  * verify-device (ba): dsync's own /status (configured AND joined == the LAN NIC) + the unit's
    ExecStart carrying `--interface <NIC>`, through the shared dsync_iface_* verdict;
  * the live apply (cambox_ro_units_apply_program) writes dsync.service inside the ONE verified rw
    window when it differs, restarts dsync only after the close and only when written, then reads
    /status back (locked + configured == the NIC); an unresolved NIC is a named FAIL with no write.

Tier-0: bash + pytest only (fake sysfs, fake curl, the issue-1407 fake read-only-root box).
"""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import build, index, log, root, run_text, starts_on_rw  # noqa: E402
from test_cambox_ro_units_1394 import UNITS, _box  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"
UNIT_LIB = LIB / "cambox-dsync-unit.sh"
IFACE_LIB = LIB / "dsync-iface.sh"
SETUP = REPO / "scripts" / "setup-device.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"

# The STEP 17 heredoc as setup-device wrote it before issue 1439 part 2 (byte for byte): the unit a
# cambox carries today. Without a NIC the rendered unit must stay exactly this.
OLD_UNIT = """[Unit]
Description=dsync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
# Sync to the venue's strih by NAME (strih.lan: strih-snv in SNV, strih-pp in Poprad), NOT the
# dantesync binary's default public NTP pool -- every node must discipline its clock to the SAME
# reference or the wall-clock genlock in src/ndi.rs silently diverges across cameras (#8). Issue 1412:
# the old literal SNV IP left every Poprad cambox with no NTP at all (cam7 ~150 s off strih-pp). The
# CLI value is resolved on every query, so an unresolvable name is a counted NTP failure, never a
# public-pool fallback, and it recovers once DNS answers.
ExecStart=/usr/local/bin/dsync --ntp-server strih.lan
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"""


def _bash(script, *args, env=None):
    """Run SCRIPT under the callers' set -euo pipefail; values only as positional arguments."""
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *args],
                          capture_output=True, text=True, cwd=str(REPO), timeout=60,
                          env=dict(os.environ, **(env or {})))


def _sysfs(tmp_path, nics):
    """A fake /sys: class/net/<nic>, with a `device` link for every nic mapped to True."""
    sysroot = tmp_path / "sys"
    (sysroot / "class" / "net").mkdir(parents=True)
    devs = tmp_path / "devices"
    for nic, has_device in nics.items():
        d = sysroot / "class" / "net" / nic
        d.mkdir()
        if has_device:
            target = devs / nic
            target.mkdir(parents=True)
            (d / "device").symlink_to(target)
    return sysroot


def _nic(sysroot):
    return _bash('. "$1"\ncambox_lan_nic "$2"', str(UNIT_LIB), str(sysroot))


def _unit(*nic):
    return _bash('. "$1"\nshift\ncambox_dsync_unit_text "$@"', str(UNIT_LIB), *nic)


# --- the LAN NIC resolver ------------------------------------------------------------------------

def test_the_one_pci_nic_is_the_lan_nic(tmp_path):
    sysroot = _sysfs(tmp_path, {"lo": False, "enp3s0": True, "enx00e04c680185": True, "enp3s0.70": False})
    r = _nic(sysroot)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "enp3s0\n"


@pytest.mark.parametrize("nics,why", [
    ({"lo": False}, "no enp"),
    ({"lo": False, "enx00e04c680185": True}, "no enp"),
    ({"enp3s0": False}, "no enp"),
    ({"enp3s0": True, "enp4s0": True}, "two"),
])
def test_no_or_two_pci_nics_fail_by_name_and_print_nothing(tmp_path, nics, why):
    r = _nic(_sysfs(tmp_path, nics))
    assert r.returncode == 1, (nics, r.stdout)
    assert r.stdout == ""
    assert "cambox_lan_nic" in r.stderr, r.stderr
    if why == "two":
        assert "enp3s0" in r.stderr and "enp4s0" in r.stderr, r.stderr


def test_a_missing_net_class_fails_by_name(tmp_path):
    r = _nic(tmp_path / "nothing")
    assert r.returncode == 1 and r.stdout == ""
    assert "cambox_lan_nic" in r.stderr


# --- the unit ------------------------------------------------------------------------------------

def test_without_a_nic_the_unit_is_the_old_heredoc_byte_for_byte():
    r = _unit()
    assert r.returncode == 0, r.stderr
    assert r.stdout == OLD_UNIT


def test_with_a_nic_only_the_execstart_gains_the_interface():
    r = _unit("enp3s0")
    assert r.returncode == 0, r.stderr
    want = OLD_UNIT.replace("ExecStart=/usr/local/bin/dsync --ntp-server strih.lan\n",
                            "ExecStart=/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0\n")
    assert r.stdout == want


@pytest.mark.parametrize("bad", ["", "enp3s0; reboot", "enp3s0'", "a b", "-i", "x" * 16,
                                 "10.77.9.61/23", "enp3s0\n", "enp$(id)"])
def test_a_malformed_nic_is_refused_and_emits_nothing(bad):
    r = _unit(bad)
    assert r.returncode != 0, bad
    assert r.stdout == "", (bad, r.stdout)


# --- setup-device STEP 17 ------------------------------------------------------------------------

def test_step17_renders_the_unit_through_the_lib_with_the_resolved_nic():
    s = SETUP.read_text()
    src = s.index('. "$HERE/lib/cambox-dsync-unit.sh"')
    guard = s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]')
    assert src < guard, "the lib loads before the source guard, so a sourced setup-device has it"
    assert "<< 'DANTEEOF'" not in s, "the unit heredoc moved into scripts/lib/cambox-dsync-unit.sh"
    resolve = s.index('_DSYNC_NIC="$(cambox_lan_nic /sys)" || fail')
    render = s.index('_DSYNC_UNIT="$(cambox_dsync_unit_text "$_DSYNC_NIC")" || fail')
    write = s.index('printf \'%s\\n\' "$_DSYNC_UNIT" > /etc/systemd/system/dsync.service')
    reload_ = s.index("systemctl daemon-reload", write)
    enable = s.index("systemctl enable dsync", write)
    assert s.index("cat > /etc/dsync/config.json") < resolve < render < write < reload_ < enable
    step = s[resolve:enable]
    assert "issue 1439" in step
    assert "systemctl restart dsync" not in step, "STEP 17 stays enable-only"


def _step17(tmp_path, nics):
    """Run STEP 17's REAL resolve + render lines with /sys and the unit path moved into tmp_path."""
    s = SETUP.read_text()
    a = s.index('_DSYNC_NIC="$(cambox_lan_nic /sys)"')
    b = s.index("systemctl daemon-reload", a)
    block = s[a:b].replace("cambox_lan_nic /sys", 'cambox_lan_nic "$SYSROOT"') \
                  .replace("/etc/systemd/system/dsync.service", '"$UNIT_OUT"')
    sysroot = _sysfs(tmp_path, nics)
    out = tmp_path / "dsync.service"
    script = ('. "$1"\nfail() { echo "FAIL $*"; exit 7; }\nSYSROOT="$2"; UNIT_OUT="$3"\n' + block
              + '\necho END\n')
    return _bash(script, str(UNIT_LIB), str(sysroot), str(out)), out


def test_step17_writes_the_bound_unit(tmp_path):
    r, out = _step17(tmp_path, {"enp3s0": True, "enx00e04c680185": True})
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    assert "--ntp-server strih.lan --interface enp3s0\n" in out.read_text()


def test_step17_fails_by_name_on_an_unresolved_nic_and_writes_nothing(tmp_path):
    r, out = _step17(tmp_path, {"enx00e04c680185": True})
    assert r.returncode == 7, r.stdout + r.stderr
    assert "FAIL" in r.stdout and "issue 1439" in r.stdout, r.stdout
    assert not out.exists()


# --- verify-device (ba) --------------------------------------------------------------------------

def test_check_ba_sits_after_ak_before_az_and_is_documented_three_times():
    s = VERIFY.read_text()
    ba = s.find("\n# (ba) ")
    assert s.find("\n# (ak) ") < ba < s.find("\n# (az) ") < s.rfind("# (q) .bak cruft drift")
    assert s.count("(ba) dsync PTP NIC") >= 3, "header, usage and the check itself"
    assert '. "$HERE/lib/cambox-dsync-unit.sh"' in s


def _status(conf, joined, locked=True):
    return json.dumps({"is_locked": locked, "ptp_interface_configured": conf, "ptp_interface": joined})


def _gather_on(tmp_path, nics, unit_exec, status_body, curl_rc=0):
    """Run the REAL (ba) gather snippet against a fake /sys, unit file and curl."""
    sysroot = _sysfs(tmp_path, nics)
    unitf = tmp_path / "dsync.service"
    if unit_exec is not None:
        unitf.write_text("[Service]\nExecStart=" + unit_exec + "\n")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    (tmp_path / "body.json").write_text(status_body)
    (bindir / "curl").write_text('#!/bin/sh\ncat "%s"\nexit %d\n' % (tmp_path / "body.json", curl_rc))
    (bindir / "curl").chmod(0o755)
    r = _bash('. "$1"\nbash -c "$(cambox_dsync_gather_remote_snippet "$2" "$3")"',
              str(UNIT_LIB), str(sysroot), str(unitf),
              env={"PATH": "%s:%s" % (bindir, os.environ["PATH"])})
    assert r.returncode == 0, r.stderr
    return r.stdout


def _verdict(block):
    r = _bash('. "$1"\ncambox_dsync_verify_verdict "$2"; echo END', str(UNIT_LIB), block)
    assert r.returncode == 0, r.stderr
    return r.stdout.split("\n")[0]


def test_the_gather_defaults_to_the_real_box_paths():
    r = _bash('. "$1"\ncambox_dsync_gather_remote_snippet', str(UNIT_LIB))
    assert r.returncode == 0, r.stderr
    assert "cambox_lan_nic /sys" in r.stdout and "/etc/systemd/system/dsync.service" in r.stdout, r.stdout
    assert "127.0.0.1:8898/status" in r.stdout, "the read is dsync's own /status, embedded via declare -f"


def test_the_gather_and_verdict_pass_a_bound_joined_box(tmp_path):
    block = _gather_on(tmp_path, {"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0",
                       _status("enp3s0", "enp3s0"))
    assert _verdict(block) == "ok:enp3s0", block


@pytest.mark.parametrize("nics,unit_exec,body,want", [
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan", _status(None, "enp3s0"),
     "unit-not-bound"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0",
     _status(None, "enp3s0"), "not-configured"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0",
     _status("enx00e04c680185", "enx00e04c680185"), "configured-elsewhere:enx00e04c680185:enp3s0"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0",
     _status("enp3s0", None), "joined-elsewhere:-:enp3s0"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0", "", "unreadable"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0", "not json",
     "unreadable"),
    ({"enx00e04c680185": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0",
     _status("enp3s0", "enp3s0"), "nic-unresolved"),
    ({"enp3s0": True}, "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0x",
     _status("enp3s0", "enp3s0"), "unit-not-bound"),
    ({"enp3s0": True}, None, _status("enp3s0", "enp3s0"), "unit-not-bound"),
])
def test_every_other_state_fails_with_its_token(tmp_path, nics, unit_exec, body, want):
    v = _verdict(_gather_on(tmp_path, nics, unit_exec, body))
    assert v.startswith(want), v
    assert not v.startswith("ok"), v


def test_an_incomplete_gather_is_unreadable_never_a_pass():
    assert _verdict("").startswith("gather-incomplete")
    assert _verdict("NIC=enp3s0\nEXEC=x --interface enp3s0\n").startswith("gather-incomplete")


def _run_ba(ssh_out, ssh_rc=0):
    s = VERIFY.read_text()
    ba = s.find("\n# (ba) ")
    block = s[ba:s.find("\n# (", ba + 10)]
    assert 'warn "' not in block
    script = ('. "$1"\nok() { echo "PASS $1"; }\nfail() { echo "FAIL $1"; }\n'
              'SSH_OUT="$2"; SSH_RC="$3"\n'
              'ssh_box() { printf "%s" "$SSH_OUT"; return "$SSH_RC"; }\n'
              + block + '\necho END\n')
    r = _bash(script, str(VERIFY), ssh_out, str(ssh_rc))
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    return r.stdout.strip().splitlines()[0]


def test_check_ba_passes_a_bound_box_and_fails_every_other_state(tmp_path):
    good = _gather_on(tmp_path, {"enp3s0": True},
                      "/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0", _status("enp3s0", "enp3s0"))
    line = _run_ba(good)
    assert line.startswith("PASS") and "enp3s0" in line, line
    line = _run_ba(good.replace("--interface enp3s0", ""))
    assert line.startswith("FAIL") and "unit-not-bound" in line, line
    assert "cambox-ro-units-apply.sh" in line, "a FAIL names the live fix"
    assert _run_ba("", 255).startswith("FAIL")
    assert _run_ba("garbage").startswith("FAIL")


# --- the live apply (cambox_ro_units_apply_program) ----------------------------------------------

def _dsync_box(tmp_path, nics, unit_text=OLD_UNIT, status=None):
    """The issue-1394 fake box + /sys under the fs root, a dsync unit and a curl answering STATUS."""
    box, unit_dir = _box(tmp_path)
    fs = box["state"] / "fs"
    sysroot = fs / "sys"
    (sysroot / "class" / "net").mkdir(parents=True)
    for nic, has_device in nics.items():
        d = sysroot / "class" / "net" / nic
        d.mkdir()
        if has_device:
            (d / "device").mkdir()
    if unit_text is not None:
        unit_dir.joinpath("dsync.service").write_text(unit_text)
    stub = box["stub"]
    stub.joinpath("python3").symlink_to(sys.executable)
    # /status answers in order, one JSON per line; past the last line the last one repeats
    seq = status if isinstance(status, list) else [status if status is not None else _status("enp3s0", "enp3s0")]
    (box["state"] / "status.jsonl").write_text("\n".join(seq) + "\n")
    stub.joinpath("curl").write_text(
        f'#!{sys.executable}\nimport os\nst = os.environ["FAKE_STATE"]\n'
        'n = len([l for l in open(os.path.join(st, "log")) if l.startswith("curl ")])\n'
        'open(os.path.join(st, "log"), "a").write("curl status\\n")\n'
        'seq = open(os.path.join(st, "status.jsonl")).read().split("\\n")[:-1]\n'
        'print(seq[min(n, len(seq) - 1)], end="")\n')
    stub.joinpath("curl").chmod(0o755)
    stub.joinpath("sleep").unlink()
    stub.joinpath("sleep").write_text(
        f'#!{sys.executable}\nimport os, sys, time\n'
        'open(os.path.join(os.environ["FAKE_STATE"], "log"), "a").write("sleep " + " ".join(sys.argv[1:]) + "\\n")\n'
        'time.sleep(float(sys.argv[1]))\n')
    stub.joinpath("sleep").chmod(0o755)
    return box, unit_dir, fs


def _program(unit_dir, fs, wait="0", poll="1"):
    return build(f'. "{UNITS}"\ncambox_ro_units_apply_program', env={
        "CAMBOX_RO_UNITS_UNIT_DIR": str(unit_dir), "CAMBOX_RO_UNITS_FS_ROOT": str(fs),
        "CAMBOX_RO_UNITS_DSYNC_WAIT_S": wait, "CAMBOX_RO_UNITS_DSYNC_POLL_S": poll})


def _bound(nic="enp3s0"):
    return OLD_UNIT.replace("--ntp-server strih.lan\n", f"--ntp-server strih.lan --interface {nic}\n")


def test_apply_writes_the_unit_in_the_window_and_restarts_dsync_after_the_close(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True, "enx00e04c680185": True})
    proc = run_text(box, _program(unit_dir, fs), strict="set -euo pipefail")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert unit_dir.joinpath("dsync.service").read_text() == _bound()
    rw = index(calls, "mount -o remount,rw / root=ro")
    close = index(calls, "mount -o remount,ro / root=rw")
    mv = [i for i, c in enumerate(calls) if c.startswith("mv ") and c.endswith("dsync.service root=rw")]
    assert len(mv) == 1 and rw < mv[0] < close, "\n".join(calls)
    reload_ = index(calls, "systemctl daemon-reload root=ro", close)
    restart = index(calls, "systemctl restart dsync root=ro", close)
    assert reload_ < restart < index(calls, "curl status", restart), "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert root(box) == "ro"
    assert "dsync PTP NIC enp3s0" in proc.stdout and "unit write" in proc.stdout, proc.stdout
    assert "OK: dsync locked on enp3s0" in proc.stdout, proc.stdout


def test_a_unit_already_bound_is_neither_written_nor_restarted(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True}, unit_text=_bound())
    # the rest of the unit set in place, so the window has nothing else to do
    first = run_text(box, _program(unit_dir, fs))
    assert first.returncode == 0, first.stderr
    box["state"].joinpath("log").write_text("")
    again = run_text(box, _program(unit_dir, fs))
    calls = log(box)
    assert again.returncode == 0, again.stdout + again.stderr
    assert not any(c.startswith("mount ") for c in calls), "\n".join(calls)
    assert not any("restart dsync" in c for c in calls), "\n".join(calls)
    assert unit_dir.joinpath("dsync.service").read_text() == _bound()
    assert "unit in-place" in again.stdout, again.stdout


def test_an_unresolved_nic_is_a_named_fail_and_writes_no_unit(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enx00e04c680185": True})
    proc = run_text(box, _program(unit_dir, fs))
    calls = log(box)
    assert proc.returncode != 0, proc.stdout
    assert "FAIL: [issue 1394]" in proc.stderr and "issue 1439" in proc.stderr, proc.stderr
    assert unit_dir.joinpath("dsync.service").read_text() == OLD_UNIT
    assert not any("dsync" in c for c in calls if c.startswith(("mv ", "systemctl restart"))), "\n".join(calls)
    assert root(box) == "ro"


def test_a_box_without_the_dsync_unit_gets_a_note_and_no_unit(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True}, unit_text=None)
    proc = run_text(box, _program(unit_dir, fs))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert not unit_dir.joinpath("dsync.service").exists()
    assert "NOTE: no " in proc.stdout and "dsync.service" in proc.stdout, proc.stdout
    assert not any("dsync" in c for c in log(box)), "\n".join(log(box))


def test_a_restart_that_never_binds_the_nic_fails_the_box(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True}, status=_status(None, "enp3s0"))
    proc = run_text(box, _program(unit_dir, fs))
    calls = log(box)
    assert proc.returncode != 0, proc.stdout
    assert "FAIL: [issue 1394]" in proc.stderr and "not-configured" in proc.stderr, proc.stderr
    assert any(c == "systemctl restart dsync root=ro" for c in calls), "\n".join(calls)
    assert root(box) == "ro"


def test_a_restart_that_never_locks_fails_the_box(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True}, status=_status("enp3s0", "enp3s0", locked=False))
    proc = run_text(box, _program(unit_dir, fs))
    assert proc.returncode != 0, proc.stdout
    assert "not locked" in proc.stderr, proc.stderr


def test_the_read_back_polls_until_dsync_locks(tmp_path):
    # the cold lock (~95 s live): unlocked first, locked on the second read
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True},
                                   status=[_status("enp3s0", "enp3s0", locked=False), _status("enp3s0", "enp3s0")])
    proc = run_text(box, _program(unit_dir, fs, wait="5", poll="1"))
    calls = log(box)
    assert proc.returncode == 0, proc.stdout + proc.stderr + "\n".join(calls)
    restart = index(calls, "systemctl restart dsync root=ro")
    after = calls[restart:]
    assert [c for c in after if c.startswith(("curl", "sleep"))] == ["curl status", "sleep 1", "curl status"], after
    assert "OK: dsync locked on enp3s0" in proc.stdout and "2 read(s)" in proc.stdout, proc.stdout


def test_the_read_back_gives_up_at_the_budget_and_names_the_reads(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True}, status=_status("enp3s0", "enp3s0", locked=False))
    proc = run_text(box, _program(unit_dir, fs, wait="2", poll="1"))
    calls = log(box)
    assert proc.returncode != 0
    reads = [c for c in calls if c == "curl status"]
    assert 2 <= len(reads) <= 3, calls
    assert "not locked" in proc.stderr and "budget 2 s" in proc.stderr, proc.stderr


def test_a_failed_reload_never_restarts_dsync(tmp_path):
    box, unit_dir, fs = _dsync_box(tmp_path, {"enp3s0": True})
    proc = run_text(box, _program(unit_dir, fs), FAKE_DAEMON_RELOAD_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert unit_dir.joinpath("dsync.service").read_text() == _bound(), "the unit landed in the window"
    assert not any("restart dsync" in c for c in calls), "\n".join(calls)
    assert "dsync not restarted" in proc.stderr and "reload failed" in proc.stderr, proc.stderr


def test_the_wait_budget_defaults_to_150_s_and_a_poll_never_exceeds_it():
    text = build(f'. "{UNITS}"\ncambox_ro_units_apply_program',
                 env={"CAMBOX_RO_UNITS_UNIT_DIR": "/x"})
    assert "_rou_ds_wait=150\n" in text and "_rou_ds_poll=5\n" in text, text
    text = build(f'. "{UNITS}"\ncambox_ro_units_apply_program', env={
        "CAMBOX_RO_UNITS_UNIT_DIR": "/x", "CAMBOX_RO_UNITS_DSYNC_WAIT_S": "3", "CAMBOX_RO_UNITS_DSYNC_POLL_S": "10"})
    assert "_rou_ds_wait=3\n" in text and "_rou_ds_poll=3\n" in text, text
    text = build(f'. "{UNITS}"\ncambox_ro_units_apply_program', env={
        "CAMBOX_RO_UNITS_UNIT_DIR": "/x", "CAMBOX_RO_UNITS_DSYNC_WAIT_S": "1e3", "CAMBOX_RO_UNITS_DSYNC_POLL_S": "0"})
    assert "_rou_ds_wait=150\n" in text and "_rou_ds_poll=5\n" in text, text


def test_a_box_without_python3_is_named_by_the_verdict():
    block = "NOPY=1\nNIC=enp3s0\nEXEC=/usr/local/bin/dsync --ntp-server strih.lan --interface enp3s0\nSTATUS=0\nEND\n"
    assert _verdict(block) == "unreadable:no-python3-on-the-box"


def test_cli_names_an_unresolved_nic_as_dsync_not_bound(tmp_path):
    from test_cambox_ro_units_1394 import _cli
    r, _ = _cli(tmp_path, "--apply", "--box", "cam1", env={"FAKE_DSYNC_NIC_HOST": "10.77.9.61"})
    assert r.returncode == 1, r.stdout + r.stderr
    assert "cam1 (dsync not bound: the LAN NIC does not resolve" in r.stderr, r.stderr
    assert "the unit files are in place" not in r.stderr, r.stderr


def test_strih_wrappers_are_the_shared_grade():
    lib = (LIB / "strih-dantesync.sh").read_text()
    assert "strih_dantesync_iface_verdict() { dsync_iface_verdict" in lib
    assert "strih_dantesync_iface_grade() { dsync_iface_grade" in lib
    assert "dsync-iface.sh" in lib
    assert shutil.which("python3")
