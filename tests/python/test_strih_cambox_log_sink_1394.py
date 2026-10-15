"""Issue 1394 (owner ROZHODNUTE 9.10.2026): the cambox off-box log sink lives on the venue strih box.

scripts/lib/strih-cambox-log-sink.sh is the receiving half (the cambox half is
scripts/lib/remote-logging.sh): setup-strih step 16k installs it enable-only, verify-strih item 46
grades it. These tests pin:
  * the renderers: a DEDICATED rsyslog instance (imudp on the netconsole port, one file per camera of
    the loaded box, the rest in other-senders.log, its config in a SUBDIRECTORY of /etc/rsyslog.d for
    the enforced AppArmor profile) + logrotate every 10 min; systemd-journal-remote switched to plain
    HTTP and capped hard; every receiver at idle priority, on the E-cores of a hybrid CPU;
  * the install: packages only when missing, write-if-changed, enable-only (never a start), a
    try-restart of a running unit whose files changed, nothing on a second run;
  * the grader rows and the setup / verify wiring, the stream role never a sink, the line budgets.
"""

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "strih-cambox-log-sink.sh"
SETUP = REPO / "scripts" / "setup-strih.sh"
VERIFY = REPO / "scripts" / "verify-strih.sh"
UNITS = ["systemd-journal-remote.socket", "cambox-netconsole-sink.service", "cambox-netconsole-rotate.timer"]


def _bash(body, *args, env=None, box="strih-snv", check=True):
    e = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/tmp"), "LIB": str(LIB), "BOX": box}
    e.update(env or {})
    script = 'set -euo pipefail\n. "$LIB"\nstrih_box_load "$BOX"\n' + body
    r = subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True, env=e, timeout=120)
    if check:
        assert r.returncode == 0, f"rc={r.returncode}\nstdout={r.stdout}\nstderr={r.stderr}"
    assert "unbound variable" not in r.stderr, r.stderr
    return r


def _fact(box, key):
    for line in (REPO / "scripts" / "strih-boxes" / f"{box}.env").read_text().splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1]
    raise AssertionError(key)


# --- the renderers -----------------------------------------------------------------------------------------

@pytest.mark.parametrize("box", ["strih-snv", "strih-pp"])
def test_the_rsyslog_instance_writes_one_file_per_camera_of_the_box(box):
    conf = _bash("strih_cambox_log_sink_rsyslog_conf", box=box).stdout
    assert 'module(load="imudp")' in conf and 'input(type="imudp" port="514" ruleset="cambox_netconsole")' in conf
    pairs = re.findall(r'if \$fromhost-ip == "([\d.]+)" then \{\n\s+action\(type="omfile" file="/var/log/cambox/(cam\d)-kernel\.log"', conf)
    want = [(f"10.77.9.6{n}", f"cam{n}") for n in _fact(box, "STRIH_CAMERAS").split()]
    assert pairs == want, conf
    assert conf.count("stop") == len(want)
    assert conf.rstrip().endswith('action(type="omfile" file="/var/log/cambox/other-senders.log" template="camboxKernelLine")\n}')
    assert "workDirectory" not in conf and "imuxsock" not in conf and "imjournal" not in conf
    assert '%msg%\\n"' in conf


def test_the_rsyslog_config_sits_in_a_subdirectory_of_rsyslog_d():
    # The enforced AppArmor profile of /usr/sbin/rsyslogd reads only /etc/rsyslog.d/**; the box's own
    # rsyslog loads /etc/rsyslog.d/*.conf, so a subdirectory is read by this instance alone.
    files = _bash("strih_cambox_log_sink_files").stdout.splitlines()
    conf = [ln.split("\t")[0] for ln in files if ln.endswith("strih_cambox_log_sink_rsyslog_conf")]
    assert conf == ["/etc/rsyslog.d/cambox-netconsole-sink/rsyslog.conf"], files
    unit = _bash('strih_cambox_log_sink_unit_text ""').stdout
    assert "ExecStart=/usr/sbin/rsyslogd -n -iNONE -f /etc/rsyslog.d/cambox-netconsole-sink/rsyslog.conf" in unit


def test_the_ports_are_the_cambox_sides_own_declaration():
    code = "\n".join(l for l in LIB.read_text().splitlines() if not l.lstrip().startswith("#"))
    assert not re.search(r"\b(514|19532)\b", code), "the ports come from scripts/lib/remote-logging.sh"
    jr = _bash('strih_cambox_log_sink_jr_dropin_text ""', env={"REMOTE_LOG_JOURNAL_PORT": "19999"}).stdout
    assert ":19999" in jr
    conf = _bash("strih_cambox_log_sink_rsyslog_conf", env={"REMOTE_LOG_NETCONSOLE_PORT": "5514"}).stdout
    assert 'port="5514"' in conf


def test_journal_remote_listens_http_and_is_capped_hard():
    d = _bash('strih_cambox_log_sink_jr_dropin_text ""').stdout
    assert "ExecStart=\nExecStart=/usr/lib/systemd/systemd-journal-remote --listen-http=-3 --output=/var/log/journal/remote/\n" in d
    c = _bash("strih_cambox_log_sink_jr_conf_text").stdout
    for want in ("[Remote]", "SplitMode=host", "Seal=false", "MaxUse=4G", "KeepFree=20G", "MaxFileSize=128M"):
        assert want in c.splitlines(), (want, c)
    files = _bash("strih_cambox_log_sink_files").stdout
    assert "/etc/systemd/journal-remote.conf.d/10-cambox-sink.conf\tstrih_cambox_log_sink_jr_conf_text" in files
    assert "/etc/systemd/journal-remote.conf\t" not in files, "never the package's conffile"


def test_the_netconsole_files_rotate_every_10_min_with_a_size_bound():
    lr = _bash("strih_cambox_log_sink_logrotate_conf").stdout
    for want in ("/var/log/cambox/*.log {", "su syslog syslog", "size 16M", "rotate 4", "compress", "copytruncate"):
        assert want in lr, (want, lr)
    svc = _bash('strih_cambox_log_sink_rotate_service_text ""').stdout
    assert "ExecStart=/usr/sbin/logrotate --state /var/lib/cambox-netconsole-rotate/logrotate.status " \
           "/etc/camera-box/cambox-log-sink/logrotate.conf" in svc
    timer = _bash("strih_cambox_log_sink_rotate_timer_text").stdout
    assert "OnUnitActiveSec=10min" in timer and "WantedBy=timers.target" in timer


def test_every_receiver_runs_at_idle_priority_on_the_e_cores(tmp_path):
    sysfs = tmp_path / "sys"
    (sysfs / "devices" / "cpu_atom").mkdir(parents=True)
    (sysfs / "devices" / "cpu_atom" / "cpus").write_text("12-15\n")
    for fn in ("strih_cambox_log_sink_unit_text", "strih_cambox_log_sink_rotate_service_text",
               "strih_cambox_log_sink_jr_dropin_text"):
        text = _bash(f'{fn} "$(strih_log_sink_ecores)"', env={"STRIH_LOG_SINK_SYSFS": str(sysfs)}).stdout
        for want in ("Nice=19", "CPUSchedulingPolicy=idle", "IOSchedulingClass=idle", "CPUAffinity=12-15"):
            assert want in text.splitlines(), (fn, want)
        # an AMD box (strih-snv) has no cpu_atom: no affinity line, every cpu
        amd = _bash(f'{fn} "$(strih_log_sink_ecores)"', env={"STRIH_LOG_SINK_SYSFS": str(tmp_path / "none")}).stdout
        assert "CPUAffinity" not in amd and "CPUSchedulingPolicy=idle" in amd


def test_the_units_are_valid_for_systemd(tmp_path):
    exe = shutil.which("systemd-analyze")
    assert exe, "systemd-analyze validates the units (systemd on dev1 and the CI runner)"
    d = tmp_path / "units"
    d.mkdir()
    sink = _bash('strih_cambox_log_sink_unit_text "12-15"').stdout
    (d / "cambox-netconsole-sink.service").write_text(re.sub(r"(?m)^ExecStart=.*$", "ExecStart=/bin/true", sink))
    rot = _bash('strih_cambox_log_sink_rotate_service_text ""').stdout
    (d / "cambox-netconsole-rotate.service").write_text(re.sub(r"(?m)^ExecStart=.*$", "ExecStart=/bin/true", rot))
    (d / "cambox-netconsole-rotate.timer").write_text(_bash("strih_cambox_log_sink_rotate_timer_text").stdout)
    for f in sorted(d.iterdir()):
        r = subprocess.run([exe, "verify", "--man=no", str(f)], capture_output=True, text=True, timeout=60)
        assert r.returncode == 0, (f.name, r.stdout + r.stderr)


# --- the install --------------------------------------------------------------------------------------------

_FAKE_SYSTEMCTL = r'''
import os, sys
open(os.environ["FAKE_LOG"], "a").write("systemctl " + " ".join(sys.argv[1:]) + "\n")
verb = sys.argv[1] if len(sys.argv) > 1 else ""
if verb == "is-enabled":
    print(os.environ.get("FAKE_ENABLED", "enabled")); sys.exit(0)
if verb == "is-active":
    unit = sys.argv[2]
    print("inactive" if unit in os.environ.get("FAKE_INACTIVE", "").split() else "active"); sys.exit(0)
sys.exit(int(os.environ.get("FAKE_SYSTEMCTL_RC", "0")))
'''

_FAKE_DPKG = r'''
import os, sys
pkg = sys.argv[-1]
if pkg in os.environ.get("FAKE_INSTALLED", "").split():
    print("install ok installed", end="")
else:
    sys.exit(1)
'''

_FAKE_APT = r'''
import os, sys
open(os.environ["FAKE_LOG"], "a").write("apt-get " + " ".join(sys.argv[1:]) + "\n")
'''

_FAKE_SS = r'''
import os, sys
args = " ".join(sys.argv[1:])
port = args.split("sport = :")[-1]
if port not in os.environ.get("FAKE_DOWN_PORTS", "").split():
    print(f"0.0.0.0:{port}")
'''


def _rig(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    for name, body in (("systemctl", _FAKE_SYSTEMCTL), ("dpkg-query", _FAKE_DPKG), ("apt-get", _FAKE_APT),
                       ("ss", _FAKE_SS)):
        (bindir / name).write_text(f"#!{sys.executable}\n{body}")
        (bindir / name).chmod(0o755)
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "FAKE_LOG": str(tmp_path / "calls"),
           "STRIH_LOG_SINK_UNIT_DIR": str(tmp_path / "etc" / "systemd" / "system"),
           "STRIH_LOG_SINK_CONF_DIR": str(tmp_path / "etc" / "camera-box" / "cambox-log-sink"),
           "STRIH_LOG_SINK_RSYSLOG_DIR": str(tmp_path / "etc" / "rsyslog.d" / "cambox-netconsole-sink"),
           "STRIH_LOG_SINK_JR_CONF_DIR": str(tmp_path / "etc" / "systemd" / "journal-remote.conf.d"),
           "STRIH_LOG_SINK_SYSFS": str(tmp_path / "sys")}
    (tmp_path / "calls").write_text("")
    return env


def _calls(tmp_path):
    return (tmp_path / "calls").read_text().splitlines()


def test_install_writes_every_file_and_only_enables(tmp_path):
    env = _rig(tmp_path)
    r = _bash("strih_cambox_log_sink_install", env=env)
    calls = _calls(tmp_path)
    assert "apt-get install -y systemd-journal-remote rsyslog logrotate" in calls, calls
    files = _bash("strih_cambox_log_sink_files", env=env).stdout.splitlines()
    assert len(files) == 7
    for line in files:
        path, fn = line.split("\t")
        want = _bash(f'{fn} "$(strih_log_sink_ecores)"', env=env).stdout
        assert Path(path).read_text() == want, path
        assert oct(Path(path).stat().st_mode & 0o777) == "0o644", path
    assert "systemctl daemon-reload" in calls
    assert "systemctl enable " + " ".join(UNITS) in calls, calls
    assert "systemctl try-restart cambox-netconsole-sink.service systemd-journal-remote.service " \
           "cambox-netconsole-rotate.timer" in calls, calls
    assert not any(c.startswith(("systemctl start", "systemctl restart")) or "--now" in c for c in calls), calls
    assert "enable-only" in r.stdout


def test_a_second_install_changes_nothing(tmp_path):
    env = _rig(tmp_path)
    env["FAKE_INSTALLED"] = "systemd-journal-remote rsyslog logrotate"
    _bash("strih_cambox_log_sink_install", env=env)
    (tmp_path / "calls").write_text("")
    r = _bash("strih_cambox_log_sink_install", env=env)
    calls = _calls(tmp_path)
    assert calls == ["systemctl enable " + " ".join(UNITS)], calls
    assert r.stdout.count(" unchanged") == 7, r.stdout


def test_install_fails_loud_on_a_failed_package_install_or_enable(tmp_path):
    env = _rig(tmp_path)
    env["PATH"] = f"{tmp_path / 'bin'}:/usr/bin:/bin"
    (tmp_path / "bin" / "apt-get").write_text("#!/bin/sh\nexit 100\n")
    r = _bash("strih_cambox_log_sink_install", env=env, check=False)
    assert r.returncode == 1 and "apt-get install" in r.stderr, r.stderr
    (tmp_path / "b").mkdir()
    env2 = _rig(tmp_path / "b")
    env2["FAKE_INSTALLED"] = "systemd-journal-remote rsyslog logrotate"
    env2["FAKE_SYSTEMCTL_RC"] = "1"
    r = _bash("strih_cambox_log_sink_install", env=env2, check=False)
    assert r.returncode == 1 and "failed" in r.stderr, r.stderr


def test_the_stream_role_is_never_a_sink():
    r = _bash('if WHY="$(obs_box_step_applies 16k)"; then echo APPLIES; else echo "SKIP: $WHY"; fi',
              env={"LIB": str(REPO / "scripts" / "lib" / "strih-provision.sh")}, box="stream-pp")
    assert r.stdout.startswith("SKIP:") and "issue 1394" in r.stdout, r.stdout
    for box in ("strih-snv", "strih-pp"):
        r = _bash('if obs_box_step_applies 16k >/dev/null; then echo APPLIES; fi',
                  env={"LIB": str(REPO / "scripts" / "lib" / "strih-provision.sh")}, box=box)
        assert r.stdout.strip() == "APPLIES", box


# --- the grader ----------------------------------------------------------------------------------------------

def _rows(env):
    return _bash("strih_cambox_log_sink_grade_rows", env=env).stdout.splitlines()


def test_the_grader_passes_an_installed_running_sink(tmp_path):
    env = _rig(tmp_path)
    env["FAKE_INSTALLED"] = "systemd-journal-remote rsyslog logrotate"
    _bash("strih_cambox_log_sink_install", env=env)
    rows = _rows(env)
    assert len(rows) == 3 and all(r.startswith("OK|") for r in rows), rows
    assert rows[0].startswith("OK|(log-sink-files)") and rows[2].startswith("OK|(log-sink-listen)")


def test_the_grader_fails_a_drifted_file_a_stopped_unit_and_a_missing_listener(tmp_path):
    env = _rig(tmp_path)
    env["FAKE_INSTALLED"] = "systemd-journal-remote rsyslog logrotate"
    _bash("strih_cambox_log_sink_install", env=env)
    conf = Path(env["STRIH_LOG_SINK_JR_CONF_DIR"]) / "10-cambox-sink.conf"
    conf.write_text(conf.read_text().replace("MaxUse=4G", "MaxUse=400G"))
    env.update({"FAKE_INACTIVE": "cambox-netconsole-sink.service", "FAKE_DOWN_PORTS": "514"})
    rows = _rows(env)
    assert rows[0].startswith("FAIL|(log-sink-files)") and "10-cambox-sink.conf=differs" in rows[0], rows
    assert rows[1].startswith("FAIL|(log-sink-units)") and "cambox-netconsole-sink.service=enabled/inactive" in rows[1]
    assert "systemctl start" in rows[1]
    assert rows[2].startswith("FAIL|(log-sink-listen)") and "UDP:514" in rows[2] and "TCP" not in rows[2], rows
    env["FAKE_INSTALLED"] = "rsyslog logrotate"
    assert "missing packages: systemd-journal-remote" in _rows(env)[0]


def test_the_report_counts_its_rows_through_the_callers_functions(tmp_path):
    env = _rig(tmp_path)
    r = _bash('ok() { echo "PASS $1"; }; bad() { echo "BAD $1"; }; note() { echo "NOTE $1"; }\n'
              'strih_cambox_log_sink_grade_report; echo rc=$?', env=env)
    lines = r.stdout.splitlines()
    assert lines[-1] == "rc=0" and len(lines) == 4, lines


# --- the wiring ---------------------------------------------------------------------------------------------

def test_setup_and_verify_wire_the_step_and_the_item():
    setup = SETUP.read_text()
    verify = VERIFY.read_text()
    i = setup.index('step "16k" ')
    body = setup[i:setup.index("\nstep ", i + 1)]
    assert 'if WHY="$(obs_box_step_applies 16k)"; then strih_cambox_log_sink_install' in body
    assert setup.index('step "16j"') < i < setup.index("step 17 ")
    j = verify.index("# 46) issue 1394")
    line = verify[verify.rindex("\n", 0, j) + 1:verify.index("\n", j)]
    assert 'if WHY="$(obs_box_step_applies 16k)"; then strih_cambox_log_sink_grade_report' in line
    assert verify.index("# 45) issue 1399") < j < verify.index("\n# 16) NIC")
    assert len(setup.splitlines()) <= 1000 and len(verify.splitlines()) <= 1000
    prov = (REPO / "scripts" / "lib" / "strih-provision.sh").read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-cambox-log-sink.sh"' in prov
