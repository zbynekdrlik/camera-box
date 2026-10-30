"""issue 1387 -- bring a live cambox's egress tree up to date without a setup-device re-run.

Every cambox provisioned before issue 1387 runs the old boot script (root fq: the JPEG XS flow capped
with NDI), and verify-device (ap) names it stale. `cambox_egress_pacing_apply_program` (in
scripts/lib/cambox-egress-pacing.sh) is what the supervisor pipes into `ssh root@<box> bash -s`:
the boot script + unit written inside ONE rw window closed by the verified close (issue 1407), then
the unit restarted and the live tree read back and graded by the lib's own verdict.

It runs here on the issue-1407 fake read-only-root box (ro_window_fakes_1407.py) with fake `ip`,
`tc` and `hostname` added; nothing touches the host. The fake `tc` prints the issue-1387 tree once
the fake `systemctl restart cambox-egress-pacing` ran (the unit's boot script applies it on a real
box), the old root fq before that, or the old fq always with FAKE_TC_STAYS_OLD=1.
"""
import hashlib
import pathlib
import shutil
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from ro_window_fakes_1407 import build, index, log, make_box, root, run_text, starts_on_rw  # noqa: E402
from test_cambox_egress_pacing_1242 import CAM7_PACED, TREE_FILTER, TREE_QDISC  # noqa: E402

REPO = HERE.parents[1]
LIB = REPO / "scripts" / "lib" / "cambox-egress-pacing.sh"
UNIT = REPO / "systemd" / "cambox-egress-pacing.service"

_IP = '''
print("default via 10.77.8.1 dev enp1s0 proto static ")
'''

_TC = r'''
import os, sys
st = os.environ["FAKE_STATE"]
args = sys.argv[1:]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("tc " + " ".join(args) + f" root={root}\n")
applied = os.path.exists(os.path.join(st, "active-cambox-egress-pacing")) \
    and os.environ.get("FAKE_TC_STAYS_OLD") != "1"
if args[:2] == ["qdisc", "show"]:
    print(os.environ["TREE_QDISC"] if applied else os.environ["OLD_QDISC"])
elif args[:2] == ["filter", "show"]:
    print(os.environ["TREE_FILTER"] if applied else "")
'''


def _box(tmp_path, script=None, unit=None, enabled=True, root_mode="ro"):
    box = make_box(tmp_path, root=root_mode)
    stub = box["stub"]
    stub.joinpath("mkdir").symlink_to(shutil.which("mkdir", path="/usr/bin:/bin"))
    stub.joinpath("hostname").write_text("#!/bin/bash\necho CAM5\n")
    stub.joinpath("hostname").chmod(0o755)
    for name, body in (("ip", _IP), ("tc", _TC)):
        stub.joinpath(name).write_text(f"#!{sys.executable}\n{body}")
        stub.joinpath(name).chmod(0o755)
    fs = box["state"] / "fs"
    sp = fs / "usr" / "local" / "sbin" / "cambox-egress-pacing"
    up = fs / "etc" / "systemd" / "system" / "cambox-egress-pacing.service"
    for path, text in ((sp, script), (up, unit)):
        if text is not None:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
    if enabled:
        box["state"].joinpath("enabled-cambox-egress-pacing").write_text("enabled\n")
    return box, sp, up


def _program(sp, up):
    pre = (f'CAMBOX_EGRESS_PACING_SCRIPT_PATH="{sp}"\n'
           f'CAMBOX_EGRESS_PACING_SERVICE_PATH="{up}"\n')
    return build(f'. "{LIB}"\n{pre}cambox_egress_pacing_apply_program')


def _boot_script():
    return build(f'. "{LIB}"\ncambox_egress_pacing_boot_script')


def _run(box, prog, **fake):
    return run_text(box, prog, strict="set -euo pipefail", TREE_QDISC=TREE_QDISC,
                    TREE_FILTER=TREE_FILTER, OLD_QDISC=CAM7_PACED, **fake)


def _old_script():
    return _boot_script().replace(
        "tc qdisc replace dev", "OLD tc qdisc replace dev", 1)


def test_apply_writes_in_one_verified_window_then_restarts_and_grades_the_tree(tmp_path):
    box, sp, up = _box(tmp_path, script=_old_script(), unit=UNIT.read_text())
    proc = _run(box, _program(sp, up))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "boot script write; unit in-place; enabled" in proc.stdout, proc.stdout
    rw = index(calls, "mount -o remount,rw /")
    close = index(calls, "mount -o remount,ro /", rw)
    reload_ = index(calls, "systemctl daemon-reload", close)
    restart = index(calls, "systemctl restart cambox-egress-pacing", reload_)
    index(calls, "tc qdisc show dev enp1s0", restart)
    index(calls, "tc filter show dev enp1s0", restart)
    assert starts_on_rw(calls) == []
    assert root(box) == "ro"
    assert sp.read_text() == _boot_script(), "byte-identical to what setup-device writes"
    assert oct(sp.stat().st_mode & 0o777) == "0o755"
    assert not sp.with_name(sp.name + ".new").exists()
    assert "READBACK (CAM5): OK prio 1:" in proc.stdout and "0x1387" in proc.stdout, proc.stdout


def test_apply_on_a_current_box_writes_nothing_and_still_reads_back(tmp_path):
    box, sp, up = _box(tmp_path, script=_boot_script(), unit=UNIT.read_text())
    proc = _run(box, _program(sp, up))
    calls = log(box)
    assert proc.returncode == 0, proc.stderr
    assert not any(c.startswith("mount ") for c in calls), "an up-to-date ro box is never remounted"
    index(calls, "systemctl restart cambox-egress-pacing")
    assert "READBACK (CAM5): OK" in proc.stdout


def test_apply_writes_a_missing_unit_and_enables_it_inside_the_window(tmp_path):
    box, sp, up = _box(tmp_path, script=None, unit=None, enabled=False)
    proc = _run(box, _program(sp, up))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert up.read_text() == UNIT.read_text(), "byte-identical to the checked-in unit"
    enable = index(calls, "systemctl enable cambox-egress-pacing")
    assert calls[enable].endswith("root=rw")
    assert enable < index(calls, "mount -o remount,ro /")
    assert root(box) == "ro"


def test_apply_puts_a_stuck_writable_root_back_before_anything_starts(tmp_path):
    box, sp, up = _box(tmp_path, script=_boot_script(), unit=UNIT.read_text(), root_mode="rw")
    proc = _run(box, _program(sp, up))
    calls = log(box)
    assert proc.returncode == 0, proc.stderr
    assert index(calls, "mount -o remount,ro /") < index(calls, "systemctl restart cambox-egress-pacing")
    assert root(box) == "ro"


def test_apply_with_a_busy_root_fails_loud_and_restarts_nothing(tmp_path):
    box, sp, up = _box(tmp_path, script=_old_script(), unit=UNIT.read_text())
    proc = _run(box, _program(sp, up), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "root is NOT read-only" in proc.stderr and "issue 1387" in proc.stderr, proc.stderr
    assert not any(c.startswith("systemctl restart") for c in calls), "\n".join(calls)


def test_apply_whose_unit_leaves_the_old_tree_fails_the_readback(tmp_path):
    box, sp, up = _box(tmp_path, script=_old_script(), unit=UNIT.read_text())
    proc = _run(box, _program(sp, up), FAKE_TC_STAYS_OLD="1")
    assert proc.returncode != 0
    assert "FAIL: [issue 1387] READBACK (CAM5): DRIFT" in proc.stderr, proc.stderr
    assert "JPEG XS" in proc.stderr
    # Review round 3: the restart "succeeded" (a fallback exits 0), so the line points at the step.
    assert "journalctl -u cambox-egress-pacing" in proc.stderr, proc.stderr
    assert root(box) == "ro"


def test_apply_whose_restart_fails_names_the_unit_journal(tmp_path):
    box, sp, up = _box(tmp_path, script=_boot_script(), unit=UNIT.read_text())
    proc = _run(box, _program(sp, up), FAKE_RESTART_FAIL_UNIT="cambox-egress-pacing")
    assert proc.returncode != 0
    assert "journalctl -u cambox-egress-pacing" in proc.stderr, proc.stderr


def test_apply_with_a_refused_rw_remount_writes_nothing(tmp_path):
    old = _old_script()
    box, sp, up = _box(tmp_path, script=old, unit=UNIT.read_text())
    proc = _run(box, _program(sp, up), FAKE_RW_FAIL="1")
    assert proc.returncode != 0
    assert "nothing was written" in proc.stderr, proc.stderr
    assert sp.read_text() == old


def test_the_program_embeds_the_one_boot_script_and_the_one_verdict():
    text = _program(pathlib.Path("/usr/local/sbin/cambox-egress-pacing"),
                    pathlib.Path("/etc/systemd/system/cambox-egress-pacing.service"))
    assert "cambox_egress_pacing_verdict ()" in text, "the verdict travels with declare -f, never a copy"
    assert "_cep_main </dev/null" in text, "off the bash -s stdin"
    assert text.count("remount,ro /") == 1, "the one shared verified close, never a hand-written one"
    want = hashlib.sha256((_boot_script().rstrip("\n") + "\n").encode()).hexdigest()
    assert want == hashlib.sha256(_boot_script().encode()).hexdigest(), "the boot script ends in one newline"


def test_the_program_refuses_without_the_checked_in_unit(tmp_path):
    import subprocess
    proc = subprocess.run(
        ["/bin/bash", "-c", f'set -euo pipefail\n. "{LIB}"\n'
         f'CAMBOX_EGRESS_PACING_UNIT_SRC="{tmp_path}/missing.service"\ncambox_egress_pacing_apply_program'],
        capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"})
    assert proc.returncode != 0 and proc.stdout == ""
    assert "systemd/" in proc.stderr
