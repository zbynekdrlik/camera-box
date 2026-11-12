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
