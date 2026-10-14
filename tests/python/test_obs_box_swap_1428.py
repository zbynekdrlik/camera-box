"""issue 1428 (owner 14.10.2026): every Linux OBS box carries swap of at least its RAM size.

Owner (verbatim): "To ze tam nieje swap je hrube a opakovane porusenie pravidiel pre linux boxy ...
aby som to neriesil dokolecka ze niekde sa urobil novy linuxovy box a nema swap a crashuje na
nedostatku pamete!!!!" -- stream-pp had none, and the OOM killer took the strih OBS browser source
and restreamer on 14.10.2026.

obs_box_swap (scripts/lib/obs-box-baseline.sh) runs in setup-strih and setup-imag; the shared grader
row `swap` (scripts/lib/obs-box-baseline-verify.sh) FAILS a box without it. The tools are stubs on
PATH and the file paths are seams, so nothing real is touched.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"

GIB = 1024 ** 3


def _box(tmp, fs="ext4", mem_kb=15392 * 1024, active=""):
    b = tmp / "bin"
    b.mkdir()
    log = tmp / "calls"
    stubs = {
        "findmnt": "echo %s" % fs,
        "fallocate": 'echo "fallocate $*" >> %s; : > "${@: -1}"' % log,
        "chmod": 'echo "chmod $*" >> %s' % log,
        "mkswap": 'echo "mkswap $*" >> %s' % log,
        "swapon": 'case "$*" in *--show*) cat %s/active 2>/dev/null ;; *) echo "swapon $*" >> %s; echo "%s/swapfile" > %s/active ;; esac'
                  % (tmp, log, tmp, tmp),
    }
    for name, body in stubs.items():
        p = b / name
        p.write_text("#!/bin/bash\n" + body + "\nexit 0\n")
        p.chmod(0o755)
    (tmp / "meminfo").write_text("MemTotal:       %d kB\n" % mem_kb)
    (tmp / "fstab").write_text("UUID=x / ext4 defaults 0 1\n")
    if active:
        (tmp / "active").write_text(active + "\n")
    env = dict(os.environ)
    env.update({"PATH": "%s:%s" % (b, env["PATH"]), "OBS_BOX_SWAPFILE": str(tmp / "swapfile"),
                "OBS_BOX_FSTAB": str(tmp / "fstab"), "OBS_BOX_MEMINFO": str(tmp / "meminfo")})
    return env, log


def _run(env, body):
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\n' + body, "h", str(BASE)],
                          capture_output=True, text=True, env=env, timeout=60)


def test_a_box_without_swap_gets_a_ram_sized_swapfile_in_fstab(tmp_path):
    env, log = _box(tmp_path)
    r = _run(env, "obs_box_swap")
    assert r.returncode == 0, r.stdout + r.stderr
    calls = log.read_text()
    assert "fallocate -l 16G %s" % (tmp_path / "swapfile") in calls    # 15 GiB RAM rounds up to 16 GiB
    assert "chmod 600" in calls and "mkswap" in calls and "swapon %s" % (tmp_path / "swapfile") in calls
    assert "%s none swap sw 0 0" % (tmp_path / "swapfile") in (tmp_path / "fstab").read_text()


def test_a_second_run_changes_nothing(tmp_path):
    env, log = _box(tmp_path)
    _run(env, "obs_box_swap")
    log.unlink()
    r = _run(env, "obs_box_swap")
    assert r.returncode == 0, r.stderr
    assert not log.exists() or "fallocate" not in log.read_text()
    assert (tmp_path / "fstab").read_text().count("swap sw") == 1


def test_a_root_that_is_not_ext4_fails_loud_and_writes_nothing(tmp_path):
    env, log = _box(tmp_path, fs="btrfs")
    r = _run(env, "obs_box_swap")
    assert r.returncode != 0
    assert "btrfs" in r.stdout + r.stderr
    assert not log.exists()


def _verdict(facts):
    r = subprocess.run(["bash", "-c", '. "$1"\nobs_box_baseline_verdict <<<"$2"', "h", str(VERIFY), facts],
                       capture_output=True, text=True, timeout=60)
    return [ln.split("|") for ln in r.stdout.splitlines() if ln.startswith("swap|")]


def test_the_grader_fails_a_box_without_active_swap():
    good = "gather_done=1\nswap_bytes=%d\nmem_bytes=%d\nswap_fstab=1\n" % (16 * GIB, 15 * GIB)
    assert _verdict(good)[0][1] == "OK"
    assert _verdict(good.replace("swap_bytes=%d" % (16 * GIB), "swap_bytes=0"))[0][1] == "FAIL"
    assert _verdict(good.replace("swap_fstab=1", "swap_fstab=0"))[0][1] == "FAIL"


def test_the_gather_reads_the_swap_facts():
    r = subprocess.run(["bash", "-c", '. "$1"\nobs_box_baseline_gather_snippet strih-snv newlevel strih-obs.service',
                        "h", str(VERIFY)], capture_output=True, text=True, timeout=60)
    for key in ("swap_bytes=", "mem_bytes=", "swap_fstab="):
        assert key in r.stdout, key


def test_both_setups_provision_it():
    assert "obs_box_swap" in (REPO / "scripts" / "setup-strih.sh").read_text()
    assert "obs_box_swap" in (REPO / "scripts" / "setup-imag.sh").read_text()
    assert len((REPO / "scripts" / "setup-strih.sh").read_text().splitlines()) <= 1000
