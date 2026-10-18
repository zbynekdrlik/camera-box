"""issue 1446 -- the kernel planner's box read must see an apt-HELD kernel package as installed.

Live 18.10.2026 on cam2: the purge step the planner printed (`apt-get purge linux-image-6.8.0-142-generic
linux-modules-... linux-modules-extra-...`) was refused by apt, because the `linux-image-generic` meta
still depended on it. The meta is HELD (`hold ok installed`), and the planner's remote read counted
only `install ok installed` as installed, so the meta never entered the stale set. These tests run
the planner's own remote read (`rt_kernel_gather_remote_snippet`) locally against a fake dpkg-query
that carries cam2's real package states.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "rt-kernel-plan.sh"
DRIVER = REPO / "scripts" / "rt-kernel-upgrade.sh"

# cam2's dpkg states read 18.10.2026 (dpkg-query -W -f '${Package} ${Status}').
CAM2 = {
    "linux-image-6.8.0-142-generic": "install ok installed",
    "linux-image-7.0.0-38-generic": "install ok installed",
    "linux-image-generic": "hold ok installed",
    "linux-image-generic-hwe-24.04": "install ok installed",
    "lowlatency-kernel": "install ok installed",
}

FAKE_DPKG = r'''#!/usr/bin/env python3
import fnmatch, json, os, sys
db = json.load(open(os.environ["FAKE_DPKG_DB"]))
args = sys.argv[1:]
fmt = None
pats = []
i = 0
while i < len(args):
    a = args[i]
    if a == "-W":
        pass
    elif a.startswith("-f="):
        fmt = a[3:]
    elif a == "-f":
        i += 1
        fmt = args[i]
    elif a.startswith("--showformat="):
        fmt = a[len("--showformat="):]
    else:
        pats.append(a)
    i += 1
hit = False
for pat in pats:
    for name in sorted(db):
        if fnmatch.fnmatchcase(name, pat):
            hit = True
            out = fmt.replace("${Package}", name).replace("${Status}", db[name]).replace("\\n", "\n")
            sys.stdout.write(out)
sys.exit(0 if hit else 1)
'''


def _stub_dir(tmp_path, db, running="7.0.0-38-generic"):
    d = tmp_path / "bin"
    d.mkdir()
    (tmp_path / "db.json").write_text(json.dumps(db))
    for name, body in (("dpkg-query", "#!%s\n%s" % (sys.executable, FAKE_DPKG.split("\n", 1)[1])),
                       ("uname", "#!/bin/sh\necho %s\n" % running),
                       ("apt-cache", "#!/bin/sh\necho '  Candidate: 7.0.0-38.38'\n")):
        p = d / name
        p.write_text(body)
        p.chmod(0o755)
    return d


def _gather(tmp_path, db, running="7.0.0-38-generic"):
    d = _stub_dir(tmp_path, db, running)
    env = dict(os.environ, PATH="%s:%s" % (d, os.environ["PATH"]), FAKE_DPKG_DB=str(tmp_path / "db.json"))
    snip = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_gather_remote_snippet', "h", str(LIB)],
                          capture_output=True, text=True, timeout=30)
    assert snip.returncode == 0, snip.stderr
    r = subprocess.run(["bash", "-c", snip.stdout], capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode == 0, r.stderr
    fields = r.stdout.split("\n")[-2 if r.stdout.endswith("\n") else -1].split()
    assert len(fields) == 6, r.stdout
    return dict(zip(("run", "inst", "gen", "grub", "cand", "stale"), fields))


def test_a_held_generic_meta_is_in_the_stale_set(tmp_path):
    f = _gather(tmp_path, CAM2)
    assert f["stale"].split(",") == ["6.8.0-142-generic", "linux-image-generic"], f


def test_a_held_old_image_is_stale_and_the_running_one_never(tmp_path):
    db = dict(CAM2, **{"linux-image-6.8.0-142-generic": "hold ok installed"})
    f = _gather(tmp_path, db)
    assert "6.8.0-142-generic" in f["stale"].split(","), f
    assert "7.0.0-38-generic" not in f["stale"], f


def test_a_removed_package_with_config_files_left_is_not_installed(tmp_path):
    db = {"linux-image-6.8.0-134-generic": "deinstall ok config-files",
          "linux-image-7.0.0-38-generic": "install ok installed",
          "linux-image-generic-hwe-24.04": "install ok installed",
          "lowlatency-kernel": "install ok installed"}
    f = _gather(tmp_path, db)
    assert f["stale"] == "-", f


def test_held_lowlatency_and_hwe_meta_read_as_installed(tmp_path):
    db = dict(CAM2, **{"lowlatency-kernel": "hold ok installed",
                       "linux-image-generic-hwe-24.04": "hold ok installed"})
    f = _gather(tmp_path, db)
    assert f["inst"] == "1" and f["gen"] == "0", f
    assert f["cand"] == "1", f


@pytest.mark.parametrize("status,want", [("install ok installed", 0), ("hold ok installed", 0),
                                         ("deinstall ok config-files", 1), ("install ok half-installed", 1),
                                         ("hold ok not-installed", 1), ("", 1)])
def test_the_installed_predicate(status, want):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_pkg_installed "$2" && echo yes || echo no',
                        "h", str(LIB), status], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == ("yes" if want == 0 else "no")


def test_the_driver_sends_the_lib_snippet_not_an_inline_copy():
    src = DRIVER.read_text()
    assert '"$(rt_kernel_gather_remote_snippet)"' in src
    assert "install ok installed" not in src


# --- the fleet kernel line is HELD wherever it is installed (issue 1446 follow-up) ---------------

def _step_command(token):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_step_command "$2"', "h", str(LIB), token],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _held_metas():
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_line_held_metas', "h", str(LIB)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.split()


def test_the_planner_install_step_holds_the_line_it_installs():
    cmd = _step_command("install-lowlatency")
    metas = _held_metas()
    work = cmd[cmd.index("_rt_work() {"):cmd.index("\n}", cmd.index("_rt_work() {"))]
    assert "linux-lowlatency-hwe-24.04 && apt-mark hold %s" % " ".join(metas) in work, work


def test_a_glob_that_matches_no_image_reads_no_stale(tmp_path):
    db = {"linux-image-generic-hwe-24.04": "install ok installed", "lowlatency-kernel": "install ok installed"}
    f = _gather(tmp_path, db)
    assert f["stale"] == "-", f


def test_an_unsigned_image_never_enters_the_stale_set(tmp_path):
    # review: the glob also matches linux-image-unsigned-<ver>-generic; as a stale entry it named
    # linux-modules-unsigned-... packages that do not exist, and apt refused the purge
    db = dict(CAM2, **{"linux-image-unsigned-6.8.0-142-generic": "install ok installed"})
    f = _gather(tmp_path, db)
    assert f["stale"].split(",") == ["6.8.0-142-generic", "linux-image-generic"], f


@pytest.mark.parametrize("status", ["install ok triggers-pending", "install ok triggers-awaited",
                                    "hold ok triggers-pending"])
def test_a_package_with_pending_triggers_is_installed(status):
    # its files are unpacked and configured; only a trigger (initramfs) has not run yet
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_pkg_installed "$2" && echo yes || echo no',
                        "h", str(LIB), status], capture_output=True, text=True, timeout=30)
    assert r.stdout.strip() == "yes", (status, r.stdout, r.stderr)


def _unheld(tmp_path, db):
    d = _stub_dir(tmp_path, db)
    env = dict(os.environ, PATH="%s:%s" % (d, os.environ["PATH"]), FAKE_DPKG_DB=str(tmp_path / "db.json"))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; rt_kernel_line_unheld_metas', "h", str(LIB)],
                       capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.split()


def test_unheld_metas_names_an_installed_line_meta_that_is_not_held(tmp_path):
    db = {"linux-lowlatency-hwe-24.04": "install ok installed", "linux-image-generic-hwe-24.04": "hold ok installed",
          "linux-headers-generic-hwe-24.04": "hold ok installed", "lowlatency-kernel": "install ok installed"}
    assert _unheld(tmp_path, db) == ["linux-lowlatency-hwe-24.04", "lowlatency-kernel"]


def test_unheld_metas_is_empty_when_every_installed_meta_is_held_or_absent(tmp_path):
    db = {"linux-lowlatency-hwe-24.04": "hold ok installed", "linux-image-generic-hwe-24.04": "hold ok installed",
          "linux-headers-generic-hwe-24.04": "hold ok not-installed", "lowlatency-kernel": "hold ok installed"}
    assert _unheld(tmp_path, db) == []


def test_the_lib_sources_alone_under_strict_mode_from_any_cwd(tmp_path):
    # setup-device sources it under set -euo pipefail; its lazy sibling loads must resolve by path
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; cd "$2"; . "$1"; declare -F ro_window_close_cmds ro_root_mount_mode',
                        "h", str(LIB), str(tmp_path)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout.split() == ["ro_window_close_cmds", "ro_root_mount_mode"], r.stderr
