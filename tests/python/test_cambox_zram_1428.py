"""issue 1428 (owner 14.10.2026): every Linux box carries swap; a cambox swaps to compressed RAM.

Owner (verbatim): "To ze tam nieje swap je hrube a opakovane porusenie pravidiel pre linux boxy".
A cambox root is a read-only USB stick (the sticks already die from wear), so its swap is zram:
systemd-zram-generator + ONE drop-in from scripts/lib/cambox-zram.sh. setup-device.sh `[zram-swap]`
and create-usb-linux.sh write it, verify-device.sh (at) grades it, and cambox_zram_apply_program
brings a live box up inside ONE verified rw window (issue 1407).

The apply program runs on the issue-1407 fake read-only-root box (ro_window_fakes_1407.py) with
fakes for dpkg-query / apt-get / modprobe / swapon added here; nothing touches the host.
"""
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from ro_window_fakes_1407 import build, index, log, make_box, root, run_text, starts_on_rw  # noqa: E402

REPO = HERE.parents[1]
LIB = REPO / "scripts" / "lib" / "cambox-zram.sh"
SETUP = REPO / "scripts" / "setup-device.sh"
USB = REPO / "scripts" / "create-usb-linux.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"

MEM_KB = 11987016  # cam5's MemTotal (11706 MiB)

_DPKG_QUERY = r'''
import os, sys
st = os.environ["FAKE_STATE"]
if os.path.exists(os.path.join(st, "pkg")):
    sys.stdout.write("install ok installed")
    sys.exit(0)
sys.stderr.write("dpkg-query: no packages found matching systemd-zram-generator\n")
sys.exit(1)
'''

_APT_GET = r'''
import os, sys
st = os.environ["FAKE_STATE"]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("apt-get " + " ".join(sys.argv[1:]) + f" root={root}\n")
if root != "rw":
    sys.stderr.write("E: Could not open lock file /var/lib/dpkg/lock-frontend - Read-only file system\n")
    sys.exit(100)
if os.environ.get("FAKE_APT_RC"):
    sys.stderr.write("E: Unable to fetch some archives\n")
    sys.exit(int(os.environ["FAKE_APT_RC"]))
open(os.path.join(st, "pkg"), "w").write("installed\n")
'''

_MODPROBE = r'''
import os, sys
st = os.environ["FAKE_STATE"]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("modprobe " + " ".join(sys.argv[1:]) + f" root={root}\n")
'''

_SWAPON = r'''
import os, sys
st = os.environ["FAKE_STATE"]
args = " ".join(sys.argv[1:])
rows = []
if os.path.exists(os.path.join(st, "active-dev-zram0.swap")):
    rows.append(("/dev/zram0", str(%d * 1024 - 4096)))
if "--show=NAME,SIZE" in args:
    for name, size in rows:
        print(f"{name} {size}")
elif "--show=NAME" in args:
    for name, _ in rows:
        print(name)
''' % MEM_KB


def _box(tmp_path, pkg=False, dropin=None, root_mode="ro"):
    box = make_box(tmp_path, root=root_mode)
    stub = box["stub"]
    for tool in ("mkdir",):
        stub.joinpath(tool).symlink_to(shutil.which(tool, path="/usr/bin:/bin"))
    stub.joinpath("hostname").write_text("#!/bin/bash\necho CAM5\n")
    stub.joinpath("hostname").chmod(0o755)
    for name, body in (("dpkg-query", _DPKG_QUERY), ("apt-get", _APT_GET), ("modprobe", _MODPROBE),
                       ("swapon", _SWAPON)):
        stub.joinpath(name).write_text(f"#!{sys.executable}\n{body}")
        stub.joinpath(name).chmod(0o755)
    if pkg:
        box["state"].joinpath("pkg").write_text("installed\n")
    path = box["state"] / "fs" / "etc" / "systemd" / "zram-generator.conf.d" / "camera-box.conf"
    if dropin is not None:
        path.parent.mkdir(parents=True)
        path.write_text(dropin)
    return box, path


def _program(dropin_path):
    return build(f'. "{LIB}"\ncambox_zram_apply_program', env={"CAMBOX_ZRAM_DROPIN": str(dropin_path)})


def _content():
    return build(f'. "{LIB}"\ncambox_zram_dropin_content')


# --- the drop-in --------------------------------------------------------------------------------

def test_the_drop_in_sizes_zram_to_ram_with_zstd_and_overrides_only_zram0():
    c = _content()
    assert "[zram0]\n" in c
    assert "zram-size = ram\n" in c, "the swap reads as large as RAM (the OBS-box rule)"
    assert "compression-algorithm = zstd\n" in c
    assert "swap-priority = 100\n" in c
    assert "/etc/systemd/zram-generator.conf.d/camera-box.conf" in LIB.read_text(), \
        "a drop-in, never an edit of the package's conffile"
    sha = build(f'. "{LIB}"\ncambox_zram_dropin_sha').strip()
    assert sha == hashlib.sha256(c.encode()).hexdigest()


def test_install_dropin_writes_the_exact_bytes_under_a_root(tmp_path):
    build(f'. "{LIB}"\ncambox_zram_install_dropin "$1"', str(tmp_path))
    p = tmp_path / "etc" / "systemd" / "zram-generator.conf.d" / "camera-box.conf"
    assert p.read_text() == _content()
    assert oct(p.stat().st_mode & 0o777) == "0o644"
    assert not p.with_name(p.name + ".new").exists()
    build(f'. "{LIB}"\ncambox_zram_install_dropin "$1"', str(tmp_path))  # a second run is the same file
    assert p.read_text() == _content()


# --- the verdict ---------------------------------------------------------------------------------

def _block(pkg="install ok installed", sha=None, mem=str(MEM_KB), swaps=None, done=True):
    sha = hashlib.sha256(_content().encode()).hexdigest() if sha is None else sha
    swaps = [("/dev/zram0", MEM_KB * 1024 - 4096)] if swaps is None else swaps
    lines = [f"PKG={pkg}", f"DROPIN_SHA={sha}", f"MEM_KB={mem}"]
    lines += [f"SWAP={n} {b}" for n, b in swaps]
    if done:
        lines.append("GATHER_DONE=1")
    return "\n".join(lines) + "\n"


def _verdict(block):
    return build(f'. "{LIB}"\ncambox_zram_verdict "$1"', block)


def test_verdict_ok_on_a_ram_sized_zram_swap():
    assert _verdict(_block()) == "ok\n"


def test_verdict_fails_closed_on_every_broken_facet():
    cases = {
        "not installed": _block(pkg=""),
        "is missing": _block(sha="__ABSENT__"),
        "differs from": _block(sha="0" * 64),
        "could not be read": _block(sha=""),
        "no zram swap is active": _block(swaps=[]),
        "under 90% of RAM": _block(swaps=[("/dev/zram0", 4 * 1024 ** 3)]),
        "never swaps to its USB stick": _block(swaps=[("/dev/zram0", MEM_KB * 1024), ("/swapfile", 1024 ** 3)]),
        "MemTotal could not be read": _block(mem=""),
        "no GATHER_DONE": _block(done=False),
    }
    for want, block in cases.items():
        out = _verdict(block)
        assert out.startswith("FAIL: "), (want, out)
        assert want in out, (want, out)


def test_the_gather_runs_on_a_box_and_feeds_the_verdict(tmp_path):
    box, path = _box(tmp_path, pkg=True, dropin=_content())
    box["state"].joinpath("active-dev-zram0.swap").write_text("active\n")
    snippet = build(f'. "{LIB}"\ncambox_zram_gather_remote_snippet', env={"CAMBOX_ZRAM_DROPIN": str(path)})
    meminfo = tmp_path / "meminfo"
    meminfo.write_text(f"MemTotal:       {MEM_KB} kB\n")
    proc = run_text(box, snippet.replace("/proc/meminfo", str(meminfo)), strict="set -euo pipefail")
    assert proc.returncode == 0, proc.stderr
    assert "GATHER_DONE=1" in proc.stdout
    assert _verdict(proc.stdout) == "ok\n", proc.stdout


# --- the live apply on the fake read-only-root box -----------------------------------------------

def test_apply_installs_and_writes_in_one_verified_window_and_starts_the_swap_after_it(tmp_path):
    box, path = _box(tmp_path)
    proc = run_text(box, _program(path), strict="set -euo pipefail")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    rw = index(calls, "mount -o remount,rw /")
    apt = index(calls, "apt-get install", rw)
    assert calls[apt].endswith("root=rw")
    assert "--force-confold" in calls[apt] and "systemd-zram-generator" in calls[apt]
    close = index(calls, "mount -o remount,ro /", apt)
    start = index(calls, "systemctl start dev-zram0.swap", close)
    assert index(calls, "systemctl daemon-reload", close) < start
    assert index(calls, "modprobe zram", close) < start
    assert starts_on_rw(calls) == []
    assert root(box) == "ro"
    assert path.read_text() == _content()
    assert "/dev/zram0" in proc.stdout


def test_apply_on_a_box_already_in_place_opens_no_window(tmp_path):
    box, path = _box(tmp_path, pkg=True, dropin=_content())
    proc = run_text(box, _program(path))
    calls = log(box)
    assert proc.returncode == 0, proc.stderr
    assert not [c for c in calls if c.startswith(("mount", "apt-get"))], calls
    assert "drop-in in-place" in proc.stdout and "package in-place" in proc.stdout
    assert root(box) == "ro"


def test_a_failed_install_closes_the_window_and_starts_nothing(tmp_path):
    box, path = _box(tmp_path)
    proc = run_text(box, _program(path), FAKE_APT_RC="100")
    calls = log(box)
    assert proc.returncode != 0
    assert "apt-get install systemd-zram-generator failed" in proc.stderr
    assert root(box) == "ro", "the EXIT trap ran the verified close"
    assert not [c for c in calls if c.startswith("systemctl start")], calls


def test_a_root_that_stays_writable_fails_by_name_and_starts_nothing(tmp_path):
    box, path = _box(tmp_path)
    proc = run_text(box, _program(path), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "root is NOT read-only" in proc.stderr
    assert not [c for c in calls if c.startswith("systemctl start")], calls


def test_a_refused_rw_remount_writes_nothing(tmp_path):
    box, path = _box(tmp_path)
    proc = run_text(box, _program(path), FAKE_RW_FAIL="1")
    assert proc.returncode != 0
    assert "could not remount / read-write" in proc.stderr
    assert not path.exists()
    assert not [c for c in log(box) if c.startswith("apt-get")]


def test_the_program_keeps_its_commands_off_its_own_stdin(tmp_path):
    box, path = _box(tmp_path)
    text = _program(path)
    assert text.rstrip().endswith("_cz_main </dev/null"), "apt/dpkg must never read the rest of the program"
    assert text.count("remount,ro /") == 1, "the ONE verified close"


# --- the wiring ----------------------------------------------------------------------------------

def test_setup_device_installs_the_package_and_the_drop_in_before_the_read_only_flip():
    s = SETUP.read_text()
    assert '. "$HERE/lib/cambox-zram.sh"' in s
    step18 = s.index("# STEP 18: Configure read-only")
    install = s.index('apt-get install -y -qq -o Dpkg::Options::=--force-confold "$CAMBOX_ZRAM_PACKAGE"')
    dropin = s.index('cambox_zram_install_dropin "" \\')
    assert install < dropin < step18
    block = s[s.index("# [zram-swap]"):step18]
    assert "systemctl start" not in block and "swapon" not in block, "enable-only: the swap comes up at boot"


def test_create_usb_bakes_the_package_and_the_drop_in():
    s = USB.read_text()
    assert '. "$SCRIPT_DIR/lib/cambox-zram.sh"' in s
    assert "    systemd-zram-generator \\\n" in s
    assert 'cambox_zram_install_dropin "$MOUNT_ROOT" || error' in s


def test_verify_device_grades_it_as_check_at_outside_the_executed_slices():
    s = VERIFY.read_text()
    assert '. "$HERE/lib/cambox-zram.sh"' in s
    at = s.index("\n# (at) zram swap (issue 1428)")
    assert s.index("\n# (as) the rig time zone") < at < s.index("\n# (ar) read-only-root unit set (issue 1394) -- HARD FAIL")
    block = s[at:s.index("\n# (ar) read-only-root unit set (issue 1394) -- HARD FAIL")]
    assert "cambox_zram_gather_remote_snippet" in block and "cambox_zram_verdict" in block
    assert 'warn "' not in block
    assert s.count("(at) zram swap (issue 1428)") == 3, "header, usage() and the check itself"
