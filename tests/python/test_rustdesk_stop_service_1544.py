"""issue 1544 -- RustDesk "Stop service" leaves `stop-service = 'Y'`; setup clears it, verify names it.

19.11.2026 18:36 (during the SNV event) someone pressed "Stop service" in the RustDesk window on
stream-snv. RustDesk wrote `stop-service = 'Y'` into [options] of both RustDesk2.toml files and
disabled the unit; from then on the service stopped itself the second it started, and setup-strih
step 16b FAILed with "no permanent password" although the password was set. These tests pin the
fifth state `stopped`, item 29's row for it, the emitter that clears it, and the step-16b wiring.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import test_rustdesk_required_1462 as base  # noqa: E402

SCRIPTS = base.SCRIPTS
OPTS_STOPPED = "rendezvous_server = ''\n\n[options]\nlocal-ip-addr = '10.77.9.204'\nstop-service = 'Y'\nverification-method = 'use-permanent-password'\n"
OPTS_RUNNING = "rendezvous_server = ''\n\n[options]\nlocal-ip-addr = '10.77.9.204'\nverification-method = 'use-permanent-password'\n"


class Box(base.Box):
    def __init__(self, tmp, opts=OPTS_STOPPED, **kw):
        super().__init__(tmp, **kw)
        self.conf2 = tmp / "RustDesk2.toml"
        if opts is not None:
            self.conf2.write_text(opts)

    def run(self, body):
        script = 'set -euo pipefail\n. "$1"\nstrih_box_load stream-snv\n. "$2"\n' + body
        env = {"PATH": "%s:/usr/bin:/bin" % self.bin, "HOME": os.environ.get("HOME", "/tmp"),
               "STRIH_RUSTDESK_CONF": str(self.conf), "STRIH_RUSTDESK_CONF2": str(self.conf2)}
        return subprocess.run(["bash", "-c", script, "h", str(base.FACTS), str(base.PROVISION)],
                              capture_output=True, text=True, env=env, timeout=60)


def test_stop_service_reads_stopped_whatever_the_unit_says(tmp_path):
    for i, active in enumerate(("inactive", "active")):
        d = tmp_path / str(i)
        d.mkdir()
        r = Box(d, active=active).run("strih_rustdesk_state")
        assert r.returncode == 0 and r.stdout == "stopped", (active, r.stdout, r.stderr)


def test_no_option_or_no_file_keeps_the_old_states(tmp_path):
    cases = [(dict(opts=OPTS_RUNNING), "ready"), (dict(opts=None), "ready"),
             (dict(opts=OPTS_RUNNING, active="inactive"), "down"),
             (dict(opts=OPTS_STOPPED, rustdesk=False), "absent")]
    for i, (kw, want) in enumerate(cases):
        d = tmp_path / str(i)
        d.mkdir()
        r = Box(d, **kw).run("strih_rustdesk_state")
        assert r.stdout == want, (kw, r.stdout, r.stderr)


def test_item_29_names_the_option_and_the_cure_never_a_password(tmp_path):
    r = Box(tmp_path).run("strih_rustdesk_grade_row stream-snv")
    assert r.returncode == 0, r.stderr
    st, text = r.stdout.strip().split("|", 1)
    assert st == "FAIL" and "stop-service" in text and "16b" in text
    assert "no permanent password" not in text


def test_the_clear_emitter_removes_only_that_line_and_starts_the_unit(tmp_path):
    root = tmp_path / "root.toml"
    user = tmp_path / "user.toml"
    root.write_text(OPTS_STOPPED)
    user.write_text(OPTS_STOPPED)
    log = tmp_path / "systemctl.log"
    stub = tmp_path / "bin"
    stub.mkdir()
    base._exec(stub / "systemctl", '#!/bin/bash\necho "$*" >> %s\n' % log)
    gen = subprocess.run(["bash", "-c", '. "$1"; strih_rustdesk_clear_stop_cmds "$2" "$3"', "h",
                          str(base.PROVISION), str(root), str(user)],
                         capture_output=True, text=True, timeout=30)
    assert gen.returncode == 0 and gen.stdout, gen.stderr
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + gen.stdout],
                       capture_output=True, text=True, timeout=30,
                       env={"PATH": "%s:/usr/bin:/bin" % stub})
    assert r.returncode == 0, r.stderr
    assert root.read_text() == OPTS_RUNNING and user.read_text() == OPTS_RUNNING
    assert "enable --now rustdesk" in log.read_text()


def test_the_clear_emitter_skips_a_missing_operator_file(tmp_path):
    root = tmp_path / "root.toml"
    root.write_text(OPTS_STOPPED)
    stub = tmp_path / "bin"
    stub.mkdir()
    base._exec(stub / "systemctl", "#!/bin/bash\nexit 0\n")
    gen = subprocess.run(["bash", "-c", '. "$1"; strih_rustdesk_clear_stop_cmds "$2" "$3"', "h",
                          str(base.PROVISION), str(root), str(tmp_path / "absent.toml")],
                         capture_output=True, text=True, timeout=30)
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + gen.stdout],
                       capture_output=True, text=True, timeout=30,
                       env={"PATH": "%s:/usr/bin:/bin" % stub})
    assert r.returncode == 0, r.stderr
    assert root.read_text() == OPTS_RUNNING


def test_step_16b_clears_a_stopped_service_before_it_decides():
    step = base._block(SCRIPTS / "setup-strih.sh", 'step "16b"', 'step "16c"')
    clear = step.index("strih_rustdesk_clear_stop_cmds")
    assert step.index('RD_STATE="$(strih_rustdesk_state)"') < clear
    assert clear < step.index('if [ "$RD_STATE" = ready ]')
    assert step.count('RD_STATE="$(strih_rustdesk_state)"') == 2
