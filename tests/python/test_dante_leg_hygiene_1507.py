"""Issue 1507 (dstack#155, two networks on one PC): nothing but Dante on the Dante leg.

On stream-snv a gvfsd-started `wsdd` (Windows network browsing) bound UDP 3702 on the Dante address
10.77.7.204, and the leg profile kept the default route metric. A box whose leg carries Dante devices
(the /24 variant) declares `ipv4.route-metric 900` and has no wsdd; the wsdd removal refuses when apt
would take anything else with it."""

import os
import re
import stat
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vban_leg_fakes_1439 as fk  # noqa: E402

REPO = HERE.parent.parent
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"
LEG_LIB = REPO / "scripts" / "lib" / "strih-vban-leg.sh"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN")


def _bash(body, *args, path_prefix=None):
    e = fk.os_env_without(LEAKY)
    if path_prefix:
        e["PATH"] = "%s:%s" % (path_prefix, e["PATH"])
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _stub(d, name, text):
    p = d / name
    p.write_text("#!/bin/bash\n" + text)
    p.chmod(p.stat().st_mode | stat.S_IXUSR)


def test_a_dante_leg_declares_route_metric_900_1507():
    r = _bash('strih_box_load stream-snv\nstrih_vban_leg_nm_props enp56s0.70 10.77.7.204 enp56s0 70')
    assert r.returncode == 0, r.stderr
    assert "ipv4.route-metric\t900" in r.stdout.splitlines(), r.stdout


def _wsdd_stubs(tmp_path, installed, sim):
    d = tmp_path / "bin"
    d.mkdir()
    log = tmp_path / "apt.log"
    _stub(d, "dpkg-query", 'echo "%s"\n' % ("install ok installed" if installed else "unknown ok not-installed"))
    _stub(d, "apt-get", 'echo "$*" >> "%s"\nif [[ " $* " == *" -s "* ]]; then printf "%%s\\n" %s; fi\n'
          % (log, " ".join("'%s'" % x for x in sim)))
    return d, log


def test_wsdd_is_purged_when_apt_takes_nothing_else_1507(tmp_path):
    d, log = _wsdd_stubs(tmp_path, True, ["Purg wsdd [2:0.8-5ubuntu1]"])
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = log.read_text().splitlines()
    assert any(re.search(r"\bpurge\b.*\bwsdd\b", c) and " -s " not in f" {c} " for c in calls), calls


def test_wsdd_removal_refuses_when_apt_would_take_more_1507(tmp_path):
    d, log = _wsdd_stubs(tmp_path, True, ["Purg wsdd [2:0.8-5ubuntu1]", "Remv gvfs-backends [1.54]"])
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode != 0 and "gvfs-backends" in r.stdout + r.stderr, r.stdout + r.stderr
    assert all(" -s " in f" {c} " for c in log.read_text().splitlines())


def test_an_absent_wsdd_is_a_no_op_1507(tmp_path):
    d, log = _wsdd_stubs(tmp_path, False, [])
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode == 0, r.stderr
    assert not log.exists()


def test_the_leg_install_removes_wsdd_on_a_dante_leg_1507():
    text = LEG_LIB.read_text()
    body = text[text.index("strih_vban_leg_install() {"):]
    body = body[:body.index("\n}\n")]
    assert "strih_vban_leg_wsdd_remove" in body
    assert body.index("strih_vban_leg_wsdd_remove") < body.index("_strih_vban_leg_nm_apply")


def _proc_stubs(tmp_path, pids):
    """A pgrep that lists `pids` for the wsdd pattern and a kill that logs its arguments."""
    d, log = _wsdd_stubs(tmp_path, False, [])
    klog = tmp_path / "kill.log"
    _stub(d, "pgrep", 'echo "$*" >> "%s"\nprintf "%%s\\n" %s\n[ -n "%s" ]\n'
          % (klog, " ".join(pids), " ".join(pids)))
    _stub(d, "kill", 'echo "kill $*" >> "%s"\n' % klog)
    return d, klog


def test_a_wsdd_still_running_after_its_package_went_is_stopped_1507(tmp_path):
    # live stream-snv 12.11.2026: the package was purged, but the wsdd gvfsd started on 1.11 (its
    # binary deleted) still held UDP 3702 on the Dante address 10.77.7.204.
    d, klog = _proc_stubs(tmp_path, ["16817"])
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = klog.read_text().splitlines()
    assert any(c.startswith("kill") and "16817" in c for c in calls), calls
    assert "16817" in r.stdout, r.stdout


def test_no_running_wsdd_kills_nothing_1507(tmp_path):
    d, klog = _proc_stubs(tmp_path, [])
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not klog.exists() or not any(c.startswith("kill") for c in klog.read_text().splitlines())


def _grade_wsdd(tmp_path, ss_out):
    d = tmp_path / "gbin"
    d.mkdir()
    _stub(d, "ss", 'cat <<\'EOF\'\n%sEOF\n' % ss_out)
    body = ('ok() { echo "OK $*"; }; bad() { echo "FAIL $*"; }; note() { echo "NOTE $*"; }\n'
            '_strih_vban_leg_wsdd_grade 10.77.7.204')
    return _bash(body, path_prefix=d)


def test_verify_fails_a_socket_on_udp_3702_of_the_dante_address_1507(tmp_path):
    r = _grade_wsdd(tmp_path, 'UNCONN 0 0 10.77.7.204:3702 0.0.0.0:* users:(("python3",pid=16817,fd=18))\n'
                              'UNCONN 0 0 10.77.9.204:3702 0.0.0.0:* users:(("python3",pid=16817,fd=10))\n')
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("FAIL (vban-leg) wsdd") and "16817" in r.stdout, r.stdout


def test_verify_passes_a_leg_without_ws_discovery_1507(tmp_path):
    r = _grade_wsdd(tmp_path, 'UNCONN 0 0 10.77.9.204:5353 0.0.0.0:* users:(("avahi",pid=1,fd=1))\n')
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("OK (vban-leg) wsdd"), r.stdout


def test_the_leg_grade_runs_the_wsdd_row_on_a_dante_leg_1507():
    text = LEG_LIB.read_text()
    body = text[text.index("strih_vban_leg_grade() {"):]
    body = body[:body.index("\n}\n")]
    assert "_strih_vban_leg_wsdd_grade" in body


def test_verify_fails_a_wildcard_or_scoped_3702_socket_too_1507(tmp_path):
    # review of the release PR: a wsdd bound to every address (0.0.0.0 / *) or shown as addr%iface also
    # answers on the Dante address.
    for i, sock in enumerate(("0.0.0.0:3702", "*:3702", "10.77.7.204%enp56s0.70:3702", "[::]:3702")):
        d = tmp_path / str(i)
        d.mkdir()
        r = _grade_wsdd(d, 'UNCONN 0 0 %s 0.0.0.0:* users:(("python3",pid=7,fd=3))\n' % sock)
        assert r.stdout.startswith("FAIL (vban-leg) wsdd"), (sock, r.stdout)


def test_the_leftover_stop_comes_after_a_refused_purge_never_before_1507(tmp_path):
    # review: a refused purge says "nothing changed", so the leftover wsdd is stopped only once the
    # package side is settled.
    d, log = _wsdd_stubs(tmp_path, True, ["Purg wsdd [2:0.8-5ubuntu1]", "Remv gvfs-backends [1.54]"])
    klog = tmp_path / "kill.log"
    _stub(d, "pgrep", 'echo "$*" >> "%s"\necho 16817\n' % klog)
    _stub(d, "kill", 'echo "kill $*" >> "%s"\n' % klog)
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode != 0, r.stdout + r.stderr
    assert not klog.exists() or not [c for c in klog.read_text().splitlines() if c.startswith("kill")], klog.read_text()


def test_the_leftover_stop_waits_for_the_process_to_go_and_says_so_1507(tmp_path):
    # wsdd catches SIGTERM and needs a few seconds (live stream-snv 12.11.2026: still there 2 s later).
    d, _log = _wsdd_stubs(tmp_path, False, [])
    count = tmp_path / "n"
    count.write_text("0")
    klog = tmp_path / "kill.log"
    _stub(d, "pgrep", 'n=$(cat "%s"); echo $((n+1)) > "%s"\n[ "$n" -lt 3 ] && echo 16817 || exit 1\n' % (count, count))
    _stub(d, "kill", 'echo "kill $*" >> "%s"\n' % klog)
    _stub(d, "sleep", 'exit 0\n')
    r = _bash("strih_vban_leg_wsdd_remove", path_prefix=d)
    assert r.returncode == 0, r.stdout + r.stderr
    assert int(count.read_text()) >= 3, "the stop polls until the process is gone"
    assert "wsdd stopped" in r.stdout, r.stdout
